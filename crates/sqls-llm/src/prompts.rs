//! SQL 도우미 프롬프트.
//!
//! 서버 버전을 반드시 넣는다. 모델은 기본적으로 최신 문법(FETCH FIRST, IDENTITY,
//! JSON_TABLE ...)을 쓰는데, 11g 에서는 전부 문법 오류다.
//!
//! 시스템 프롬프트는 접속(버전·사용자)마다 같게 둔다 — 할 일·문법 안내·스키마처럼 요청마다 바뀌는 것은
//! 사용자 메시지로 보낸다. 앞부분이 같으면 로컬 서버(Ollama·llama.cpp)가 앞서 읽은 부분(KV 캐시)을
//! 다시 써서 첫 글자가 빨리 나온다.

use crate::{ChatRequest, Message};

/// 도우미가 할 일
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Task {
    /// 자연어 → SQL
    Generate,
    /// SQL 설명
    Explain,
    /// 튜닝 (실행계획 포함)
    Optimize,
    /// ORA 오류 고치기
    Fix,
    /// 자유 대화
    Chat,
    /// 값 추적 결과로 "이 값이 어디서 왔나" 를 풀어 준다 (고치지 않는다)
    Trace,
}

/// 요청에 들어갈 재료
#[derive(Debug, Clone, Default)]
pub struct Context {
    /// "11.2.0.4.0" 같은 서버 버전
    pub server_version: Option<String>,
    pub current_user: Option<String>,
    /// meta::schema_brief 로 만든 테이블 요약들
    pub schema: Vec<String>,
    pub sql: Option<String>,
    /// DBMS_XPLAN 출력
    pub plan: Option<String>,
    /// "ORA-00904: ..." 등
    pub error: Option<String>,
    /// 사용자의 질문/지시
    pub question: Option<String>,
    /// 이 요청에 맞춰 고른 Oracle 문법 안내 (guides::render)
    pub guides: String,
    /// 이 DB 에서 비슷한 요청에 썼던 (질문, SQL) — 사내 테이블 이름과 관행을 따라 하게
    pub examples: Vec<(String, String)>,
}

/// 메이저 버전 (11, 12, 19 ...)
pub fn major_version(v: &str) -> Option<u32> {
    v.trim().split('.').next()?.trim().parse().ok()
}

fn version_rules(major: Option<u32>) -> &'static str {
    match major {
        Some(m) if m < 12 => {
            "대상은 Oracle 11g 다. 다음은 11g 에 없으므로 절대 쓰지 마라:\n\
             - FETCH FIRST / OFFSET ... ROWS → ROWNUM 으로 감싼 인라인 뷰를 쓴다\n\
             - IDENTITY 컬럼, DEFAULT seq.NEXTVAL → 시퀀스 + 트리거\n\
             - LATERAL, CROSS/OUTER APPLY, JSON_* 함수, LISTAGG ... ON OVERFLOW\n\
             - 32767 바이트 VARCHAR2(MAX_STRING_SIZE), 인라인 PL/SQL 함수(WITH FUNCTION)\n\
             - APPROX_COUNT_DISTINCT, VALIDATE_CONVERSION, CAST ... DEFAULT ON CONVERSION ERROR\n\
             LISTAGG 는 11.2 부터 있다 (WITHIN GROUP 필수)."
        }
        Some(m) if m < 18 => "대상은 Oracle 12c 다. 18c 이후 기능(예: JSON_OBJECT 일부 문법, 다형 테이블 함수)은 피한다.",
        _ => "대상 Oracle 버전에 맞는 문법만 쓴다.",
    }
}

pub fn system_prompt(ctx: &Context) -> String {
    let major = ctx.server_version.as_deref().and_then(major_version);
    let mut s = String::from(
        "너는 Oracle SQL/PL-SQL 전문가로, 데스크톱 SQL 에디터 안에서 개발자를 돕는다.\n\
         규칙:\n\
         - 스키마 정보에 없는 테이블·컬럼을 지어내지 마라. 모르면 무엇이 필요한지 물어라.\n\
         - SQL 은 ```sql 코드 블록 하나에 담는다. 문장 끝에 ; 를 붙인다.\n\
         - 데이터를 바꾸는 문장(DML/DDL)은 먼저 영향을 설명하고, 되돌리는 방법을 함께 적는다.\n\
         - 리터럴 대신 바인드 변수(:name)를 쓴다.\n\
         - 한국어로 짧게 답한다.\n",
    );
    s.push_str(version_rules(major));
    s.push('\n');
    if let Some(v) = &ctx.server_version {
        s.push_str(&format!("서버 버전: {v}\n"));
    }
    if let Some(u) = &ctx.current_user {
        s.push_str(&format!("접속 사용자: {u} (스키마를 생략한 이름은 이 사용자 소유로 본다)\n"));
    }
    s
}

/// 작업별 지시 — 사용자 메시지의 요청 앞에 넣는다
fn task_line(task: Task) -> &'static str {
    match task {
        Task::Generate => "할 일: 요청을 만족하는 SQL 하나를 만든다. 가정한 것이 있으면 한 줄로 밝힌다.",
        Task::Explain => "할 일: SQL 이 무엇을 하는지 단계별로 설명하고, 결과가 틀릴 수 있는 지점(NULL, 중복, 조인 누락)을 짚는다.",
        Task::Optimize => {
            "할 일: 실행계획을 근거로 병목을 찾고 개선안을 낸다. 결과 건수가 바뀌는 변경은 하지 마라.\n\
             인덱스 제안은 CREATE INDEX 문으로 쓰되, 기존 인덱스와 겹치는지 먼저 확인한다.\n\
             추측이 아니라 계획의 어느 줄(Id)을 근거로 하는지 밝힌다."
        }
        Task::Fix => "할 일: 오류의 원인을 한 줄로 밝히고, 고친 SQL 을 낸다. 바꾼 부분을 짚는다.",
        Task::Chat => "할 일: 질문에 답한다.",
        Task::Trace => {
            "할 일: 위 '값 추적' 결과만 근거로, 이 값이 어디서 왔는지(출처)를 밝힌다. 고치는 방법·새 SQL 은 내지 않는다 (SQL 코드 블록을 쓰지 마라).
             형식:
             1) 출처 경로 — `대상 열 ← 넣은 문장(객체·줄) ← 원본 열 ← …` 을 한 줄로
             2) 단계마다 한 줄: 원본 값이 어떤 식·조건을 거쳐 이 값이 되었는지 (원본 값 → 식 → 결과 값)
             3) 확실하지 않은 곳: 재현되지 않은 문장, 추적이 끊긴 곳(분석하지 않은 소스·동적 SQL·값을 모르는 변수)
             '재현됨' 인 문장이 이 행을 만든 길이다. 추적에 없는 테이블·문장은 지어내지 않는다."
        }
    }
}

/// 사용자 메시지 본문. 문법 안내 → 스키마 → SQL → 계획 → 오류 → 할 일 → 질문 순서.
pub fn user_message(task: Task, ctx: &Context) -> String {
    let mut m = String::new();
    if !ctx.guides.trim().is_empty() {
        m.push_str(ctx.guides.trim_start());
        m.push('\n');
    }
    if !ctx.examples.is_empty() {
        m.push_str("## 이 DB 에서 비슷한 요청에 썼던 SQL (테이블·조인·코드값 관행을 참고한다. 그대로 베끼지 않는다)\n");
        for (q, sql) in &ctx.examples {
            m.push_str(&format!("요청: {}\n```sql\n{}\n```\n", q.trim(), sql.trim()));
        }
        m.push('\n');
    }
    if !ctx.schema.is_empty() {
        m.push_str("## 관련 스키마\n```\n");
        for t in &ctx.schema {
            m.push_str(t);
        }
        m.push_str("```\n\n");
    }
    if let Some(sql) = &ctx.sql {
        m.push_str(&format!("## SQL\n```sql\n{}\n```\n\n", sql.trim()));
    }
    if let Some(p) = &ctx.plan {
        m.push_str(&format!("## 실행계획 (DBMS_XPLAN)\n```\n{}\n```\n\n", p.trim_end()));
    }
    if let Some(e) = &ctx.error {
        m.push_str(&format!("## 오류\n{}\n\n", e.trim()));
    }
    let q = ctx.question.clone().unwrap_or_else(|| {
        match task {
            Task::Generate => "",
            Task::Explain => "이 SQL 을 설명해 줘.",
            Task::Optimize => "이 SQL 을 튜닝해 줘.",
            Task::Fix => "이 오류를 고쳐 줘.",
            Task::Chat | Task::Trace => "",
        }
        .to_string()
    });
    m.push_str(&format!("## 요청\n{}\n", task_line(task)));
    if !q.is_empty() {
        m.push_str(&q);
        m.push('\n');
    }
    m
}

pub fn build(task: Task, ctx: &Context, history: &[Message]) -> ChatRequest {
    let mut messages = history.to_vec();
    messages.push(Message::user(user_message(task, ctx)));
    ChatRequest { system: system_prompt(ctx), messages, json_schema: None }
}

/// 검증에서 걸린 SQL 을 고쳐 달라는 메시지 — 오류와 그 테이블의 전체 컬럼을 준다.
/// 작은 모델은 혼자서는 컬럼을 지어내도, 구체적인 오류와 목록을 주면 잘 고친다.
pub fn repair_message(problem: &str, schema: &[String]) -> String {
    let mut m = format!("## 검증 결과\n앞의 SQL 을 확인했더니 문제가 있다:\n{}\n\n", problem.trim());
    if !schema.is_empty() {
        m.push_str("## 관련 테이블의 전체 컬럼\n```\n");
        for t in schema {
            m.push_str(t);
        }
        m.push_str("```\n\n");
    }
    m.push_str("## 요청\n문제를 고친 SQL 하나를 ```sql 블록으로 다시 낸다. 위 목록에 있는 테이블·컬럼만 쓴다. 무엇을 고쳤는지 한 줄로 밝힌다.\n");
    m
}

/// SQL 을 만들기 전에 쓸 테이블을 고르게 하는 요청 — 후보(이름 | 주석)를 enum 으로 묶는다.
/// 작은 모델은 고르기와 쓰기를 한 번에 시키면 컬럼을 지어낸다. 고르기만 따로 시키면 몇 초면 끝난다
pub fn table_pick_request(question: &str, cands: &[(String, String)], max: usize) -> Option<ChatRequest> {
    if cands.is_empty() {
        return None;
    }
    let list: String = cands.iter().map(|(n, c)| format!("{n} | {}\n", c.trim())).collect();
    let names: Vec<&str> = cands.iter().map(|(n, _)| n.as_str()).collect();
    let q: String = question.chars().take(1500).collect();
    Some(ChatRequest {
        system: format!(
            "너는 Oracle SQL 도우미의 테이블 선택기다. 요청을 SQL 로 만들 때 필요한 테이블을 아래 목록에서 고른다.\n\
             - 꼭 필요한 것만, 많아야 {max}개. 조인에 필요한 테이블도 넣는다.\n\
             - 목록에 있는 이름만 쓴다. 맞는 것이 없으면 빈 배열.\n\
             - 답은 JSON 하나만: {{\"tables\": [\"이름\", ...]}} — 설명을 붙이지 않는다."
        ),
        messages: vec![Message::user(format!("## 테이블 목록 (이름 | 주석)\n{list}\n## 요청\n{q}"))],
        json_schema: Some(serde_json::json!({
            "type": "object",
            "properties": { "tables": { "type": "array", "items": { "type": "string", "enum": names } } },
            "required": ["tables"],
        })),
    })
}

/// 테이블 고르기 답 — 형식이 조금 어긋나도 읽는다. 후보에 없는 이름은 버린다
pub fn parse_pick(answer: &str, cands: &[(String, String)], max: usize) -> Vec<String> {
    let t = answer.trim();
    let v: Option<serde_json::Value> = [('{', '}'), ('[', ']')].iter().find_map(|(a, b)| {
        let (s, e) = (t.find(*a)?, t.rfind(*b)?);
        (s < e).then(|| serde_json::from_str(&t[s..=e]).ok()).flatten()
    });
    let arr = match &v {
        Some(serde_json::Value::Array(a)) => a.clone(),
        Some(o) => o["tables"].as_array().cloned().unwrap_or_default(),
        None => vec![],
    };
    let mut out: Vec<String> = Vec::new();
    for x in arr.iter().filter_map(|x| x.as_str()) {
        if let Some((n, _)) = cands.iter().find(|(n, _)| n.eq_ignore_ascii_case(x.trim())) {
            if !out.contains(n) {
                out.push(n.clone());
            }
        }
    }
    out.truncate(max);
    out
}

/// 답에서 첫 번째 ```sql 블록을 꺼낸다 ("에디터에 넣기" 버튼)
pub fn extract_sql(answer: &str) -> Option<String> {
    let mut rest = answer;
    while let Some(i) = rest.find("```") {
        let after = &rest[i + 3..];
        let (lang, body) = after.split_once('\n')?;
        let end = body.find("```")?;
        let lang = lang.trim().to_lowercase();
        if lang.is_empty() || lang == "sql" || lang == "plsql" || lang == "oracle" {
            return Some(body[..end].trim().to_string());
        }
        rest = &body[end + 3..];
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eleven_g_rules_included() {
        let ctx = Context { server_version: Some("11.2.0.4.0".into()), ..Default::default() };
        let s = system_prompt(&ctx);
        assert!(s.contains("FETCH FIRST"));
        assert!(s.contains("ROWNUM"));
        let ctx19 = Context { server_version: Some("19.3.0.0.0".into()), ..Default::default() };
        assert!(!system_prompt(&ctx19).contains("ROWNUM"));
    }

    #[test]
    fn message_order() {
        let ctx = Context {
            schema: vec!["TABLE SCOTT.EMP\n  EMPNO NUMBER\n".into()],
            sql: Some("select * from emp".into()),
            error: Some("ORA-00942".into()),
            ..Default::default()
        };
        let m = user_message(Task::Fix, &ctx);
        let (a, b, c) = (m.find("스키마").unwrap(), m.find("## SQL").unwrap(), m.find("ORA-00942").unwrap());
        assert!(a < b && b < c);
        assert!(m.contains("고쳐"));
    }

    #[test]
    fn system_stays_same_across_tasks() {
        // 요청마다 바뀌는 것(할 일·문법 안내)은 사용자 메시지로 — 시스템 프롬프트가 같아야 캐시를 다시 쓴다
        let ctx = Context { server_version: Some("19.3".into()), guides: "
## Oracle 문법 참고
### MERGE
…
".into(), ..Default::default() };
        let a = build(Task::Generate, &ctx, &[]);
        let b = build(Task::Fix, &Context { guides: String::new(), ..ctx.clone() }, &[]);
        assert_eq!(a.system, b.system);
        assert!(!a.system.contains("MERGE") && a.messages[0].content.starts_with("## Oracle 문법 참고"));
        assert!(a.messages[0].content.contains("할 일: 요청을 만족하는 SQL"));
    }

    #[test]
    fn examples_and_pick() {
        let ctx = Context { examples: vec![("부서별 인원".into(), "SELECT deptno, COUNT(*) FROM emp GROUP BY deptno;".into())], schema: vec!["TABLE EMP\n".into()], ..Default::default() };
        let m = user_message(Task::Generate, &ctx);
        assert!(m.find("비슷한 요청").unwrap() < m.find("## 관련 스키마").unwrap() && m.contains("요청: 부서별 인원"));
        let cands = vec![("ORDERS".to_string(), "주문".to_string()), ("CUSTOMERS".to_string(), "고객".to_string())];
        let r = table_pick_request("고객별 주문 합계", &cands, 4).unwrap();
        assert_eq!(r.json_schema.unwrap()["properties"]["tables"]["items"]["enum"].as_array().unwrap().len(), 2);
        assert_eq!(parse_pick("```json\n{\"tables\": [\"orders\", \"NOPE\", \"CUSTOMERS\"]}\n```", &cands, 4), vec!["ORDERS", "CUSTOMERS"]);
        assert_eq!(parse_pick("[\"CUSTOMERS\", \"ORDERS\"]", &cands, 1), vec!["CUSTOMERS"]);
        assert!(parse_pick("모르겠다", &cands, 4).is_empty());
    }

    #[test]
    fn trace_explains_origin_not_fix() {
        let ctx = Context { question: Some("ORD_SUM(ORD_NO=A123).TOT_AMT = 0
  ← MERGE …".into()), ..Default::default() };
        let m = user_message(Task::Trace, &ctx);
        assert!(m.contains("어디서 왔는지") && m.contains("새 SQL 은 내지 않는다") && m.contains("출처 경로"), "{m}");
        assert!(m.ends_with("← MERGE …
"));
    }

    #[test]
    fn sql_extraction() {
        let a = "설명\n```text\nnot this\n```\n그리고\n```sql\nselect 1 from dual;\n```\n";
        assert_eq!(extract_sql(a).unwrap(), "select 1 from dual;");
        assert_eq!(extract_sql("```\nselect 2 from dual\n```").unwrap(), "select 2 from dual");
        assert!(extract_sql("no code").is_none());
    }
}
