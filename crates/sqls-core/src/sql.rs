//! SQL 텍스트 분석 — 스크립트 분리, 문장 종류 판정, 바인드 변수 추출.
//!
//! DB 에 보내기 전에 하는 일은 전부 여기서 한다. 서버에 묻지 않으므로
//! 빠르고, 접속 없이 테스트할 수 있다.
//!
//! 규칙 (SQL*Plus 와 같게):
//! - 일반 SQL 은 `;` 에서 끝난다. 끝의 `;` 는 떼고 보낸다 (붙이면 ORA-00911).
//! - PL/SQL 블록(BEGIN/DECLARE, CREATE PROCEDURE/FUNCTION/PACKAGE/TRIGGER/TYPE)은
//!   안쪽의 `;` 로 끝나지 않는다. 한 줄에 `/` 만 있는 곳에서 끝난다.
//! - 한 줄에 `/` 만 있으면 어떤 문장이든 거기서 끝난다.
//! - 주석, 문자열(`'..'`, `q'[..]'`), 따옴표 식별자(`".."`) 안의 `;` `/` `:` 는 무시한다.

use serde::Serialize;

/// 문장 종류. 실행 경로와 안전 판정이 이것으로 갈린다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StmtKind {
    /// SELECT / WITH — 결과 집합이 있다.
    Query,
    /// INSERT / UPDATE / DELETE / MERGE / LOCK TABLE
    Dml,
    /// CREATE / ALTER / DROP / TRUNCATE / GRANT ...
    Ddl,
    /// 익명 블록, CALL
    Plsql,
    /// CREATE PROCEDURE / FUNCTION / PACKAGE / TRIGGER / TYPE — 컴파일 대상
    PlsqlUnit,
    /// COMMIT / ROLLBACK / SAVEPOINT / SET TRANSACTION
    Tcl,
    /// ALTER SESSION
    Session,
    /// EXPLAIN PLAN
    Explain,
    /// SQL*Plus 명령 (PROMPT, SET, SPOOL ...) — 서버에 보내지 않는다.
    SqlPlus,
    /// 판정하지 못함
    Other,
}

impl StmtKind {
    /// 화면에 보일 이름
    pub fn label(self) -> &'static str {
        match self {
            StmtKind::Query => "조회(SELECT)",
            StmtKind::Dml => "데이터 변경(DML)",
            StmtKind::Ddl => "구조 변경(DDL)",
            StmtKind::Plsql => "PL/SQL 블록",
            StmtKind::PlsqlUnit => "PL/SQL 컴파일",
            StmtKind::Tcl => "트랜잭션 제어",
            StmtKind::Session => "세션 설정",
            StmtKind::Explain => "EXPLAIN PLAN",
            StmtKind::SqlPlus => "SQL*Plus 명령",
            StmtKind::Other => "기타",
        }
    }
}

/// 스크립트에서 잘라낸 문장 하나.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Statement {
    /// 서버에 보낼 텍스트 (끝의 `;` 와 `/` 는 정리된 상태)
    pub text: String,
    pub kind: StmtKind,
    /// 원문에서의 바이트 위치 — 에디터에서 오류 위치를 표시할 때 쓴다.
    pub start: usize,
    pub end: usize,
    /// 원문 기준 시작 줄 (1부터)
    pub line: usize,
}

// ─────────────────────────────────────────────────────────────
// 렉서 — 주석·문자열을 건너뛰며 "코드" 영역만 알려 준다.
// ─────────────────────────────────────────────────────────────

/// 바이트 단위로 훑으며 코드 영역의 문자만 콜백에 넘긴다.
/// 주석·문자열·따옴표 식별자 안은 건너뛴다. 콜백은 (바이트 위치, 문자) 를 받는다.
fn scan_code(src: &str, mut on_code: impl FnMut(usize, char) -> bool) {
    let b = src.as_bytes();
    let n = b.len();
    let mut i = 0;
    while i < n {
        let c = b[i];
        // -- 주석
        if c == b'-' && i + 1 < n && b[i + 1] == b'-' {
            while i < n && b[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        // /* */ 주석
        if c == b'/' && i + 1 < n && b[i + 1] == b'*' {
            i += 2;
            while i + 1 < n && !(b[i] == b'*' && b[i + 1] == b'/') {
                i += 1;
            }
            i = (i + 2).min(n);
            continue;
        }
        // q'[ ... ]' 대체 인용 (앞 글자가 식별자의 일부가 아니어야 한다)
        if (c == b'q' || c == b'Q')
            && i + 2 < n
            && b[i + 1] == b'\''
            && (i == 0 || !is_ident_byte(b[i - 1]) || is_nq_prefix(b, i))
        {
            let open = b[i + 2];
            let close = match open {
                b'[' => b']',
                b'(' => b')',
                b'{' => b'}',
                b'<' => b'>',
                other => other,
            };
            i += 3;
            while i + 1 < n && !(b[i] == close && b[i + 1] == b'\'') {
                i += 1;
            }
            i = (i + 2).min(n);
            continue;
        }
        // '...' 문자열 ('' 는 이스케이프)
        if c == b'\'' {
            i += 1;
            while i < n {
                if b[i] == b'\'' {
                    if i + 1 < n && b[i + 1] == b'\'' {
                        i += 2;
                        continue;
                    }
                    break;
                }
                i += 1;
            }
            i = (i + 1).min(n);
            continue;
        }
        // "..." 따옴표 식별자
        if c == b'"' {
            i += 1;
            while i < n && b[i] != b'"' {
                i += 1;
            }
            i = (i + 1).min(n);
            continue;
        }
        // 코드 문자 — UTF-8 경계에서 char 를 꺼낸다
        let ch = src[i..].chars().next().unwrap_or('\0');
        if !on_code(i, ch) {
            return;
        }
        i += ch.len_utf8().max(1);
    }
}

/// `nq'..'` (N 접두 대체 인용) 의 q 위치인지
fn is_nq_prefix(b: &[u8], q_pos: usize) -> bool {
    q_pos >= 1
        && (b[q_pos - 1] == b'n' || b[q_pos - 1] == b'N')
        && (q_pos == 1 || !is_ident_byte(b[q_pos - 2]))
}

fn is_ident_byte(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c == b'$' || c == b'#' || c >= 0x80
}

fn is_ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$' || c == '#'
}

/// 코드 영역의 단어(대문자)를 앞에서부터 최대 `limit` 개.
fn leading_words(src: &str, limit: usize) -> Vec<String> {
    let mut words = Vec::new();
    let mut cur = String::new();
    let mut last_end = 0usize;
    scan_code(src, |pos, ch| {
        if is_ident_char(ch) {
            // 주석/문자열을 건너뛴 자리는 단어 경계다
            if !cur.is_empty() && pos != last_end {
                words.push(std::mem::take(&mut cur));
            }
            cur.extend(ch.to_uppercase());
            last_end = pos + ch.len_utf8();
        } else {
            if !cur.is_empty() {
                words.push(std::mem::take(&mut cur));
            }
            // 괄호로 시작하는 쿼리 "(SELECT ..." 를 위해 여는 괄호도 기록한다
            if ch == '(' && words.is_empty() {
                words.push("(".into());
            }
        }
        words.len() < limit
    });
    if !cur.is_empty() && words.len() < limit {
        words.push(cur);
    }
    words
}

// ─────────────────────────────────────────────────────────────
// 판정
// ─────────────────────────────────────────────────────────────

const PLSQL_UNITS: &[&str] = &[
    "PROCEDURE",
    "FUNCTION",
    "PACKAGE",
    "TRIGGER",
    "TYPE",
    "LIBRARY",
    "JAVA",
];

/// 서버로 가지 않는 SQL*Plus 명령. (SET 은 따로 본다)
const SQLPLUS_CMDS: &[&str] = &[
    "PROMPT", "REM", "REMARK", "SPOOL", "SHOW", "DEFINE", "UNDEFINE", "PAUSE", "WHENEVER",
    "COLUMN", "COL", "TTITLE", "BTITLE", "BREAK", "COMPUTE", "CLEAR", "CONNECT", "CONN",
    "DISCONNECT", "EXIT", "QUIT", "HOST", "VARIABLE", "VAR", "PRINT", "ACCEPT",
];

fn kind_from_words(w: &[String]) -> StmtKind {
    let first = match w.first() {
        Some(f) => f.as_str(),
        None => return StmtKind::Other,
    };
    match first {
        "SELECT" | "WITH" | "(" => StmtKind::Query,
        "INSERT" | "UPDATE" | "DELETE" | "MERGE" | "LOCK" => StmtKind::Dml,
        "BEGIN" | "DECLARE" | "CALL" | "EXEC" | "EXECUTE" => StmtKind::Plsql,
        "COMMIT" | "ROLLBACK" | "SAVEPOINT" => StmtKind::Tcl,
        "EXPLAIN" => StmtKind::Explain,
        "SET" => match w.get(1).map(String::as_str) {
            Some("TRANSACTION") => StmtKind::Tcl,
            Some("ROLE") | Some("CONSTRAINT") | Some("CONSTRAINTS") => StmtKind::Session,
            _ => StmtKind::SqlPlus,
        },
        "ALTER" if w.get(1).map(String::as_str) == Some("SESSION") => StmtKind::Session,
        "CREATE" => {
            // CREATE [OR REPLACE] [EDITIONABLE|NONEDITIONABLE] <unit>
            let unit = w
                .iter()
                .skip(1)
                .map(String::as_str)
                .find(|x| !matches!(*x, "OR" | "REPLACE" | "EDITIONABLE" | "NONEDITIONABLE"));
            match unit {
                Some(u) if PLSQL_UNITS.contains(&u) => StmtKind::PlsqlUnit,
                _ => StmtKind::Ddl,
            }
        }
        "ALTER" | "DROP" | "TRUNCATE" | "RENAME" | "COMMENT" | "GRANT" | "REVOKE" | "ANALYZE"
        | "AUDIT" | "NOAUDIT" | "PURGE" | "FLASHBACK" => StmtKind::Ddl,
        f if SQLPLUS_CMDS.contains(&f) => StmtKind::SqlPlus,
        _ => StmtKind::Other,
    }
}

/// 문장 하나의 종류.
pub fn classify(stmt: &str) -> StmtKind {
    let t = stmt.trim_start();
    if t.starts_with('@') {
        return StmtKind::SqlPlus;
    }
    kind_from_words(&leading_words(stmt, 6))
}

/// 읽기 전용으로 실행해도 되는 문장인지.
///
/// SELECT/WITH 중 `FOR UPDATE` 가 없는 것만 참이다. 이것은 1차 관문일 뿐이고,
/// MCP 경로는 여기에 더해 `SET TRANSACTION READ ONLY` 로 서버에서도 막는다.
pub fn is_read_only(stmt: &str) -> bool {
    if classify(stmt) != StmtKind::Query {
        return false;
    }
    // 코드 영역의 단어 열에서 FOR UPDATE 를 찾는다
    let mut prev_for = false;
    let mut found = false;
    let mut cur = String::new();
    let check = |w: &str, prev_for: &mut bool, found: &mut bool| {
        if *prev_for && w == "UPDATE" {
            *found = true;
        }
        *prev_for = w == "FOR";
    };
    scan_code(stmt, |_, ch| {
        if is_ident_char(ch) {
            cur.extend(ch.to_uppercase());
        } else if !cur.is_empty() {
            let w = std::mem::take(&mut cur);
            check(&w, &mut prev_for, &mut found);
        }
        !found
    });
    if !cur.is_empty() {
        check(&cur, &mut prev_for, &mut found);
    }
    !found
}

/// 실행 전에 사람에게 확인을 받아야 하는 문장이면 그 이유.
///
/// - WHERE 없는 UPDATE / DELETE (전체 행)
/// - DROP / TRUNCATE (되돌릴 수 없음, 암묵적 커밋)
/// - ALTER SYSTEM, SHUTDOWN 류
pub fn needs_confirmation(stmt: &str) -> Option<&'static str> {
    let words = code_words(stmt);
    let first = words.first().map(String::as_str)?;
    match first {
        "UPDATE" | "DELETE" if !words.iter().any(|w| w == "WHERE") => {
            Some("WHERE 절이 없습니다. 테이블의 모든 행이 바뀝니다.")
        }
        "DROP" => Some("DROP 은 되돌릴 수 없고, 진행 중인 트랜잭션을 커밋합니다."),
        "TRUNCATE" => Some("TRUNCATE 는 되돌릴 수 없고, 진행 중인 트랜잭션을 커밋합니다."),
        "ALTER" if words.get(1).map(String::as_str) == Some("SYSTEM") => {
            Some("ALTER SYSTEM 은 인스턴스 전체에 영향을 줍니다.")
        }
        "SHUTDOWN" | "STARTUP" => Some("인스턴스를 멈추거나 띄웁니다."),
        _ => None,
    }
}

/// 코드 영역의 모든 단어 (대문자)
fn code_words(src: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut cur = String::new();
    scan_code(src, |_, ch| {
        if is_ident_char(ch) {
            cur.extend(ch.to_uppercase());
        } else if !cur.is_empty() {
            words.push(std::mem::take(&mut cur));
        }
        true
    });
    if !cur.is_empty() {
        words.push(cur);
    }
    words
}

/// 바인드 변수 이름 (등장 순서, 중복 제거, 대문자).
///
/// `:=` 대입, PL/SQL 단위 안의 `:NEW`/`:OLD` 는 바인드가 아니다.
pub fn bind_names(stmt: &str) -> Vec<String> {
    if classify(stmt) == StmtKind::PlsqlUnit {
        return Vec::new();
    }
    let mut names: Vec<String> = Vec::new();
    let mut in_bind = false;
    let mut cur = String::new();
    let mut prev_colon = false;
    let mut prev_ident = false; // 직전 문자가 식별자 문자였는지
    let mut colon_after_ident = false; // 직전 콜론이 식별자 바로 뒤였는지
    let flush = |cur: &mut String, names: &mut Vec<String>| {
        if !cur.is_empty() {
            let up = cur.to_uppercase();
            if !names.contains(&up) {
                names.push(up);
            }
            cur.clear();
        }
    };
    scan_code(stmt, |_, ch| {
        if in_bind {
            if is_ident_char(ch) {
                cur.push(ch);
                return true;
            }
            in_bind = false;
            flush(&mut cur, &mut names);
        }
        let ident = is_ident_char(ch);
        if prev_colon && ident && !colon_after_ident {
            in_bind = true;
            cur.push(ch);
            prev_colon = false;
            prev_ident = true;
            return true;
        }
        if ch == ':' {
            colon_after_ident = prev_ident;
        }
        prev_colon = ch == ':';
        prev_ident = ident;
        true
    });
    if in_bind {
        flush(&mut cur, &mut names);
    }
    names
}

// ─────────────────────────────────────────────────────────────
// 스크립트 분리
// ─────────────────────────────────────────────────────────────

/// 스크립트를 문장 단위로 자른다 (Toad 의 F5 "스크립트 실행").
pub fn split_script(src: &str) -> Vec<Statement> {
    let mut out = Vec::new();
    let mut start: Option<usize> = None; // 현재 문장 시작 (공백 제외)
    let mut kind: Option<StmtKind> = None; // 시작 직후 판정
    // 한 줄 전체가 "/" 인지 (앞뒤 공백 허용)
    let slash_line_at = |pos: usize| -> Option<usize> {
        let line_start = src[..pos].rfind('\n').map(|p| p + 1).unwrap_or(0);
        let line_end = src[pos..].find('\n').map(|p| pos + p).unwrap_or(src.len());
        if src[line_start..line_end].trim() == "/" {
            Some(line_end)
        } else {
            None
        }
    };

    let push = |out: &mut Vec<Statement>, s: usize, e: usize, k: StmtKind| {
        let raw = &src[s..e];
        let text = finish_text(raw, k);
        if text.trim().is_empty() {
            return;
        }
        out.push(Statement {
            text,
            kind: k,
            start: s,
            end: e,
            line: src[..s].matches('\n').count() + 1,
        });
    };

    // 한 줄짜리 SQL*Plus 명령은 줄 끝에서 끝난다
    let mut skip_to: usize = 0;
    let mut first_code_in_stmt = true;

    scan_code(src, |pos, ch| {
        if pos < skip_to {
            return true;
        }
        if start.is_none() {
            if ch.is_whitespace() || ch == ';' {
                return true;
            }
            if ch == '/' {
                if let Some(end) = slash_line_at(pos) {
                    skip_to = end; // 빈 "/" 줄
                    return true;
                }
            }
            start = Some(pos);
            first_code_in_stmt = true;
        }
        let s = start.unwrap();
        if first_code_in_stmt {
            first_code_in_stmt = false;
            let k = classify(&src[s..]);
            kind = Some(k);
            if k == StmtKind::SqlPlus {
                let line_end = src[s..].find('\n').map(|p| s + p).unwrap_or(src.len());
                push(&mut out, s, line_end, k);
                start = None;
                skip_to = line_end;
                return true;
            }
        }
        let k = kind.unwrap_or(StmtKind::Other);
        let block = matches!(k, StmtKind::PlsqlUnit)
            || (k == StmtKind::Plsql && !starts_with_exec(&src[s..]));
        if ch == '/' {
            if let Some(end) = slash_line_at(pos) {
                push(&mut out, s, pos, k);
                start = None;
                skip_to = end;
                return true;
            }
        }
        if ch == ';' && !block {
            push(&mut out, s, pos + 1, k);
            start = None;
        }
        true
    });
    if let Some(s) = start {
        let k = kind.unwrap_or_else(|| classify(&src[s..]));
        push(&mut out, s, src.len(), k);
    }
    out
}

fn starts_with_exec(s: &str) -> bool {
    matches!(
        leading_words(s, 1).first().map(String::as_str),
        Some("EXEC") | Some("EXECUTE")
    )
}

/// 서버로 보낼 형태로 다듬는다.
fn finish_text(raw: &str, kind: StmtKind) -> String {
    let t = raw.trim();
    match kind {
        // PL/SQL 은 마지막 "END;" 의 ; 가 문법의 일부다
        StmtKind::PlsqlUnit => t.to_string(),
        StmtKind::Plsql => {
            if starts_with_exec(t) {
                // EXEC proc(1)  →  BEGIN proc(1); END;
                let body = t
                    .split_once(char::is_whitespace)
                    .map(|(_, r)| r)
                    .unwrap_or("")
                    .trim()
                    .trim_end_matches(';')
                    .trim();
                format!("BEGIN {body}; END;")
            } else {
                t.to_string()
            }
        }
        _ => t.trim_end_matches(';').trim_end().to_string(),
    }
}

/// 커서 위치의 문장 (Toad 의 Ctrl+Enter / F9).
pub fn statement_at(src: &str, cursor: usize) -> Option<Statement> {
    let stmts = split_script(src);
    // 커서가 문장 안이거나, 문장 끝 바로 뒤(같은 줄)에 있으면 그 문장
    stmts
        .iter()
        .find(|s| cursor >= s.start && cursor <= s.end)
        .or_else(|| stmts.iter().rev().find(|s| s.end <= cursor))
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_basic() {
        assert_eq!(classify("select 1 from dual"), StmtKind::Query);
        assert_eq!(classify("  /* c */ WITH a AS (select 1 from dual) select * from a"), StmtKind::Query);
        assert_eq!(classify("(select 1 from dual)"), StmtKind::Query);
        assert_eq!(classify("update t set a=1"), StmtKind::Dml);
        assert_eq!(classify("create table t (a number)"), StmtKind::Ddl);
        assert_eq!(classify("create or replace package body p is end;"), StmtKind::PlsqlUnit);
        assert_eq!(classify("CREATE OR REPLACE EDITIONABLE TRIGGER trg"), StmtKind::PlsqlUnit);
        assert_eq!(classify("alter session set nls_date_format='YYYY'"), StmtKind::Session);
        assert_eq!(classify("set serveroutput on"), StmtKind::SqlPlus);
        assert_eq!(classify("set transaction read only"), StmtKind::Tcl);
        assert_eq!(classify("-- 주석\nselect 1 from dual"), StmtKind::Query);
    }

    #[test]
    fn read_only_guard() {
        assert!(is_read_only("select * from emp"));
        assert!(!is_read_only("select * from emp for update"));
        assert!(!is_read_only("select * from emp FOR\n  UPDATE nowait"));
        // 문자열·주석 속 FOR UPDATE 는 상관없다
        assert!(is_read_only("select 'for update' from dual -- for update"));
        assert!(!is_read_only("delete from emp"));
        assert!(!is_read_only("begin delete from emp; end;"));
    }

    #[test]
    fn confirmation() {
        assert!(needs_confirmation("delete from emp").is_some());
        assert!(needs_confirmation("update emp set sal = 0").is_some());
        assert!(needs_confirmation("delete from emp where empno = 1").is_none());
        // 문자열·주석 속 WHERE 는 WHERE 가 아니다
        assert!(needs_confirmation("update emp set note = 'where' -- where").is_some());
        assert!(needs_confirmation("drop table emp").is_some());
        assert!(needs_confirmation("truncate table emp").is_some());
        assert!(needs_confirmation("select * from emp").is_none());
        assert!(needs_confirmation("insert into emp select * from emp2").is_none());
    }

    #[test]
    fn binds() {
        assert_eq!(
            bind_names("select * from emp where deptno = :dno and ename = :Name and x = :dno"),
            vec!["DNO", "NAME"]
        );
        // 문자열 안, 대입 연산자, 시각 리터럴은 바인드가 아니다
        assert!(bind_names("select to_date('12:30','HH24:MI') from dual").is_empty());
        assert_eq!(bind_names("begin x := :a; end;"), vec!["A"]);
        // 트리거의 :new/:old 는 바인드가 아니다
        assert!(bind_names("create trigger t before insert on e for each row begin :new.a := 1; end;").is_empty());
        assert_eq!(bind_names("select :1, :2 from dual"), vec!["1", "2"]);
        // 식별자 바로 뒤의 콜론은 바인드가 아니다
        assert!(bind_names("select a:b from dual").is_empty());
    }

    #[test]
    fn split_mixed_script() {
        let src = "\
select 1 from dual;
-- 주석 ; 은 무시
insert into t values ('a;b');
create or replace procedure p is
begin
  null;
end;
/
begin
  p;
end;
/
prompt done
select q'[x;y]' from dual
";
        let s = split_script(src);
        let kinds: Vec<_> = s.iter().map(|x| x.kind).collect();
        assert_eq!(
            kinds,
            vec![
                StmtKind::Query,
                StmtKind::Dml,
                StmtKind::PlsqlUnit,
                StmtKind::Plsql,
                StmtKind::SqlPlus,
                StmtKind::Query
            ]
        );
        assert_eq!(s[0].text, "select 1 from dual");
        assert_eq!(s[1].text, "insert into t values ('a;b')");
        assert!(s[2].text.ends_with("end;"));
        assert_eq!(s[5].text, "select q'[x;y]' from dual");
        assert_eq!(s[2].line, 4);
    }

    #[test]
    fn exec_becomes_block() {
        let s = split_script("exec dbms_stats.gather_table_stats('SCOTT','EMP');\nselect 1 from dual;");
        assert_eq!(s[0].text, "BEGIN dbms_stats.gather_table_stats('SCOTT','EMP'); END;");
        assert_eq!(s[1].kind, StmtKind::Query);
    }

    #[test]
    fn slash_terminates_plain_sql() {
        let s = split_script("select 1 from dual\n/\nselect 2 from dual\n/\n");
        assert_eq!(s.len(), 2);
        assert_eq!(s[1].text, "select 2 from dual");
    }

    #[test]
    fn division_is_not_terminator() {
        let s = split_script("select 4 / 2 from dual;");
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].text, "select 4 / 2 from dual");
    }

    #[test]
    fn cursor_statement() {
        let src = "select 1 from dual;\n\nselect 2 from dual;\n";
        let at = src.find("2").unwrap();
        assert_eq!(statement_at(src, at).unwrap().text, "select 2 from dual");
        assert_eq!(statement_at(src, 3).unwrap().text, "select 1 from dual");
    }

    #[test]
    fn unterminated_string_does_not_panic() {
        let _ = split_script("select 'abc from dual");
        let _ = split_script("select q'[abc from dual");
        let _ = split_script("/* never closed");
        let _ = bind_names(":");
    }
}
