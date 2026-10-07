//! 결과 그리드 편집 — 단일 테이블 SELECT 에 ROWID 를 붙여 다시 조회하고, 고친 셀을 UPDATE/DELETE 문으로 만든다.
//!
//! 문장은 화면에 보여 주고 사람이 확인한 뒤 실행한다. 값은 전부 바인드 변수로 넣는다 (SQL 에 값을 이어 붙이지 않는다).
//! 날짜·타임스탬프는 그리드에 나온 글자 형식 그대로 `TO_DATE`/`TO_TIMESTAMP` 로 되돌린다.

use serde::{Deserialize, Serialize};

use crate::sql;

/// ROWID 를 받는 열 이름 (결과 첫 열)
pub const ROWID_COL: &str = "SQLS_ROWID";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Editable {
    /// ROWID 를 붙인 SELECT
    pub sql: String,
    /// 적힌 그대로의 테이블 이름 (대문자, 스키마 포함 가능)
    pub table: String,
}

fn words(stmt: &str) -> Vec<String> {
    // 주석·문자열을 뺀 대문자 토큰 (단순)
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut code = String::new();
    sql::code_only(stmt, &mut code);
    for c in code.chars() {
        if c.is_alphanumeric() || matches!(c, '_' | '$' | '#' | '.' | '"') {
            cur.push(c);
        } else {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur).to_uppercase());
            }
            if !c.is_whitespace() {
                out.push(c.to_string());
            }
        }
    }
    if !cur.is_empty() {
        out.push(cur.to_uppercase());
    }
    out
}

/// 편집할 수 있는 SELECT 인지 보고, ROWID 를 붙인 문장을 만든다. 안 되면 이유.
///
/// 조건: SELECT 하나, FROM 에 테이블 하나 (조인·쉼표·서브쿼리 없음), DISTINCT·GROUP BY·집합 연산·CONNECT BY 없음.
pub fn editable(stmt: &str) -> Result<Editable, String> {
    let text = stmt.trim().trim_end_matches(';').trim();
    let w = words(text);
    if w.first().map(|s| s.as_str()) != Some("SELECT") {
        return Err("편집은 SELECT 결과에서만 됩니다".into());
    }
    for bad in ["DISTINCT", "UNIQUE", "GROUP", "UNION", "INTERSECT", "MINUS", "CONNECT", "JOIN", "PIVOT", "MODEL", "WITH"] {
        if w.iter().any(|x| x == bad) {
            return Err(format!("{bad} 가 있는 조회는 편집할 수 없습니다 — 테이블 하나를 그대로 조회하세요"));
        }
    }
    // 바깥 FROM (괄호 깊이 0)
    let mut depth = 0i32;
    let mut from = None;
    for (i, t) in w.iter().enumerate() {
        match t.as_str() {
            "(" => depth += 1,
            ")" => depth -= 1,
            "FROM" if depth == 0 => {
                from = Some(i);
                break;
            }
            _ => {}
        }
    }
    let from = from.ok_or("FROM 을 찾지 못했습니다")?;
    if w[1..from].iter().any(|t| t == "SELECT") {
        return Err("SELECT 목록에 서브쿼리가 있으면 편집할 수 없습니다".into());
    }
    let table = w.get(from + 1).cloned().ok_or("테이블 이름이 없습니다")?;
    if table == "(" || table == "DUAL" || table.contains('@') {
        return Err("테이블 하나를 직접 조회할 때만 편집할 수 있습니다 (인라인 뷰·DUAL·DB 링크 제외)".into());
    }
    // 별칭, 그다음은 끝 또는 WHERE/ORDER/FOR
    let mut k = from + 2;
    let mut alias = None;
    if let Some(a) = w.get(k) {
        if !matches!(a.as_str(), "WHERE" | "ORDER" | "FOR" | "SAMPLE" | "PARTITION") {
            if a == "," {
                return Err("FROM 에 테이블이 여럿이면 편집할 수 없습니다".into());
            }
            alias = Some(a.clone());
            k += 1;
        }
    }
    if let Some(n) = w.get(k) {
        if n == "," || !matches!(n.as_str(), "WHERE" | "ORDER" | "FOR" | "SAMPLE" | "PARTITION") {
            return Err("FROM 에 테이블이 여럿이면 편집할 수 없습니다".into());
        }
    }
    let q = alias.clone().unwrap_or_else(|| table.rsplit('.').next().unwrap_or(&table).to_string());
    // 원문에서 SELECT 다음을 바꾼다: "*" 는 "q.*" 로 (ROWID, * 는 문법 오류)
    let body = text[6..].trim_start();
    let rest = if let Some(r) = body.strip_prefix('*') { format!("{q}.*{r}") } else { body.to_string() };
    Ok(Editable { sql: format!("SELECT ROWIDTOCHAR({q}.ROWID) AS {ROWID_COL}, {rest}"), table })
}

/// 바꿀 열 하나
#[derive(Debug, Clone, Deserialize)]
pub struct CellEdit {
    pub column: String,
    /// 그리드의 형식 이름 (NUMBER, DATE, TIMESTAMP(6), VARCHAR2(30) …)
    pub type_name: String,
    /// None = NULL
    pub value: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RowEdit {
    pub rowid: String,
    pub cells: Vec<CellEdit>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Change {
    pub sql: String,
    pub binds: Vec<(String, Option<String>)>,
    /// 화면에 보일 문장 (값을 채워 넣은 것 — 읽기용, 실행하지 않는다)
    pub preview: String,
}

fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

fn quote_table(t: &str) -> String {
    t.split('.')
        .map(|p| if p.starts_with('"') { p.to_string() } else { quote_ident(p) })
        .collect::<Vec<_>>()
        .join(".")
}

/// 그리드 글자 → 열 형식에 맞는 식
fn value_expr(type_name: &str, bind: &str) -> String {
    let t = type_name.to_uppercase();
    if t.starts_with("DATE") {
        format!("TO_DATE(:{bind}, 'YYYY-MM-DD HH24:MI:SS')")
    } else if t.starts_with("TIMESTAMP") && t.contains("TIME ZONE") {
        format!("TO_TIMESTAMP_TZ(:{bind}, 'YYYY-MM-DD HH24:MI:SS.FF TZH:TZM')")
    } else if t.starts_with("TIMESTAMP") {
        format!("TO_TIMESTAMP(:{bind}, 'YYYY-MM-DD HH24:MI:SS.FF')")
    } else if t.starts_with("NUMBER") || t.starts_with("FLOAT") || t.starts_with("BINARY_") || t.starts_with("INTEGER") {
        format!("TO_NUMBER(:{bind})")
    } else {
        format!(":{bind}")
    }
}

/// 편집할 수 없는 형식 (그리드에 실제 값이 아닌 것이 나온다)
pub fn editable_type(type_name: &str) -> bool {
    let t = type_name.to_uppercase();
    !(t.contains("LOB") || t.starts_with("LONG") || t.starts_with("RAW") || t.starts_with("BFILE") || t.contains("ROWID") || t.starts_with("XMLTYPE") || t.contains("INTERVAL"))
}

fn show(v: &Option<String>) -> String {
    match v {
        None => "NULL".into(),
        Some(s) => format!("'{}'", s.replace('\'', "''")),
    }
}

/// UPDATE/DELETE 문들. 행마다 ROWID 로 찾는다.
pub fn changes(table: &str, edits: &[RowEdit], deletes: &[String]) -> Result<Vec<Change>, String> {
    let qt = quote_table(table);
    let mut out = Vec::new();
    for e in edits {
        if e.cells.is_empty() {
            continue;
        }
        let mut sets = Vec::new();
        let mut shown = Vec::new();
        let mut binds = Vec::new();
        for (i, c) in e.cells.iter().enumerate() {
            if !editable_type(&c.type_name) {
                return Err(format!("{} ({}) 는 그리드에서 바꿀 수 없습니다", c.column, c.type_name));
            }
            let b = format!("V{}", i + 1);
            let expr = value_expr(&c.type_name, &b);
            sets.push(format!("{} = {}", quote_ident(&c.column), if c.value.is_none() { "NULL".to_string() } else { expr.clone() }));
            shown.push(format!("{} = {}", quote_ident(&c.column), if c.value.is_none() { "NULL".to_string() } else { expr.replace(&format!(":{b}"), &show(&c.value)) }));
            if c.value.is_some() {
                binds.push((b, c.value.clone()));
            }
        }
        binds.push(("RID".into(), Some(e.rowid.clone())));
        out.push(Change {
            sql: format!("UPDATE {qt} SET {} WHERE ROWID = CHARTOROWID(:RID)", sets.join(", ")),
            preview: format!("UPDATE {qt} SET {} WHERE ROWID = '{}'", shown.join(", "), e.rowid),
            binds,
        });
    }
    for rid in deletes {
        out.push(Change {
            sql: format!("DELETE FROM {qt} WHERE ROWID = CHARTOROWID(:RID)"),
            preview: format!("DELETE FROM {qt} WHERE ROWID = '{rid}'"),
            binds: vec![("RID".into(), Some(rid.clone()))],
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editable_queries() {
        let e = editable("select * from emp where deptno = 10 order by ename;").unwrap();
        assert_eq!(e.sql, "SELECT ROWIDTOCHAR(EMP.ROWID) AS SQLS_ROWID, EMP.* from emp where deptno = 10 order by ename");
        assert_eq!(e.table, "EMP");
        let e = editable("SELECT e.ename, e.sal FROM scott.emp e WHERE e.sal > 1000").unwrap();
        assert_eq!(e.sql, "SELECT ROWIDTOCHAR(E.ROWID) AS SQLS_ROWID, e.ename, e.sal FROM scott.emp e WHERE e.sal > 1000");
        assert_eq!(e.table, "SCOTT.EMP");
        // 주석·문자열 속 낱말에 속지 않는다
        assert!(editable("select a from t where b = 'x join y' -- group by\n").is_ok());
        for bad in [
            "select * from a, b",
            "select * from a join b on a.id = b.id",
            "select distinct x from t",
            "select deptno, count(*) from emp group by deptno",
            "select * from (select * from t)",
            "select * from t@link",
            "select sysdate from dual",
            "with x as (select 1 from dual) select * from x",
            "select (select 1 from dual) v from t",
            "update t set a = 1",
        ] {
            assert!(editable(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn change_statements() {
        let edits = vec![RowEdit {
            rowid: "AAAR3sAAEAAAACXAAA".into(),
            cells: vec![
                CellEdit { column: "SAL".into(), type_name: "NUMBER(7,2)".into(), value: Some("1500.5".into()) },
                CellEdit { column: "HIREDATE".into(), type_name: "DATE".into(), value: Some("2026-10-07 09:00:00".into()) },
                CellEdit { column: "COMM".into(), type_name: "NUMBER".into(), value: None },
                CellEdit { column: "ENAME".into(), type_name: "VARCHAR2(10)".into(), value: Some("O'NEIL".into()) },
            ],
        }];
        let c = changes("SCOTT.EMP", &edits, &["AAAR3sAAEAAAACXAAB".into()]).unwrap();
        assert_eq!(
            c[0].sql,
            "UPDATE \"SCOTT\".\"EMP\" SET \"SAL\" = TO_NUMBER(:V1), \"HIREDATE\" = TO_DATE(:V2, 'YYYY-MM-DD HH24:MI:SS'), \"COMM\" = NULL, \"ENAME\" = :V4 WHERE ROWID = CHARTOROWID(:RID)"
        );
        assert_eq!(c[0].binds.len(), 4);
        assert!(c[0].preview.contains("\"ENAME\" = 'O''NEIL'"));
        assert_eq!(c[1].sql, "DELETE FROM \"SCOTT\".\"EMP\" WHERE ROWID = CHARTOROWID(:RID)");
        assert!(changes("T", &[RowEdit { rowid: "x".into(), cells: vec![CellEdit { column: "DOC".into(), type_name: "CLOB".into(), value: Some("a".into()) }] }], &[]).is_err());
    }
}
