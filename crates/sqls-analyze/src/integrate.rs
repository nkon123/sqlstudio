//! 통합 분석 — 저장된 단위 결과를 이어서 전체 그림을 만든다. 모델이 없어도 된다.
//!
//! - 호출 그래프: 조각에서 뽑은 호출 이름을 분석한 단위들의 서브프로그램으로 푼다
//!   (같은 패키지 → 같은 스키마의 패키지/단독 프로시저 → 다른 스키마). 못 푼 것은 외부로 남긴다.
//! - 테이블 CRUD 행렬: 테이블마다 누가 읽고·넣고·바꾸고·지우는지
//! - 시작점(아무도 부르지 않는 공개 서브프로그램·트리거), 순환 호출, 안 쓰이는 비공개 서브프로그램
//! - 트랜잭션: 시작점마다 닿는 곳 중 COMMIT/ROLLBACK 하는 곳
//! - 영향 범위: 테이블을 바꾸는 서브프로그램에 닿는 시작점
//! - 확인할 것 목록: 예외 삼킴, 동적 SQL, 모델이 짚은 위험
//! - 사전 대조: 사전 의존성(ALL_DEPENDENCIES)과 파서의 테이블을 맞대어 쓰임마다 근거를 붙이고, 다른 곳을 모은다 (deps)

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};

use serde::{Deserialize, Serialize};

use crate::deps::{self, Guess, UnitDeps};
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
    /// 트리거면 걸린 테이블 (스키마를 붙인 이름). SCHEMA/DATABASE 트리거는 없음
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_on: Option<String>,
    /// 트리거 사건 "INSERT,UPDATE"
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_events: Option<String>,
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
    /// 이 테이블을 바꾸는(C/U/D) 노드에 닿는 시작점 (화면으로 보낼 때는 앞의 몇 개만 — [`for_screen`])
    pub impacted_entries: Vec<String>,
    /// 화면으로 보낼 때 줄였으면 원래 개수
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub impacted_total: Option<usize>,
    /// 커서 흐름으로 이 테이블에 데이터를 대는 테이블
    #[serde(default)]
    pub fed_from: Vec<String>,
    /// 이 테이블의 데이터가 커서 흐름으로 들어가는 테이블
    #[serde(default)]
    pub feeds_into: Vec<String>,
    /// 노드 id → 근거: ok (사전·소스 모두) / src (소스만) / dict (사전만, 연산 모름) / llm (모델 추정). 사전을 못 본 단위는 없음
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub conf: BTreeMap<String, String>,
}

/// 사전과 소스가 다른 곳 — "확인 필요" 목록
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Gap {
    pub unit: String,
    /// dict_only (사전에만) / source_only (소스에만) / dynamic (동적 SQL) / no_dict (사전이 비어 대조 못 함)
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub table: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    /// 모델이 추정한 연산 (있으면)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ops: Option<String>,
    pub note: String,
}

/// 사전 대조 숫자 — (단위, 테이블) 쌍 기준
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DictStats {
    /// 사전을 본 단위 수
    pub checked: usize,
    /// 사전이 비어 대조하지 못한 단위 수
    #[serde(default)]
    pub unusable: usize,
    pub both: usize,
    pub dict_only: usize,
    pub source_only: usize,
    /// 동적 SQL 문 수
    pub dynamic: usize,
    /// 모델이 연산·테이블을 채운 것
    pub guessed: usize,
}

/// 통합에 넣는 사전 정보 — 단위 키 → 의존성, 모델 보완
#[derive(Debug, Default)]
pub struct Dict {
    pub deps: HashMap<String, UnitDeps>,
    pub guesses: Vec<Guess>,
}

/// 커서(또는 SELECT INTO) 로 읽은 데이터가 DML 로 들어가는 흐름 — 테이블 사이의 데이터 계보
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Flow {
    pub node: String,
    pub unit: String,
    pub cursor: String,
    /// declared / implicit_loop / ref_cursor / select_into
    pub cursor_kind: String,
    /// 커서가 읽는 테이블 (스키마를 붙인 이름)
    pub from: Vec<String>,
    pub to: String,
    pub ops: String,
    pub line: u32,
    /// record R / variable V / CURRENT OF / loop body
    pub via: String,
    /// 커서(긴 SQL)의 모델 요약이 있으면
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor_summary: Option<String>,
}

/// 모든 SQL 문 (statements.json)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StmtRow {
    pub node: String,
    pub unit: String,
    #[serde(flatten)]
    pub stmt: crate::flow::SqlStmt,
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
    /// 커서 → DML 흐름
    #[serde(default)]
    pub flows: Vec<Flow>,
    /// 모델로 만든 전체 요약 (요청했을 때)
    pub overview: Option<Insight>,
    /// 사전과 소스가 다른 곳
    #[serde(default)]
    pub gaps: Vec<Gap>,
    /// 사전을 하나도 못 봤고 동적 SQL 도 없으면 None
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dict: Option<DictStats>,
}

/// 화면으로 보낼 통합 결과 — 테이블마다의 '영향 받는 시작점' 은 개수와 앞의 몇 개만.
/// 큰 결과에서 이 목록이 전체의 절반 넘게 차지하는데(테이블마다 수천 개), 화면은 개수와 툴팁에만 쓴다.
/// 디스크의 integrated.json 은 그대로 둔다 (MCP·CLI 는 전체를 쓴다)
pub fn for_screen(mut g: Integrated) -> Integrated {
    const KEEP: usize = 50;
    for t in &mut g.tables {
        if t.impacted_entries.len() > KEEP {
            t.impacted_total = Some(t.impacted_entries.len());
            t.impacted_entries.truncate(KEEP);
        }
    }
    g
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

/// 트리거 머리에서 걸린 테이블과 사건을 읽는다.
/// "TRIGGER t BEFORE INSERT OR UPDATE OF a, b ON app.emp FOR EACH ROW" → ("APP.EMP", "INSERT,UPDATE")
pub fn trigger_target(owner: &str, signature: &str) -> Option<(String, String)> {
    let toks: Vec<String> = signature
        .split(|c: char| c.is_whitespace() || matches!(c, ',' | '(' | ')'))
        .filter(|t| !t.is_empty())
        .map(|t| t.to_uppercase())
        .collect();
    let start = toks.iter().position(|t| matches!(t.as_str(), "BEFORE" | "AFTER" | "INSTEAD" | "FOR"))?;
    let mut events: Vec<&str> = Vec::new();
    let mut i = start;
    while i < toks.len() {
        match toks[i].as_str() {
            w @ ("INSERT" | "UPDATE" | "DELETE") if !events.contains(&w) => events.push(w),
            "ON" => {
                let mut t = toks.get(i + 1)?.as_str();
                // INSTEAD OF … ON NESTED TABLE col OF view
                if t == "NESTED" {
                    t = toks.get(i + 5)?.as_str();
                }
                if matches!(t, "SCHEMA" | "DATABASE") || t.ends_with(".SCHEMA") || events.is_empty() {
                    return None;
                }
                return Some((qualify(owner, &t.replace('"', "")), events.join(",")));
            }
            _ => {}
        }
        i += 1;
    }
    None
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
    integrate_with(units, &Dict::default())
}

/// 어느 서브프로그램인지 모를 때 단위 자리 — 단독 프로시저·함수면 그 노드, 패키지면 "(사전)"
fn unit_node(u: &UnitResult) -> String {
    match u.unit_type.as_str() {
        "PROCEDURE" | "FUNCTION" | "TRIGGER" => u
            .subprograms
            .iter()
            .find(|s| s.parent_is_top() && s.kind != crate::plsql::SubKind::Init)
            .map(|s| node_id(u, &s.path))
            .unwrap_or_else(|| format!("{}.{}", u.owner, u.name)),
        _ => format!("{}.{}.(사전)", u.owner, u.name),
    }
}

pub fn integrate_with(units: &[UnitResult], dict: &Dict) -> Integrated {
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
                trigger_on: None,
                trigger_events: None,
            });
            if s.kind == crate::plsql::SubKind::Trigger {
                if let Some((t, ev)) = trigger_target(&u.owner, &s.signature) {
                    e.trigger_on = Some(t);
                    e.trigger_events = Some(ev);
                }
            }
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

    // 테이블 — 동의어로 적은 이름은 사전으로 실제 테이블로 바꾼다
    let alias: HashMap<&str, HashMap<String, String>> =
        units.iter().map(|u| (u.key.as_str(), dict.deps.get(&u.key).map(|d| deps::aliases(&u.owner, &d.deps)).unwrap_or_default())).collect();
    let qualify = |u: &UnitResult, name: &str| -> String {
        let q = qualify(&u.owner, name);
        alias.get(u.key.as_str()).and_then(|a| a.get(&q)).cloned().unwrap_or(q)
    };
    let mut tables: BTreeMap<String, TableRow> = BTreeMap::new();
    for u in units {
        for s in &u.subprograms {
            let id = if s.kind == crate::plsql::SubKind::Init { format!("{}.{}.(초기화)", u.owner, u.name) } else { node_id(u, &s.path) };
            for t in &s.facts.tables {
                let name = qualify(u, &t.name);
                let row = tables.entry(name.clone()).or_insert_with(|| TableRow { table: name.clone(), ..Default::default() });
                let ops = row.by.entry(id.clone()).or_default();
                let mut set: BTreeSet<char> = ops.chars().collect();
                set.extend(t.ops.chars());
                *ops = ["C", "R", "U", "D"].iter().filter(|o| set.contains(&o.chars().next().unwrap())).copied().collect();
            }
        }
        // 전역 커서 등
        for t in &u.globals.tables {
            let name = qualify(u, &t.name);
            let row = tables.entry(name.clone()).or_insert_with(|| TableRow { table: name.clone(), ..Default::default() });
            row.by.entry(format!("{}.{}.(전역)", u.owner, u.name)).or_insert_with(|| t.ops.clone());
        }
    }
    // 사전 대조
    let mut stats = DictStats::default();
    for u in units {
        let guess = |table: Option<&str>, line: Option<u32>| dict.guesses.iter().find(|g| g.unit == u.key && g.table.as_deref() == table && g.line == line);
        // 동적 SQL — 사전도 파서도 모른다. 모델이 추정했으면 그 테이블을 넣는다
        for s in &u.subprograms {
            let node = if s.kind == crate::plsql::SubKind::Init { format!("{}.{}.(초기화)", u.owner, u.name) } else { node_id(u, &s.path) };
            for d in &s.facts.dynamic_sql {
                stats.dynamic += 1;
                let g = guess(None, Some(d.line));
                let uses = g.map(|g| g.uses.as_slice()).unwrap_or(&[]);
                if uses.is_empty() {
                    let note = match g {
                        Some(g) if !g.reason.is_empty() => format!("모델도 테이블을 찾지 못함: {}", g.reason),
                        Some(_) => "모델도 테이블을 찾지 못했습니다".into(),
                        None => format!("{} — 실행할 때 정해지는 SQL", d.what),
                    };
                    out.gaps.push(Gap { unit: u.key.clone(), kind: "dynamic".into(), table: None, node: Some(node.clone()), line: Some(d.line), ops: None, note });
                }
                for (t, ops) in uses {
                    let name = qualify(u, t);
                    let row = tables.entry(name.clone()).or_insert_with(|| TableRow { table: name.clone(), ..Default::default() });
                    if row.by.contains_key(&node) {
                        continue; // 파서가 문자열에서 이미 읽었다
                    }
                    row.by.insert(node.clone(), ops.clone());
                    row.conf.insert(node.clone(), "llm".into());
                    stats.guessed += 1;
                    out.gaps.push(Gap {
                        unit: u.key.clone(),
                        kind: "dynamic".into(),
                        table: Some(name),
                        node: Some(node.clone()),
                        line: Some(d.line),
                        ops: Some(ops.clone()),
                        note: g.map(|g| g.reason.clone()).unwrap_or_default(),
                    });
                }
            }
        }
        let Some(ud) = dict.deps.get(&u.key) else { continue };
        let al = &alias[u.key.as_str()];
        let parser: BTreeSet<String> = deps::parser_tables(u).into_iter().map(|t| al.get(&t).cloned().unwrap_or(t)).collect();
        if !deps::dict_usable(&parser, &ud.deps) {
            stats.unusable += 1;
            out.gaps.push(Gap {
                unit: u.key.clone(),
                kind: "no_dict".into(),
                table: None,
                node: None,
                line: None,
                ops: None,
                note: "사전 의존성이 0건 — 대조하지 않았습니다. ALL_DEPENDENCIES 조회 권한이나 객체 상태(INVALID)를 확인하세요".into(),
            });
            continue;
        }
        stats.checked += 1;
        let cmp = deps::compare(&parser, &ud.deps);
        stats.both += cmp.both.len();
        stats.dict_only += cmp.dict_only.len();
        stats.source_only += cmp.source_only.len();
        // 이 단위의 노드들
        let mut mine: BTreeSet<String> = u
            .subprograms
            .iter()
            .map(|s| if s.kind == crate::plsql::SubKind::Init { format!("{}.{}.(초기화)", u.owner, u.name) } else { node_id(u, &s.path) })
            .collect();
        mine.insert(format!("{}.{}.(전역)", u.owner, u.name));
        for (t, word) in cmp.both.iter().map(|t| (t, "ok")).chain(cmp.source_only.iter().map(|t| (t, "src"))) {
            if let Some(row) = tables.get_mut(t) {
                let nodes: Vec<String> = row.by.keys().filter(|n| mine.contains(*n)).cloned().collect();
                for n in nodes {
                    row.conf.entry(n).or_insert_with(|| word.into());
                }
            }
        }
        for t in &cmp.source_only {
            let node = tables.get(t).and_then(|r| r.by.keys().find(|n| mine.contains(*n)).cloned());
            out.gaps.push(Gap {
                unit: u.key.clone(),
                kind: "source_only".into(),
                table: Some(t.clone()),
                node,
                line: None,
                ops: None,
                note: "사전에 없음 — 실행 중 만드는 테이블, 동적 SQL 안의 이름, 또는 파서가 잘못 읽은 것일 수 있습니다".into(),
            });
        }
        // 사전에만 — 파서가 쓰는 곳을 못 찾았다. 모델이 추정했으면 그 연산, 아니면 '?'
        for t in &cmp.dict_only {
            let g = guess(Some(t), None);
            let ops = g.and_then(|g| g.uses.first()).map(|(_, o)| o.clone()).filter(|o| !o.is_empty());
            let node = g
                .and_then(|g| g.subprogram.as_deref())
                .and_then(|name| u.subprograms.iter().find(|s| s.parent_is_top() && s.name.eq_ignore_ascii_case(name)))
                .map(|s| node_id(u, &s.path))
                .unwrap_or_else(|| unit_node(u));
            let row = tables.entry(t.clone()).or_insert_with(|| TableRow { table: t.clone(), ..Default::default() });
            row.by.entry(node.clone()).or_insert_with(|| ops.clone().unwrap_or_else(|| "?".into()));
            row.conf.insert(node.clone(), if ops.is_some() { "llm" } else { "dict" }.into());
            if ops.is_some() {
                stats.guessed += 1;
            }
            let via = ud.deps.iter().find(|d| &d.qualified() == t).and_then(|d| d.via.clone()).map(|v| format!(" (동의어 {v})")).unwrap_or_default();
            let note = match g {
                Some(g) if !g.reason.is_empty() => g.reason.clone(),
                _ if u.subprograms.is_empty() => format!("소스를 읽지 못한 단위(wrap 등) — 사전으로만 압니다{via}"),
                _ => format!("사전에는 있으나 소스에서 쓰는 곳을 찾지 못했습니다{via}"),
            };
            out.gaps.push(Gap { unit: u.key.clone(), kind: "dict_only".into(), table: Some(t.clone()), node: Some(node), line: None, ops, note });
        }
    }
    if stats.checked > 0 || stats.dynamic > 0 || stats.unusable > 0 {
        out.dict = Some(stats);
    }
    for row in tables.values_mut() {
        let writers: BTreeSet<&String> = row.by.iter().filter(|(_, o)| o.contains(['C', 'U', 'D'])).map(|(n, _)| n).collect();
        row.impacted_entries = reach_of.iter().filter(|(_, r)| writers.iter().any(|w| r.contains(*w))).map(|(e, _)| e.clone()).collect();
    }
    // 커서 흐름
    for u in units {
        for s in &u.subprograms {
            let node = if s.kind == crate::plsql::SubKind::Init { format!("{}.{}.(초기화)", u.owner, u.name) } else { node_id(u, &s.path) };
            for c in &s.facts.cursors {
                for f in &c.feeds {
                    out.flows.push(Flow {
                        node: node.clone(),
                        unit: u.key.clone(),
                        cursor: c.name.clone(),
                        cursor_kind: c.kind.clone(),
                        from: c.reads.iter().map(|r| qualify(u, r)).collect(),
                        to: qualify(u, &f.table),
                        ops: f.ops.clone(),
                        line: f.line,
                        via: f.via.clone(),
                        cursor_summary: cursor_summary(u, s, &c.name),
                    });
                }
            }
        }
    }
    for f in &out.flows {
        for src in &f.from {
            if src == &f.to {
                continue;
            }
            if let Some(row) = tables.get_mut(&f.to) {
                if !row.fed_from.contains(src) {
                    row.fed_from.push(src.clone());
                }
            }
            if let Some(row) = tables.get_mut(src) {
                if !row.feeds_into.contains(&f.to) {
                    row.feeds_into.push(f.to.clone());
                }
            }
        }
    }
    for row in tables.values_mut() {
        row.fed_from.sort();
        row.feeds_into.sort();
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

/// 커서의 뜻: 긴 SQL 요약 → (짧아서 한 조각에 든 커서면) 그 조각의 요약
fn cursor_summary(u: &UnitResult, s: &crate::store::SubResult, name: &str) -> Option<String> {
    u.sql_summaries
        .iter()
        .find(|q| q.cursor.as_deref() == Some(name) && (q.subprogram.is_none() || q.subprogram.as_deref() == Some(s.path.as_str())))
        .map(|q| q.summary.summary.clone())
        .filter(|x| !x.is_empty())
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

    if !g.flows.is_empty() {
        s.push_str("## 커서 → DML 흐름\n\n| 서브프로그램 | 커서 | 읽는 테이블 | → 쓰는 테이블 | 줄 | 근거 |\n|---|---|---|---|---|---|\n");
        for f in &g.flows {
            let sum = f.cursor_summary.as_ref().map(|x| format!("<br><small>{}</small>", x.replace('|', "\\|").replace('\n', " "))).unwrap_or_default();
            s.push_str(&format!("| `{}` | `{}` ({}){sum} | {} | {} {} | {} | {} |\n", f.node, f.cursor, f.cursor_kind, f.from.join(", "), f.to, f.ops, f.line, f.via));
        }
        s.push('\n');
    }

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
    if let Some(d) = &g.dict {
        s.push_str("## 사전 대조 (ALL_DEPENDENCIES)\n\n");
        s.push_str(&format!(
            "사전을 본 단위 {}개 (사전이 비어 못 본 단위 {}) · 사전·소스 모두 {} · 사전에만 {} · 소스에만 {} · 동적 SQL {} · 모델 추정 {}\n\n",
            d.checked, d.unusable, d.both, d.dict_only, d.source_only, d.dynamic, d.guessed
        ));
        if !g.gaps.is_empty() {
            s.push_str("| 단위 | 종류 | 테이블 | 어디 | 줄 | 추정 연산 | 내용 |\n|---|---|---|---|---|---|---|\n");
            for x in &g.gaps {
                let kind = match x.kind.as_str() {
                    "dict_only" => "사전에만",
                    "source_only" => "소스에만",
                    "no_dict" => "사전 없음",
                    _ => "동적 SQL",
                };
                s.push_str(&format!(
                    "| {} | {kind} | {} | {} | {} | {} | {} |\n",
                    x.unit,
                    x.table.as_deref().unwrap_or(""),
                    x.node.as_deref().map(|n| format!("`{n}`")).unwrap_or_default(),
                    x.line.map(|l| l.to_string()).unwrap_or_default(),
                    x.ops.as_deref().map(|o| format!("{o} (추정)")).unwrap_or_default(),
                    x.note.replace('|', "\\|").replace('\n', " ")
                ));
            }
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
    let dict = Dict {
        deps: units.iter().filter_map(|u| store.read_deps(&u.key).map(|d| (u.key.clone(), d))).collect(),
        guesses: store.read_integrated("supplement.json").unwrap_or_default(),
    };
    let mut g = integrate_with(&units, &dict);
    g.overview = overview;
    let mut files = Vec::new();
    files.push(store.write_integrated("graph.json", &serde_json::json!({ "nodes": g.nodes, "edges": g.edges, "entries": g.entries, "cycles": g.cycles, "unused": g.unused, "transactions": g.transactions }))?);
    files.push(store.write_integrated("crud.json", &g.tables)?);
    files.push(store.write_integrated("findings.json", &g.findings)?);
    files.push(store.write_integrated("flows.json", &g.flows)?);
    files.push(store.write_integrated("gaps.json", &g.gaps)?);
    let stmts: Vec<StmtRow> = units
        .iter()
        .flat_map(|u| {
            let mut rows: Vec<StmtRow> = Vec::new();
            for st in &u.globals.statements {
                rows.push(StmtRow { node: format!("{}.{}.(전역)", u.owner, u.name), unit: u.key.clone(), stmt: st.clone() });
            }
            for s in &u.subprograms {
                for st in &s.facts.statements {
                    rows.push(StmtRow { node: node_id(u, &s.path), unit: u.key.clone(), stmt: st.clone() });
                }
            }
            rows
        })
        .collect();
    files.push(store.write_integrated("statements.json", &stmts)?);
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
