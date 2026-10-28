//! Oracle 문법 안내 — 요청(질문·SQL·오류)과 관련된 카드만 골라 시스템 프롬프트에 넣는다.
//!
//! 작은 로컬 모델은 Oracle 문법(ROWNUM 페이징, MERGE, LISTAGG, CONNECT BY …)을 자주 틀린다.
//! 전체를 넣으면 문맥이 넘치므로, 낱말이 맞는 카드 몇 장만 (글자 수 상한 안에서) 넣는다.
//! 기본 카드는 oracle-guides.md (바이너리에 들어간다), 사용자는 설정 폴더의 같은 이름 파일로 더하거나 바꾼다.

use std::path::Path;
use std::sync::OnceLock;

use crate::prompts::Task;

const BUILTIN: &str = include_str!("oracle-guides.md");

/// 카드 몇 장까지 (항상 넣는 것 빼고)
const MAX_CARDS: usize = 4;
/// 넣을 본문 글자 수 상한 (로컬 모델 문맥 — 대략 1,500 토큰)
const BUDGET: usize = 3200;

#[derive(Debug, Clone, PartialEq)]
pub struct Guide {
    pub id: String,
    pub title: String,
    /// 소문자로 바꾼 찾을 낱말. "*" 면 항상
    pub keys: Vec<String>,
    pub min: Option<u32>,
    pub max: Option<u32>,
    pub body: String,
}

/// "@@ 아이디 | 제목 | 낱말, 낱말 | 버전" 머리와 본문
pub fn parse(text: &str) -> Vec<Guide> {
    let mut out: Vec<Guide> = Vec::new();
    for line in text.lines() {
        if let Some(head) = line.strip_prefix("@@") {
            let f: Vec<&str> = head.split('|').map(|x| x.trim()).collect();
            if f.len() < 3 || f[0].is_empty() {
                continue;
            }
            let ver = f.get(3).copied().unwrap_or("");
            let (min, max) = match ver.split_once('-') {
                Some((a, b)) => (a.trim().parse().ok(), b.trim().parse().ok()),
                None => (ver.parse().ok(), None),
            };
            out.push(Guide {
                id: f[0].to_string(),
                title: f[1].to_string(),
                keys: f[2].split(',').map(|k| k.trim().to_lowercase()).filter(|k| !k.is_empty()).collect(),
                min,
                max,
                body: String::new(),
            });
        } else if let Some(g) = out.last_mut() {
            g.body.push_str(line);
            g.body.push('\n');
        }
        // 첫 @@ 앞의 줄(# 설명)은 버린다
    }
    for g in &mut out {
        g.body = g.body.trim().to_string();
    }
    out
}

fn builtin() -> &'static [Guide] {
    static B: OnceLock<Vec<Guide>> = OnceLock::new();
    B.get_or_init(|| parse(BUILTIN))
}

/// 기본 카드 + 사용자 파일 (같은 아이디면 사용자 것으로)
pub fn load(user_file: Option<&Path>) -> Vec<Guide> {
    let mut all = builtin().to_vec();
    if let Some(text) = user_file.and_then(|p| std::fs::read_to_string(p).ok()) {
        for g in parse(&text) {
            match all.iter_mut().find(|x| x.id == g.id) {
                Some(x) => *x = g,
                None => all.push(g),
            }
        }
    }
    all
}

/// 기본 카드 (사용자 파일과 견줄 때)
pub fn builtin_cards() -> &'static [Guide] {
    builtin()
}

/// 기본 카드 하나의 원문 ("@@ 머리" 줄부터 다음 카드 앞까지) — 사용자 파일에 옮겨 고치게
pub fn builtin_block(id: &str) -> Option<String> {
    let mut out: Option<String> = None;
    for line in BUILTIN.lines() {
        if let Some(head) = line.strip_prefix("@@") {
            if out.is_some() {
                break;
            }
            if head.split('|').next().map(str::trim) == Some(id) {
                out = Some(String::new());
            }
        }
        if let Some(o) = out.as_mut() {
            o.push_str(line);
            o.push('\n');
        }
    }
    out.map(|o| format!("{}\n", o.trim_end()))
}

/// 사용자 파일을 처음 만들 때 넣을 내용 (형식 설명 + 예)
pub fn user_template() -> String {
    let head: String = BUILTIN.lines().take_while(|l| !l.starts_with("@@")).map(|l| format!("{l}\n")).collect();
    format!(
        "{head}\n# 예) 회사 규칙 — 요청에 '날짜 컬럼'·'일자' 가 있으면 넣는다. 지우고 쓰면 된다.\n\
         @@ company_date | 우리 회사 날짜 컬럼 규칙 | 날짜 컬럼, 일자, yyyymmdd |\n\
         - 업무 날짜는 CHAR(8) 'YYYYMMDD' 로 저장한다. 조회 조건은 TO_CHAR(SYSDATE, 'YYYYMMDD') 와 문자로 비교한다.\n\
         \n# 기본 카드를 바꾸려면 같은 아이디(예: core, merge)로 쓴다. 기본 카드 목록: {}\n",
        builtin().iter().map(|g| g.id.as_str()).collect::<Vec<_>>().join(", ")
    )
}

fn version_ok(g: &Guide, major: Option<u32>) -> bool {
    let Some(m) = major else { return g.max.is_none() || g.min.is_none() };
    g.min.is_none_or(|x| m >= x) && g.max.is_none_or(|x| m <= x)
}

/// 낱말 점수가 이만큼이면 낱말만으로 충분하다 (자동 모드에서 AI 에게 묻지 않는다).
/// ORA 번호 하나(20), 긴 낱말 하나(3+8), 짧은 낱말 둘이면 넘는다
pub const STRONG: i32 = 12;

/// 낱말로 매긴 후보 — 항상 넣는 카드, 점수 순 카드
struct Scored<'a> {
    always: Vec<&'a Guide>,
    scored: Vec<(i32, &'a Guide)>,
}

fn score<'a>(all: &'a [Guide], task: Task, text: &str, major: Option<u32>) -> Scored<'a> {
    let t = text.to_lowercase();
    let major = major.or(Some(11));
    let mut always: Vec<&Guide> = Vec::new();
    let mut scored: Vec<(i32, &Guide)> = Vec::new();
    for g in all.iter().filter(|g| version_ok(g, major)) {
        if g.keys.iter().any(|k| k == "*") {
            // 설명·튜닝에는 기본 규칙이 덜 필요하다 — 만들기·고치기·대화에만
            if matches!(task, Task::Generate | Task::Fix | Task::Chat) {
                always.push(g);
            }
            continue;
        }
        let mut score: i32 = g
            .keys
            .iter()
            .filter(|k| t.contains(k.as_str()))
            .map(|k| if k.starts_with("ora-") || k.starts_with("pls-") { 20 } else { 3 + (k.chars().count() as i32).min(8) })
            .sum();
        if task == Task::Optimize && g.id == "tuning" {
            score += 30;
        }
        if score > 0 {
            scored.push((score, g));
        }
    }
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.id.cmp(&b.1.id)));
    Scored { always, scored }
}

/// 항상 넣는 카드 뒤에 `order` 순으로 MAX_CARDS 장, 글자 수 상한 안에서
fn fill<'a, T: Copy>(always: Vec<(&'a Guide, T)>, order: impl IntoIterator<Item = (&'a Guide, T)>) -> Vec<(&'a Guide, T)> {
    let mut used: usize = always.iter().map(|(g, _)| g.body.len()).sum();
    let mut out = always;
    let mut n = 0;
    for (g, why) in order {
        if n >= MAX_CARDS {
            break;
        }
        if out.iter().any(|(x, _)| x.id == g.id) || (used + g.body.len() > BUDGET && n > 0) {
            continue;
        }
        used += g.body.len();
        out.push((g, why));
        n += 1;
    }
    out
}

/// 요청에 맞는 카드 (낱말만). 항상 넣는 카드(core 등) + 낱말 점수가 높은 순으로 MAX_CARDS 장, 글자 수 상한 안에서.
/// 버전을 모르면 11g/12c 로 갈린 카드 중 11g 쪽을 쓴다 (11g 문법은 12c 에서도 돈다).
pub fn pick<'a>(all: &'a [Guide], task: Task, text: &str, major: Option<u32>) -> Vec<&'a Guide> {
    let s = score(all, task, text, major);
    fill(s.always.into_iter().map(|g| (g, ())).collect(), s.scored.into_iter().map(|(_, g)| (g, ()))).into_iter().map(|(g, _)| g).collect()
}

/// 낱말만으로 충분한가 — 자동 모드에서 AI 에게 고르게 할지 정한다
pub fn keyword_strong(all: &[Guide], task: Task, text: &str, major: Option<u32>) -> bool {
    score(all, task, text, major).scored.first().is_some_and(|(s, _)| *s >= STRONG)
}

/// 카드가 뽑힌 까닭
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Why {
    /// 항상 넣는 카드
    Always,
    Keyword,
    Ai,
    /// 낱말도 AI 도
    Both,
}

/// 낱말 점수와 AI 가 고른 아이디를 합친다: 둘 다 고른 것 → 낱말이 강하게 맞은 것 → AI 가 고른 것 → 나머지 낱말
pub fn pick_with<'a>(all: &'a [Guide], task: Task, text: &str, major: Option<u32>, ai: &[String]) -> Vec<(&'a Guide, Why)> {
    let s = score(all, task, text, major);
    let by_ai = |g: &Guide| ai.iter().any(|x| *x == g.id);
    let mut order: Vec<(&Guide, Why)> = Vec::new();
    order.extend(s.scored.iter().filter(|(_, g)| by_ai(g)).map(|(_, g)| (*g, Why::Both)));
    order.extend(s.scored.iter().filter(|(sc, g)| *sc >= STRONG && !by_ai(g)).map(|(_, g)| (*g, Why::Keyword)));
    // AI 가 고른 순서대로 (버전이 맞고 항상 넣는 카드가 아닌 것만)
    let major_ok = major.or(Some(11));
    for id in ai {
        if let Some(g) = all.iter().find(|g| g.id == *id && version_ok(g, major_ok) && !g.keys.iter().any(|k| k == "*")) {
            if !s.scored.iter().any(|(_, x)| x.id == g.id) {
                order.push((g, Why::Ai));
            }
        }
    }
    order.extend(s.scored.iter().filter(|(sc, g)| *sc < STRONG && !by_ai(g)).map(|(_, g)| (*g, Why::Keyword)));
    fill(s.always.into_iter().map(|g| (g, Why::Always)).collect(), order)
}

/// AI 에게 카드를 고르게 하는 요청 — 아이디·주제·대표 낱말 목록만 보낸다. 고를 카드가 없으면 None
pub fn route_request(all: &[Guide], task: Task, text: &str, major: Option<u32>) -> Option<crate::ChatRequest> {
    let major = major.or(Some(11));
    let cands: Vec<&Guide> = all.iter().filter(|g| version_ok(g, major) && !g.keys.iter().any(|k| k == "*")).collect();
    if cands.is_empty() {
        return None;
    }
    let list: String = cands
        .iter()
        .map(|g| format!("{} | {} | {}\n", g.id, g.title, g.keys.iter().take(5).cloned().collect::<Vec<_>>().join(", ")))
        .collect();
    let kind = match task {
        Task::Generate => "SQL 만들기",
        Task::Explain => "SQL 설명",
        Task::Optimize => "튜닝",
        Task::Fix => "오류 고치기",
        Task::Chat => "대화",
        Task::Trace => "값 출처 설명",
    };
    // 요청이 길면 앞부분만 (주제를 가늠하는 데는 충분하다)
    let req: String = text.chars().take(2000).collect();
    let ids: Vec<&str> = cands.iter().map(|g| g.id.as_str()).collect();
    Some(crate::ChatRequest {
        system: "너는 Oracle SQL 도우미의 분류기다. 사용자의 요청에 답할 때 참고하면 좋은 Oracle 문법 안내 카드를 아래 목록에서 고른다.\n\
                 - 요청에 실제로 필요한 것만, 많아야 3개. 관련 있는 것이 없으면 빈 배열.\n\
                 - 목록에 있는 아이디만 쓴다.\n\
                 - 답은 JSON 하나만: {\"ids\": [\"아이디\", ...]} — 설명을 붙이지 않는다."
            .into(),
        messages: vec![crate::Message::user(format!("## 카드 목록 (아이디 | 주제 | 예시 낱말)\n{list}\n## 요청 종류\n{kind}\n\n## 요청\n{req}"))],
        json_schema: Some(serde_json::json!({
            "type": "object",
            "properties": { "ids": { "type": "array", "items": { "type": "string", "enum": ids } } },
            "required": ["ids"],
        })),
    })
}

/// AI 의 답에서 카드 아이디 — 형식이 조금 어긋나도 (앞뒤 글, ```json, 배열만) 읽는다. 없는 아이디는 버리고 많아야 3개
pub fn parse_route(answer: &str, all: &[Guide]) -> Vec<String> {
    let t = answer.trim();
    let v: Option<serde_json::Value> = [('{', '}'), ('[', ']')].iter().find_map(|(a, b)| {
        let (s, e) = (t.find(*a)?, t.rfind(*b)?);
        (s < e).then(|| serde_json::from_str(&t[s..=e]).ok()).flatten()
    });
    let arr = match &v {
        Some(serde_json::Value::Array(a)) => a.clone(),
        Some(o) => o["ids"].as_array().cloned().unwrap_or_default(),
        None => vec![],
    };
    let mut out: Vec<String> = Vec::new();
    for x in arr.iter().filter_map(|x| x.as_str()) {
        let id = x.trim();
        if all.iter().any(|g| g.id == id) && !out.iter().any(|o| o == id) {
            out.push(id.to_string());
        }
    }
    out.truncate(3);
    out
}

/// 시스템 프롬프트에 붙일 글
pub fn render(cards: &[&Guide]) -> String {
    if cards.is_empty() {
        return String::new();
    }
    let mut s = String::from("\n## Oracle 문법 참고 (이 요청과 관련된 것 — 이 규칙을 따른다)\n");
    for g in cards {
        s.push_str(&format!("### {}\n{}\n", g.title, g.body));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn route_parse_and_merge() {
        let all = builtin();
        // 형식이 조금 어긋나도 읽고, 없는 아이디는 버린다
        assert_eq!(parse_route("{\"ids\": [\"paging11\", \"없는것\", \"date\"]}", all), vec!["paging11", "date"]);
        assert_eq!(parse_route("답:\n```json\n[\"merge\"]\n```", all), vec!["merge"]);
        assert_eq!(parse_route("{\"ids\":[\"a\",\"date\",\"date\",\"join\",\"merge\",\"null\"]}", all), vec!["date", "join", "merge"]);
        assert!(parse_route("모르겠습니다", all).is_empty());
        // 낱말에 없는 표현: 낱말로는 약하고, AI 가 고른 카드가 들어간다
        let q = "앞에서 다섯 개만 보여줘";
        assert!(!keyword_strong(all, Task::Generate, q, Some(11)));
        let r = pick_with(all, Task::Generate, q, Some(11), &["paging11".into()]);
        assert_eq!(r[0].0.id, "core");
        assert_eq!(r[0].1, Why::Always);
        assert!(r.iter().any(|(g, w)| g.id == "paging11" && *w == Why::Ai));
        // 버전이 안 맞는 카드는 AI 가 골라도 넣지 않는다
        let r = pick_with(all, Task::Generate, q, Some(19), &["paging11".into()]);
        assert!(!r.iter().any(|(g, _)| g.id == "paging11"));
        // 둘 다 고르면 Both, 맨 앞
        let q = "ORA-01722 invalid number";
        assert!(keyword_strong(all, Task::Fix, q, Some(19)));
        let kw = pick(all, Task::Fix, q, Some(19));
        let first = kw.iter().find(|g| g.id != "core").unwrap().id.clone();
        let r = pick_with(all, Task::Fix, q, Some(19), &[first.clone()]);
        assert_eq!((r[1].0.id.as_str(), r[1].1), (first.as_str(), Why::Both));
        // AI 없이 합치면 낱말만 쓴 것과 같다
        let q = "최근 7일 주문을 고객별로 상위 10건만";
        assert_eq!(pick_with(all, Task::Generate, q, Some(11), &[]).iter().map(|(g, _)| g.id.clone()).collect::<Vec<_>>(), ids(&pick(all, Task::Generate, q, Some(11))));
        // 요청 — 아이디 목록이 스키마의 enum 에 들어간다
        let req = route_request(all, Task::Generate, q, Some(11)).unwrap();
        let ids_in = req.json_schema.unwrap()["properties"]["ids"]["items"]["enum"].as_array().unwrap().len();
        assert!(ids_in > 10 && !req.messages[0].content.contains("| Oracle 기본 규칙 |"));
    }

    #[test]
    fn builtin_block_round_trips() {
        for g in builtin() {
            let block = builtin_block(&g.id).unwrap();
            assert!(block.starts_with("@@"), "{}", g.id);
            assert_eq!(parse(&block), vec![g.clone()], "{}", g.id);
        }
        assert_eq!(builtin_block("없는_카드"), None);
    }

    fn ids(v: &[&Guide]) -> Vec<String> {
        v.iter().map(|g| g.id.clone()).collect()
    }

    #[test]
    fn builtin_parses() {
        let b = builtin();
        assert!(b.len() >= 30, "{}", b.len());
        for g in b {
            assert!(!g.title.is_empty() && !g.body.is_empty() && !g.keys.is_empty(), "{}", g.id);
            assert!(g.body.len() < 1600, "{} 이 너무 길다 ({})", g.id, g.body.len());
        }
        let ids: std::collections::HashSet<&str> = b.iter().map(|g| g.id.as_str()).collect();
        assert_eq!(ids.len(), b.len(), "아이디 중복");
        let p11 = b.iter().find(|g| g.id == "paging11").unwrap();
        assert_eq!((p11.min, p11.max), (None, Some(11)));
        let p12 = b.iter().find(|g| g.id == "paging12").unwrap();
        assert_eq!((p12.min, p12.max), (Some(12), None));
    }

    #[test]
    fn picks_by_request_and_version() {
        let all = builtin();
        let r = pick(all, Task::Generate, "최근 7일 주문을 고객별로 상위 10건만", Some(11));
        let v = ids(&r);
        assert_eq!(v[0], "core");
        assert!(v.contains(&"paging11".to_string()) && v.contains(&"date".to_string()), "{v:?}");
        assert!(!v.contains(&"paging12".to_string()));
        let v = ids(&pick(all, Task::Generate, "상위 10건만", Some(19)));
        assert!(v.contains(&"paging12".to_string()) && !v.contains(&"paging11".to_string()), "{v:?}");
        // 버전을 모르면 11g 쪽
        let v = ids(&pick(all, Task::Generate, "상위 10건만", None));
        assert!(v.contains(&"paging11".to_string()), "{v:?}");
        // ORA 오류는 강하게
        let v = ids(&pick(all, Task::Fix, "ORA-01722: invalid number", Some(19)));
        assert_eq!(v[1], "conversion", "{v:?}");
        let v = ids(&pick(all, Task::Generate, "있으면 수정하고 없으면 insert 하는 merge 문", Some(11)));
        assert!(v.contains(&"merge".to_string()), "{v:?}");
        // 설명에는 기본 규칙을 넣지 않는다, 튜닝은 tuning 카드
        let v = ids(&pick(all, Task::Optimize, "select * from orders", Some(19)));
        assert_eq!(v, vec!["tuning"]);
        assert!(ids(&pick(all, Task::Explain, "select 1 from dual", Some(19))).is_empty());
    }

    #[test]
    fn budget_and_cap() {
        let all = builtin();
        let text = "상위 10건 날짜 최근 7일 listagg 한 줄로 계층 트리 누적 합계 pivot merge 중복 조인 동적 bulk";
        let r = pick(all, Task::Generate, text, Some(11));
        assert!(r.len() <= MAX_CARDS + 1, "{}", r.len());
        let len: usize = r.iter().map(|g| g.body.len()).sum();
        assert!(len <= BUDGET + 1600, "{len}");
    }

    #[test]
    fn user_file_adds_and_overrides() {
        let p = std::env::temp_dir().join(format!("sqls-guides-{}.md", std::process::id()));
        std::fs::write(&p, "# 회사\n@@ core | 회사 기본 규칙 | * |\n- 모든 테이블은 TB_ 로 시작한다.\n@@ company | 날짜 규칙 | 일자 |\n- 일자는 CHAR(8)\n").unwrap();
        let all = load(Some(&p));
        let core = all.iter().find(|g| g.id == "core").unwrap();
        assert_eq!(core.title, "회사 기본 규칙");
        let v = ids(&pick(&all, Task::Generate, "주문 일자별 건수", Some(19)));
        assert!(v.contains(&"company".to_string()), "{v:?}");
        let _ = std::fs::remove_file(&p);
        assert!(user_template().contains("@@ company_date"));
        assert_eq!(parse(&user_template()).len(), 1);
    }
}
