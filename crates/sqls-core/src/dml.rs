//! UPDATE·DELETE 를 실행하기 전에: 몇 행이 바뀌는지 보고(미리보기), 바뀌기 전 값을 잡아 되돌리는 스크립트를 만든다.
//!
//! - 문장을 가볍게 읽어 대상 테이블·별칭·SET 컬럼·WHERE 만 뽑는다. 모르는 모양이면(서브쿼리 대상, DB 링크 …) None — 아무것도 하지 않는다.
//! - 값은 형식마다 정확한 글자로 바꿔 받는다 (날짜는 TO_CHAR, RAW 는 RAWTOHEX). 되살릴 수 없는 형식(BLOB·LONG 등)이면 스크립트를 만들지 않는다.
//! - 잡는 행 수에 상한을 둔다 — 큰 문장을 두 번 도는 일이 없게 ROWNUM 으로 끊는다.

use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Update,
    Delete,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Target {
    pub kind: Kind,
    /// 적힌 그대로 (SCHEMA.TABLE 일 수 있다)
    pub table: String,
    pub alias: Option<String>,
    /// UPDATE 의 SET 컬럼 (별칭 뗀 이름, 대문자)
    pub set_cols: Vec<String>,
    /// WHERE 뒤 (없으면 모든 행)
    pub where_sql: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Word(String),
    Quoted(String),
    Str,
    Bind(String),
    Punct(char),
}

/// (토큰, 시작, 끝) — 문자열·주석·따옴표 이름을 안다
fn lex(sql: &str) -> Vec<(Tok, usize, usize)> {
    let b: Vec<char> = sql.chars().collect();
    // 글자 위치 → 바이트 위치
    let mut byte = Vec::with_capacity(b.len() + 1);
    let mut acc = 0;
    for c in &b {
        byte.push(acc);
        acc += c.len_utf8();
    }
    byte.push(acc);
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c.is_whitespace() {
            i += 1;
        } else if c == '-' && b.get(i + 1) == Some(&'-') {
            while i < b.len() && b[i] != '\n' {
                i += 1;
            }
        } else if c == '/' && b.get(i + 1) == Some(&'*') {
            i += 2;
            while i + 1 < b.len() && !(b[i] == '*' && b[i + 1] == '/') {
                i += 1;
            }
            i += 2;
        } else if c == '\'' || ((c == 'q' || c == 'Q') && b.get(i + 1) == Some(&'\'')) {
            let s = i;
            if c != '\'' {
                // q'[...]'
                let open = b.get(i + 2).copied().unwrap_or('[');
                let close = match open { '[' => ']', '(' => ')', '{' => '}', '<' => '>', x => x };
                i += 3;
                while i + 1 < b.len() && !(b[i] == close && b[i + 1] == '\'') {
                    i += 1;
                }
                i += 2;
            } else {
                i += 1;
                while i < b.len() {
                    if b[i] == '\'' {
                        if b.get(i + 1) == Some(&'\'') { i += 2; continue; }
                        break;
                    }
                    i += 1;
                }
                i += 1;
            }
            out.push((Tok::Str, byte[s], byte[i.min(b.len())]));
        } else if c == '"' {
            let s = i;
            i += 1;
            while i < b.len() && b[i] != '"' {
                i += 1;
            }
            let name: String = b[s + 1..i.min(b.len())].iter().collect();
            i += 1;
            out.push((Tok::Quoted(name), byte[s], byte[i.min(b.len())]));
        } else if c == ':' && b.get(i + 1).is_some_and(|x| x.is_alphanumeric() || *x == '_') {
            let s = i;
            i += 1;
            while i < b.len() && (b[i].is_alphanumeric() || b[i] == '_') {
                i += 1;
            }
            out.push((Tok::Bind(b[s + 1..i].iter().collect::<String>().to_uppercase()), byte[s], byte[i]));
        } else if c.is_alphanumeric() || c == '_' || c == '$' || c == '#' {
            let s = i;
            while i < b.len() && (b[i].is_alphanumeric() || matches!(b[i], '_' | '$' | '#')) {
                i += 1;
            }
            out.push((Tok::Word(b[s..i].iter().collect::<String>().to_uppercase()), byte[s], byte[i]));
        } else {
            out.push((Tok::Punct(c), byte[i], byte[i + 1]));
            i += 1;
        }
    }
    out
}

fn ident(t: &Tok) -> Option<String> {
    match t {
        Tok::Word(w) => Some(w.clone()),
        Tok::Quoted(q) => Some(format!("\"{q}\"")),
        _ => None,
    }
}

const STOP: &[&str] = &["WHERE", "SET", "RETURNING", "RETURN", "LOG", "PARTITION", "SUBPARTITION"];

/// UPDATE·DELETE 문을 읽는다. 다른 문장이거나 모르는 모양이면 None
pub fn parse(sql: &str) -> Option<Target> {
    let toks = lex(sql);
    let mut i = 0;
    let word = |i: usize| match toks.get(i) { Some((Tok::Word(w), _, _)) => Some(w.as_str()), _ => None };
    let kind = match word(0)? {
        "UPDATE" => Kind::Update,
        "DELETE" => Kind::Delete,
        _ => return None,
    };
    i += 1;
    if kind == Kind::Delete && word(i) == Some("FROM") {
        i += 1;
    }
    // ONLY (t) 이나 (SELECT …) 대상은 모른다
    let mut table = ident(&toks.get(i)?.0)?;
    i += 1;
    if matches!(toks.get(i), Some((Tok::Punct('.'), _, _))) {
        table = format!("{table}.{}", ident(&toks.get(i + 1)?.0)?);
        i += 2;
    }
    if matches!(toks.get(i), Some((Tok::Punct('@'), _, _))) {
        return None; // DB 링크
    }
    let mut alias = None;
    if let Some((t, _, _)) = toks.get(i) {
        if let Some(a) = ident(t) {
            if !STOP.contains(&a.as_str()) {
                alias = Some(a);
                i += 1;
            }
        }
    }
    let mut set_cols = Vec::new();
    if kind == Kind::Update {
        if word(i) != Some("SET") {
            return None;
        }
        i += 1;
        // SET 목록: 깊이 0 의 "이름 =" 또는 "(이름, 이름) ="
        let mut depth = 0i32;
        let mut expect_col = true;
        while i < toks.len() {
            match &toks[i].0 {
                Tok::Punct('(') => {
                    if depth == 0 && expect_col {
                        // (a, b) = (서브쿼리)
                        let mut j = i + 1;
                        while j < toks.len() && !matches!(toks[j].0, Tok::Punct(')')) {
                            if let Some(n) = ident(&toks[j].0) {
                                if !matches!(toks.get(j + 1), Some((Tok::Punct('.'), _, _))) {
                                    set_cols.push(n);
                                }
                            }
                            j += 1;
                        }
                        i = j;
                        expect_col = false;
                    } else {
                        depth += 1;
                    }
                }
                Tok::Punct(')') => depth -= 1,
                Tok::Punct(',') if depth == 0 => expect_col = true,
                Tok::Word(w) if depth == 0 && (w == "WHERE" || w == "RETURNING" || w == "RETURN" || w == "LOG") => break,
                t if depth == 0 && expect_col => {
                    if let Some(n) = ident(t) {
                        // 별칭.컬럼 → 컬럼
                        if matches!(toks.get(i + 1), Some((Tok::Punct('.'), _, _))) {
                            i += 2;
                            set_cols.push(ident(&toks.get(i)?.0)?);
                        } else {
                            set_cols.push(n);
                        }
                        expect_col = false;
                    }
                }
                _ => {}
            }
            i += 1;
        }
        if set_cols.is_empty() {
            return None;
        }
    }
    // WHERE … (깊이 0 의 RETURNING·LOG ERRORS·; 앞까지)
    let mut where_sql = None;
    let mut depth = 0i32;
    let mut start = None;
    let mut end = sql.trim_end().trim_end_matches(';').len();
    while i < toks.len() {
        match &toks[i].0 {
            Tok::Punct('(') => depth += 1,
            Tok::Punct(')') => depth -= 1,
            Tok::Word(w) if depth == 0 && w == "WHERE" && start.is_none() => start = Some(toks[i].2),
            Tok::Word(w) if depth == 0 && start.is_some() && (w == "RETURNING" || w == "RETURN" || w == "LOG") => {
                end = toks[i].1;
                break;
            }
            Tok::Punct(';') if depth == 0 => {
                end = toks[i].1;
                break;
            }
            _ => {}
        }
        i += 1;
    }
    if let Some(s) = start {
        let w = sql[s..end.max(s)].trim().to_string();
        if !w.is_empty() {
            where_sql = Some(w);
        }
    }
    Some(Target { kind, table, alias, set_cols, where_sql })
}

/// 글에 나오는 바인드 이름 (:X) — 문자열 안의 것은 빼고, 대문자
pub fn binds_in(sql: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for (t, _, _) in lex(sql) {
        if let Tok::Bind(b) = t {
            if !out.contains(&b) {
                out.push(b);
            }
        }
    }
    out
}

impl Target {
    fn from_where(&self) -> String {
        let alias = self.alias.as_deref().map(|a| format!(" {a}")).unwrap_or_default();
        match &self.where_sql {
            Some(w) => format!("{}{alias} WHERE ({w})", self.table),
            None => format!("{}{alias} WHERE 1 = 1", self.table),
        }
    }

    /// 바뀔 행 수
    pub fn count_sql(&self) -> String {
        format!("SELECT COUNT(*) FROM {}", self.from_where())
    }

    /// 바뀔 행 몇 개 (미리보기)
    pub fn sample_sql(&self, n: usize) -> String {
        let star = self.alias.as_deref().map(|a| format!("{a}.*")).unwrap_or_else(|| "t.*".into());
        if self.alias.is_some() {
            format!("SELECT {star} FROM {} AND ROWNUM <= {n}", self.from_where())
        } else {
            // 별칭이 없으면 SELECT * 로 (WHERE 가 테이블 이름으로 컬럼을 가리킬 수 있다)
            format!("SELECT * FROM {} AND ROWNUM <= {n}", self.from_where())
        }
    }
}

/// 되살릴 값으로 바꿔 받을 식, 다시 쓸 때의 감싸기 — 형식이 되살릴 수 없으면 None
pub fn value_expr(col: &str, data_type: &str) -> Option<(String, &'static str)> {
    let t = data_type.to_uppercase();
    let q = col; // 이미 따옴표 처리된 이름
    Some(if t.starts_with("DATE") {
        (format!("TO_CHAR({q}, 'YYYY-MM-DD HH24:MI:SS')"), "DATE")
    } else if t.starts_with("TIMESTAMP") && t.contains("TIME ZONE") {
        (format!("TO_CHAR({q}, 'YYYY-MM-DD HH24:MI:SS.FF9 TZH:TZM')"), "TSTZ")
    } else if t.starts_with("TIMESTAMP") {
        (format!("TO_CHAR({q}, 'YYYY-MM-DD HH24:MI:SS.FF9')"), "TS")
    } else if t.starts_with("NUMBER") || t.starts_with("FLOAT") || t.starts_with("INTEGER") || t.starts_with("BINARY_") {
        (format!("TO_CHAR({q}, 'TM9', 'NLS_NUMERIC_CHARACTERS=''.,''')"), "NUM")
    } else if t.starts_with("RAW") {
        (format!("RAWTOHEX({q})"), "RAW")
    } else if t.starts_with("NVARCHAR2") || t.starts_with("NCHAR") {
        (q.to_string(), "NSTR")
    } else if t.starts_with("VARCHAR2") || t.starts_with("CHAR") || t.starts_with("VARCHAR") {
        (q.to_string(), "STR")
    } else if t == "CLOB" || t == "NCLOB" {
        // 4000 자까지만 글로 다시 쓸 수 있다 — 넘으면 스크립트를 만들지 않는다
        (format!("CASE WHEN DBMS_LOB.GETLENGTH({q}) <= 3900 THEN DBMS_LOB.SUBSTR({q}, 3900, 1) ELSE '\u{1}TOO_LONG' END"), if t == "NCLOB" { "NCLOB" } else { "CLOB" })
    } else {
        return None; // BLOB·LONG·LONG RAW·BFILE·XMLTYPE·객체 형식 …
    })
}

/// 받은 글자 → SQL 값
pub fn literal(v: Option<&str>, how: &str) -> Result<String, String> {
    let Some(v) = v else { return Ok("NULL".into()) };
    let esc = |s: &str| s.replace('\'', "''");
    Ok(match how {
        "DATE" => format!("TO_DATE('{v}', 'YYYY-MM-DD HH24:MI:SS')"),
        "TS" => format!("TO_TIMESTAMP('{v}', 'YYYY-MM-DD HH24:MI:SS.FF9')"),
        "TSTZ" => format!("TO_TIMESTAMP_TZ('{v}', 'YYYY-MM-DD HH24:MI:SS.FF9 TZH:TZM')"),
        "NUM" => v.to_string(),
        "RAW" => format!("HEXTORAW('{v}')"),
        "NSTR" | "NCLOB" => format!("N'{}'", esc(v)),
        "CLOB" if v == "\u{1}TOO_LONG" => return Err("CLOB 값이 4000자를 넘습니다".into()),
        _ if v == "\u{1}TOO_LONG" => return Err("NCLOB 값이 4000자를 넘습니다".into()),
        _ => format!("'{}'", esc(v)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_update_and_delete() {
        let t = parse("update app.orders o set o.status = 'X', amt = amt * 1.1, (a, b) = (select 1, 2 from dual) where o.id = :id and note <> 'where' returning id into :x").unwrap();
        assert_eq!(t.kind, Kind::Update);
        assert_eq!(t.table, "APP.ORDERS");
        assert_eq!(t.alias.as_deref(), Some("O"));
        assert_eq!(t.set_cols, vec!["STATUS", "AMT", "A", "B"]);
        assert_eq!(t.where_sql.as_deref(), Some("o.id = :id and note <> 'where'"));
        assert_eq!(binds_in(t.where_sql.as_deref().unwrap()), vec!["ID"]);
        let d = parse("DELETE FROM orders WHERE created < SYSDATE - 30;").unwrap();
        assert_eq!((d.kind, d.table.as_str(), d.alias.as_deref()), (Kind::Delete, "ORDERS", None));
        assert_eq!(d.where_sql.as_deref(), Some("created < SYSDATE - 30"));
        let d = parse("delete orders").unwrap();
        assert_eq!(d.where_sql, None);
        assert_eq!(d.count_sql(), "SELECT COUNT(*) FROM ORDERS WHERE 1 = 1");
        assert!(parse("update (select * from t) set a = 1").is_none());
        assert!(parse("delete from t@remote where 1=1").is_none());
        assert!(parse("insert into t values (1)").is_none());
        let t = parse("UPDATE t SET a = (SELECT MAX(x) FROM y WHERE y.k = t.k) WHERE id IN (SELECT id FROM z WHERE q = 1)").unwrap();
        assert_eq!(t.set_cols, vec!["A"]);
        assert_eq!(t.where_sql.as_deref(), Some("id IN (SELECT id FROM z WHERE q = 1)"));
        assert_eq!(t.sample_sql(20), "SELECT * FROM T WHERE (id IN (SELECT id FROM z WHERE q = 1)) AND ROWNUM <= 20");
    }

    #[test]
    fn literals() {
        assert_eq!(literal(Some("O'Neil"), "STR").unwrap(), "'O''Neil'");
        assert_eq!(literal(None, "DATE").unwrap(), "NULL");
        assert_eq!(literal(Some("2026-10-01 13:00:00"), "DATE").unwrap(), "TO_DATE('2026-10-01 13:00:00', 'YYYY-MM-DD HH24:MI:SS')");
        assert_eq!(literal(Some("-12.5"), "NUM").unwrap(), "-12.5");
        assert!(literal(Some("\u{1}TOO_LONG"), "CLOB").is_err());
        assert!(value_expr("PHOTO", "BLOB").is_none());
        assert_eq!(value_expr("D", "DATE").unwrap().1, "DATE");
    }
}
