//! 통합 분석 — 저장된 단위 결과를 이어서 전체 그림을 만든다. 모델이 없어도 된다.
//!
//! - 호출 그래프: 조각에서 뽑은 호출 이름을 분석한 단위들의 서브프로그램으로 푼다
//!   (같은 패키지 → 같은 스키마의 패키지/단독 프로시저 → 다른 스키마). 못 푼 것은 외부로 남긴다.
//! - 테이블 CRUD 행렬: 테이블마다 누가 읽고·넣고·바꾸고·지우는지
//! - 시작점(아무도 부르지 않는 공개 서브프로그램·트리거), 순환 호출, 안 쓰이는 비공개 서브프로그램
//! - 트랜잭션: 시작점마다 닿는 곳 중 COMMIT/ROLLBACK 하는 곳
//! - 영향 범위: 테이블을 바꾸는 서브프로그램에 닿는 시작점
//! - 확인할 것 목록: 예외 삼킴, 동적 SQL, 모델이 짚은 위험

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};

use serde::{Deserialize, Serialize};

use crate::llm::Insight;
use crate::store::{Store, UnitResult};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Node {
    /// "OWNER.PACKAGE.PROC" / 단독이면 "OWNER.PROC" / 중첩이면 "OWNER.PKG.OUTER.INNER"
    pub id: String,
    pub unit: String,
    pub path: String,
    pub kind: String,
    pub signature: String,
    pub start_line: u32,
    pub end_line: u32,
    pub public: Option<bool>,
    pub summary: Option<String>,
    pub complexity: u32,
    pub lines: u32,
    pub overloads: u32,
    /// 이 노드에서 COMMIT/ROLLBACK 하는지
    pub commits: bool,
    pub autonomous: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Edge {
    pub from: String,
    /// 푼 노드 id, 못 풀었으면 적힌 이름
    pub to: String,
    pub resolved: bool,
    /// 못 푼 것의 분류: "system" (DBMS_/UTL_ 등) / "unknown"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub external: Option<String>,
    pub lines: Vec<u32>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TableRow {
    pub table: String,
    /// 노드 id → "CRUD" 중 쓰는 것
    pub by: BTreeMap<String, String>,
    /// 이 테이블을 바꾸는(C/U/D) 노드에 닿는 시작점
    pub impacted_entries: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Finding {
    pub node: String,
    pub unit: String,
    pub line: Option<u32>,
    /// static | llm
    pub source: String,
    pub kind: String,
    pub message: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Integrated {
    pub generated_at: u64,
    pub units: usize,
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    pub tables: Vec<TableRow>,
    pub entries: Vec<String>,
    /// 서로 부르는 묶음 (노드 2개 이상, 또는 자기 재귀)
    pub cycles: Vec<Vec<String>>,
    /// 부르는 곳이 없는 비공개 서브프로그램
    pub unused: Vec<String>,
    /// 시작점 → 닿는 곳 중 COMMIT/ROLLBACK 하는 노드
    pub transactions: BTreeMap<String, Vec<String>>,
    pub findings: Vec<Finding>,
    /// 모델로 만든 전체 요약 (요청했을 때)
    pub overview: Option<Insight>,
}

fn is_system(name: &str) -> bool {
    let first = name.split('.').next().unwrap_or("");
    let n = if first == "SYS" || first == "PUBLIC" { name.split('.').nth(1).unwrap_or("") } else { first };
    n.starts_with("DBMS_") || n.starts_with("UTL_") || n.starts_with("OWA") || n.starts_with("HTP") || n.starts_with("HTF")
        || n.starts_with("APEX_") || n.starts_with("CTX_") || n.starts_with("SDO_") || n == "STANDARD" || n == "DBMS_OUTPUT" || first == "SYS"
}

/// 스키마가 없는 테이블 이름에 단위의 스키마를 붙인다 ("EMP@LINK" → "APP.EMP@LINK")
fn qualify(owner: &str, name: &str) -> String {
    let base = name.split('@').next().unwrap_or(name);
    if base.contains('.') {
        name.to_string()
    } else {
        format!("{owner}.{name}")
    }
}

fn node_id(u: &UnitResult, path: &str) -> String {
    match u.unit_type.as_str() {
        "PROCEDURE" | "FUNCTION" => format!("{}.{}", u.owner, path),
        "TRIGGER" => format!("{}.{}", u.owner, u.name),
        _ => format!("{}.{}.{}", u.owner, u.name, path),
    }
}

/// 이름 → 노드 찾기용 색인
struct Index {
    /// (owner, unit name) → 단위 키 (패키지·형식 본문)
    containers: HashMap<(String, String), String>,
    /// (owner, name) → 단독 프로시저/함수 노드
    standalone: HashMap<(String, String), String>,
    /// 단위 키 → (서브프로그램 경로 → 노드)
    members: HashMap<String, HashMap<String, String>>,
    /// 단위 이름 → 그 이름의 단위 키들 (다른 스키마)
    by_name: HashMap<String, Vec<String>>,
}

pub fn integrate(units: &[UnitResult]) -> Integrated {
    let mut out = Integrated { generated_at: crate::store::now(), units: units.len(), ..Default::default() };
    let mut ix = Index { containers: HashMap::new(), standalone: HashMap::new(), members: HashMap::new(), by_name: HashMap::new() };
    let mut nodes: BTreeMap<String, Node> = BTreeMap::new();

    for u in units {
        let container = u.unit_type.ends_with("BODY");
        if container {
            ix.containers.insert((u.owner.clone(), u.name.clone()), u.key.clone());
            ix.by_name.entry(u.name.clone()).or_default().push(u.key.clone());
        }
        let m = ix.members.entry(u.key.clone()).or_default();
        for s in &u.subprograms {
            if s.kind == crate::plsql::SubKind::Init {
                continue;
            }
            let id = node_id(u, &s.path);
            m.insert(s.path.clone(), id.clone());
            if !container && s.parent_is_top() {
                ix.standalone.insert((u.owner.clone(), s.name.clone()), id.clone());
            }
            let e = nodes.entry(id.clone()).or_insert_with(|| Node {
                id: id.clone(),
                unit: u.key.clone(),
                path: s.path.clone(),
                kind: crate::chunk::kind_word(s.kind).to_string(),
                signature: s.signature.clone(),
                start_line: s.start_line,
                end_line: s.end_line,
                public: s.public,
                summary: s.summary.as_ref().map(|i| i.summary.clone()).filter(|x| !x.is_empty()),
                complexity: 0,
                lines: 0,
                overloads: 0,
                commits: false,
                autonomous: false,
            });
            e.overloads += 1;
            e.complexity = e.complexity.max(s.facts.complexity);
            e.lines += s.end_line.saturating_sub(s.start_line) + 1;
            e.commits |= s.facts.transactions.iter().any(|t| t.what == "COMMIT" || t.what == "ROLLBACK");
            e.autonomous |= s.facts.transactions.iter().any(|t| t.what.contains("AUTONOMOUS"));
            if e.public != Some(true) && s.public == Some(true) {
                e.public = Some(true);
            }
        }
    }

    // 호출 풀기
    let mut edges: BTreeMap<(String, String), Edge> = BTreeMap::new();
    for u in units {
        for s in &u.subprograms {
            let from = if s.kind == crate::plsql::SubKind::Init { format!("{}.{}.(초기화)", u.owner, u.name) } else { node_id(u, &s.path) };
            for c in &s.facts.calls {
                let (to, resolved) = match resolve(&ix, u, &s.path, &c.name) {
                    Some(id) => (id, true),
                    None => (c.name.clone(), false),
                };
                let external = (!resolved).then(|| if is_system(&c.name) { "system".to_string() } else { "unknown".to_string() });
                let e = edges.entry((from.clone(), to.clone())).or_insert_with(|| Edge { from: from.clone(), to: to.clone(), resolved, external, lines: Vec::new() });
                e.lines.extend(c.lines.iter().copied());
                e.lines.sort();
                e.lines.dedup();
            }
        }
    }
    out.edges = edges.into_values().collect();

    // 그래프
    let mut fwd: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut inbound: HashMap<&str, usize> = HashMap::new();
    for e in out.edges.iter().filter(|e| e.resolved) {
        fwd.entry(e.from.as_str()).or_default().push(e.to.as_str());
        if e.from != e.to {
            *inbound.entry(e.to.as_str()).or_default() += 1;
        }
    }

    // 시작점: 들어오는 호출이 없고 (공개이거나 공개 여부를 모름, 또는 트리거)
    out.entries = nodes
        .values()
        .filter(|n| inbound.get(n.id.as_str()).copied().unwrap_or(0) == 0 && !n.path.contains('.') && n.public != Some(false))
        .map(|n| n.id.clone())
        .collect();
    out.unused = nodes
        .values()
        .filter(|n| inbound.get(n.id.as_str()).copied().unwrap_or(0) == 0 && n.public == Some(false))
        .map(|n| n.id.clone())
        .collect();

    out.cycles = sccs(&nodes.keys().map(|k| k.as_str()).collect::<Vec<_>>(), &fwd);

    // 시작점마다 닿는 곳
    let reach = |start: &str| -> BTreeSet<String> {
        let mut seen = BTreeSet::new();
        let mut q = VecDeque::from([start.to_string()]);
        while let Some(n) = q.pop_front() {
            if !seen.insert(n.clone()) {
                continue;
            }
            for &m in fwd.get(n.as_str()).map(|v| v.as_slice()).unwrap_or(&[]) {
                if !seen.contains(m) {
                    q.push_back(m.to_string());
                }
            }
        }
        seen
    };
    let mut reach_of: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for e in &out.entries {
        let r = reach(e);
        let commits: Vec<String> = r.iter().filter(|n| nodes.get(*n).is_some_and(|x| x.commits)).cloned().collect();
        if !commits.is_empty() {
            out.transactions.insert(e.clone(), commits);
        }
        reach_of.insert(e.clone(), r);
    }

    // 테이블
    let mut tables: BTreeMap<String, TableRow> = BTreeMap::new();
    for u in units {
        for s in &u.subprograms {
            let id = if s.kind == crate::plsql::SubKind::Init { format!("{}.{}.(초기화)", u.owner, u.name) } else { node_id(u, &s.path) };
            for t in &s.facts.tables {
                let name = qualify(&u.owner, &t.name);
                let row = tables.entry(name.clone()).or_insert_with(|| TableRow { table: name.clone(), ..Default::default() });
                let ops = row.by.entry(id.clone()).or_default();
                let mut set: BTreeSet<char> = ops.chars().collect();
                set.extend(t.ops.chars());
                *ops = ["C", "R", "U", "D"].iter().filter(|o| set.contains(&o.chars().next().unwrap())).copied().collect();
            }
        }
        // 전역 커서 등
        for t in &u.globals.tables {
            let name = qualify(&u.owner, &t.name);
            let row = tables.entry(name.clone()).or_insert_with(|| TableRow { table: name.clone(), ..Default::default() });
            row.by.entry(format!("{}.{}.(전역)", u.owner, u.name)).or_insert_with(|| t.ops.clone());
        }
    }
    for row in tables.values_mut() {
        let writers: BTreeSet<&String> = row.by.iter().filter(|(_, o)| o.contains(['C', 'U', 'D'])).map(|(n, _)| n).collect();
        row.impacted_entries = reach_of.iter().filter(|(_, r)| writers.iter().any(|w| r.contains(*w))).map(|(e, _)| e.clone()).collect();
    }
    out.tables = tables.into_values().collect();

    // 확인할 것
    for u in units {
        for s in &u.subprograms {
            let node = node_id(u, &s.path);
            for &l in &s.facts.swallowed {
                out.findings.push(Finding { node: node.clone(), unit: u.key.clone(), line: Some(l), source: "static".into(), kind: "예외 삼킴".into(), message: "예외를 잡고 아무것도 하지 않습니다 (WHEN … THEN NULL)".into() });
            }
            for d in &s.facts.dynamic_sql {
                out.findings.push(Finding { node: node.clone(), unit: u.key.clone(), line: Some(d.line), source: "static".into(), kind: "동적 SQL".into(), message: format!("{} — 정적 분석에 잡히지 않는 테이블·호출이 있을 수 있습니다", d.what) });
            }
            if let Some(ins) = &s.summary {
                for r in &ins.risks {
                    out.findings.push(Finding { node: node.clone(), unit: u.key.clone(), line: r.line, source: "llm".into(), kind: "모델 지적".into(), message: r.issue.clone() });
                }
            }
        }
        if let Some(w) = &u.warning {
            out.findings.push(Finding { node: u.key.clone(), unit: u.key.clone(), line: None, source: "static".into(), kind: "구조".into(), message: w.clone() });
        }
    }
    for c in &out.cycles {
        out.findings.push(Finding { node: c[0].clone(), unit: String::new(), line: None, source: "static".into(), kind: "순환 호출".into(), message: c.join(" → ") });
    }

    out.nodes = nodes.into_values().collect();
    out
}

impl crate::store::SubResult {
    fn parent_is_top(&self) -> bool {
        !self.path.contains('.')
    }
}

/// 호출 이름 → 노드 id
fn resolve(ix: &Index, u: &UnitResult, caller_path: &str, name: &str) -> Option<String> {
    let parts: Vec<&str> = name.split('.').collect();
    let members = ix.members.get(&u.key);
    match parts.as_slice() {
        [x] => {
            // 안쪽 범위부터: OUTER.INNER 안에서 X → OUTER.INNER.X, OUTER.X, X
            if let Some(m) = members {
                let mut scope: Vec<&str> = caller_path.split('.').collect();
                loop {
                    let cand = if scope.is_empty() { x.to_string() } else { format!("{}.{x}", scope.join(".")) };
                    if let Some(id) = m.get(&cand) {
                        return Some(id.clone());
                    }
                    if scope.pop().is_none() {
                        break;
                    }
                }
            }
            ix.standalone.get(&(u.owner.clone(), x.to_string())).cloned()
        }
        [a, b] => {
            // 자기 패키지 이름을 붙여 부른 것
            if *a == u.name {
                if let Some(id) = members.and_then(|m| m.get(*b)) {
                    return Some(id.clone());
                }
            }
            // 같은 스키마의 패키지
            if let Some(k) = ix.containers.get(&(u.owner.clone(), a.to_string())) {
                return ix.members.get(k).and_then(|m| m.get(*b)).cloned();
            }
            // 스키마.단독프로시저
            if let Some(id) = ix.standalone.get(&(a.to_string(), b.to_string())) {
                return Some(id.clone());
            }
            // 다른 스키마의 같은 이름 패키지 (동의어로 부른 것) — 하나뿐일 때만
            if let Some(keys) = ix.by_name.get(*a) {
                if keys.len() == 1 {
                    return ix.members.get(&keys[0]).and_then(|m| m.get(*b)).cloned();
                }
            }
            None
        }
        [s, a, b] => ix.containers.get(&(s.to_string(), a.to_string())).and_then(|k| ix.members.get(k)).and_then(|m| m.get(*b)).cloned(),
        _ => None,
    }
}

/// 강하게 연결된 묶음 (Tarjan, 반복형 — 깊은 호출 사슬에서도 스택이 넘치지 않는다)
fn sccs(nodes: &[&str], fwd: &HashMap<&str, Vec<&str>>) -> Vec<Vec<String>> {
    let idx_of: HashMap<&str, usize> = nodes.iter().enumerate().map(|(i, n)| (*n, i)).collect();
    let n = nodes.len();
    let adj: Vec<Vec<usize>> = nodes.iter().map(|x| fwd.get(x).map(|v| v.iter().filter_map(|t| idx_of.get(t).copied()).collect()).unwrap_or_default()).collect();
    let mut index = vec![usize::MAX; n];
    let mut low = vec![0; n];
    let mut on = vec![false; n];
    let mut stack = Vec::new();
    let mut next = 0;
    let mut out = Vec::new();
    for s in 0..n {
        if index[s] != usize::MAX {
            continue;
        }
        let mut work: Vec<(usize, usize)> = vec![(s, 0)];
        while let Some(&mut (v, ref mut ei)) = work.last_mut() {
            if *ei == 0 && index[v] == usize::MAX {
                index[v] = next;
                low[v] = next;
                next += 1;
                stack.push(v);
                on[v] = true;
            }
            if *ei < adj[v].len() {
                let w = adj[v][*ei];
                *ei += 1;
                if index[w] == usize::MAX {
                    work.push((w, 0));
                } else if on[w] {
                    low[v] = low[v].min(index[w]);
                }
                continue;
            }
            work.pop();
            if let Some(&(p, _)) = work.last() {
                low[p] = low[p].min(low[v]);
            }
            if low[v] == index[v] {
                let mut comp = Vec::new();
                while let Some(w) = stack.pop() {
                    on[w] = false;
                    comp.push(nodes[w].to_string());
                    if w == v {
                        break;
                    }
                }
                let self_loop = comp.len() == 1 && adj[v].contains(&v);
                if comp.len() > 1 || self_loop {
                    comp.sort();
                    out.push(comp);
                }
            }
        }
    }
    out.sort();
    out
}

/// 사람이 읽을 보고서 (Markdown)
pub fn report(g: &Integrated, units: &[UnitResult]) -> String {
    let mut s = String::new();
    s.push_str("# PL/SQL 통합 분석\n\n");
    s.push_str(&format!(
        "단위 {}개 · 서브프로그램 {}개 · 호출 {}개 (못 푼 것 {}개) · 테이블 {}개\n\n",
        g.units,
        g.nodes.len(),
        g.edges.len(),
        g.edges.iter().filter(|e| !e.resolved && e.external.as_deref() != Some("system")).count(),
        g.tables.len()
    ));
    if let Some(o) = &g.overview {
        s.push_str("## 전체 요약\n\n");
        s.push_str(&format!("{}\n\n", o.summary));
        for x in &o.steps {
            s.push_str(&format!("- {x}\n"));
        }
        s.push('\n');
    }

    s.push_str("## 단위\n\n");
    for u in units {
        s.push_str(&format!("### {} {}.{}\n\n", u.unit_type, u.owner, u.name));
        if let Some(i) = &u.summary {
            s.push_str(&format!("{}\n\n", i.summary));
            for r in &i.rules {
                s.push_str(&format!("- 규칙: {r}\n"));
            }
        }
        s.push_str("| 서브프로그램 | 공개 | 줄 | 하는 일 |\n|---|---|---|---|\n");
        for sp in &u.subprograms {
            let vis = match sp.public {
                Some(true) => "공개",
                Some(false) => "비공개",
                None => "",
            };
            let sum = sp.summary.as_ref().map(|i| i.summary.replace('|', "\\|").replace('\n', " ")).unwrap_or_default();
            s.push_str(&format!("| `{}` | {vis} | {}~{} | {sum} |\n", sp.path, sp.start_line, sp.end_line));
        }
        s.push('\n');
    }

    s.push_str("## 시작점과 트랜잭션\n\n");
    for e in &g.entries {
        let tx = g.transactions.get(e).map(|v| format!(" — COMMIT/ROLLBACK: {}", v.join(", "))).unwrap_or_default();
        s.push_str(&format!("- `{e}`{tx}\n"));
    }
    s.push('\n');

    s.push_str("## 테이블 CRUD\n\n| 테이블 | 쓰는 곳 | 영향 받는 시작점 |\n|---|---|---|\n");
    for t in &g.tables {
        let by: Vec<String> = t.by.iter().map(|(n, o)| format!("`{n}` {o}")).collect();
        s.push_str(&format!("| {} | {} | {} |\n", t.table, by.join("<br>"), t.impacted_entries.len()));
    }
    s.push('\n');

    // 호출 그래프 (Mermaid) — 크면 읽을 수 없으므로 푼 호출만, 최대 150개
    let resolved: Vec<&Edge> = g.edges.iter().filter(|e| e.resolved && e.from != e.to).take(150).collect();
    if !resolved.is_empty() {
        s.push_str("## 호출 그래프\n\n```mermaid\nflowchart LR\n");
        let mut ids: BTreeMap<&str, String> = BTreeMap::new();
        for e in &resolved {
            for n in [e.from.as_str(), e.to.as_str()] {
                let k = ids.len();
                ids.entry(n).or_insert_with(|| format!("n{k}"));
            }
        }
        for (n, id) in &ids {
            let short = n.splitn(2, '.').nth(1).unwrap_or(n);
            s.push_str(&format!("  {id}[\"{}\"]\n", short.replace('"', "'")));
        }
        for e in &resolved {
            s.push_str(&format!("  {} --> {}\n", ids[e.from.as_str()], ids[e.to.as_str()]));
        }
        s.push_str("```\n\n");
    }

    if !g.cycles.is_empty() {
        s.push_str("## 순환 호출\n\n");
        for c in &g.cycles {
            s.push_str(&format!("- {}\n", c.join(" ↔ ")));
        }
        s.push('\n');
    }
    if !g.unused.is_empty() {
        s.push_str("## 부르는 곳이 없는 비공개 서브프로그램\n\n");
        for c in &g.unused {
            s.push_str(&format!("- `{c}`\n"));
        }
        s.push('\n');
    }
    let unknown: BTreeSet<&str> = g.edges.iter().filter(|e| e.external.as_deref() == Some("unknown")).map(|e| e.to.as_str()).collect();
    if !unknown.is_empty() {
        s.push_str("## 못 푼 호출 (분석하지 않은 단위, 동의어, 또는 형식 생성자·컬렉션)\n\n");
        s.push_str(&unknown.iter().map(|x| format!("`{x}`")).collect::<Vec<_>>().join(", "));
        s.push_str("\n\n");
    }
    if !g.findings.is_empty() {
        s.push_str("## 확인할 것\n\n| 어디 | 줄 | 종류 | 내용 |\n|---|---|---|---|\n");
        for f in &g.findings {
            s.push_str(&format!("| `{}` | {} | {} ({}) | {} |\n", f.node, f.line.map(|l| l.to_string()).unwrap_or_default(), f.kind, f.source, f.message.replace('|', "\\|").replace('\n', " ")));
        }
    }
    s
}

/// 저장소의 모든 단위로 통합 분석을 만들고 파일로 쓴다. 돌려주는 것은 쓴 파일들.
pub fn write_all(store: &Store, overview: Option<Insight>) -> std::io::Result<(Integrated, Vec<std::path::PathBuf>)> {
    let units = store.units();
    let mut g = integrate(&units);
    g.overview = overview;
    let mut files = Vec::new();
    files.push(store.write_integrated("graph.json", &serde_json::json!({ "nodes": g.nodes, "edges": g.edges, "entries": g.entries, "cycles": g.cycles, "unused": g.unused, "transactions": g.transactions }))?);
    files.push(store.write_integrated("crud.json", &g.tables)?);
    files.push(store.write_integrated("findings.json", &g.findings)?);
    files.push(store.write_integrated("integrated.json", &g)?);
    files.push(store.write_text("integrated/report.md", &report(&g, &units))?);
    Ok((g, files))
}

/// 모델로 전체 요약 (단위 요약들을 묶어서)
pub async fn overview(store: &Store, client: &sqls_llm::Client, cfg: &sqls_llm::ProviderConfig, lang: crate::llm::Lang, budget: usize) -> Option<Insight> {
    let units = store.units();
    let items: Vec<String> = units
        .iter()
        .filter_map(|u| {
            u.summary.as_ref().map(|s| {
                let w: Vec<&str> = u.facts.tables.iter().filter(|t| t.ops.contains(['C', 'U', 'D'])).map(|t| t.name.as_str()).take(10).collect();
                format!("{} {}.{}: {} (writes: {})\n", u.unit_type, u.owner, u.name, s.summary, w.join(", "))
            })
        })
        .collect();
    if items.is_empty() {
        return None;
    }
    let sys = crate::llm::rollup_system(lang, "a whole PL/SQL application (many packages)");
    let mut level = items;
    for _ in 0..5 {
        if level.concat().chars().count() <= budget || level.len() <= 1 {
            break;
        }
        let mut groups: Vec<String> = Vec::new();
        let mut cur = String::new();
        for it in &level {
            if !cur.is_empty() && cur.chars().count() + it.chars().count() > budget {
                groups.push(std::mem::take(&mut cur));
            }
            cur.push_str(it);
        }
        if !cur.is_empty() {
            groups.push(cur);
        }
        let mut next = Vec::new();
        for g in &groups {
            let (ins, _) = crate::llm::ask_summary(client, cfg, sys.clone(), format!("Summarize this group of units.\n{g}")).await.ok()?;
            next.push(format!("- {}\n", ins.summary));
        }
        if next.len() >= level.len() {
            break;
        }
        level = next;
    }
    crate::llm::ask_summary(client, cfg, sys, format!("Describe the whole application from its units.\n{}", level.concat())).await.ok().map(|(i, _)| i)
}
