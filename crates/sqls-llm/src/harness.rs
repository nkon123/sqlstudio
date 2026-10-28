//! 작은 로컬 모델을 위한 하네스 — 문맥 예산, 반복 감지, 버전 문법 검사, 대화 기록 줄이기.
//!
//! 4B 급 모델은 문맥이 길어지면 중간을 놓치고, 생각 모드에서 같은 말을 되풀이하다 끝나지 않기도 한다.
//! 모델을 키우는 대신 넣는 양을 줄이고, 답을 검사해 구체적인 오류로 다시 묻는다.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{Message, ModelProfile, ProviderConfig, Role};

/// 토큰 수 어림 — ASCII 는 약 4자, 한글 등은 약 1.5자에 1토큰 (Qwen·Gemma 계열 기준, 대략이면 된다)
pub fn estimate_tokens(s: &str) -> usize {
    let (mut ascii, mut other) = (0usize, 0usize);
    for c in s.chars() {
        if c.is_ascii() {
            ascii += 1;
        } else {
            other += 1;
        }
    }
    ascii.div_ceil(4) + (other * 2).div_ceil(3)
}

/// 넣을 문맥의 상한(토큰). 설정에 없으면 로컬은 num_ctx 의 60% 와 8K 중 작은 것 —
/// 작은 모델은 문맥이 커도 8K 를 넘으면 정확도가 떨어진다. 외부(프론티어) 모델은 넉넉히.
pub fn context_budget(cfg: &crate::ProviderConfig) -> usize {
    if let Some(b) = cfg.context_budget {
        return b as usize;
    }
    match cfg.kind {
        crate::ProviderKind::Ollama => {
            // num_ctx 를 모델의 문맥보다 크게 잡아도 모델이 읽는 것은 거기까지다
            let ctx = cfg.num_ctx.unwrap_or(16384).min(profile(cfg).and_then(|p| p.context).unwrap_or(u32::MAX));
            (ctx as usize * 6 / 10).min(8000)
        }
        _ if is_local_model(cfg) => 8000,
        _ => 60000,
    }
}

/// 작은 로컬 모델로 볼 공급자 — Ollama, 또는 사내 주소의 OpenAI 호환 서버(LM Studio·vLLM·llama.cpp).
/// 생각 끄기·낮은 temperature 같은 하네스 기본값은 이 공급자에만 쓴다 (프론티어 모델은 그대로)
pub fn is_local_model(cfg: &crate::ProviderConfig) -> bool {
    match cfg.kind {
        crate::ProviderKind::Ollama => true,
        crate::ProviderKind::OpenaiCompatible => reqwest::Url::parse(&cfg.base_url())
            .ok()
            .and_then(|u| u.host_str().map(crate::is_internal_host))
            .unwrap_or(false),
        crate::ProviderKind::Anthropic => false,
    }
}

/// 지금 모델로 잰 측정 결과 (모델을 바꿨으면 없다)
pub fn profile(cfg: &ProviderConfig) -> Option<&ModelProfile> {
    cfg.profile.as_ref().filter(|p| p.model == cfg.model)
}

/// 이보다 느리면(초당 토큰) 중간 단계(문법 카드·테이블 고르기)를 건너뛰고 고치기는 한 번만
pub const SLOW_TOK_PER_SEC: f32 = 8.0;

/// 하네스 단계 — 모델 측정 결과로 켜고 끈다. 측정 전이면 로컬 4B 급 기본값
#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    /// 형식(JSON 스키마)으로 묶은 중간 단계를 쓸지 (문법 카드·테이블 고르기)
    pub routing: bool,
    /// 형식 지정을 보낼지 (못 따르는 모델은 처음부터 형식 없이 — 400 을 거쳐 다시 보내지 않게)
    pub json: bool,
    /// 생각 조절(think)을 보낼지 (생각 모드가 없는 모델에 보내면 400)
    pub think: bool,
    /// 답의 SQL 이 걸리면 다시 묻는 횟수
    pub repairs: u32,
    /// 문맥에 넣을 테이블 수
    pub tables: usize,
    /// 측정한 속도 (화면에 보인다)
    pub tok_per_sec: Option<f32>,
}

pub fn plan(cfg: &ProviderConfig) -> Plan {
    let local = is_local_model(cfg);
    let mut p = Plan { routing: true, json: true, think: true, repairs: 2, tables: if local { 4 } else { 8 }, tok_per_sec: None };
    if let Some(m) = profile(cfg) {
        p.json = m.json;
        p.think = m.think;
        p.tok_per_sec = Some(m.tok_per_sec);
        if local && m.tok_per_sec > 0.0 && m.tok_per_sec < SLOW_TOK_PER_SEC {
            p.routing = false;
            p.repairs = 1;
            p.tables = 3;
        }
    }
    p
}

/// 질문·SQL 에서 고를 낱말 — 영문 식별자는 대문자로, 한글은 앞 두 글자 (조사가 붙어도 주석과 맞게: "주문일을" → "주문")
pub fn focus_words(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for w in text.split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '$' || c == '#')) {
        let w = w.trim();
        if w.chars().count() < 2 || w.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let k = if w.is_ascii() { w.to_uppercase() } else { w.chars().take(2).collect() };
        if !out.contains(&k) {
            out.push(k);
        }
    }
    out
}

/// 내 쿼리 예시 — AI 가 만든 SQL 중 서버 파싱을 통과하고 사용자가 에디터에 넣은 것을 질문과 함께 쌓는다.
/// 비슷한 질문이 오면 1~2개를 예시로 넣는다 (사내 테이블 이름·조인·코드값 관행을 따라 하게).
/// 이 PC 의 데이터 폴더에만 둔다 — 로컬 모델이 아니면 쓰지 않는다
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Example {
    /// 접속 구분 (사용자@접속 이름) — 다른 DB 의 예시는 쓰지 않는다
    pub db: String,
    pub q: String,
    pub sql: String,
}

/// 쌓아 둘 예시 수 (넘으면 오래된 것부터 버린다)
const MAX_EXAMPLES: usize = 500;

pub struct ExampleStore {
    path: PathBuf,
}

impl ExampleStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 깨진 줄은 건너뛴다
    pub fn load(&self) -> Vec<Example> {
        std::fs::read_to_string(&self.path)
            .map(|t| t.lines().filter_map(|l| serde_json::from_str(l).ok()).collect())
            .unwrap_or_default()
    }

    /// 같은 접속·같은 SQL(공백·대소문자 무시)이면 질문만 새것으로 바꿔 맨 뒤로
    pub fn add(&self, ex: Example) -> std::io::Result<()> {
        let key = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ").trim_end_matches(';').to_uppercase();
        let k = key(&ex.sql);
        let mut all: Vec<Example> = self.load().into_iter().filter(|e| !(e.db == ex.db && key(&e.sql) == k)).collect();
        all.push(ex);
        if all.len() > MAX_EXAMPLES {
            all.drain(..all.len() - MAX_EXAMPLES);
        }
        if let Some(d) = self.path.parent() {
            std::fs::create_dir_all(d)?;
        }
        let text: String = all.iter().filter_map(|e| serde_json::to_string(e).ok()).map(|l| l + "\n").collect();
        std::fs::write(&self.path, text)
    }

    pub fn clear(&self) -> std::io::Result<()> {
        match std::fs::remove_file(&self.path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }

    /// 질문과 낱말이 많이 겹치는 예시 — 겹치는 낱말이 2개 이상(영문 식별자가 겹치면 1개)인 것만, 같으면 최근 것
    pub fn search(&self, db: &str, question: &str, n: usize) -> Vec<Example> {
        let qw = focus_words(question);
        if qw.is_empty() {
            return vec![];
        }
        let mut scored: Vec<(usize, usize, Example)> = self
            .load()
            .into_iter()
            .enumerate()
            .filter(|(_, e)| e.db == db)
            .filter_map(|(i, e)| {
                let ew = focus_words(&format!("{} {}", e.q, e.sql));
                let hits: Vec<&String> = qw.iter().filter(|w| ew.contains(w)).collect();
                let score = hits.len() + hits.iter().filter(|w| w.is_ascii()).count();
                (score >= 2).then_some((score, i, e))
            })
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)));
        scored.into_iter().take(n).map(|(_, _, e)| e).collect()
    }
}

/// 대화 기록을 예산 안으로 — 최근 `keep` 번의 주고받기는 그대로, 그 앞의 답은 SQL(없으면 앞부분)만 남긴다.
/// 그래도 넘치면 오래된 것부터 버린다. 생각(<think>…</think>)은 기록에서 뺀다.
pub fn trim_history(history: &[Message], budget: usize, keep: usize) -> Vec<Message> {
    let n = history.len();
    let mut out: Vec<Message> = history
        .iter()
        .enumerate()
        .map(|(i, m)| {
            let recent = i + keep * 2 >= n;
            let content = strip_think(&m.content);
            let content = if recent || m.role == Role::User && estimate_tokens(&content) < 300 {
                content
            } else if m.role == Role::Assistant {
                match crate::prompts::extract_sql(&content) {
                    Some(sql) => format!("(앞서 낸 SQL)\n```sql\n{sql}\n```"),
                    None => format!("{}…", content.chars().take(200).collect::<String>()),
                }
            } else {
                // 앞선 요청의 스키마·문법 안내는 다시 보낼 필요가 없다 — 요청 부분만
                match content.rfind("## 요청\n") {
                    Some(i) => content[i..].to_string(),
                    None => format!("{}…", content.chars().take(300).collect::<String>()),
                }
            };
            Message { role: m.role, content }
        })
        .collect();
    // 주고받기 단위(사용자+답)로 앞에서 버린다
    while out.len() > 2 && out.iter().map(|m| estimate_tokens(&m.content)).sum::<usize>() > budget {
        out.drain(..2.min(out.len()));
    }
    if out.iter().map(|m| estimate_tokens(&m.content)).sum::<usize>() > budget {
        out.clear();
    }
    out
}

/// `<think>…</think>` 를 뺀다 (생각을 본문에 섞어 내는 서버)
pub fn strip_think(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find("<think>") {
        out.push_str(&rest[..i]);
        match rest[i..].find("</think>") {
            Some(j) => rest = &rest[i + j + "</think>".len()..],
            None => {
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);
    out.trim().to_string()
}

/// 스트림 반복 감지 — 끝부분 80자가 최근 2,000자 안에 4번 이상 나오면 되풀이에 빠진 것으로 본다.
/// 글자가 계속 들어오니 유휴 타임아웃으로는 잡히지 않는다.
#[derive(Default)]
pub struct RepeatGuard {
    buf: String,
    since: usize,
}

impl RepeatGuard {
    const TAIL: usize = 80;
    const WINDOW: usize = 2000;
    const TIMES: usize = 4;

    /// 새 글자를 더한다. 되풀이로 보이면 true
    pub fn push(&mut self, t: &str) -> bool {
        self.buf.push_str(t);
        self.since += t.len();
        // 너무 자주 보지 않는다
        if self.since < 200 {
            return false;
        }
        self.since = 0;
        if self.buf.len() > Self::WINDOW * 2 {
            let cut = floor_char(&self.buf, self.buf.len() - Self::WINDOW);
            self.buf.drain(..cut);
        }
        let start = floor_char(&self.buf, self.buf.len().saturating_sub(Self::WINDOW));
        let win = &self.buf[start..];
        let tail_at = floor_char(win, win.len().saturating_sub(Self::TAIL));
        let tail = &win[tail_at..];
        // 공백만 이어지는 것(코드 들여쓰기)은 반복으로 치지 않는다
        if tail.trim().chars().count() < 20 {
            return false;
        }
        win.matches(tail).count() >= Self::TIMES
    }
}

fn floor_char(s: &str, mut i: usize) -> usize {
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// 주석·문자열·따옴표 식별자를 공백으로 (낱말 검사용)
pub(crate) fn code_only(sql: &str) -> String {
    let b: Vec<char> = sql.chars().collect();
    let mut out = String::with_capacity(sql.len());
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c == '-' && b.get(i + 1) == Some(&'-') {
            while i < b.len() && b[i] != '\n' {
                i += 1;
            }
        } else if c == '/' && b.get(i + 1) == Some(&'*') {
            i += 2;
            while i < b.len() && !(b[i] == '*' && b.get(i + 1) == Some(&'/')) {
                i += 1;
            }
            i += 2;
        } else if (c == 'q' || c == 'Q') && b.get(i + 1) == Some(&'\'') && i + 2 < b.len() {
            // q'[ … ]'
            let close = match b[i + 2] {
                '[' => ']',
                '(' => ')',
                '{' => '}',
                '<' => '>',
                x => x,
            };
            i += 3;
            while i < b.len() && !(b[i] == close && b.get(i + 1) == Some(&'\'')) {
                i += 1;
            }
            i += 2;
        } else if c == '\'' || c == '"' {
            i += 1;
            while i < b.len() {
                if b[i] == c {
                    if b.get(i + 1) == Some(&c) {
                        i += 2;
                        continue;
                    }
                    break;
                }
                i += 1;
            }
            i += 1;
        } else {
            out.push(c.to_ascii_uppercase());
            i += 1;
            continue;
        }
        out.push(' ');
    }
    out
}

/// 대상 버전에 없는 문법 — 서버에 보내기 전에 잡는다 (11g 에서 모델이 가장 자주 틀리는 것들)
pub fn lint_version(sql: &str, major: Option<u32>) -> Vec<String> {
    let Some(m) = major else { return vec![] };
    let words: Vec<String> = code_only(sql)
        .split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '$' || c == '#'))
        .filter(|w| !w.is_empty())
        .map(str::to_string)
        .collect();
    let has = |seq: &[&str]| words.windows(seq.len()).any(|w| w.iter().zip(seq).all(|(a, b)| a == b));
    let mut out = Vec::new();
    let mut need = |cond: bool, min: u32, what: &str| {
        if cond && m < min {
            out.push(format!("{what} 은(는) Oracle {min}c 부터 있습니다 (대상 {m})"));
        }
    };
    need(has(&["FETCH", "FIRST"]) || has(&["FETCH", "NEXT"]), 12, "FETCH FIRST/NEXT … ROWS");
    need(words.windows(3).any(|w| w[0] == "OFFSET" && w[1].chars().all(|c| c.is_ascii_digit()) && (w[2] == "ROWS" || w[2] == "ROW")), 12, "OFFSET … ROWS");
    need(has(&["AS", "IDENTITY"]), 12, "IDENTITY 컬럼");
    need(has(&["CROSS", "APPLY"]) || has(&["OUTER", "APPLY"]), 12, "CROSS/OUTER APPLY");
    need(has(&["LATERAL"]), 12, "LATERAL");
    need(words.iter().any(|w| w.starts_with("JSON_")), 12, "JSON_* 함수");
    need(has(&["ON", "OVERFLOW"]), 12, "LISTAGG … ON OVERFLOW");
    need(has(&["VALIDATE_CONVERSION"]) || has(&["ON", "CONVERSION", "ERROR"]), 12, "변환 오류 처리(VALIDATE_CONVERSION, ON CONVERSION ERROR)");
    need(has(&["APPROX_COUNT_DISTINCT"]), 12, "APPROX_COUNT_DISTINCT");
    need(has(&["WITH", "FUNCTION"]), 12, "WITH FUNCTION");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_rough() {
        assert_eq!(estimate_tokens("select"), 2);
        assert_eq!(estimate_tokens("고객별 합계"), 5);
    }

    #[test]
    fn repeat_detected() {
        let mut g = RepeatGuard::default();
        let line = "잠깐, 다시 생각해 보면 이 테이블의 컬럼은 무엇일까? ";
        let hit = (0..40).any(|_| g.push(line));
        assert!(hit);
        let mut g = RepeatGuard::default();
        let normal: String = (0..300).map(|i| format!("줄 {i}: 컬럼 c{i} 을 본다. ")).collect();
        assert!(!normal.as_bytes().chunks(50).any(|c| g.push(std::str::from_utf8(c).unwrap_or(""))), "다른 글은 반복 아님");
        let mut g = RepeatGuard::default();
        assert!(!(0..100).any(|_| g.push("        ")), "공백은 반복 아님");
    }

    #[test]
    fn plan_from_profile() {
        let mut c = ProviderConfig::new("t", crate::ProviderKind::Ollama, "qwen3.5:4b");
        assert_eq!(plan(&c), Plan { routing: true, json: true, think: true, repairs: 2, tables: 4, tok_per_sec: None });
        c.profile = Some(ModelProfile { model: "qwen3.5:4b".into(), tok_per_sec: 4.5, json: false, think: false, context: Some(8192) });
        let p = plan(&c);
        assert!(!p.routing && !p.json && !p.think && p.repairs == 1 && p.tables == 3);
        // 모델 문맥이 num_ctx 보다 작으면 그것을 기준으로
        assert_eq!(context_budget(&c), 8192 * 6 / 10);
        // 모델을 바꾸면 측정값은 쓰지 않는다
        c.model = "gemma4:e2b".into();
        assert_eq!(plan(&c).tok_per_sec, None);
        assert_eq!(context_budget(&c), 8000);
    }

    #[test]
    fn examples_store_and_search() {
        let dir = std::env::temp_dir().join(format!("sqls-ex-{}", std::process::id()));
        let st = ExampleStore::new(dir.join("ai-examples.jsonl"));
        let _ = st.clear();
        let ex = |db: &str, q: &str, sql: &str| Example { db: db.into(), q: q.into(), sql: sql.into() };
        st.add(ex("APP@개발", "부서별 사원 수", "SELECT deptno, COUNT(*) FROM emp GROUP BY deptno;")).unwrap();
        st.add(ex("APP@개발", "최근 주문 고객별 합계", "SELECT cust_id, SUM(amt) FROM orders WHERE order_dt > SYSDATE - 7 GROUP BY cust_id;")).unwrap();
        st.add(ex("APP@운영", "최근 주문 고객별 합계", "SELECT 1 FROM dual;")).unwrap();
        // 같은 SQL 은 하나로 (질문은 새것)
        st.add(ex("APP@개발", "부서별 사원 수 세기", "select deptno, count(*)  from emp group by deptno")).unwrap();
        assert_eq!(st.load().len(), 3);
        let hit = st.search("APP@개발", "지난달 주문을 고객별로 합계", 2);
        assert_eq!(hit.len(), 1);
        assert!(hit[0].sql.contains("orders"));
        // 영문 식별자가 겹치면 하나로도
        assert_eq!(st.search("APP@개발", "EMP 에서 뽑아 줘", 2)[0].q, "부서별 사원 수 세기");
        assert!(st.search("APP@개발", "날씨", 2).is_empty());
        st.clear().unwrap();
        assert!(st.load().is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn version_lint() {
        let q = "select * from emp order by sal desc fetch first 10 rows only";
        assert_eq!(lint_version(q, Some(11)).len(), 1);
        assert!(lint_version(q, Some(19)).is_empty());
        assert!(lint_version(q, None).is_empty());
        // 문자열·주석 안은 보지 않는다
        assert!(lint_version("select 'fetch first' x from dual -- json_value\n", Some(11)).is_empty());
        assert_eq!(lint_version("select json_value(d, '$.a') from t offset 5 rows", Some(11)).len(), 2);
    }

    #[test]
    fn history_trim() {
        let h = vec![
            Message::user("## 관련 스키마\n```\nTABLE A\n".to_string() + &"  C NUMBER\n".repeat(200) + "```\n## 요청\n첫 질문"),
            Message::assistant("설명 ".repeat(100) + "\n```sql\nselect 1 from dual;\n```"),
            Message::user("두번째"),
            Message::assistant("<think>음…</think>답"),
        ];
        let t = trim_history(&h, 10000, 1);
        assert_eq!(t[0].content, "## 요청\n첫 질문");
        assert!(t[1].content.contains("select 1 from dual") && !t[1].content.contains("설명"));
        assert_eq!(t[3].content, "답");
        assert_eq!(trim_history(&h, 5, 1).len(), 2, "최근 한 번만 남는다");
        assert!(trim_history(&h, 2, 1).is_empty());
    }
}
