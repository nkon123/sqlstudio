//! 값 추적 명령 — 결과 그리드의 한 셀에서 "이 행의 이 값은 어떻게 나왔나".
//!
//! 셀의 열을 테이블 열로 풀고(값의 출처와 같은 계보), 그 행의 키(PK)를 정한 뒤, 분석 결과에서 그 열에 값을 넣는
//! 문장마다 원본 쿼리를 이 키로 다시 돌린다 (sqls_analyze::trace). 다시 계산한 값이 지금 값과 같으면 "재현됨".
//! 0건이면 거르는 조건마다 O/X, 값이 나오면 값을 만든 원본 행과 그 키 — 화면이 한 단계씩 더 거슬러 오른다.
//!
//! DB 에 보내는 것은 SELECT 뿐이고 (보내기 전에 문장 종류를 다시 확인한다), 운영 DB·실행 중인 세션에서는 SQL 만 보인다.
//! 한 질의는 10초까지 — 넘으면 취소한다.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use sqls_analyze::colwrite::StmtLite;
use sqls_analyze::trace::{self, Input, Path};
use sqls_core::meta;
use sqls_core::session::Session;
use tauri::State;

use crate::state::{AppState, ErrView};

type R<T> = Result<T, ErrView>;

fn yes() -> bool {
    true
}

/// 질의 하나의 상한
const QUERY_SECS: u64 = 10;
/// 값을 모르는 변수를 빼고 다시 돌리는 횟수
const MAX_UNKNOWN: usize = 4;
/// 원본 행을 보일 수
const MAX_INPUT_ROWS: usize = 5;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceTarget {
    pub owner: Option<String>,
    pub table: String,
    pub column: String,
    /// (키 열, 값)
    pub keys: Vec<(String, String)>,
    /// 지금 값 (NULL 이면 None)
    pub value: Option<String>,
    /// 지금 값을 아는지 (결과 열이 식이었으면 모른다)
    #[serde(default = "yes")]
    pub known: bool,
    /// 사용자가 넣은 변수 값 (이름, 값)
    #[serde(default)]
    pub vars: Vec<(String, String)>,
}

#[derive(Serialize)]
pub struct TraceStep {
    target: TraceTarget,
    /// 이 접속의 분석 결과가 있는지
    analyzed: bool,
    /// 다시 계산을 돌렸는지 (아니면 그 까닭)
    not_run: Option<String>,
    writers: Vec<WriterTrace>,
    /// 결과 열이 식이었으면 어떤 식이었는지 등
    notes: Vec<String>,
    /// 이 열을 넣지 않는 INSERT — 이 문장으로 들어온 행이면 이 열은 기본값·NULL
    lacking: Vec<Lacking>,
    /// 이 열의 기본값 (DEFAULT)
    default: Option<String>,
    /// 탐색 기록 — 어떻게 타고 들어가 어떤 쿼리를 몇 번 돌렸는지
    log: Vec<LogEntry>,
}

/// 그 테이블에 행을 넣지만 이 열은 넣지 않는 문장
#[derive(Serialize, Default)]
pub struct Lacking {
    node: String,
    unit: String,
    line: u32,
    end_line: u32,
    kind: String,
    /// 이 키의 행을 이 문장이 만드는지 (원본을 이 키로 다시 돌려 봄). 모르면 None
    produces: Option<bool>,
    sql: Option<String>,
    binds: Vec<(String, String)>,
    error: Option<String>,
}

/// 탐색 기록 한 줄 — 어디를 거치며(at) 무엇을 하려고(purpose) 어떤 쿼리를 보냈고 결과가 몇 건·몇 ms 였는지.
/// sql 이 없으면 쿼리 없이 길을 고른 것 (문장을 찾음, 서브쿼리 안으로 등)
#[derive(Serialize, Clone)]
pub struct LogEntry {
    seq: usize,
    at: String,
    purpose: String,
    sql: Option<String>,
    binds: Vec<(String, String)>,
    /// 보냈는지 (운영 DB·실행 중이면 만들기만 하고 안 보냄)
    sent: bool,
    /// "3건", "0행", "5행 이상" 등
    result: Option<String>,
    ms: u64,
    error: Option<String>,
    /// 소스 위치 — 이 쿼리가 다시 계산한 문장 (분석 단위 키, 첫 줄, 끝 줄)
    unit: Option<String>,
    line: Option<u32>,
    end_line: Option<u32>,
    /// 거쳐 온 커서·SELECT INTO 의 줄
    marks: Vec<u32>,
    /// 소스에서 짚을 낱말 — 조인 단계면 그 테이블, 모르는 변수면 그 변수
    hint: Option<String>,
}

/// 소스 위치 (분석 단위, 첫 줄, 끝 줄, 거쳐 온 줄)
#[derive(Clone, Default)]
struct Loc {
    unit: String,
    line: u32,
    end_line: u32,
    marks: Vec<u32>,
    /// 이 자리의 쿼리마다 짚을 낱말 — 서브쿼리 층이면 그 안의 첫 테이블
    hint: Option<String>,
}

/// 탐색 기록 — 쿼리를 보낼 때마다 쌓는다 (await 너머로 잠그지 않는다)
#[derive(Default)]
struct Log {
    at: Mutex<String>,
    loc: Mutex<Option<Loc>>,
    hint: Mutex<Option<String>>,
    list: Mutex<Vec<LogEntry>>,
}

impl Log {
    /// 지금 보는 곳 — 소스 위치는 지운다 (place 로 다시)
    fn at(&self, at: impl Into<String>) {
        *self.at.lock().unwrap() = at.into();
        *self.loc.lock().unwrap() = None;
    }
    fn place(&self, unit: &str, line: u32, end_line: u32, marks: Vec<u32>, hint: Option<String>) {
        *self.loc.lock().unwrap() = Some(Loc { unit: unit.to_string(), line, end_line: end_line.max(line), marks, hint });
    }
    /// 다음 한 줄에만 붙는 짚을 낱말
    fn hint(&self, h: impl Into<String>) {
        *self.hint.lock().unwrap() = Some(h.into());
    }
    #[allow(clippy::too_many_arguments)]
    fn push(&self, purpose: impl Into<String>, sql: Option<&str>, binds: &[(String, String)], sent: bool, result: Option<String>, ms: u64, error: Option<String>) {
        let at = self.at.lock().unwrap().clone();
        let loc = self.loc.lock().unwrap().clone().unwrap_or_default();
        let hint = self.hint.lock().unwrap().take().or(loc.hint);
        let mut l = self.list.lock().unwrap();
        let seq = l.len() + 1;
        let unit = (!loc.unit.is_empty()).then_some(loc.unit);
        let (line, end_line) = if unit.is_some() { (Some(loc.line), Some(loc.end_line)) } else { (None, None) };
        l.push(LogEntry { seq, at, purpose: purpose.into(), sql: sql.map(str::to_string), binds: binds.to_vec(), sent, result, ms, error, unit, line, end_line, marks: loc.marks, hint });
    }
    /// 쿼리 없이 길을 고른 것
    fn note(&self, purpose: impl Into<String>) {
        self.push(purpose, None, &[], false, None, 0, None);
    }
    /// 만들었지만 보내지 않은 쿼리
    fn unsent(&self, purpose: impl Into<String>, sql: &str, binds: &[(String, String)]) {
        self.push(purpose, Some(sql), binds, false, None, 0, None);
    }
    fn take(self) -> Vec<LogEntry> {
        self.list.into_inner().unwrap()
    }
}

/// 조인 단계 하나를 센 결과
#[derive(Serialize)]
pub struct JoinRun {
    alias: String,
    table: String,
    on: Vec<String>,
    rows: Option<i64>,
    sql: String,
}

/// 행 추적 — "이 키의 행은 왜 없나"
#[derive(Serialize)]
pub struct RowTrace {
    target: TraceTarget,
    analyzed: bool,
    not_run: Option<String>,
    /// 지금 이 키의 행이 있는지 (돌리지 않았으면 None)
    exists: Option<bool>,
    exists_sql: String,
    /// 이 테이블에 행을 넣는 문장마다 — 원본에 이 키가 있는지, 없으면 어디서 끊겼는지
    writers: Vec<WriterTrace>,
    /// 이 테이블에서 행을 지우는 문장
    deleters: Vec<Deleter>,
    log: Vec<LogEntry>,
}

#[derive(Serialize)]
pub struct Deleter {
    node: String,
    unit: String,
    line: u32,
    end_line: u32,
    kind: String,
    text: String,
}

/// 행 추적을 열 때 미리 채울 것 — 테이블, 키 열, (지금 SQL 의 WHERE 에서 찾은) 키 값
#[derive(Serialize)]
pub struct RowPrep {
    owner: String,
    table: String,
    key_cols: Vec<String>,
    values: Vec<Option<String>>,
    /// PK·고유 인덱스 (없으면 빈 것)
    pk: Vec<String>,
    /// 테이블의 모든 열 — 키 열을 고를 때
    columns: Vec<String>,
}

#[derive(Serialize, Default)]
pub struct WriterTrace {
    node: String,
    unit: String,
    line: u32,
    end_line: u32,
    kind: String,
    label: String,
    expr: String,
    source: Option<String>,
    notes: Vec<String>,
    sql: Option<String>,
    binds: Vec<(String, String)>,
    error: Option<String>,
    /// 다시 계산을 실제로 돌렸는지
    ran: bool,
    /// 다시 계산한 값 (여러 행이면 여럿)
    values: Vec<Option<String>>,
    /// 지금 값과 같은지 (지금 값을 모르거나 돌리지 않았으면 None)
    reproduced: Option<bool>,
    /// 값을 모르는 변수 때문에 뺀 조건
    dropped: Vec<String>,
    unknown_vars: Vec<String>,
    /// 이 키로 값이 안 나왔을 때 — 거르는 조건마다 O/X
    flag_conds: Vec<String>,
    flag_rows: Vec<Vec<Option<String>>>,
    flags_sql: Option<String>,
    /// 값을 만든 원본 행 — 다음 단계로
    inputs: Vec<InputRows>,
    rows_sql: Option<String>,
    /// 대상 키가 원본 쪽 무엇과 이어지는지 (키 열, 원본 쪽 식, 값)
    keys: Vec<(String, String, String)>,
    /// 거쳐 온 층 (대상 쪽 → 원본 쪽): 문장, 커서, SELECT INTO, 서브쿼리
    via: Vec<trace::Via>,
    /// 서브쿼리 안으로 내려가 다시 계산한 층들 (via 의 서브쿼리 층마다 하나, 바깥 → 안쪽)
    layers: Vec<WriterTrace>,
    /// 이 키로 값이 안 나왔을 때 — 키를 쥔 테이블부터 조인을 하나씩 더하며 남는 행 수
    join_steps: Vec<JoinRun>,
}

#[derive(Serialize)]
pub struct InputRows {
    owner: Option<String>,
    table: String,
    column: String,
    alias: String,
    key_cols: Vec<String>,
    rows: Vec<InputRow>,
    /// 키가 모두 NULL 인 원본 행 수 — 아우터 조인에서 이 테이블에 짝이 없었다 (그래서 이 값은 NULL)
    missing: usize,
}

#[derive(Serialize, PartialEq)]
pub struct InputRow {
    keys: Vec<(String, String)>,
    value: Option<String>,
}

/// 테이블의 키 — PK, 없으면 첫 고유 인덱스
fn key_of(d: &meta::TableDesc) -> Vec<String> {
    if !d.primary_key.is_empty() {
        return d.primary_key.clone();
    }
    d.indexes.iter().find(|i| i.unique).map(|i| i.columns.clone()).unwrap_or_default()
}

/// 결과 그리드의 셀에서 시작 — 열을 테이블 열로 풀고, 행에서 키 값을 찾는다
#[tauri::command]
pub async fn value_trace_start(st: State<'_, AppState>, id: u64, sql: String, pos: Option<usize>, column: String, row: Vec<(String, Option<String>)>, key_cols: Option<Vec<String>>) -> R<TraceStep> {
    let lineage = crate::complete::column_lineage(&st, id, &sql, pos, &column).await?;
    let Some(src) = lineage.sources().into_iter().next().cloned() else {
        return Err(ErrView::msg("invalid", format!("{column} 은(는) 테이블 열에서 오지 않았습니다 (식·상수이거나 찾지 못함)")));
    };
    let mut notes = Vec::new();
    if lineage.kind != "column" {
        notes.push(format!("결과 열 {column} 은(는) {} — 원본 열 {}.{} 의 값으로 거슬러 오릅니다", lineage.expr.clone().unwrap_or_else(|| lineage.kind.clone()), src.table, src.column));
    }
    let (s, _, _) = st.session(id)?;
    let qualified = match &src.owner {
        Some(o) => format!("{o}.{}", src.table),
        None => src.table.clone(),
    };
    let desc = meta::describe(&s, &qualified).await?;
    // 키: 사용자가 고른 열, 없으면 PK·고유 인덱스
    let kc: Vec<String> = match key_cols.filter(|k| !k.is_empty()) {
        Some(k) => {
            let k: Vec<String> = k.into_iter().map(|c| c.to_uppercase()).collect();
            if let Some(bad) = k.iter().find(|c| !desc.columns.iter().any(|d| d.name == **c)) {
                return Err(ErrView::msg("invalid", format!("{} 에 {bad} 열이 없습니다", desc.name)));
            }
            k
        }
        None => key_of(&desc),
    };
    if kc.is_empty() {
        return Err(ErrView::msg("nokey", format!("{}.{} 에 PK·고유 인덱스가 없습니다 — 이 행을 집을 열을 골라 주세요", desc.owner, desc.name)));
    }
    // 키 값: 같은 이름의 결과 열, 없으면 결과 열마다 계보를 풀어 그 키 열에서 온 것
    let mut keys = Vec::new();
    let mut missing = Vec::new();
    for k in &kc {
        let by_name = row.iter().find(|(n, _)| n.eq_ignore_ascii_case(k));
        let found = match by_name {
            Some((_, v)) => v.clone(),
            None => {
                let mut hit = None;
                for (n, v) in row.iter().take(60) {
                    if let Ok(l) = crate::complete::column_lineage(&st, id, &sql, None, n).await {
                        if l.kind == "column" && l.source.as_ref().is_some_and(|x| x.table.eq_ignore_ascii_case(&desc.name) && x.column.eq_ignore_ascii_case(k)) {
                            hit = Some(v.clone());
                            break;
                        }
                    }
                }
                hit.flatten()
            }
        };
        match found {
            Some(v) => keys.push((k.clone(), v)),
            None => missing.push(k.clone()),
        }
    }
    if !missing.is_empty() {
        return Err(ErrView::msg("invalid", format!("이 행의 키 열 {} 이(가) 결과에 없습니다 — 결과에 넣고(SELECT 에 추가) 다시 해 보세요", missing.join(", "))));
    }
    let known = lineage.kind == "column";
    let value = if known { row.iter().find(|(n, _)| n.eq_ignore_ascii_case(&column)).and_then(|(_, v)| v.clone()) } else { None };
    let target = TraceTarget { owner: Some(desc.owner.clone()), table: desc.name.clone(), column: src.column.clone(), keys, value, known, vars: vec![] };
    let mut step = trace_step(&st, id, &s, target, Some(desc)).await?;
    step.notes.splice(0..0, notes);
    Ok(step)
}

/// 한 단계 — 그 열에 값을 넣는 문장마다 다시 계산 (화면이 원본 행의 "거슬러 오르기" 로 부른다)
#[tauri::command]
pub async fn value_trace_step(st: State<'_, AppState>, id: u64, target: TraceTarget) -> R<TraceStep> {
    let (s, _, _) = st.session(id)?;
    trace_step(&st, id, &s, target, None).await
}

async fn trace_step(st: &AppState, id: u64, s: &Session, mut target: TraceTarget, desc: Option<meta::TableDesc>) -> R<TraceStep> {
    let qualified = |o: &Option<String>, t: &str| match o {
        Some(o) => format!("{o}.{t}"),
        None => t.to_string(),
    };
    let desc = match desc {
        Some(d) => d,
        None => meta::describe(s, &qualified(&target.owner, &target.table)).await?,
    };
    target.owner = Some(desc.owner.clone());
    target.table = desc.name.clone();
    target.column = target.column.to_uppercase();
    let mut descs: HashMap<String, Option<meta::TableDesc>> = HashMap::new();
    descs.insert(desc.name.clone(), Some(desc.clone()));
    let not_run = if crate::db::is_production(st, id) {
        Some("운영 DB 라 다시 계산은 돌리지 않았습니다 — SQL 을 복사해 확인하세요".to_string())
    } else if s.is_busy() {
        Some("세션이 실행 중이라 돌리지 않았습니다".to_string())
    } else {
        None
    };
    let log = Log::default();
    log.at(format!("{}.{}({}).{}", desc.owner, desc.name, key_line(&target.keys), target.column));
    if let Some(n) = &not_run {
        log.note(n.clone());
    }
    let path = crate::analyze::store_for(st, id)?.root().join("integrated").join("statements.json");
    let stmts = tauri::async_runtime::spawn_blocking(move || crate::analyze::statements(&path)).await.map_err(|e| ErrView::msg("io", e.to_string()))?;
    let Some(stmts) = stmts else {
        log.note("분석 결과(statements.json) 없음 — 여기서 멈춤");
        return Ok(TraceStep { target, analyzed: false, not_run, writers: vec![], notes: vec![], lacking: vec![], default: None, log: log.take() });
    };
    let writes = sqls_analyze::colwrite::column_writes(&stmts, Some(&desc.owner), &desc.name, &target.column);
    log.note(format!("분석 결과에서 {}.{} 에 값을 넣는 문장 {}개를 찾음{}", desc.name, target.column, writes.len(), if writes.len() > 12 { " (앞 12개만)" } else { "" }));
    let list: Vec<(String, u32, String)> = writes.iter().take(12).map(|w| (w.node.clone(), w.line, w.kind.clone())).collect();
    let mut writers = trace_writes(s, &log, &stmts, &list, &desc, &target, &mut descs, not_run.is_none()).await;
    // 이 열을 넣지 않는 INSERT·MERGE — 이 키의 행을 만들면 이 열은 기본값·NULL 로 들어간다
    let mut lacking = Vec::new();
    let key0 = target.keys.first().map(|(k, _)| k.clone());
    let lack: Vec<&StmtLite> = stmts.iter().filter(|x| matches!(x.kind.as_str(), "INSERT" | "MERGE") && inserts_into(x, &desc) && !writes.iter().any(|w| w.node == x.node && w.line == x.line)).take(8).collect();
    if !lack.is_empty() {
        log.at(format!("{}.{}", desc.name, target.column));
        log.note(format!("{} 에 행을 넣지만 {} 은(는) 넣지 않는 문장 {}개 — 이 키의 행을 만드는지 키 열로 다시 계산", desc.name, target.column, lack.len()));
    }
    for st0 in lack {
        let mut l = Lacking { node: st0.node.clone(), unit: st0.unit.clone(), line: st0.line, end_line: st0.end_line, kind: st0.kind.clone(), ..Default::default() };
        if let Some(k0) = &key0 {
            let kt = TraceTarget { column: k0.clone(), known: false, value: None, ..target.clone() };
            let w = trace_writes(s, &log, &stmts, &[(st0.node.clone(), st0.line, st0.kind.clone())], &desc, &kt, &mut descs, not_run.is_none()).await;
            if let Some(w) = w.into_iter().find(|w| w.sql.is_some()) {
                l.produces = w.ran.then_some(!w.values.is_empty());
                l.sql = w.sql;
                l.binds = w.binds;
                l.error = w.error;
            }
        }
        lacking.push(l);
    }
    // 이 키의 행을 만든다고 확인된 것을 위로
    lacking.sort_by_key(|l| match l.produces {
        Some(true) => 0,
        None => 1,
        Some(false) => 2,
    });
    let default = desc.columns.iter().find(|c| c.name == target.column).and_then(|c| c.default.clone());
    // 재현된 길을 위로
    writers.sort_by_key(|w| match w.reproduced {
        Some(true) => 0,
        None if !w.values.is_empty() => 1,
        Some(false) => 2,
        None => 3,
    });
    Ok(TraceStep { target, analyzed: true, not_run, writers, notes: vec![], lacking, default, log: log.take() })
}

/// 문장이 이 테이블에 행을 넣는지 (C)
fn inserts_into(x: &StmtLite, desc: &meta::TableDesc) -> bool {
    x.writes.iter().any(|w| {
        let base = w.name.split('@').next().unwrap_or(&w.name);
        let (o, t) = base.rsplit_once('.').map(|(o, t)| (Some(o), t)).unwrap_or((None, base));
        w.ops.contains('C') && t == desc.name && o.is_none_or(|o| o == desc.owner)
    })
}

/// 넣는 문장들(노드·줄·종류)마다 길을 풀어 다시 계산한다 — 서브쿼리면 층마다
#[allow(clippy::too_many_arguments)]
async fn trace_writes(s: &Session, log: &Log, stmts: &[StmtLite], list: &[(String, u32, String)], desc: &meta::TableDesc, target: &TraceTarget, descs: &mut HashMap<String, Option<meta::TableDesc>>, run: bool) -> Vec<WriterTrace> {
    let key_cols: Vec<String> = target.keys.iter().map(|(k, _)| k.clone()).collect();
    let table_cols: Vec<String> = desc.columns.iter().map(|c| c.name.clone()).collect();
    let mut writers = Vec::new();
    for (node, line, kind) in list {
        let Some(stmt) = stmts.iter().find(|x| x.node == *node && x.line == *line && x.kind == *kind) else { continue };
        let here = format!("{} {}줄 {}", stmt.node, stmt.line, stmt.kind);
        log.at(here.clone());
        log.place(&stmt.unit, stmt.line, stmt.end_line, vec![], None);
        let text = if stmt.text.ends_with('…') { full_text(s, log, stmt).await.unwrap_or_else(|| stmt.text.clone()) } else { stmt.text.clone() };
        let node_stmts: Vec<StmtLite> = stmts.iter().filter(|x| x.node == stmt.node).cloned().collect();
        let paths = trace::paths(stmt, &node_stmts, &text, &desc.name, &target.column, &key_cols, &table_cols);
        if paths.is_empty() {
            log.note(format!("문장에서 {} 의 식을 찾지 못함", target.column));
            writers.push(WriterTrace {
                node: stmt.node.clone(),
                unit: stmt.unit.clone(),
                line: stmt.line,
                end_line: stmt.end_line,
                kind: stmt.kind.clone(),
                label: stmt.kind.clone(),
                notes: vec!["이 문장에서 그 열의 식을 찾지 못했습니다".into()],
                ..Default::default()
            });
            continue;
        }
        for p in paths {
            let at = format!("{here} · {}", p.label);
            log.at(at.clone());
            let marks: Vec<u32> = p.via.iter().filter_map(|v| v.line).filter(|l| *l != stmt.line).collect();
            log.place(&stmt.unit, stmt.line, stmt.end_line, marks.clone(), None);
            let vias: Vec<String> = p.via.iter().filter(|v| v.kind != "문장").map(|v| format!("{} {}{}", v.kind, v.name, v.line.map(|l| format!(" ({l}줄)")).unwrap_or_default())).collect();
            log.note(format!("길: {} = {}{}", target.column, p.expr_text(), if vias.is_empty() { String::new() } else { format!(" ← {}", vias.join(" ← ")) }));
            let mut wt = WriterTrace {
                node: stmt.node.clone(),
                unit: stmt.unit.clone(),
                line: stmt.line,
                end_line: stmt.end_line,
                kind: stmt.kind.clone(),
                label: p.label.clone(),
                expr: p.expr_text(),
                source: p.source_text(),
                notes: p.notes.clone(),
                ..Default::default()
            };
            run_path(s, log, &p, target, descs, run, &mut wt).await;
            wt.keys = key_map(&p, target);
            wt.via = p.via.clone();
            // 식이 서브쿼리(인라인 뷰)의 열이면 그 안으로 내려가 층마다 다시 계산한다 (값이 어느 층에서 어떻게 되었는지)
            let mut cur = p.clone();
            let mut at = at;
            for _ in 0..3 {
                let Some(inner) = trace::unwrap_view(&cur) else { break };
                at = format!("{at} › {}", inner.label);
                log.at(at.clone());
                log.place(&stmt.unit, stmt.line, stmt.end_line, marks.clone(), inner.src.as_ref().and_then(|x| x.tables.first()).map(|(_, t)| t.clone()));
                log.note(format!("서브쿼리 안으로: {} = {}", target.column, inner.expr_text()));
                let mut lt = WriterTrace { label: inner.label.clone(), expr: inner.expr_text(), source: inner.source_text(), notes: inner.notes.clone(), ..Default::default() };
                run_path(s, log, &inner, target, descs, run, &mut lt).await;
                lt.reproduced = None;
                lt.keys = key_map(&inner, target);
                lt.via = inner.via.last().cloned().into_iter().collect();
                if !lt.inputs.is_empty() {
                    wt.inputs = std::mem::take(&mut lt.inputs);
                }
                wt.via = inner.via.clone();
                wt.layers.push(lt);
                cur = inner;
            }
            writers.push(wt);
        }
    }
    writers
}

/// 행 추적을 열 때 — 테이블(없으면 SQL 의 첫 테이블), 키 열, SQL 의 WHERE 에서 찾은 `키 = 값`
#[tauri::command]
pub async fn row_trace_prepare(st: State<'_, AppState>, id: u64, sql: Option<String>, table: Option<String>) -> R<RowPrep> {
    let (s, _, _) = st.session(id)?;
    let name = table
        .filter(|t| !t.trim().is_empty())
        .or_else(|| sql.as_deref().and_then(|q| meta::referenced_tables(q).into_iter().next()))
        .ok_or_else(|| ErrView::msg("invalid", "추적할 테이블을 정하세요"))?;
    let desc = meta::describe(&s, &name).await?;
    let pk = key_of(&desc);
    let lit = |k: &str| sql.as_deref().and_then(|q| literal_for(q, k));
    // PK 값을 SQL 에서 다 찾으면 PK 만, 아니면 PK + SQL 에서 `열 = 값` 으로 쓴 열
    let mut key_cols = pk.clone();
    if pk.is_empty() || pk.iter().any(|k| lit(k).is_none()) {
        for c in &desc.columns {
            if !key_cols.contains(&c.name) && lit(&c.name).is_some() {
                key_cols.push(c.name.clone());
            }
        }
    }
    let values = key_cols.iter().map(|k| lit(k)).collect();
    let columns = desc.columns.iter().map(|c| c.name.clone()).collect();
    Ok(RowPrep { owner: desc.owner, table: desc.name, key_cols, values, pk, columns })
}

/// SQL 에서 `[별칭.]열 = '값'` · `= 123` 의 값
fn literal_for(sql: &str, col: &str) -> Option<String> {
    use sqls_analyze::plsql::{lex, Kind};
    let t = lex(sql);
    (0..t.len()).find_map(|i| {
        let is_col = matches!(&t[i].kind, Kind::Word(w) if w.eq_ignore_ascii_case(col)) && !t.get(i + 1).is_some_and(|x| matches!(x.kind, Kind::Sym(".")));
        if !is_col || !t.get(i + 1).is_some_and(|x| matches!(x.kind, Kind::Sym("="))) {
            return None;
        }
        match &t.get(i + 2)?.kind {
            Kind::Str(v) | Kind::Num(v) => Some(v.clone()),
            _ => None,
        }
    })
}

/// 행 추적 — 이 테이블에 행을 넣는 문장마다 원본을 이 키로 다시 돌려, 원본에 있는지·어디서 끊겼는지
#[tauri::command]
pub async fn row_trace(st: State<'_, AppState>, id: u64, owner: Option<String>, table: String, keys: Vec<(String, String)>, #[allow(unused)] vars: Option<Vec<(String, String)>>) -> R<RowTrace> {
    let (s, _, _) = st.session(id)?;
    let qualified = match &owner {
        Some(o) => format!("{o}.{table}"),
        None => table.clone(),
    };
    let desc = meta::describe(&s, &qualified).await?;
    let keys: Vec<(String, String)> = keys.into_iter().filter(|(_, v)| !v.trim().is_empty()).map(|(k, v)| (k.to_uppercase(), v)).collect();
    if keys.is_empty() {
        return Err(ErrView::msg("invalid", "키 값을 넣으세요"));
    }
    if let Some((k, _)) = keys.iter().find(|(k, _)| !desc.columns.iter().any(|c| c.name == *k)) {
        return Err(ErrView::msg("invalid", format!("{} 에 {k} 열이 없습니다", desc.name)));
    }
    let target = TraceTarget { owner: Some(desc.owner.clone()), table: desc.name.clone(), column: keys[0].0.clone(), keys: keys.clone(), value: None, known: false, vars: vars.unwrap_or_default() };
    let not_run = if crate::db::is_production(&st, id) {
        Some("운영 DB 라 다시 계산은 돌리지 않았습니다 — SQL 을 복사해 확인하세요".to_string())
    } else if s.is_busy() {
        Some("세션이 실행 중이라 돌리지 않았습니다".to_string())
    } else {
        None
    };
    let cond: Vec<String> = keys.iter().enumerate().map(|(n, (k, _))| format!("{k} = :K{}", n + 1)).collect();
    let exists_sql = format!("SELECT COUNT(*) AS N__ FROM {}.{} WHERE {}", desc.owner, desc.name, cond.join(" AND "));
    let binds: Vec<(String, String)> = keys.iter().enumerate().map(|(n, (_, v))| (format!("K{}", n + 1), v.clone())).collect();
    let log = Log::default();
    log.at(format!("{}.{}({})", desc.owner, desc.name, key_line(&keys)));
    let exists = if let Some(n) = &not_run {
        log.note(n.clone());
        log.unsent("지금 이 키의 행이 있는지", &exists_sql, &binds);
        None
    } else {
        select(&s, &log, "지금 이 키의 행이 있는지", &exists_sql, &binds, 1).await.ok().and_then(|p| p.rows.first().and_then(|r| r.first().cloned().flatten())).map(|n| n != "0")
    };
    let path = crate::analyze::store_for(&st, id)?.root().join("integrated").join("statements.json");
    let stmts = tauri::async_runtime::spawn_blocking(move || crate::analyze::statements(&path)).await.map_err(|e| ErrView::msg("io", e.to_string()))?;
    let Some(stmts) = stmts else {
        log.note("분석 결과(statements.json) 없음 — 여기서 멈춤");
        return Ok(RowTrace { target, analyzed: false, not_run, exists, exists_sql, writers: vec![], deleters: vec![], log: log.take() });
    };
    let mut descs: HashMap<String, Option<meta::TableDesc>> = HashMap::new();
    descs.insert(desc.name.clone(), Some(desc.clone()));
    let all: Vec<&StmtLite> = stmts.iter().filter(|x| matches!(x.kind.as_str(), "INSERT" | "MERGE") && inserts_into(x, &desc)).collect();
    log.note(format!("분석 결과에서 {} 에 행을 넣는 문장 {}개를 찾음{}", desc.name, all.len(), if all.len() > 12 { " (앞 12개만)" } else { "" }));
    let list: Vec<(String, u32, String)> = all.iter().take(12).map(|x| (x.node.clone(), x.line, x.kind.clone())).collect();
    let mut writers = trace_writes(&s, &log, &stmts, &list, &desc, &target, &mut descs, not_run.is_none()).await;
    // 원본에 이 키가 있는 문장을 위로
    writers.sort_by_key(|w| if !w.values.is_empty() { 0 } else if w.ran { 1 } else { 2 });
    let deleters = stmts
        .iter()
        .filter(|x| {
            x.writes.iter().any(|w| {
                let base = w.name.split('@').next().unwrap_or(&w.name);
                w.ops.contains('D') && base.rsplit('.').next() == Some(desc.name.as_str())
            })
        })
        .take(12)
        .map(|x| Deleter { node: x.node.clone(), unit: x.unit.clone(), line: x.line, end_line: x.end_line, kind: x.kind.clone(), text: x.text.clone() })
        .collect::<Vec<_>>();
    log.at(desc.name.clone());
    log.note(format!("분석 결과에서 {} 의 행을 지우는 문장 {}개를 찾음", desc.name, deleters.len()));
    Ok(RowTrace { target, analyzed: true, not_run, exists, exists_sql, writers, deleters, log: log.take() })
}

/// 추적 화면 왼쪽에 보일 소스 전체 — 분석 단위 키(OWNER.NAME.PACKAGE_BODY) 의 ALL_SOURCE 줄들
#[tauri::command]
pub async fn trace_source(st: State<'_, AppState>, id: u64, unit: String) -> R<Vec<String>> {
    let (s, _, _) = st.session(id)?;
    let mut parts = unit.splitn(3, '.');
    let (Some(owner), Some(name), Some(kind)) = (parts.next(), parts.next(), parts.next()) else {
        return Err(ErrView::msg("invalid", format!("{unit} 은(는) DB 객체가 아닙니다")));
    };
    let lines = sqls_core::debug::source(&s, owner, name, &kind.replace('_', " ")).await?;
    if lines.is_empty() {
        return Err(ErrView::msg("invalid", format!("{owner}.{name} 의 소스를 ALL_SOURCE 에서 찾지 못했습니다")));
    }
    Ok(lines)
}

/// 저장된 본문이 잘렸을 때 — DB 객체면 ALL_SOURCE 에서 그 줄들을 읽어 문장을 다시 자른다
async fn full_text(s: &Session, log: &Log, st: &StmtLite) -> Option<String> {
    let mut parts = st.unit.splitn(3, '.');
    let (owner, name, kind) = (parts.next()?, parts.next()?, parts.next()?.replace('_', " "));
    let binds = [("O".to_string(), owner.to_string()), ("N".into(), name.into()), ("T".into(), kind), ("A".into(), st.line.to_string()), ("B".into(), st.end_line.to_string())];
    let page = select(s, log, "문장 원문 읽기 (저장된 본문이 잘림)", "SELECT text FROM all_source WHERE owner = :o AND name = :n AND type = :t AND line BETWEEN :a AND :b ORDER BY line", &binds, 5000).await.ok()?;
    let src: String = page.rows.iter().filter_map(|r| r.first().cloned().flatten()).collect();
    trace::cut_statement(&src, &st.kind)
}

async fn describe_cached<'a>(s: &Session, descs: &'a mut HashMap<String, Option<meta::TableDesc>>, table: &str) -> Option<&'a meta::TableDesc> {
    if !descs.contains_key(table) {
        let d = meta::describe(s, table).await.ok();
        descs.insert(table.to_string(), d);
    }
    descs.get(table).and_then(|d| d.as_ref())
}

/// SELECT 만 보낸다 — 10초를 넘으면 취소. 보낸 것은 모두 기록에 남는다
async fn select(s: &Session, log: &Log, purpose: &str, sql: &str, binds: &[(String, String)], max: usize) -> Result<sqls_core::session::RowPage, String> {
    let stmts = sqls_core::sql::split_script(sql);
    if stmts.len() != 1 || stmts[0].kind != sqls_core::sql::StmtKind::Query {
        let e = "만든 문장이 SELECT 하나가 아니라 보내지 않았습니다".to_string();
        log.push(purpose, Some(sql), binds, false, None, 0, Some(e.clone()));
        return Err(e);
    }
    let b: Vec<(String, Option<String>)> = binds.iter().map(|(k, v)| (k.clone(), Some(v.clone()))).collect();
    let t0 = Instant::now();
    let r = match tokio::time::timeout(Duration::from_secs(QUERY_SECS), s.query(sql, b, max)).await {
        Ok(Ok(p)) => Ok(p),
        Ok(Err(e)) => Err(e.to_string().lines().filter(|l| !l.contains("ORA-06512")).collect::<Vec<_>>().join("\n")),
        Err(_) => {
            let _ = s.cancel();
            Err(format!("{QUERY_SECS}초를 넘겨 멈췄습니다 (원본 쪽 키에 인덱스가 없을 수 있습니다)"))
        }
    };
    let ms = t0.elapsed().as_millis() as u64;
    match &r {
        Ok(p) => log.push(purpose, Some(sql), binds, true, Some(page_result(p)), ms, None),
        Err(e) => log.push(purpose, Some(sql), binds, true, None, ms, Some(first_line(e).to_string())),
    }
    r
}

/// 결과 한 마디 — COUNT(*) AS N__ 면 그 수, 아니면 행 수
fn page_result(p: &sqls_core::session::RowPage) -> String {
    if p.columns.len() == 1 && p.columns[0].name.eq_ignore_ascii_case("N__") {
        if let Some(n) = p.rows.first().and_then(|r| r.first().cloned().flatten()) {
            return format!("{n}건");
        }
    }
    format!("{}행{}", p.rows.len(), if p.has_more { " 이상" } else { "" })
}

fn key_line(keys: &[(String, String)]) -> String {
    keys.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join(", ")
}

async fn run_path(s: &Session, log: &Log, p: &Path, target: &TraceTarget, descs: &mut HashMap<String, Option<meta::TableDesc>>, run: bool, wt: &mut WriterTrace) {
    // 값에 쓰인 원본 열 — 테이블이 하나면 맨 이름도 (그 테이블의 열인 것만)
    let (mut inputs, bare) = p.inputs();
    if let Some(src) = &p.src {
        if let [(alias, table)] = src.tables.as_slice() {
            if let Some(d) = describe_cached(s, descs, table).await {
                for b in bare {
                    if d.columns.iter().any(|c| c.name == b) {
                        inputs.push(Input { alias: alias.clone(), table: table.clone(), column: b });
                    }
                }
            }
        }
    }
    let mut pks: Vec<(String, Vec<String>)> = Vec::new();
    for i in &inputs {
        if pks.iter().any(|(a, _)| *a == i.alias) {
            continue;
        }
        if let Some(d) = describe_cached(s, descs, &i.table).await {
            pks.push((i.alias.clone(), key_of(d)));
        }
    }
    let mut unknown: Vec<String> = Vec::new();
    loop {
        let q = match trace::build(p, &target.keys, &target.vars, &unknown, &inputs, &pks) {
            Ok(q) => q,
            Err(e) => {
                wt.error = Some(e);
                break;
            }
        };
        wt.sql = Some(q.value.clone());
        wt.binds = q.binds.clone();
        wt.dropped = q.dropped.clone();
        wt.flag_conds = q.flag_conds.clone();
        wt.flags_sql = q.flags.clone();
        wt.rows_sql = q.rows.clone();
        wt.unknown_vars = unknown.clone();
        if p.keys.len() < target.keys.len() && p.key_vars.is_empty() {
            wt.notes.push("키로 거르지 못해 이 행의 값인지 알 수 없습니다 — 돌리지 않았습니다".into());
            log.note("키로 거르지 못해 돌리지 않음");
            break;
        }
        let purpose = if unknown.is_empty() { "이 키로 다시 계산".to_string() } else { format!("이 키로 다시 계산 (모르는 변수 {} 의 조건을 빼고)", unknown.join(", ")) };
        if !run {
            log.unsent(purpose, &q.value, &q.binds);
            break;
        }
        match select(s, log, &purpose, &q.value, &q.binds, 5).await {
            Err(e) => {
                if let Some(v) = trace::unknown_identifier(&e).filter(|v| !unknown.contains(v)) {
                    if unknown.len() < MAX_UNKNOWN {
                        log.hint(v.clone());
                        log.note(format!("{v} 은(는) 값을 모르는 PL/SQL 변수 — 이 변수를 쓰는 조건을 빼고 다시"));
                        unknown.push(v);
                        continue;
                    }
                }
                wt.error = Some(e);
            }
            Ok(page) => {
                wt.ran = true;
                wt.values = page.rows.iter().map(|r| r.first().cloned().flatten()).collect();
                if target.known && !wt.values.is_empty() {
                    wt.reproduced = Some(wt.values.iter().any(|v| trace::same_value(v.as_deref(), target.value.as_deref())));
                }
                if wt.values.is_empty() {
                    if let Some(f) = &q.flags {
                        match select(s, log, "0건 — 거르는 조건마다 통과 여부", f, &q.binds, 20).await {
                            Ok(fp) => wt.flag_rows = fp.rows.iter().map(|r| r[..q.flag_conds.len().min(r.len())].to_vec()).collect(),
                            Err(e) => wt.notes.push(format!("조건 진단 실패: {}", first_line(&e))),
                        }
                    }
                    // 어느 조인에서 이 키의 행이 사라지나
                    for js in trace::join_steps(p, &target.keys, &target.vars, &unknown) {
                        log.hint(js.table.clone());
                        let what = if js.on.is_empty() { format!("조인 단계: {} {} 에서 이 키", js.table, js.alias) } else { format!("조인 단계: + {} {}", js.table, js.alias) };
                        let rows = select(s, log, &what, &js.sql, &q.binds, 1).await.ok().and_then(|pg| pg.rows.first().and_then(|r| r.first().cloned().flatten())).and_then(|n| n.parse().ok());
                        let stop = rows == Some(0);
                        wt.join_steps.push(JoinRun { alias: js.alias, table: js.table, on: js.on, rows, sql: js.sql });
                        if stop {
                            log.note(format!("{} 에서 0건 — 여기서 끊김, 조인 단계 멈춤", js_table_last(&wt.join_steps)));
                            break;
                        }
                    }
                } else if let Some(rs) = &q.rows {
                    match select(s, log, "값을 만든 원본 행과 그 키", rs, &q.binds, 20).await {
                        Ok(rp) => wt.inputs = input_rows(&q.row_cols, &rp.rows, &pks, &inputs),
                        Err(e) => wt.notes.push(format!("원본 행 읽기 실패: {}", first_line(&e))),
                    }
                }
            }
        }
        break;
    }
}

fn js_table_last(j: &[JoinRun]) -> String {
    j.last().map(|x| x.table.clone()).unwrap_or_default()
}

/// (키 열, 원본 쪽 식, 이 행의 값)
fn key_map(p: &Path, target: &TraceTarget) -> Vec<(String, String, String)> {
    let mut out: Vec<(String, String, String)> = p
        .keys
        .iter()
        .filter_map(|(kc, ke)| target.keys.iter().find(|(k, _)| k.eq_ignore_ascii_case(kc)).map(|(_, v)| (kc.clone(), trace::sql_text(ke), v.clone())))
        .collect();
    for (var, kc) in &p.key_vars {
        if let Some((_, v)) = target.keys.iter().find(|(k, _)| k.eq_ignore_ascii_case(kc)) {
            out.push((kc.clone(), var.clone(), v.clone()));
        }
    }
    out
}

fn first_line(s: &str) -> &str {
    s.lines().next().unwrap_or("")
}

/// 원본 행 쿼리 결과 → 입력 열마다 (원본 키, 값)
fn input_rows(cols: &[trace::RowCol], rows: &[Vec<Option<String>>], pks: &[(String, Vec<String>)], inputs: &[Input]) -> Vec<InputRows> {
    let mut out = Vec::new();
    for inp in inputs {
        let Some(ci) = cols.iter().position(|c| c.kind == "I" && c.alias == inp.alias && c.column == inp.column) else { continue };
        let key_cols = pks.iter().find(|(a, _)| *a == inp.alias).map(|(_, k)| k.clone()).unwrap_or_default();
        let mut list: Vec<InputRow> = Vec::new();
        let mut missing = 0;
        for r in rows {
            // 키가 모두 NULL — 아우터 조인에서 짝이 없었다
            if !key_cols.is_empty() && key_cols.iter().all(|k| cols.iter().position(|c| c.alias == inp.alias && c.column == *k).is_none_or(|i| r.get(i).cloned().flatten().is_none())) {
                missing += 1;
                continue;
            }
            let keys: Option<Vec<(String, String)>> = key_cols
                .iter()
                .map(|k| {
                    let i = cols.iter().position(|c| c.alias == inp.alias && c.column == *k)?;
                    r.get(i).cloned().flatten().map(|v| (k.clone(), v))
                })
                .collect();
            let Some(keys) = keys else { continue };
            let row = InputRow { keys, value: r.get(ci).cloned().flatten() };
            if !list.contains(&row) {
                list.push(row);
            }
            if list.len() >= MAX_INPUT_ROWS {
                break;
            }
        }
        let (owner, table) = match inp.table.rsplit_once('.') {
            Some((o, t)) => (Some(o.to_string()), t.to_string()),
            None => (None, inp.table.clone()),
        };
        out.push(InputRows { owner, table, column: inp.column.clone(), alias: inp.alias.clone(), key_cols, rows: list, missing });
    }
    out
}
