//! 값 추적 명령 — 결과 그리드의 한 셀에서 "이 행의 이 값은 어떻게 나왔나".
//!
//! 셀의 열을 테이블 열로 풀고(값의 출처와 같은 계보), 그 행의 키(PK)를 정한 뒤, 분석 결과에서 그 열에 값을 넣는
//! 문장마다 원본 쿼리를 이 키로 다시 돌린다 (sqls_analyze::trace). 다시 계산한 값이 지금 값과 같으면 "재현됨".
//! 0건이면 거르는 조건마다 O/X, 값이 나오면 값을 만든 원본 행과 그 키 — 화면이 한 단계씩 더 거슬러 오른다.
//!
//! DB 에 보내는 것은 SELECT 뿐이고 (보내기 전에 문장 종류를 다시 확인한다), 운영 DB·실행 중인 세션에서는 SQL 만 보인다.
//! 한 질의는 10초까지 — 넘으면 취소한다.

use std::collections::HashMap;
use std::time::Duration;

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
}

#[derive(Serialize, Default)]
pub struct WriterTrace {
    node: String,
    unit: String,
    line: u32,
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
}

#[derive(Serialize)]
pub struct InputRows {
    owner: Option<String>,
    table: String,
    column: String,
    alias: String,
    key_cols: Vec<String>,
    rows: Vec<InputRow>,
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
pub async fn value_trace_start(st: State<'_, AppState>, id: u64, sql: String, pos: Option<usize>, column: String, row: Vec<(String, Option<String>)>) -> R<TraceStep> {
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
    let kc = key_of(&desc);
    if kc.is_empty() {
        return Err(ErrView::msg("invalid", format!("{}.{} 에 PK·고유 인덱스가 없어 행을 집어 거슬러 오를 수 없습니다", desc.owner, desc.name)));
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
    let path = crate::analyze::store_for(st, id)?.root().join("integrated").join("statements.json");
    let stmts = tauri::async_runtime::spawn_blocking(move || crate::analyze::statements(&path)).await.map_err(|e| ErrView::msg("io", e.to_string()))?;
    let Some(stmts) = stmts else {
        return Ok(TraceStep { target, analyzed: false, not_run, writers: vec![], notes: vec![] });
    };
    let writes = sqls_analyze::colwrite::column_writes(&stmts, Some(&desc.owner), &desc.name, &target.column);
    let key_cols: Vec<String> = target.keys.iter().map(|(k, _)| k.clone()).collect();
    let table_cols: Vec<String> = desc.columns.iter().map(|c| c.name.clone()).collect();
    let mut writers = Vec::new();
    for w in writes.iter().take(12) {
        let Some(stmt) = stmts.iter().find(|x| x.node == w.node && x.line == w.line && x.kind == w.kind) else { continue };
        let text = if stmt.text.ends_with('…') { full_text(s, stmt).await.unwrap_or_else(|| stmt.text.clone()) } else { stmt.text.clone() };
        let node_stmts: Vec<StmtLite> = stmts.iter().filter(|x| x.node == stmt.node).cloned().collect();
        let paths = trace::paths(stmt, &node_stmts, &text, &desc.name, &target.column, &key_cols, &table_cols);
        if paths.is_empty() {
            writers.push(WriterTrace {
                node: w.node.clone(),
                unit: w.unit.clone(),
                line: w.line,
                kind: w.kind.clone(),
                label: w.kind.clone(),
                notes: vec!["이 문장에서 그 열의 식을 찾지 못했습니다".into()],
                sql: None,
                ..Default::default()
            });
            continue;
        }
        for p in paths {
            let mut wt = WriterTrace {
                node: w.node.clone(),
                unit: w.unit.clone(),
                line: w.line,
                kind: w.kind.clone(),
                label: p.label.clone(),
                expr: p.expr_text(),
                source: p.source_text(),
                notes: p.notes.clone(),
                ..Default::default()
            };
            run_path(s, &p, &target, &mut descs, not_run.is_none(), &mut wt).await;
            writers.push(wt);
        }
    }
    // 재현된 길을 위로
    writers.sort_by_key(|w| match w.reproduced {
        Some(true) => 0,
        None if !w.values.is_empty() => 1,
        Some(false) => 2,
        None => 3,
    });
    Ok(TraceStep { target, analyzed: true, not_run, writers, notes: vec![] })
}

/// 저장된 본문이 잘렸을 때 — DB 객체면 ALL_SOURCE 에서 그 줄들을 읽어 문장을 다시 자른다
async fn full_text(s: &Session, st: &StmtLite) -> Option<String> {
    let mut parts = st.unit.splitn(3, '.');
    let (owner, name, kind) = (parts.next()?, parts.next()?, parts.next()?.replace('_', " "));
    let page = s
        .query(
            "SELECT text FROM all_source WHERE owner = :o AND name = :n AND type = :t AND line BETWEEN :a AND :b ORDER BY line",
            vec![
                ("O".into(), Some(owner.into())),
                ("N".into(), Some(name.into())),
                ("T".into(), Some(kind)),
                ("A".into(), Some(st.line.to_string())),
                ("B".into(), Some(st.end_line.to_string())),
            ],
            5000,
        )
        .await
        .ok()?;
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

/// SELECT 만 보낸다 — 10초를 넘으면 취소
async fn select(s: &Session, sql: &str, binds: &[(String, String)], max: usize) -> Result<sqls_core::session::RowPage, String> {
    let stmts = sqls_core::sql::split_script(sql);
    if stmts.len() != 1 || stmts[0].kind != sqls_core::sql::StmtKind::Query {
        return Err("만든 문장이 SELECT 하나가 아니라 보내지 않았습니다".into());
    }
    let b: Vec<(String, Option<String>)> = binds.iter().map(|(k, v)| (k.clone(), Some(v.clone()))).collect();
    match tokio::time::timeout(Duration::from_secs(QUERY_SECS), s.query(sql, b, max)).await {
        Ok(Ok(p)) => Ok(p),
        Ok(Err(e)) => Err(e.to_string().lines().filter(|l| !l.contains("ORA-06512")).collect::<Vec<_>>().join("\n")),
        Err(_) => {
            let _ = s.cancel();
            Err(format!("{QUERY_SECS}초를 넘겨 멈췄습니다 (원본 쪽 키에 인덱스가 없을 수 있습니다)"))
        }
    }
}

async fn run_path(s: &Session, p: &Path, target: &TraceTarget, descs: &mut HashMap<String, Option<meta::TableDesc>>, run: bool, wt: &mut WriterTrace) {
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
            break;
        }
        if !run {
            break;
        }
        match select(s, &q.value, &q.binds, 5).await {
            Err(e) => {
                if let Some(v) = trace::unknown_identifier(&e).filter(|v| !unknown.contains(v)) {
                    if unknown.len() < MAX_UNKNOWN {
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
                        match select(s, f, &q.binds, 20).await {
                            Ok(fp) => wt.flag_rows = fp.rows.iter().map(|r| r[..q.flag_conds.len().min(r.len())].to_vec()).collect(),
                            Err(e) => wt.notes.push(format!("조건 진단 실패: {}", first_line(&e))),
                        }
                    }
                } else if let Some(rs) = &q.rows {
                    match select(s, rs, &q.binds, 20).await {
                        Ok(rp) => wt.inputs = input_rows(&q.row_cols, &rp.rows, &pks, &inputs),
                        Err(e) => wt.notes.push(format!("원본 행 읽기 실패: {}", first_line(&e))),
                    }
                }
            }
        }
        break;
    }
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
        for r in rows {
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
        out.push(InputRows { owner, table, column: inp.column.clone(), alias: inp.alias.clone(), key_cols, rows: list });
    }
    out
}
