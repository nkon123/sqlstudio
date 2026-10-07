//! PL/SQL 분석 결과를 읽는 툴 — DB 에 붙지 않는다. `sqlstudio-analyze` / 앱의 분석 화면이 쓴 JSON 만 읽는다.
//!
//! AI 가 "ORDERS 를 바꾸는 시작점은?", "C_REPORT 커서는 무엇을 하나?" 같은 질문에 소스를 다시 읽지 않고 답하게 한다.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use serde_json::{json, Value};
use sqls_analyze::integrate::Integrated;
use sqls_analyze::store::{Store, UnitResult};

/// 결과 폴더별 캐시 (integrated.json 이 바뀌면 다시 읽는다)
#[derive(Default, Clone)]
pub struct Cache {
    inner: Arc<Mutex<HashMap<PathBuf, (SystemTime, Arc<Integrated>)>>>,
}

impl Cache {
    pub fn integrated(&self, dir: &Path) -> Result<Arc<Integrated>, String> {
        let p = dir.join("integrated").join("integrated.json");
        let mtime = std::fs::metadata(&p).and_then(|m| m.modified()).map_err(|_| no_analysis(dir))?;
        if let Some((t, g)) = self.inner.lock().unwrap().get(dir) {
            if *t == mtime {
                return Ok(g.clone());
            }
        }
        let text = std::fs::read_to_string(&p).map_err(|e| format!("{}: {e}", p.display()))?;
        let g: Integrated = serde_json::from_str(&text).map_err(|e| format!("{}: {e}", p.display()))?;
        let g = Arc::new(g);
        self.inner.lock().unwrap().insert(dir.to_path_buf(), (mtime, g.clone()));
        Ok(g)
    }
}

fn no_analysis(dir: &Path) -> String {
    format!(
        "No PL/SQL analysis found in {}. Ask the user to run it first (SQLStudio > 분석, or `sqlstudio-analyze run --connection <name>`).",
        dir.display()
    )
}

fn clip(s: &str, n: usize) -> String {
    if s.chars().count() > n {
        format!("{}…", s.chars().take(n).collect::<String>())
    } else {
        s.to_string()
    }
}

fn matches_name(full: &str, q: &str) -> bool {
    let (f, q) = (full.to_uppercase(), q.trim().to_uppercase());
    f == q || f.ends_with(&format!(".{q}"))
}

pub fn overview(dir: &Path, g: &Integrated) -> Value {
    let store = Store::open(dir).ok();
    let units: Vec<Value> = store
        .map(|s| s.units())
        .unwrap_or_default()
        .iter()
        .take(300)
        .map(|u| {
            json!({
                "unit": format!("{}.{}", u.owner, u.name),
                "type": u.unit_type,
                "lines": u.lines,
                "summary": u.summary.as_ref().map(|i| clip(&i.summary, 300)),
            })
        })
        .collect();
    json!({
        "units": g.units,
        "subprograms": g.nodes.len(),
        "calls": g.edges.len(),
        "tables": g.tables.len(),
        "cursor_flows": g.flows.len(),
        "entry_points": g.entries.iter().take(100).collect::<Vec<_>>(),
        "cycles": g.cycles,
        "overview": g.overview.as_ref().map(|o| &o.summary),
        "unit_list": units,
        "note": "Calls, table CRUD and cursor flows come from static analysis of the source (reliable). Summaries come from a language model (may be wrong).",
    })
}

fn find_unit(store: &Store, name: &str) -> Result<UnitResult, String> {
    let mut cands: Vec<UnitResult> = store.units().into_iter().filter(|u| matches_name(&format!("{}.{}", u.owner, u.name), name)).collect();
    // 패키지 본문을 먼저
    cands.sort_by_key(|u| if u.unit_type.ends_with("BODY") { 0 } else { 1 });
    cands.into_iter().next().ok_or_else(|| format!("No analyzed unit named '{name}'. Use analysis_overview to list them."))
}

pub fn unit(dir: &Path, g: &Integrated, name: &str) -> Result<Value, String> {
    let store = Store::open(dir).map_err(|e| e.to_string())?;
    let u = find_unit(&store, name)?;
    let subs: Vec<Value> = u
        .subprograms
        .iter()
        .map(|s| {
            json!({
                "name": s.path,
                "public": s.public,
                "lines": format!("{}-{}", s.start_line, s.end_line),
                "signature": s.signature,
                "summary": s.summary.as_ref().map(|i| clip(&i.summary, 400)),
                "rules": s.summary.as_ref().map(|i| i.rules.iter().take(8).collect::<Vec<_>>()),
                "tables": s.facts.tables.iter().map(|t| format!("{}({})", t.name, t.ops)).collect::<Vec<_>>(),
                "calls": s.facts.calls.iter().map(|c| c.name.clone()).collect::<Vec<_>>(),
                "commits": s.facts.transactions.iter().map(|t| format!("{}@{}", t.what, t.line)).collect::<Vec<_>>(),
            })
        })
        .collect();
    let flows: Vec<Value> = g
        .flows
        .iter()
        .filter(|f| f.unit == u.key)
        .map(|f| json!({"in": f.node, "cursor": f.cursor, "meaning": f.cursor_summary, "reads": f.from, "writes": format!("{}({})", f.to, f.ops), "line": f.line, "evidence": f.via}))
        .collect();
    let findings: Vec<Value> = g
        .findings
        .iter()
        .filter(|f| f.unit == u.key)
        .take(50)
        .map(|f| json!({"where": f.node, "line": f.line, "kind": f.kind, "source": f.source, "message": f.message}))
        .collect();
    Ok(json!({
        "unit": format!("{}.{}", u.owner, u.name),
        "type": u.unit_type,
        "lines": u.lines,
        "summary": u.summary.as_ref().map(|i| &i.summary),
        "rules": u.summary.as_ref().map(|i| &i.rules),
        "subprograms": subs,
        "long_sql": u.sql_summaries.iter().map(|q| json!({"kind": q.kind, "cursor": q.cursor, "lines": format!("{}-{}", q.line, q.end_line), "summary": q.summary.summary})).collect::<Vec<_>>(),
        "cursor_flows": flows,
        "findings": findings,
        "warning": u.warning,
        "model": u.model,
    }))
}

pub fn table_usage(g: &Integrated, table: &str) -> Result<Value, String> {
    let rows: Vec<&sqls_analyze::integrate::TableRow> = g.tables.iter().filter(|t| matches_name(&t.table, table)).collect();
    if rows.is_empty() {
        return Err(format!("No analyzed code reads or writes '{table}'."));
    }
    Ok(json!(rows
        .iter()
        .map(|t| {
            let flows_in: Vec<Value> = g.flows.iter().filter(|f| f.to == t.table).map(|f| json!({"from": f.from, "cursor": f.cursor, "meaning": f.cursor_summary, "in": f.node, "ops": f.ops, "line": f.line, "evidence": f.via})).collect();
            let flows_out: Vec<Value> = g.flows.iter().filter(|f| f.from.contains(&t.table)).map(|f| json!({"to": f.to, "ops": f.ops, "cursor": f.cursor, "in": f.node, "line": f.line})).collect();
            json!({
                "table": t.table,
                "used_by": t.by.iter().map(|(n, o)| json!({"subprogram": n, "ops": o})).collect::<Vec<_>>(),
                "data_comes_from": t.fed_from,
                "data_goes_to": t.feeds_into,
                "cursor_flows_in": flows_in,
                "cursor_flows_out": flows_out,
                "impacted_entry_points": t.impacted_entries,
                "legend": "ops: C=insert R=select U=update D=delete. impacted_entry_points = entry points that can reach code writing this table.",
            })
        })
        .collect::<Vec<_>>()))
}

pub fn relations(g: &Integrated, name: &str) -> Result<Value, String> {
    let nodes: Vec<&sqls_analyze::integrate::Node> = g.nodes.iter().filter(|n| matches_name(&n.id, name)).take(10).collect();
    if nodes.is_empty() {
        return Err(format!("No analyzed subprogram named '{name}'. Use analysis_unit to list a unit's subprograms."));
    }
    Ok(json!(nodes
        .iter()
        .map(|n| {
            json!({
                "subprogram": n.id,
                "kind": n.kind,
                "signature": n.signature,
                "lines": format!("{}-{}", n.start_line, n.end_line),
                "public": n.public,
                "summary": n.summary,
                "complexity": n.complexity,
                "called_by": g.edges.iter().filter(|e| e.to == n.id).map(|e| json!({"caller": e.from, "lines": e.lines})).collect::<Vec<_>>(),
                "calls": g.edges.iter().filter(|e| e.from == n.id).map(|e| json!({"callee": e.to, "resolved": e.resolved, "external": e.external, "lines": e.lines})).collect::<Vec<_>>(),
                "tables": g.tables.iter().filter_map(|t| t.by.get(&n.id).map(|o| format!("{}({o})", t.table))).collect::<Vec<_>>(),
                "cursor_flows": g.flows.iter().filter(|f| f.node == n.id).map(|f| json!({"cursor": f.cursor, "meaning": f.cursor_summary, "reads": f.from, "writes": format!("{}({})", f.to, f.ops), "line": f.line})).collect::<Vec<_>>(),
                "is_entry_point": g.entries.contains(&n.id),
                "commits_reachable": g.transactions.get(&n.id),
            })
        })
        .collect::<Vec<_>>()))
}

pub fn findings(g: &Integrated, kind: Option<&str>) -> Value {
    let list: Vec<Value> = g
        .findings
        .iter()
        .filter(|f| kind.is_none_or(|k| f.kind.contains(k)))
        .take(200)
        .map(|f| json!({"where": f.node, "line": f.line, "kind": f.kind, "source": f.source, "message": f.message}))
        .collect();
    let mut kinds: HashMap<&str, usize> = HashMap::new();
    for f in &g.findings {
        *kinds.entry(f.kind.as_str()).or_default() += 1;
    }
    json!({ "counts": kinds, "findings": list, "note": "source=static is from the code; source=llm is a model's opinion — verify it." })
}
