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

/// 요청에 맞는 카드. 항상 넣는 카드(core 등) + 낱말 점수가 높은 순으로 MAX_CARDS 장, 글자 수 상한 안에서.
/// 버전을 모르면 11g/12c 로 갈린 카드 중 11g 쪽을 쓴다 (11g 문법은 12c 에서도 돈다).
pub fn pick<'a>(all: &'a [Guide], task: Task, text: &str, major: Option<u32>) -> Vec<&'a Guide> {
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
    let mut used: usize = always.iter().map(|g| g.body.len()).sum();
    let mut out = always;
    let mut n = 0;
    for (_, g) in scored {
        if n >= MAX_CARDS {
            break;
        }
        if used + g.body.len() > BUDGET && n > 0 {
            continue;
        }
        used += g.body.len();
        out.push(g);
        n += 1;
    }
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
