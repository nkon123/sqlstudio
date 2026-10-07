//! SQL 도우미 프롬프트.
//!
//! 서버 버전을 반드시 넣는다. 모델은 기본적으로 최신 문법(FETCH FIRST, IDENTITY,
//! JSON_TABLE ...)을 쓰는데, 11g 에서는 전부 문법 오류다.

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

pub fn system_prompt(task: Task, ctx: &Context) -> String {
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
    s.push_str(match task {
        Task::Generate => "할 일: 요청을 만족하는 SQL 하나를 만든다. 가정한 것이 있으면 한 줄로 밝힌다.",
        Task::Explain => "할 일: SQL 이 무엇을 하는지 단계별로 설명하고, 결과가 틀릴 수 있는 지점(NULL, 중복, 조인 누락)을 짚는다.",
        Task::Optimize => {
            "할 일: 실행계획을 근거로 병목을 찾고 개선안을 낸다. 결과 건수가 바뀌는 변경은 하지 마라.\n\
             인덱스 제안은 CREATE INDEX 문으로 쓰되, 기존 인덱스와 겹치는지 먼저 확인한다.\n\
             추측이 아니라 계획의 어느 줄(Id)을 근거로 하는지 밝힌다."
        }
        Task::Fix => "할 일: 오류의 원인을 한 줄로 밝히고, 고친 SQL 을 낸다. 바꾼 부분을 짚는다.",
        Task::Chat => "할 일: 질문에 답한다.",
    });
    s
}

/// 사용자 메시지 본문. 스키마 → SQL → 계획 → 오류 → 질문 순서.
pub fn user_message(task: Task, ctx: &Context) -> String {
    let mut m = String::new();
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
            Task::Chat => "",
        }
        .to_string()
    });
    if !q.is_empty() {
        m.push_str(&format!("## 요청\n{q}\n"));
    }
    m
}

pub fn build(task: Task, ctx: &Context, history: &[Message]) -> ChatRequest {
    let mut messages = history.to_vec();
    messages.push(Message::user(user_message(task, ctx)));
    ChatRequest { system: system_prompt(task, ctx), messages }
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
        let s = system_prompt(Task::Generate, &ctx);
        assert!(s.contains("FETCH FIRST"));
        assert!(s.contains("ROWNUM"));
        let ctx19 = Context { server_version: Some("19.3.0.0.0".into()), ..Default::default() };
        assert!(!system_prompt(Task::Generate, &ctx19).contains("ROWNUM"));
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
    fn sql_extraction() {
        let a = "설명\n```text\nnot this\n```\n그리고\n```sql\nselect 1 from dual;\n```\n";
        assert_eq!(extract_sql(a).unwrap(), "select 1 from dual;");
        assert_eq!(extract_sql("```\nselect 2 from dual\n```").unwrap(), "select 2 from dual");
        assert!(extract_sql("no code").is_none());
    }
}
