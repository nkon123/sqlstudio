//! 단위 하나를 끝까지: 조각 나누기 → (저장된 결과 재사용) → 조각마다 모델 → 서브프로그램·단위 요약 → 저장.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;

use futures_util::stream::{self, StreamExt};
use serde::{Deserialize, Serialize};
use sqls_llm::{Client, ProviderConfig};

use crate::chunk::{self, fnv, Limits, Plan, UnitSource};
use crate::facts::{self, Facts};
use crate::llm::{self, AskError, Insight, Lang, PROMPT_VERSION};
use crate::store::{now, ChunkResult, Stats, Store, SubResult, UnitResult, FORMAT_VERSION};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Options {
    #[serde(default)]
    pub limits: Limits,
    #[serde(default)]
    pub lang: Lang,
    /// 동시에 물을 조각 수. 로컬 모델은 1 (Ollama 가 OLLAMA_NUM_PARALLEL 로 여러 개를 받게 했으면 늘린다)
    #[serde(default = "one")]
    pub jobs: usize,
    /// 저장된 결과가 있어도 다시 묻는다
    #[serde(default)]
    pub force: bool,
    /// 서브프로그램·단위 요약까지 만든다
    #[serde(default = "yes")]
    pub rollup: bool,
}

fn one() -> usize {
    1
}
fn yes() -> bool {
    true
}

impl Default for Options {
    fn default() -> Self {
        Self { limits: Limits::default(), lang: Lang::Ko, jobs: 1, force: false, rollup: true }
    }
}

/// 진행 상황 (화면·CLI 로 보낸다)
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    Unit { key: String, index: usize, total: usize, chunks: usize },
    Chunk { key: String, id: String, done: u32, total: u32, status: &'static str, elapsed_ms: u64, message: Option<String> },
    Rollup { key: String, what: String },
    UnitDone { key: String, stats: Stats },
}

pub type OnEvent = Arc<dyn Fn(Event) + Send + Sync>;

pub struct Llm<'a> {
    pub client: &'a Client,
    pub cfg: &'a ProviderConfig,
}

/// 공급자 오류가 이만큼 이어지면 멈춘다 (서버가 죽었는데 수천 조각을 하나씩 실패하지 않게)
const MAX_PROVIDER_ERRORS: u32 = 3;

/// 저장 키 — 조각 해시 + 모델 + 프롬프트 버전 + 언어 + 덧붙인 문맥(커서의 뜻). 하나라도 바뀌면 다시 묻는다.
pub fn analysis_key(c: &chunk::Chunk, cfg: &ProviderConfig, lang: Lang, extra: &str) -> String {
    if extra.is_empty() {
        fnv(&[&c.hash, &cfg.model, &PROMPT_VERSION.to_string(), &format!("{lang:?}")])
    } else {
        fnv(&[&c.hash, &cfg.model, &PROMPT_VERSION.to_string(), &format!("{lang:?}"), extra])
    }
}

/// 한 단위를 도는 동안 같이 쓰는 것
struct RunCtx<'a> {
    store: &'a Store,
    key: String,
    label: String,
    total: u32,
    llm: Option<(&'a Client, &'a ProviderConfig)>,
    opts: &'a Options,
    on: OnEvent,
    cancel: Arc<AtomicBool>,
    done: AtomicU32,
    provider_errors: AtomicU32,
    asked: AtomicU32,
    cached: AtomicU32,
    failed: AtomicU32,
}

/// 긴 SQL·커서 요약의 캐시 상태 (단위 결과를 만들기 전부터 쓴다)
struct SumState {
    keys: BTreeMap<String, String>,
    prev: Vec<crate::store::SqlSummary>,
    sums: Vec<crate::store::SqlSummary>,
}

/// 조각 하나: 저장된 결과가 맞으면 그대로, 아니면 모델에 묻고 바로 파일로
async fn run_chunk(rc: &RunCtx<'_>, c: &chunk::Chunk, extra: &str) -> Result<ChunkResult, String> {
    let report = |status: &'static str, ms: u64, msg: Option<String>| {
        let n = rc.done.fetch_add(1, Ordering::Relaxed) + 1;
        (rc.on)(Event::Chunk { key: rc.key.clone(), id: c.id.clone(), done: n, total: rc.total, status, elapsed_ms: ms, message: msg });
    };
    let base = ChunkResult {
        format: FORMAT_VERSION,
        unit: rc.key.clone(),
        chunk: c.clone(),
        analysis_key: None,
        insight: None,
        llm: None,
        error: None,
        raw: None,
        analyzed_at: now(),
    };
    let Some((client, cfg)) = rc.llm else {
        // 이전에 모델로 분석한 결과가 있고 조각이 같으면 그대로 둔다
        let keep = rc.store.read_chunk(&rc.key, &c.id).filter(|o| o.chunk.hash == c.hash && o.insight.is_some());
        let r = keep.unwrap_or(base);
        rc.store.write_chunk(&r).map_err(|e| e.to_string())?;
        report("static", 0, None);
        return Ok(r);
    };
    let ak = analysis_key(c, cfg, rc.opts.lang, extra);
    if !rc.opts.force {
        if let Some(old) = rc.store.read_chunk(&rc.key, &c.id) {
            if old.analysis_key.as_deref() == Some(ak.as_str()) && old.error.is_none() && old.insight.is_some() {
                rc.cached.fetch_add(1, Ordering::Relaxed);
                report("cached", 0, None);
                return Ok(old);
            }
        }
    }
    if rc.cancel.load(Ordering::Relaxed) {
        return Err("중지했습니다".into());
    }
    if rc.provider_errors.load(Ordering::Relaxed) >= MAX_PROVIDER_ERRORS {
        return Err("모델 서버 오류가 이어져 멈췄습니다".into());
    }
    rc.asked.fetch_add(1, Ordering::Relaxed);
    let mut r = ChunkResult { analysis_key: Some(ak), ..base };
    match llm::ask_chunk(client, cfg, &rc.label, c, rc.opts.lang, extra).await {
        Ok((ins, meta)) => {
            rc.provider_errors.store(0, Ordering::Relaxed);
            let ms = meta.elapsed_ms;
            r.insight = Some(ins);
            r.llm = Some(meta);
            rc.store.write_chunk(&r).map_err(|e| e.to_string())?;
            report("done", ms, None);
        }
        Err(AskError::Unreadable { raw, meta }) => {
            rc.provider_errors.store(0, Ordering::Relaxed);
            rc.failed.fetch_add(1, Ordering::Relaxed);
            let ms = meta.elapsed_ms;
            r.error = Some(format!("답을 JSON 으로 읽지 못했습니다 ({}번 시도)", meta.attempts));
            r.raw = Some(raw);
            r.llm = Some(meta);
            rc.store.write_chunk(&r).map_err(|e| e.to_string())?;
            report("failed", ms, r.error.clone());
        }
        Err(AskError::Provider(e)) => {
            rc.provider_errors.fetch_add(1, Ordering::Relaxed);
            rc.failed.fetch_add(1, Ordering::Relaxed);
            r.error = Some(e.clone());
            // 공급자 오류는 결과로 남기되 다음에 다시 묻는다 (error 가 있으면 재시도)
            rc.store.write_chunk(&r).map_err(|e| e.to_string())?;
            report("failed", 0, Some(e));
        }
    }
    Ok(r)
}

async fn run_many(rc: &RunCtx<'_>, list: Vec<(&chunk::Chunk, String)>) -> Vec<Result<ChunkResult, String>> {
    let jobs = if rc.llm.is_some() { rc.opts.jobs.max(1) } else { 8 };
    // Vec 으로 모은다 — 이터레이터 채로 넘기면 tauri::spawn 의 Send 검사에서 수명 추론이 막힌다
    let work: Vec<_> = list.into_iter().map(|(c, extra)| async move { run_chunk(rc, c, &extra).await }).collect();
    stream::iter(work).buffered(jobs).collect().await
}

/// 조각이 쓰는 커서 이름들
fn cursor_refs(c: &chunk::Chunk) -> Vec<String> {
    let mut out: Vec<String> = c.facts.cursors.iter().map(|x| x.name.clone()).collect();
    for s in &c.facts.statements {
        out.extend(s.cursor.iter().cloned());
        out.extend(s.fed_by.iter().map(|f| f.cursor.clone()));
    }
    out.sort();
    out.dedup();
    out
}

/// 커서 선언이 이 조각에서 보이는지 (전역 커서는 어디서나, 서브프로그램의 커서는 그 안과 중첩에서)
fn in_scope(scope: &Option<String>, c: &chunk::Chunk) -> bool {
    match (scope, &c.subprogram) {
        (None, _) => true,
        (Some(s), Some(p)) => p == s || p.starts_with(&format!("{s}.")),
        (Some(_), None) => false,
    }
}

/// 단위 하나 분석. `llm` 이 None 이면 정적 분석만 (모델 없이도 통합 분석은 된다).
///
/// 순서: ① 여러 조각에 걸친 긴 커서의 조각 → 커서마다 뜻(긴 커서는 조각 답을 모아, 짧은 커서는 SQL 만 보여 주고)
///       ② 나머지 조각 — 그 조각이 쓰는 커서의 뜻을 프롬프트에 넣는다 → ③ 서브프로그램·단위 요약
pub async fn analyze_unit(
    store: &Store,
    src: &UnitSource,
    spec: Option<&str>,
    llm: Option<Llm<'_>>,
    opts: &Options,
    on: OnEvent,
    cancel: Arc<AtomicBool>,
) -> Result<UnitResult, String> {
    let plan = chunk::plan(src, spec, opts.limits);
    let prev = store.read_unit(&plan.key);
    let rc = RunCtx {
        store,
        key: plan.key.clone(),
        label: llm::unit_label(&plan.unit_type, &plan.owner, &plan.name),
        total: plan.chunks.len() as u32,
        llm: llm.as_ref().map(|l| (l.client, l.cfg)),
        opts,
        on: on.clone(),
        cancel: cancel.clone(),
        done: AtomicU32::new(0),
        provider_errors: AtomicU32::new(0),
        asked: AtomicU32::new(0),
        cached: AtomicU32::new(0),
        failed: AtomicU32::new(0),
    };
    let mut ss = SumState {
        keys: prev.as_ref().map(|p| p.rollup_keys.clone()).unwrap_or_default(),
        prev: prev.as_ref().map(|p| p.sql_summaries.clone()).unwrap_or_default(),
        sums: Vec::new(),
    };

    // 커서 선언 (조각, 문장)
    let decls: Vec<(&chunk::Chunk, &crate::flow::SqlStmt)> =
        plan.chunks.iter().flat_map(|c| c.facts.statements.iter().filter(|s| s.kind == "CURSOR").map(move |s| (c, s))).collect();
    let spans = |s: &crate::flow::SqlStmt, owner: &chunk::Chunk| -> Vec<&chunk::Chunk> {
        plan.chunks.iter().filter(|c| c.start_line <= s.end_line && c.end_line >= s.line && c.subprogram == owner.subprogram).collect()
    };
    let mut first_ids: Vec<String> = Vec::new();
    if rc.llm.is_some() {
        for (owner, s) in &decls {
            let parts = spans(s, owner);
            if parts.len() > 1 {
                first_ids.extend(parts.iter().map(|c| c.id.clone()));
            }
        }
    }
    first_ids.sort();
    first_ids.dedup();

    let mut results: BTreeMap<String, Result<ChunkResult, String>> = BTreeMap::new();
    // ① 긴 커서의 조각
    let first: Vec<(&chunk::Chunk, String)> = plan.chunks.iter().filter(|c| first_ids.contains(&c.id)).map(|c| (c, String::new())).collect();
    for r in run_many(&rc, first).await {
        let id = match &r {
            Ok(c) => c.chunk.id.clone(),
            Err(_) => format!("~err{}", results.len()),
        };
        results.insert(id, r);
    }
    // ①' 커서의 뜻
    let mut meanings: Vec<(Option<String>, String, String, Vec<String>)> = Vec::new(); // (범위, 이름, 뜻, 선언 조각)
    if let Some(l) = llm.as_ref() {
        let done_a: Vec<ChunkResult> = results.values().filter_map(|r| r.as_ref().ok().cloned()).collect();
        sql_rollups(&mut ss, &done_a, |st| st.kind == "CURSOR", &rc.key, l, opts, &on, &cancel).await;
        for (owner, s) in &decls {
            let name = s.cursor.clone().unwrap_or_default();
            if spans(s, owner).len() > 1 {
                continue; // 위에서 모았다
            }
            // 선언과 같은 조각에서만 쓰이면 모델이 둘 다 본다 — 따로 묻지 않는다
            let used_elsewhere = plan.chunks.iter().any(|c| c.id != owner.id && in_scope(&owner.subprogram, c) && cursor_refs(c).contains(&name));
            if !used_elsewhere || cancel.load(Ordering::Relaxed) {
                continue;
            }
            cursor_meaning(&mut ss, owner, s, &plan, &rc.label, l, opts, &on).await;
        }
        for q in ss.sums.iter().filter(|q| q.kind == "CURSOR") {
            if let Some(n) = &q.cursor {
                if !q.summary.summary.is_empty() {
                    meanings.push((q.subprogram.clone(), n.clone(), q.summary.summary.clone(), q.chunk_ids.clone()));
                }
            }
        }
    }
    // ② 나머지 — 쓰는 커서의 뜻을 붙여서
    let rest: Vec<(&chunk::Chunk, String)> = plan
        .chunks
        .iter()
        .filter(|c| !first_ids.contains(&c.id))
        .map(|c| {
            let refs = cursor_refs(c);
            let mut extra = String::new();
            for (scope, name, meaning, decl_ids) in &meanings {
                if refs.contains(name) && in_scope(scope, c) && !decl_ids.contains(&c.id) {
                    extra.push_str(&format!("- cursor {name}: {meaning}\n"));
                }
            }
            if !extra.is_empty() {
                extra = format!("Cursor meanings (analyzed first):\n{extra}");
            }
            (c, extra)
        })
        .collect();
    for r in run_many(&rc, rest).await {
        let id = match &r {
            Ok(c) => c.chunk.id.clone(),
            Err(_) => format!("~err{}", results.len()),
        };
        results.insert(id, r);
    }

    // 계획 순서로
    let mut chunks = Vec::with_capacity(plan.chunks.len());
    let mut stop: Option<String> = None;
    for c in &plan.chunks {
        if let Some(Ok(r)) = results.remove(&c.id) {
            chunks.push(r);
        }
    }
    for (_, r) in results {
        if let Err(e) = r {
            stop.get_or_insert(e);
        }
    }

    let stats = Stats {
        chunks: rc.total,
        asked: rc.asked.load(Ordering::Relaxed),
        cached: rc.cached.load(Ordering::Relaxed),
        failed: rc.failed.load(Ordering::Relaxed),
    };
    let mut unit = build_unit(&plan, spec, &chunks, stats, llm.as_ref().map(|l| l.cfg), prev.as_ref());
    if llm.is_some() {
        unit.rollup_keys = ss.keys;
        unit.sql_summaries = ss.sums;
    }

    // ③ 요약 — 조각이 다 있고 모델이 있을 때만
    if let (Some(l), true, None) = (llm.as_ref(), opts.rollup, &stop) {
        rollups(&mut unit, &chunks, l, opts, &on, &cancel).await;
    }
    store.write_unit(&unit).map_err(|e| e.to_string())?;
    on(Event::UnitDone { key: rc.key.clone(), stats: unit.stats.clone() });
    match stop {
        Some(e) => Err(e),
        None => Ok(unit),
    }
}

/// 짧은 커서 하나의 뜻 (SQL 본문만 보여 준다)
#[allow(clippy::too_many_arguments)]
async fn cursor_meaning(ss: &mut SumState, owner: &chunk::Chunk, s: &crate::flow::SqlStmt, plan: &Plan, label: &str, l: &Llm<'_>, opts: &Options, on: &OnEvent) {
    let name = s.cursor.clone().unwrap_or_default();
    // 이 커서의 데이터가 들어가는 곳 (어느 조각에서든)
    let mut feeds: Vec<String> = Vec::new();
    for c in plan.chunks.iter().filter(|c| in_scope(&owner.subprogram, c)) {
        for cu in c.facts.cursors.iter().filter(|x| x.name == name) {
            for f in &cu.feeds {
                let t = format!("{}({})", f.table, f.ops);
                if !feeds.contains(&t) {
                    feeds.push(t);
                }
            }
        }
    }
    let body = llm::cursor_message(label, &name, s.line, &s.text, &s.reads, &feeds);
    let model = format!("{}#{}#{:?}", l.cfg.model, PROMPT_VERSION, opts.lang);
    let rk = format!("cursor@{}", s.line);
    let k = fnv(&[&body, &model]);
    let prev = ss.prev.iter().find(|x| x.line == s.line && x.kind == "CURSOR").cloned();
    let summary = match prev {
        Some(p) if ss.keys.get(&rk) == Some(&k) && !opts.force => Some(p.summary),
        _ => {
            on(Event::Rollup { key: plan.key.clone(), what: format!("커서 {name}") });
            let sys = llm::rollup_system(opts.lang, "one SQL cursor (what rows it returns)");
            match llm::ask_summary(l.client, l.cfg, sys, body).await {
                Ok((ins, _)) => {
                    ss.keys.insert(rk, k);
                    Some(ins)
                }
                Err(e) => {
                    tracing::warn!("{} 커서 {name} 뜻 실패: {e:?}", plan.key);
                    None
                }
            }
        }
    };
    if let Some(summary) = summary {
        ss.sums.push(crate::store::SqlSummary {
            subprogram: owner.subprogram.clone(),
            kind: "CURSOR".into(),
            cursor: Some(name),
            line: s.line,
            end_line: s.end_line,
            chunk_ids: vec![owner.id.clone()],
            summary,
        });
    }
}

fn build_unit(plan: &Plan, spec: Option<&str>, chunks: &[ChunkResult], stats: Stats, cfg: Option<&ProviderConfig>, prev: Option<&UnitResult>) -> UnitResult {
    let public_decls = spec.map(|s| crate::plsql::structure(s).0.decls).unwrap_or_default();
    let st = &plan.structure;
    let mut subs = Vec::new();
    for s in &st.subprograms {
        let mine: Vec<&ChunkResult> = chunks
            .iter()
            .filter(|c| c.chunk.subprogram.as_deref() == Some(s.path.as_str()) && c.chunk.overload == s.overload)
            .collect();
        let f = facts::merge(&mine.iter().map(|c| &c.chunk.facts).collect::<Vec<_>>());
        let summary = if mine.len() == 1 { mine[0].insight.clone() } else { None };
        let public = if s.parent.is_some() {
            Some(false)
        } else if spec.is_some() {
            Some(public_decls.iter().any(|d| d.name == s.name))
        } else if matches!(plan.unit_type.as_str(), "PROCEDURE" | "FUNCTION" | "TRIGGER") {
            Some(true)
        } else {
            None
        };
        subs.push(SubResult {
            path: s.path.clone(),
            name: s.name.clone(),
            overload: s.overload,
            kind: s.kind,
            signature: s.signature.clone(),
            start_line: s.start_line,
            end_line: s.end_line,
            public,
            facts: f,
            summary,
            chunk_ids: mine.iter().map(|c| c.chunk.id.clone()).collect(),
        });
    }
    let globals: Vec<&Facts> = chunks.iter().filter(|c| c.chunk.subprogram.is_none()).map(|c| &c.chunk.facts).collect();
    let all: Vec<&Facts> = chunks.iter().map(|c| &c.chunk.facts).collect();
    let mut u = UnitResult {
        format: FORMAT_VERSION,
        key: plan.key.clone(),
        owner: plan.owner.clone(),
        name: plan.name.clone(),
        unit_type: plan.unit_type.clone(),
        lines: plan.lines,
        hash: plan.hash.clone(),
        public_decls,
        warning: st.warning.clone(),
        subprograms: subs,
        globals: facts::merge(&globals),
        facts: facts::merge(&all),
        summary: None,
        chunk_ids: chunks.iter().map(|c| c.chunk.id.clone()).collect(),
        stats,
        provider: cfg.map(|c| c.name.clone()),
        model: cfg.map(|c| c.model.clone()),
        sql_summaries: Vec::new(),
        rollup_keys: BTreeMap::new(),
        analyzed_at: now(),
    };
    // 이전 요약 이어받기 (rollups 에서 키가 같으면 그대로 쓴다)
    if let Some(p) = prev {
        u.rollup_keys = p.rollup_keys.clone();
        for s in &mut u.subprograms {
            if s.summary.is_none() {
                if let Some(ps) = p.subprograms.iter().find(|x| x.path == s.path && x.overload == s.overload) {
                    s.summary = ps.summary.clone();
                }
            }
        }
        u.summary = p.summary.clone();
        u.sql_summaries = p.sql_summaries.clone();
    }
    u
}

fn insight_text(i: &Insight, max_steps: usize) -> String {
    let mut s = format!("  summary: {}\n", i.summary);
    for st in i.steps.iter().take(max_steps) {
        s.push_str(&format!("  - {st}\n"));
    }
    for r in &i.rules {
        s.push_str(&format!("  rule: {r}\n"));
    }
    for r in &i.risks {
        match r.line {
            Some(l) => s.push_str(&format!("  risk@{l}: {}\n", r.issue)),
            None => s.push_str(&format!("  risk: {}\n", r.issue)),
        }
    }
    s
}

fn facts_line(f: &Facts) -> String {
    let t: Vec<String> = f.tables.iter().map(|t| format!("{}({})", t.name, t.ops)).collect();
    let c: Vec<&str> = f.calls.iter().map(|c| c.name.as_str()).collect();
    let mut s = String::new();
    if !t.is_empty() {
        s.push_str(&format!("tables: {}; ", t.join(", ")));
    }
    if !c.is_empty() {
        s.push_str(&format!("calls: {}; ", c.join(", ")));
    }
    if !f.transactions.is_empty() {
        s.push_str("commits/rollbacks; ");
    }
    s
}

/// 여러 조각에 걸친 SQL 문을 하나로 요약해 `ss.sums` 에 더한다
#[allow(clippy::too_many_arguments)]
async fn sql_rollups(ss: &mut SumState, chunks: &[ChunkResult], pick: impl Fn(&crate::flow::SqlStmt) -> bool, unit_key: &str, l: &Llm<'_>, opts: &Options, on: &OnEvent, cancel: &AtomicBool) {
    let model = format!("{}#{}#{:?}", l.cfg.model, PROMPT_VERSION, opts.lang);
    let all_stmts: Vec<(&ChunkResult, &crate::flow::SqlStmt)> = chunks.iter().flat_map(|c| c.chunk.facts.statements.iter().map(move |s| (c, s))).collect();
    for (owner_chunk, st) in all_stmts {
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        if !pick(st) || ss.sums.iter().any(|q| q.line == st.line && q.kind == st.kind) {
            continue;
        }
        let parts: Vec<&ChunkResult> = chunks
            .iter()
            .filter(|c| c.chunk.start_line <= st.end_line && c.chunk.end_line >= st.line && c.chunk.subprogram == owner_chunk.chunk.subprogram)
            .collect();
        if parts.len() < 2 || parts.iter().any(|p| p.insight.is_none()) {
            continue;
        }
        let label = match (&st.cursor, st.kind.as_str()) {
            (Some(c), "CURSOR") => format!("cursor {c}"),
            (Some(c), k) => format!("{k} ({c})"),
            (None, k) => k.to_string(),
        };
        // 조각 문맥의 "SQL 전체 구조 / 읽는 테이블 / 들어가는 곳" 줄을 그대로 쓴다
        let facts: Vec<&str> = parts[0].chunk.context.lines().filter(|l| l.starts_with("SQL 전체 구조") || l.starts_with("이 SQL") || l.starts_with("이 커서")).collect();
        let mut body = format!("Combine these parts into one description of ONE SQL statement: {label} (lines {}-{}).\n{}\n", st.line, st.end_line, facts.join("\n"));
        for p in &parts {
            let here = p.chunk.context.lines().find(|l| l.starts_with("긴 SQL 의 일부")).unwrap_or("");
            body.push_str(&format!("Part (lines {}-{}) {here}\n{}", p.chunk.start_line, p.chunk.end_line, insight_text(p.insight.as_ref().unwrap(), 8)));
        }
        let rk = format!("sql@{}", st.line);
        let k = fnv(&[&body, &model]);
        let prev = ss.prev.iter().find(|x| x.line == st.line && x.kind == st.kind).cloned();
        let summary = match prev {
            Some(p) if ss.keys.get(&rk) == Some(&k) && !opts.force => Some(p.summary),
            _ => {
                on(Event::Rollup { key: unit_key.to_string(), what: format!("SQL {label}") });
                let sys = llm::rollup_system(opts.lang, "one SQL statement (what rows it selects or changes, from which tables, under which conditions)");
                match llm::ask_summary(l.client, l.cfg, sys, body).await {
                    Ok((ins, _)) => {
                        ss.keys.insert(rk, k);
                        Some(ins)
                    }
                    Err(e) => {
                        tracing::warn!("{unit_key} SQL {label} 요약 실패: {e:?}");
                        None
                    }
                }
            }
        };
        if let Some(summary) = summary {
            ss.sums.push(crate::store::SqlSummary {
                subprogram: owner_chunk.chunk.subprogram.clone(),
                kind: st.kind.clone(),
                cursor: st.cursor.clone(),
                line: st.line,
                end_line: st.end_line,
                chunk_ids: parts.iter().map(|p| p.chunk.id.clone()).collect(),
                summary,
            });
        }
    }
}

async fn rollups(unit: &mut UnitResult, chunks: &[ChunkResult], l: &Llm<'_>, opts: &Options, on: &OnEvent, cancel: &AtomicBool) {
    let model = format!("{}#{}#{:?}", l.cfg.model, PROMPT_VERSION, opts.lang);
    // 0) 여러 조각에 걸친 긴 SQL 문 중 커서가 아닌 것 (INSERT … SELECT, MERGE …) — 커서는 앞에서 이미 했다
    let mut ss = SumState { keys: std::mem::take(&mut unit.rollup_keys), prev: unit.sql_summaries.clone(), sums: std::mem::take(&mut unit.sql_summaries) };
    sql_rollups(&mut ss, chunks, |st| st.kind != "CURSOR", &unit.key, l, opts, on, cancel).await;
    unit.rollup_keys = ss.keys;
    unit.sql_summaries = ss.sums;

    // 1) 여러 조각으로 나뉜 서브프로그램
    for i in 0..unit.subprograms.len() {
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        let s = &unit.subprograms[i];
        if s.chunk_ids.len() < 2 {
            continue;
        }
        let parts: Vec<&ChunkResult> = s.chunk_ids.iter().filter_map(|id| chunks.iter().find(|c| &c.chunk.id == id)).collect();
        if parts.iter().any(|p| p.insight.is_none()) {
            continue;
        }
        let mut body = format!("Combine these parts into one description of {} ({}).\nSignature: {}\nFacts: {}\n", s.path, unit.name, s.signature, facts_line(&s.facts));
        for q in unit.sql_summaries.iter().filter(|q| q.subprogram.as_deref() == Some(s.path.as_str())) {
            body.push_str(&format!("Long SQL {} {} (lines {}-{}): {}\n", q.kind, q.cursor.clone().unwrap_or_default(), q.line, q.end_line, q.summary.summary));
        }
        for p in &parts {
            body.push_str(&format!("Part {}/{} (lines {}-{}):\n{}", p.chunk.part, p.chunk.parts, p.chunk.start_line, p.chunk.end_line, insight_text(p.insight.as_ref().unwrap(), 8)));
        }
        let rk = format!("{}#{}", s.path, s.overload);
        let k = fnv(&[&body, &model]);
        if unit.rollup_keys.get(&rk) == Some(&k) && s.summary.is_some() && !opts.force {
            continue;
        }
        on(Event::Rollup { key: unit.key.clone(), what: s.path.clone() });
        let sys = llm::rollup_system(opts.lang, "one PL/SQL subprogram");
        match llm::ask_summary(l.client, l.cfg, sys, body).await {
            Ok((ins, _)) => {
                unit.subprograms[i].summary = Some(ins);
                unit.rollup_keys.insert(rk, k);
            }
            Err(e) => tracing::warn!("{} 요약 실패: {e:?}", unit.subprograms[i].path),
        }
    }

    // 2) 단위 — 서브프로그램 요약이 많으면 묶음으로 나눠 요약한 뒤 다시 합친다
    if cancel.load(Ordering::Relaxed) {
        return;
    }
    let mut items: Vec<String> = Vec::new();
    let global_sql: Vec<&crate::store::SqlSummary> = unit.sql_summaries.iter().filter(|q| q.subprogram.is_none()).collect();
    for q in &global_sql {
        items.push(format!("(global) {} {}: {}\n", q.kind, q.cursor.clone().unwrap_or_default(), q.summary.summary));
    }
    // 긴 SQL 요약에 들지 않은 전역 조각
    for g in chunks.iter().filter(|c| c.chunk.subprogram.is_none() && !global_sql.iter().any(|q| q.chunk_ids.contains(&c.chunk.id))) {
        if let Some(i) = &g.insight {
            items.push(format!("(global declarations):\n{}", insight_text(i, 3)));
        }
    }
    for s in &unit.subprograms {
        if s.path.contains('.') {
            continue; // 중첩은 바깥 요약에 들어 있다
        }
        let Some(ins) = &s.summary else { continue };
        let vis = match s.public {
            Some(true) => "public",
            Some(false) => "private",
            None => "",
        };
        items.push(format!("{} {} [{}] {}\n  summary: {}\n", crate::chunk::kind_word(s.kind), s.name, vis, facts_line(&s.facts), ins.summary));
    }
    if items.is_empty() {
        return;
    }
    let budget = opts.limits.max_chars.max(2000);
    let head = format!("Describe the whole unit {} {}.{} from its parts.\n", unit.unit_type, unit.owner, unit.name);
    let all = items.concat();
    let k = fnv(&[&all, &model]);
    if unit.rollup_keys.get("_unit") == Some(&k) && unit.summary.is_some() && !opts.force {
        return;
    }
    on(Event::Rollup { key: unit.key.clone(), what: "(단위)".into() });
    let sys = llm::rollup_system(opts.lang, "one PL/SQL package or program unit");
    let mut level = items;
    // 한도에 들어갈 때까지 묶어서 줄인다
    for _round in 0..4 {
        if level.concat().chars().count() <= budget {
            break;
        }
        let mut next = Vec::new();
        let mut batch = String::new();
        let flush = |batch: &mut String, next: &mut Vec<String>| {
            if !batch.is_empty() {
                next.push(std::mem::take(batch));
            }
        };
        for it in &level {
            if batch.chars().count() + it.chars().count() > budget {
                flush(&mut batch, &mut next);
            }
            batch.push_str(it);
        }
        flush(&mut batch, &mut next);
        let mut reduced = Vec::new();
        for (bi, b) in next.iter().enumerate() {
            if cancel.load(Ordering::Relaxed) {
                return;
            }
            match llm::ask_summary(l.client, l.cfg, sys.clone(), format!("{head}This is group {} of {}.\n{b}", bi + 1, next.len())).await {
                Ok((ins, _)) => reduced.push(format!("group {}:\n{}", bi + 1, insight_text(&ins, 8))),
                Err(e) => {
                    tracing::warn!("{} 묶음 요약 실패: {e:?}", unit.key);
                    return;
                }
            }
        }
        if reduced.len() >= level.len() {
            break;
        }
        level = reduced;
    }
    match llm::ask_summary(l.client, l.cfg, sys, format!("{head}Facts: {}\n{}", facts_line(&unit.facts), level.concat())).await {
        Ok((ins, _)) => {
            unit.summary = Some(ins);
            unit.rollup_keys.insert("_unit".into(), k);
        }
        Err(e) => tracing::warn!("{} 단위 요약 실패: {e:?}", unit.key),
    }
}
