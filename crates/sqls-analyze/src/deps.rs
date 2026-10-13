//! 사전 의존성(ALL_DEPENDENCIES)으로 분석을 맞대어 본다.
//!
//! ```text
//! 사전(ALL_DEPENDENCIES) ── 확실한 뼈대: 단위가 참조하는 테이블·뷰 (동의어는 실제 테이블로 푼다, wrap 된 단위도)
//! 파서(facts)            ── 세부: 어느 서브프로그램·어느 줄·어떤 연산(C/R/U/D)
//!             맞대기 ─┬─ 둘 다     → 확정
//!                    ├─ 사전만   → 파서가 놓쳤다 → 그 이름이 나오는 줄만 LLM 에 물어 연산을 채운다 (추정 표시)
//!                    └─ 파서만   → 동적 SQL·DB 링크·실행 중 만드는 테이블 등 → 확인 필요
//! 동적 SQL(EXECUTE IMMEDIATE …)은 둘 다 모른다 → 그 문장 주변만 LLM 에 물어 테이블·연산을 추정한다
//! ```
//! 사전은 단위(패키지 본문) 단위로만 알려 주고 연산을 모른다 — 그래서 파서를 대신하지 않고 확인·보완에 쓴다.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqls_core::session::Session;
use sqls_llm::{ChatRequest, Client, Message, ProviderConfig};

use crate::store::{Store, UnitResult};

/// 단위가 참조하는 객체 하나 (동의어면 대상으로 푼 것)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Dep {
    pub owner: String,
    pub name: String,
    /// TABLE / VIEW / MATERIALIZED VIEW / SEQUENCE / PACKAGE / PROCEDURE / FUNCTION / TYPE / SYNONYM(대상을 못 푼 것) …
    pub kind: String,
    /// 동의어를 거쳤으면 그 동의어 ("PUBLIC.ORDERS", "APP.ORD_SYN")
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub via: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
}

impl Dep {
    pub fn qualified(&self) -> String {
        format!("{}.{}", self.owner, self.name)
    }
    /// 데이터를 담는 것 (CRUD 대상)
    pub fn is_data(&self) -> bool {
        matches!(self.kind.as_str(), "TABLE" | "VIEW" | "MATERIALIZED VIEW") && self.link.is_none()
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UnitDeps {
    pub key: String,
    pub fetched_at: u64,
    pub deps: Vec<Dep>,
}

/// 단위 하나의 의존성. 동의어는 대상과 그 종류까지 푼다. 자기 명세(패키지 본문 → 패키지)는 뺀다.
pub async fn fetch(s: &Session, owner: &str, name: &str, unit_type: &str) -> sqls_core::Result<Vec<Dep>> {
    let sql = "SELECT d.referenced_owner, d.referenced_name, d.referenced_type, d.referenced_link_name, \
                      sy.table_owner, sy.table_name, \
                      (SELECT MIN(o.object_type) FROM all_objects o WHERE o.owner = sy.table_owner AND o.object_name = sy.table_name \
                          AND o.object_type IN ('TABLE','VIEW','MATERIALIZED VIEW','SEQUENCE','PACKAGE','PROCEDURE','FUNCTION','TYPE')) \
                 FROM all_dependencies d \
                 LEFT JOIN all_synonyms sy ON d.referenced_type = 'SYNONYM' AND sy.owner = d.referenced_owner AND sy.synonym_name = d.referenced_name \
                WHERE d.owner = :o AND d.name = :n AND d.type = :t";
    let page = s
        .query(sql, vec![("O".into(), Some(owner.to_uppercase())), ("N".into(), Some(name.to_uppercase())), ("T".into(), Some(unit_type.to_uppercase()))], 10_000)
        .await?;
    let cell = |r: &Vec<Option<String>>, i: usize| r.get(i).cloned().flatten();
    let mut out: Vec<Dep> = Vec::new();
    for r in &page.rows {
        let (ro, rn, rt) = (cell(r, 0).unwrap_or_default(), cell(r, 1).unwrap_or_default(), cell(r, 2).unwrap_or_default());
        // 자기 자신·자기 명세, 오라클 기본 패키지
        if ro == owner.to_uppercase() && rn == name.to_uppercase() {
            continue;
        }
        if ro == "SYS" && matches!(rn.as_str(), "STANDARD" | "DBMS_STANDARD" | "SYS_STUB_FOR_PURITY_ANALYSIS" | "PLITBLM") {
            continue;
        }
        let dep = match (rt.as_str(), cell(r, 4), cell(r, 5)) {
            ("SYNONYM", Some(to), Some(tn)) => Dep { owner: to, name: tn, kind: cell(r, 6).unwrap_or_else(|| "SYNONYM".into()), via: Some(format!("{ro}.{rn}")), link: cell(r, 3) },
            _ => Dep { owner: ro, name: rn, kind: rt, via: None, link: cell(r, 3) },
        };
        if !out.contains(&dep) {
            out.push(dep);
        }
    }
    out.sort_by(|a, b| (a.kind.as_str(), a.owner.as_str(), a.name.as_str()).cmp(&(b.kind.as_str(), b.owner.as_str(), b.name.as_str())));
    Ok(out)
}

/// 가져와서 단위 폴더에 저장한다. 못 가져와도 분석은 이어 간다 (권한이 없는 사전 등) — 오류 문구를 돌려준다
pub async fn fetch_into(s: &Session, store: &Store, owner: &str, name: &str, unit_type: &str) -> Option<String> {
    let key = crate::chunk::unit_key(owner, name, unit_type);
    match fetch(s, owner, name, unit_type).await {
        Ok(deps) => store.write_deps(&UnitDeps { key, fetched_at: crate::store::now(), deps }).err().map(|e| e.to_string()),
        Err(e) => {
            tracing::warn!("{owner}.{name} 의존성 조회 실패: {e}");
            Some(e.to_string())
        }
    }
}

/// 소스에 적힌 이름 → 실제 테이블 이름 (동의어를 거친 것). "PUBLIC.X" 는 단위 스키마의 "OWNER.X" 로도 적힌다
pub fn aliases(unit_owner: &str, deps: &[Dep]) -> HashMap<String, String> {
    let mut m = HashMap::new();
    for d in deps {
        if let Some(v) = &d.via {
            m.insert(v.clone(), d.qualified());
            if let Some((_, syn)) = v.split_once('.') {
                m.entry(format!("{unit_owner}.{syn}")).or_insert_with(|| d.qualified());
            }
        }
    }
    m
}

/// 맞대어 본 결과
#[derive(Debug, Default, PartialEq)]
pub struct Compared {
    /// 둘 다 (확정)
    pub both: BTreeSet<String>,
    /// 사전만 — 파서가 놓쳤다
    pub dict_only: Vec<String>,
    /// 파서만 — 동적 SQL·DB 링크·실행 중 만든 테이블·파서 오인 등
    pub source_only: Vec<String>,
}

/// 파서 테이블(이미 실제 이름으로 바꾼 것)과 사전의 데이터 객체를 맞댄다. DUAL·DB 링크는 비교하지 않는다.
pub fn compare(parser: &BTreeSet<String>, deps: &[Dep]) -> Compared {
    let ignore = |t: &str| t.ends_with(".DUAL") || t == "DUAL" || t.contains('@');
    let dict: BTreeSet<String> = deps.iter().filter(|d| d.is_data()).map(|d| d.qualified()).filter(|t| !ignore(t)).collect();
    let parser: BTreeSet<String> = parser.iter().filter(|t| !ignore(t)).cloned().collect();
    Compared {
        both: parser.intersection(&dict).cloned().collect(),
        dict_only: dict.difference(&parser).cloned().collect(),
        source_only: parser.difference(&dict).cloned().collect(),
    }
}

/// 저장된 조각에서 단위 소스의 줄을 다시 모은다 (조각 코드는 "  12| 내용" 형식)
pub fn source_lines(store: &Store, u: &UnitResult) -> BTreeMap<u32, String> {
    let mut lines = BTreeMap::new();
    for c in store.chunks_of(u) {
        for l in c.chunk.code.lines() {
            if let Some((n, text)) = l.split_once('|') {
                if let Ok(n) = n.trim().parse::<u32>() {
                    lines.entry(n).or_insert_with(|| text.strip_prefix(' ').unwrap_or(text).to_string());
                }
            }
        }
    }
    lines
}

/// 이름이 (낱말로) 나오는 줄 — 주석(--) 뒤는 보지 않는다
pub fn find_lines(lines: &BTreeMap<u32, String>, name: &str) -> Vec<u32> {
    let short = name.rsplit('.').next().unwrap_or(name).to_uppercase();
    let is_word = |c: char| c.is_alphanumeric() || c == '_' || c == '$' || c == '#';
    lines
        .iter()
        .filter(|(_, t)| {
            let code = t.split("--").next().unwrap_or("").to_uppercase();
            code.match_indices(&short).any(|(i, _)| {
                let before = code[..i].chars().next_back();
                let after = code[i + short.len()..].chars().next();
                !before.is_some_and(is_word) && !after.is_some_and(is_word)
            })
        })
        .map(|(n, _)| *n)
        .take(40)
        .collect()
}

/// 줄들 주변을 모은 조각 (±k 줄, 최대 max 줄)
pub fn snippet(lines: &BTreeMap<u32, String>, at: &[u32], k: u32, max: usize) -> String {
    let mut keep: BTreeSet<u32> = BTreeSet::new();
    for &l in at {
        for x in l.saturating_sub(k)..=l + k {
            if lines.contains_key(&x) {
                keep.insert(x);
            }
        }
        if keep.len() >= max {
            break;
        }
    }
    let mut out = String::new();
    let mut prev = 0;
    for l in keep.into_iter().take(max) {
        if prev != 0 && l > prev + 1 {
            out.push_str("   …\n");
        }
        out.push_str(&format!("{l:>5}| {}\n", lines[&l]));
        prev = l;
    }
    out
}

// ── LLM 보완 ─────────────────────────────────────

/// LLM 이 추정한 것 (확정이 아니다 — 화면에 '추정'으로 표시한다)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Guess {
    /// 단위 키
    pub unit: String,
    /// 사전만에 대한 추정이면 그 테이블, 동적 SQL 이면 None
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub table: Option<String>,
    /// 동적 SQL 이면 그 줄
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    /// (테이블, C/R/U/D) — 사전만이면 한 개, 동적 SQL 이면 여러 개
    pub uses: Vec<(String, String)>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subprogram: Option<String>,
    pub reason: String,
    pub model: String,
    /// 묻는 글의 해시 — 같으면 다시 묻지 않는다
    pub input: String,
}

const SYSTEM: &str = "You read Oracle PL/SQL snippets and report which tables are used and how. \
Reply with ONLY a JSON object. ops is a string of letters: C (INSERT/MERGE insert), R (SELECT/read), U (UPDATE/MERGE update), D (DELETE). \
Use only what the code shows; if unsure, say so in reason. reason is one short Korean sentence.";

fn ask_body_dict(unit: &str, table: &str, snip: &str) -> String {
    format!("Unit: {unit}\nThe Oracle data dictionary says this unit references table {table}, but a simple parser could not find how.\n\
             Lines where the name appears:\n{snip}\nHow does the code use {table}? \
             Answer as {{\"ops\": \"…\", \"subprogram\": \"procedure/function name or empty\", \"reason\": \"…\"}}")
}

fn ask_body_dynamic(unit: &str, line: u32, snip: &str) -> String {
    format!("Unit: {unit}\nLine {line} runs dynamic SQL (EXECUTE IMMEDIATE / DBMS_SQL / OPEN FOR string). Code around it:\n{snip}\n\
             Which tables does the dynamic SQL read or write, and how? Include only tables you can see in the strings. \
             Answer as {{\"tables\": [{{\"name\": \"TABLE\", \"ops\": \"…\"}}], \"reason\": \"…\"}}")
}

fn hash(s: &str) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{h:016x}")
}

/// 답에서 첫 JSON 객체
fn first_json(text: &str) -> Option<Value> {
    let start = text.find('{')?;
    let mut depth = 0i32;
    let mut in_str = false;
    let mut esc = false;
    for (i, ch) in text[start..].char_indices() {
        match ch {
            _ if esc => esc = false,
            '\\' if in_str => esc = true,
            '"' => in_str = !in_str,
            '{' if !in_str => depth += 1,
            '}' if !in_str => {
                depth -= 1;
                if depth == 0 {
                    return serde_json::from_str(&text[start..start + i + 1]).ok();
                }
            }
            _ => {}
        }
    }
    None
}

fn clean_ops(s: &str) -> String {
    let up = s.to_uppercase();
    ["C", "R", "U", "D"].iter().filter(|o| up.contains(*o)).copied().collect()
}

async fn ask(client: &Client, cfg: &ProviderConfig, body: String) -> Option<Value> {
    let mut cfg = cfg.clone();
    if cfg.temperature.is_none() && cfg.kind != sqls_llm::ProviderKind::Anthropic {
        cfg.temperature = Some(0.1);
    }
    cfg.max_tokens = cfg.max_tokens.min(600);
    let mut req = ChatRequest { system: SYSTEM.into(), messages: vec![Message::user(body)], json_schema: Some(json!({"type": "object"})) };
    for _ in 0..2 {
        match client.chat(&cfg, &req, |_| {}).await {
            Ok(r) => return first_json(&r.text),
            Err(sqls_llm::Error::Api { status: 400, .. }) if req.json_schema.is_some() => req.json_schema = None,
            Err(e) => {
                tracing::warn!("사전 대조 보완 질문 실패: {e}");
                return None;
            }
        }
    }
    None
}

/// 차이 나는 곳만 LLM 에 묻고 integrated/supplement.json 에 쌓는다 (같은 질문은 다시 묻지 않는다). 새로 물은 수
pub async fn supplement(store: &Store, client: &Client, cfg: &ProviderConfig, cancel: &std::sync::atomic::AtomicBool, on: &(dyn Fn(String) + Sync)) -> usize {
    let mut have: Vec<Guess> = store.read_integrated("supplement.json").unwrap_or_default();
    let units = store.units();
    let mut asked = 0;
    for u in &units {
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            break;
        }
        let Some(ud) = store.read_deps(&u.key) else { continue };
        let lines = source_lines(store, u);
        if lines.is_empty() {
            continue; // wrap 된 단위 — 소스가 없어 물을 것도 없다
        }
        let label = format!("{} {}.{}", u.unit_type, u.owner, u.name);
        let al = aliases(&u.owner, &ud.deps);
        let parser: BTreeSet<String> = parser_tables(u).into_iter().map(|t| al.get(&t).cloned().unwrap_or(t)).collect();
        let cmp = compare(&parser, &ud.deps);
        // 사전만: 이름이 나오는 줄 주변
        for t in &cmp.dict_only {
            let at = find_lines(&lines, t);
            if at.is_empty() {
                continue; // 소스에 이름이 없다 (동적 SQL 이나 동의어 이름) — 동적 SQL 쪽에서 본다
            }
            let body = ask_body_dict(&label, t, &snippet(&lines, &at, 3, 70));
            let input = hash(&format!("{}{}", cfg.model, body));
            if have.iter().any(|g| g.unit == u.key && g.table.as_deref() == Some(t) && g.input == input) {
                continue;
            }
            on(format!("사전 대조: {} — {t} 사용 확인", u.name));
            let Some(v) = ask(client, cfg, body).await else { continue };
            asked += 1;
            let ops = clean_ops(v["ops"].as_str().unwrap_or(""));
            have.retain(|g| !(g.unit == u.key && g.table.as_deref() == Some(t)));
            have.push(Guess {
                unit: u.key.clone(),
                table: Some(t.clone()),
                line: None,
                uses: if ops.is_empty() { vec![] } else { vec![(t.clone(), ops)] },
                subprogram: v["subprogram"].as_str().filter(|s| !s.trim().is_empty()).map(|s| s.trim().to_uppercase()),
                reason: v["reason"].as_str().unwrap_or("").to_string(),
                model: cfg.model.clone(),
                input,
            });
        }
        // 동적 SQL: 그 줄 주변
        for m in u.facts.dynamic_sql.iter().take(30) {
            let body = ask_body_dynamic(&label, m.line, &snippet(&lines, &[m.line], 8, 40));
            let input = hash(&format!("{}{}", cfg.model, body));
            if have.iter().any(|g| g.unit == u.key && g.line == Some(m.line) && g.input == input) {
                continue;
            }
            on(format!("사전 대조: {} — {}행 동적 SQL", u.name, m.line));
            let Some(v) = ask(client, cfg, body).await else { continue };
            asked += 1;
            let uses: Vec<(String, String)> = v["tables"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|t| {
                    let name = t["name"].as_str()?.trim().trim_matches('"').to_uppercase();
                    let ops = clean_ops(t["ops"].as_str().unwrap_or(""));
                    (!name.is_empty() && !ops.is_empty() && name.chars().all(|c| c.is_alphanumeric() || "_$#.".contains(c))).then_some((name, ops))
                })
                .collect();
            have.retain(|g| !(g.unit == u.key && g.line == Some(m.line)));
            have.push(Guess { unit: u.key.clone(), table: None, line: Some(m.line), uses, subprogram: None, reason: v["reason"].as_str().unwrap_or("").to_string(), model: cfg.model.clone(), input });
        }
        let _ = store.write_integrated("supplement.json", &have);
    }
    asked
}

/// 파서가 찾은 이 단위의 테이블 (스키마를 붙인 이름)
pub fn parser_tables(u: &UnitResult) -> BTreeSet<String> {
    let q = |n: &str| {
        let base = n.split('@').next().unwrap_or(n);
        if base.contains('.') { n.to_string() } else { format!("{}.{n}", u.owner) }
    };
    u.subprograms.iter().flat_map(|s| s.facts.tables.iter()).chain(u.globals.tables.iter()).map(|t| q(&t.name)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dep(o: &str, n: &str, k: &str, via: Option<&str>) -> Dep {
        Dep { owner: o.into(), name: n.into(), kind: k.into(), via: via.map(String::from), link: None }
    }

    #[test]
    fn compare_and_aliases() {
        let deps = vec![
            dep("APP", "ORDERS", "TABLE", None),
            dep("ERP", "CUSTOMERS", "TABLE", Some("PUBLIC.CUSTOMERS")),
            dep("APP", "STOCK", "TABLE", None),
            dep("APP", "ORDER_SEQ", "SEQUENCE", None),
            dep("APP", "UTIL_PKG", "PACKAGE", None),
            dep("SYS", "DUAL", "TABLE", Some("PUBLIC.DUAL")),
        ];
        let al = aliases("APP", &deps);
        assert_eq!(al.get("APP.CUSTOMERS").map(String::as_str), Some("ERP.CUSTOMERS"), "공개 동의어는 단위 스키마 이름으로도");
        let parser: BTreeSet<String> = ["APP.ORDERS", "APP.CUSTOMERS", "APP.TMP_WORK", "APP.DUAL", "APP.X@REMOTE"]
            .iter()
            .map(|t| al.get(*t).cloned().unwrap_or_else(|| t.to_string()))
            .collect();
        let c = compare(&parser, &deps);
        assert_eq!(c.both.iter().cloned().collect::<Vec<_>>(), vec!["APP.ORDERS", "ERP.CUSTOMERS"]);
        assert_eq!(c.dict_only, vec!["APP.STOCK"]);
        assert_eq!(c.source_only, vec!["APP.TMP_WORK"], "DUAL·DB 링크는 비교하지 않는다");
    }

    #[test]
    fn lines_and_snippet() {
        let lines: BTreeMap<u32, String> = [
            (10, "  UPDATE stock SET qty = qty - 1".to_string()),
            (11, "   WHERE item_id = p_id; -- stock 차감".to_string()),
            (12, "  v_stock_cnt := 0;".to_string()),
            (13, "  SELECT COUNT(*) INTO v FROM app.stock s;".to_string()),
        ]
        .into_iter()
        .collect();
        assert_eq!(find_lines(&lines, "APP.STOCK"), vec![10, 13], "주석·다른 낱말(v_stock_cnt)은 빼고");
        let s = snippet(&lines, &[10], 1, 10);
        assert!(s.contains("   10| ") && s.contains("   11| ") && !s.contains("   13| "), "{s}");
    }

    #[test]
    fn json_from_messy_answer() {
        let v = first_json("음… ```json\n{\"ops\": \"ru\", \"reason\": \"갱신 {괄호}\"}\n``` 끝").unwrap();
        assert_eq!(clean_ops(v["ops"].as_str().unwrap()), "RU");
        assert!(first_json("없음").is_none());
    }
}
