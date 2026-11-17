//! PL/SQL 분석 결과를 읽는 툴 — DB 에 붙지 않는다. `sqlstudio-analyze` / 앱의 분석 화면이 쓴 JSON 만 읽는다.
//!
//! AI 가 "ORDERS 를 바꾸는 시작점은?", "C_REPORT 커서는 무엇을 하나?" 같은 질문에 소스를 다시 읽지 않고 답하게 한다.
//!
//! 흐름도 쪽 (데이터 검증 하네스가 코드 흐름을 따라가게):
//! - [`reports`] / [`report`] — 사람이 손본 흐름도 보고서 (integrated/reports/<id>.json): 상자·선·메모
//! - [`lineage`] — 테이블에서 위(데이터가 어디서 오나)·아래(어디로 가나)로 테이블 → 처리 → 테이블 단계
//! - [`sql_of`] — 서브프로그램의 SQL 문 (statements.json): 줄, 읽기·쓰기, INTO, 데이터를 대는 커서
//! - [`column_writers`] — 열 하나에 값을 넣는 문장과 그 문장에 데이터를 대는 커서 쿼리

use sqls_core::sync::MutexExt;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use serde_json::{json, Value};
use sqls_analyze::colwrite::{column_writes, StmtLite};
use sqls_analyze::integrate::{Integrated, StmtRow};
use sqls_analyze::store::{Store, UnitResult};

/// 결과 폴더별 캐시 (integrated.json · statements.json 이 바뀌면 다시 읽는다)
#[derive(Default, Clone)]
pub struct Cache {
    inner: Arc<Mutex<HashMap<PathBuf, (SystemTime, Arc<Integrated>)>>>,
    stmts: Arc<Mutex<HashMap<PathBuf, (SystemTime, Arc<Vec<StmtRow>>)>>>,
}

impl Cache {
    pub fn statements(&self, dir: &Path) -> Result<Arc<Vec<StmtRow>>, String> {
        let p = dir.join("integrated").join("statements.json");
        let mtime = std::fs::metadata(&p).and_then(|m| m.modified()).map_err(|_| no_analysis(dir))?;
        if let Some((t, v)) = self.stmts.lk().get(dir) {
            if *t == mtime {
                return Ok(v.clone());
            }
        }
        let f = std::fs::File::open(&p).map_err(|e| format!("{}: {e}", p.display()))?;
        let v: Vec<StmtRow> = serde_json::from_reader(std::io::BufReader::new(f)).map_err(|e| format!("{}: {e}", p.display()))?;
        let v = Arc::new(v);
        self.stmts.lk().insert(dir.to_path_buf(), (mtime, v.clone()));
        Ok(v)
    }

    pub fn integrated(&self, dir: &Path) -> Result<Arc<Integrated>, String> {
        let p = dir.join("integrated").join("integrated.json");
        let mtime = std::fs::metadata(&p).and_then(|m| m.modified()).map_err(|_| no_analysis(dir))?;
        if let Some((t, g)) = self.inner.lk().get(dir) {
            if *t == mtime {
                return Ok(g.clone());
            }
        }
        let text = std::fs::read_to_string(&p).map_err(|e| format!("{}: {e}", p.display()))?;
        let g: Integrated = serde_json::from_str(&text).map_err(|e| format!("{}: {e}", p.display()))?;
        let g = Arc::new(g);
        self.inner.lk().insert(dir.to_path_buf(), (mtime, g.clone()));
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

// ─────────────────────────────────────────────────────────────
// 흐름도 — 보고서, 계보, 문장
// ─────────────────────────────────────────────────────────────

fn reports_dir(dir: &Path) -> PathBuf {
    dir.join("integrated").join("reports")
}

fn read_reports(dir: &Path) -> Vec<Value> {
    let Ok(rd) = std::fs::read_dir(reports_dir(dir)) else { return Vec::new() };
    let mut v: Vec<Value> = rd
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .filter_map(|e| std::fs::read_to_string(e.path()).ok())
        .filter_map(|t| serde_json::from_str::<Value>(&t).ok())
        .filter(|r| r["id"].is_string())
        .collect();
    v.sort_by_key(|r| std::cmp::Reverse(r["updated_at"].as_u64().unwrap_or(0)));
    v
}

/// 저장된 흐름도 보고서 목록
pub fn reports(dir: &Path) -> Value {
    let list: Vec<Value> = read_reports(dir)
        .iter()
        .map(|r| {
            let boxes = r["nodes"].as_array().map(|n| n.iter().filter(|x| x["type"] != "note").count()).unwrap_or(0);
            json!({
                "id": r["id"], "name": r["name"], "title": r["title"],
                "description": clip(r["description"].as_str().unwrap_or(""), 300),
                "center": r["basis"]["center"], "direction": r["basis"]["dir"], "boxes": boxes, "updated_at": r["updated_at"],
            })
        })
        .collect();
    json!({
        "reports": list,
        "note": "Flow diagrams a person saved in SQLStudio (분석 > 흐름도 > 보고서). Open one with flow_report. With no reports, use data_lineage on a table instead.",
    })
}

fn tables_of(g: &Integrated, node: &str) -> Vec<String> {
    g.tables.iter().filter_map(|t| t.by.get(node).map(|o| format!("{}({o})", t.table))).collect()
}

fn box_label(n: &Value) -> String {
    n["text"].as_str().filter(|t| !t.is_empty()).or(n["label"].as_str()).unwrap_or("").to_string()
}

/// 흐름도 보고서 하나 — 숨긴 것은 빼고, 메모·선 설명은 붙여서
pub fn report(dir: &Path, g: &Integrated, id: &str) -> Result<Value, String> {
    let all = read_reports(dir);
    let q = id.trim().to_lowercase();
    let same = |v: &Value| v.as_str().is_some_and(|x| x.to_lowercase() == q);
    let r = all
        .iter()
        .find(|r| same(&r["id"]))
        .or_else(|| all.iter().find(|r| same(&r["name"]) || same(&r["title"])))
        .ok_or_else(|| format!("No flow report '{id}'. Use flow_reports to list them."))?;
    let empty = Vec::new();
    let nodes = r["nodes"].as_array().unwrap_or(&empty);
    let shown = |v: &Value| v["hidden"] != true;
    let by_id: HashMap<&str, &Value> = nodes.iter().filter_map(|n| n["id"].as_str().map(|i| (i, n))).collect();
    let name_of = |i: &str| by_id.get(i).map(|n| box_label(n)).unwrap_or_else(|| i.to_string());

    let boxes: Vec<Value> = nodes
        .iter()
        .filter(|n| n["type"] != "note" && shown(n))
        .map(|n| {
            let rf = n["ref"].as_str().unwrap_or("");
            let mut o = json!({"id": n["id"], "ref": rf, "type": n["type"], "label": box_label(n)});
            if n["gone"] == true {
                o["gone"] = json!("no longer in the analysis");
            }
            if matches!(n["type"].as_str(), Some("proc" | "trig")) {
                o["tables"] = json!(tables_of(g, rf));
                if let Some(x) = g.nodes.iter().find(|x| x.id == rf) {
                    o["summary"] = json!(x.summary.as_deref().map(|s| clip(s, 300)));
                    o["lines"] = json!(format!("{}-{}", x.start_line, x.end_line));
                }
            }
            o
        })
        .collect();
    let notes: Vec<Value> = nodes
        .iter()
        .filter(|n| n["type"] == "note" && shown(n))
        .map(|n| json!({"text": n["text"], "about": n["attach"].as_str().map(name_of)}))
        .collect();
    let edges_v: Vec<&Value> = r["edges"].as_array().map(|e| e.iter().filter(|x| shown(x)).collect()).unwrap_or_default();
    let edges: Vec<Value> = edges_v
        .iter()
        .map(|e| {
            let (s, t) = (e["source"].as_str().unwrap_or(""), e["target"].as_str().unwrap_or(""));
            let mut o = json!({"from": name_of(s), "to": name_of(t), "kind": e["kind"], "ops": e["label"]});
            if let Some(x) = e["text"].as_str().filter(|x| !x.is_empty()) {
                o["note"] = json!(x);
            }
            for k in ["guess", "gone"] {
                if e[k] == true {
                    o[k] = json!(true);
                }
            }
            o
        })
        .collect();

    let mut m = Mermaid::default();
    for n in nodes.iter().filter(|n| n["type"] != "note" && shown(n)) {
        m.node(n["id"].as_str().unwrap_or(""), &box_label(n), n["type"].as_str().unwrap_or(""));
    }
    for e in &edges_v {
        let lab = e["text"].as_str().filter(|x| !x.is_empty()).or(e["label"].as_str()).unwrap_or("");
        m.edge(e["source"].as_str().unwrap_or(""), e["target"].as_str().unwrap_or(""), e["kind"].as_str().unwrap_or(""), lab);
    }
    Ok(json!({
        "id": r["id"], "title": r["title"], "description": r["description"], "basis": r["basis"],
        "boxes": boxes, "edges": edges, "notes": notes, "mermaid": m.out(),
        "legend": "kind: read/write/call/trig(trigger fires)/flow(cursor data)/use(dictionary only, op unknown)/manual(drawn by a person). ops: C=insert R=select U=update D=delete. notes and edge notes are the person's description of the business flow — validate the data against them.",
    }))
}

/// Mermaid flowchart (LR) — 보고서·계보를 그림 글로도
#[derive(Default)]
struct Mermaid {
    ids: HashMap<String, String>,
    lines: Vec<String>,
}

impl Mermaid {
    fn id(&mut self, key: &str) -> String {
        let n = self.ids.len();
        self.ids.entry(key.to_string()).or_insert_with(|| format!("n{n}")).clone()
    }
    fn node(&mut self, key: &str, label: &str, ty: &str) {
        if self.ids.contains_key(key) {
            return;
        }
        let id = self.id(key);
        let l = label.replace('"', "'");
        self.lines.push(match ty {
            "table" => format!("  {id}[(\"{l}\")]"),
            "xml" | "xmlfile" => format!("  {id}[/\"{l}\"/]"),
            _ => format!("  {id}[\"{l}\"]"),
        });
    }
    fn edge(&mut self, s: &str, t: &str, kind: &str, label: &str) {
        let line = {
            let (s, t) = (self.id(s), self.id(t));
            let arrow = if matches!(kind, "call" | "use") { "-.->" } else { "-->" };
            let l = label.replace(['"', '|'], "'");
            if l.is_empty() { format!("  {s} {arrow} {t}") } else { format!("  {s} {arrow}|\"{l}\"| {t}") }
        };
        if !self.lines.contains(&line) {
            self.lines.push(line);
        }
    }
    fn out(&self) -> String {
        format!("flowchart LR\n{}", self.lines.join("\n"))
    }
}

fn writes(ops: &str) -> bool {
    ops.contains(['C', 'U'])
}

/// 테이블에서 위·아래로 데이터 계보. depth 는 테이블 단계 (처리 하나 + 테이블 하나 = 1)
pub fn lineage(g: &Integrated, table: &str, direction: &str, depth: usize) -> Result<Value, String> {
    let start = g
        .tables
        .iter()
        .find(|t| matches_name(&t.table, table))
        .ok_or_else(|| format!("No analyzed code reads or writes '{table}'."))?;
    let up = match direction.trim() {
        "up" | "upstream" | "" => true,
        "down" | "downstream" => false,
        d => return Err(format!("direction must be 'up' (where the data comes from) or 'down' (where it goes), not '{d}'")),
    };
    let depth = depth.clamp(1, 10);
    let by_table: HashMap<&str, &sqls_analyze::integrate::TableRow> = g.tables.iter().map(|t| (t.table.as_str(), t)).collect();
    let mut seen: std::collections::HashSet<String> = [start.table.clone()].into();
    let mut frontier = vec![start.table.clone()];
    let mut steps: Vec<Value> = Vec::new();
    let mut m = Mermaid::default();
    let mut truncated = false;
    'levels: for level in 1..=depth {
        let mut next = Vec::new();
        for tname in &frontier {
            let Some(t) = by_table.get(tname.as_str()) else { continue };
            // up: 이 테이블을 쓰는 처리 / down: 이 테이블을 읽는 처리
            for (node, ops) in t.by.iter().filter(|(_, o)| if up { writes(o) } else { o.contains('R') }) {
                if steps.len() >= 300 {
                    truncated = true;
                    break 'levels;
                }
                let other: Vec<String> = g
                    .tables
                    .iter()
                    .filter(|x| x.table != *tname)
                    .filter(|x| x.by.get(node).is_some_and(|o| if up { o.contains('R') } else { writes(o) }))
                    .map(|x| x.table.clone())
                    .collect();
                let flows: Vec<Value> = g
                    .flows
                    .iter()
                    .filter(|f| f.node == *node && if up { f.to == *tname } else { f.from.contains(tname) })
                    .map(|f| json!({"cursor": f.cursor, "reads": f.from, "writes": format!("{}({})", f.to, f.ops), "line": f.line, "evidence": f.via, "meaning": f.cursor_summary}))
                    .collect();
                m.node(tname, tname, "table");
                m.node(node, node, "proc");
                if up {
                    m.edge(node, tname, "write", ops);
                } else {
                    m.edge(tname, node, "read", "R");
                }
                for o in &other {
                    m.node(o, o, "table");
                    if up {
                        m.edge(o, node, "read", "R");
                    } else {
                        let oo = by_table.get(o.as_str()).and_then(|x| x.by.get(node)).cloned().unwrap_or_default();
                        m.edge(node, o, "write", &oo);
                    }
                    if seen.insert(o.clone()) {
                        next.push(o.clone());
                    }
                }
                let n = g.nodes.iter().find(|x| x.id == *node);
                let mut step = json!({
                    "level": level,
                    "table": tname,
                    "subprogram": node,
                    "ops_on_table": ops,
                    "cursor_flows": flows,
                    "lines": n.map(|n| format!("{}-{}", n.start_line, n.end_line)),
                    "summary": n.and_then(|n| n.summary.as_deref()).map(|s| clip(s, 200)),
                    "is_entry_point": g.entries.contains(node),
                    "confidence": t.conf.get(node),
                });
                step[if up { "reads_from" } else { "writes_to" }] = json!(other);
                steps.push(step);
            }
        }
        if next.is_empty() {
            break;
        }
        frontier = next;
    }
    Ok(json!({
        "table": start.table,
        "direction": if up { "up" } else { "down" },
        "steps": steps,
        "truncated": truncated,
        "mermaid": m.out(),
        "legend": "up: each step is a subprogram that writes `table` and the tables it reads (candidate sources). down: a subprogram that reads `table` and the tables it writes. cursor_flows are proven data paths (cursor rows → DML); reads_from/writes_to without a cursor flow only means the same subprogram touches both. confidence: ok=dictionary+source, src=source only, dict=dictionary only, llm=model guess. Next: subprogram_sql for the statements, column_writers for one column.",
    }))
}

fn stmt_json(r: &StmtRow) -> Value {
    let s = &r.stmt;
    json!({
        "node": r.node, "lines": format!("{}-{}", s.line, s.end_line), "kind": s.kind, "cursor": s.cursor,
        "reads": s.reads, "writes": s.writes.iter().map(|w| format!("{}({})", w.name, w.ops)).collect::<Vec<_>>(),
        "into": s.into, "fed_by": s.fed_by.iter().map(|f| format!("{} via {}", f.cursor, f.via)).collect::<Vec<_>>(),
        "sql": s.text,
    })
}

/// 서브프로그램(또는 패키지 전체)의 SQL 문 — 파일 순서 그대로
pub fn sql_of(stmts: &[StmtRow], name: &str) -> Result<Value, String> {
    let q = name.trim().to_uppercase();
    let mut rows: Vec<&StmtRow> = stmts.iter().filter(|r| matches_name(&r.node, &q)).collect();
    if rows.is_empty() {
        // 패키지 이름이면 그 안의 서브프로그램 전부
        let (head, mid) = (format!("{q}."), format!(".{q}."));
        rows = stmts.iter().filter(|r| r.node.starts_with(&head) || r.node.contains(&mid)).collect();
    }
    if rows.is_empty() {
        return Err(format!("No SQL statements stored for '{name}'. Use analysis_unit to see its subprograms."));
    }
    let total = rows.len();
    Ok(json!({
        "statements": rows.iter().take(200).map(|r| stmt_json(r)).collect::<Vec<_>>(),
        "total": total,
        "truncated": total > 200,
        "legend": "kind: CURSOR/FOR LOOP/OPEN FOR/SELECT INTO/SELECT/INSERT/UPDATE/DELETE/MERGE/EXECUTE IMMEDIATE. fed_by: the cursor whose rows reach this DML (record R = loop record field, variable V = fetched variable, CURRENT OF, loop body = weak link). sql is clipped at 2000 chars (ends with …) — use get_ddl for the full source.",
    }))
}

fn lite(r: &StmtRow) -> StmtLite {
    StmtLite {
        node: r.node.clone(),
        unit: r.unit.clone(),
        line: r.stmt.line,
        end_line: r.stmt.end_line,
        kind: r.stmt.kind.clone(),
        writes: r.stmt.writes.clone(),
        text: r.stmt.text.clone(),
        cursor: r.stmt.cursor.clone(),
        into: r.stmt.into.clone(),
        fed_by: r.stmt.fed_by.clone(),
    }
}

/// 열 하나에 값을 넣는 문장 + 그 문장에 데이터를 대는 커서 쿼리
pub fn column_writers(stmts: &[StmtRow], table: &str, column: &str) -> Result<Value, String> {
    let t = table.trim().to_uppercase();
    let (owner, name) = match t.rsplit_once('.') {
        Some((o, n)) => (Some(o.to_string()), n.to_string()),
        None => (None, t.clone()),
    };
    let col = column.trim().to_uppercase();
    let lites: Vec<StmtLite> = stmts.iter().map(lite).collect();
    let found = column_writes(&lites, owner.as_deref(), &name, &col);
    let list: Vec<Value> = found
        .iter()
        .take(100)
        .map(|w| {
            let me = stmts.iter().find(|r| r.node == w.node && r.stmt.line == w.line);
            let feeds: Vec<Value> = me
                .map(|r| r.stmt.fed_by.as_slice())
                .unwrap_or_default()
                .iter()
                .map(|f| {
                    let decl = stmts.iter().find(|x| {
                        (x.node == w.node || x.unit == w.unit)
                            && x.stmt.cursor.as_deref() == Some(f.cursor.as_str())
                            && matches!(x.stmt.kind.as_str(), "CURSOR" | "FOR LOOP" | "OPEN FOR")
                    });
                    json!({
                        "cursor": f.cursor, "via": f.via,
                        "query_lines": decl.map(|d| format!("{}-{}", d.stmt.line, d.stmt.end_line)),
                        "query": decl.map(|d| d.stmt.text.clone()),
                        "reads": decl.map(|d| d.stmt.reads.clone()),
                    })
                })
                .collect();
            json!({
                "node": w.node, "lines": format!("{}-{}", w.line, w.end_line), "kind": w.kind,
                "how": w.how, "sql": w.text, "reads": me.map(|r| r.stmt.reads.clone()), "fed_by": feeds,
            })
        })
        .collect();
    if list.is_empty() {
        return Err(format!("No analyzed statement writes {t}.{col}. Check the name with table_usage — the column may be set by a trigger, a default, or code outside the analysis."));
    }
    Ok(json!({
        "table": t, "column": col, "writers": list,
        "legend": "how: set = the statement names this column, all = INSERT without a column list, maybe = statement text was clipped. fed_by.query is the cursor that supplies the row (map its SELECT list to the column). Turn each writer into checks: recompute the value from its source query and compare.",
    }))
}
