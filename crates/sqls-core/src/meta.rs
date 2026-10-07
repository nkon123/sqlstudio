//! 데이터 사전 조회. 화면의 객체 탐색기, AI 문맥, MCP 툴이 모두 이것을 쓴다.
//!
//! 전부 ALL_* 뷰만 쓴다 (DBA_* 권한이 없는 일반 계정에서도 동작해야 한다).
//! 11g 에 있는 컬럼만 쓴다 (예: ALL_TAB_COLUMNS.IDENTITY_COLUMN 은 12c 부터라 안 쓴다).

use serde::Serialize;

use crate::error::{Error, Result};
use crate::session::Session;

/// 식별자 정규화: 따옴표로 감싼 것은 그대로, 아니면 대문자.
pub fn norm_ident(s: &str) -> String {
    let t = s.trim();
    if t.len() >= 2 && t.starts_with('"') && t.ends_with('"') {
        t[1..t.len() - 1].to_string()
    } else {
        t.to_uppercase()
    }
}

/// `OWNER.NAME` 또는 `NAME` 을 나눈다.
pub fn split_qualified(s: &str, default_owner: &str) -> (String, String) {
    match s.split_once('.') {
        Some((o, n)) => (norm_ident(o), norm_ident(n)),
        None => (default_owner.to_uppercase(), norm_ident(s)),
    }
}

fn cell(row: &[Option<String>], i: usize) -> String {
    row.get(i).cloned().flatten().unwrap_or_default()
}

#[derive(Debug, Clone, Serialize)]
pub struct ObjectEntry {
    pub owner: String,
    pub name: String,
    pub object_type: String,
    pub status: String,
    pub last_ddl_time: String,
}

/// 객체 목록. `name_like` 는 LIKE 패턴 (예: `EMP%`). 대소문자 무시.
pub async fn list_objects(
    s: &Session,
    owner: Option<&str>,
    object_type: Option<&str>,
    name_like: Option<&str>,
    limit: usize,
) -> Result<Vec<ObjectEntry>> {
    let owner = owner.map(norm_ident).unwrap_or_else(|| s.info().user.clone());
    let sql = "SELECT owner, object_name, object_type, status, \
               TO_CHAR(last_ddl_time, 'YYYY-MM-DD HH24:MI:SS') \
               FROM all_objects \
               WHERE owner = :owner \
                 AND (:otype IS NULL OR object_type = :otype) \
                 AND (:pat IS NULL OR object_name LIKE :pat ESCAPE '\\') \
                 AND object_name NOT LIKE 'BIN$%' \
               ORDER BY object_type, object_name";
    let page = s
        .query(
            sql,
            vec![
                ("OWNER".into(), Some(owner)),
                ("OTYPE".into(), object_type.map(|t| t.to_uppercase())),
                ("PAT".into(), name_like.map(|p| p.to_uppercase())),
            ],
            limit,
        )
        .await?;
    Ok(page
        .rows
        .iter()
        .map(|r| ObjectEntry {
            owner: cell(r, 0),
            name: cell(r, 1),
            object_type: cell(r, 2),
            status: cell(r, 3),
            last_ddl_time: cell(r, 4),
        })
        .collect())
}

pub async fn list_schemas(s: &Session) -> Result<Vec<String>> {
    let page = s.query("SELECT username FROM all_users ORDER BY username", vec![], 10_000).await?;
    Ok(page.rows.iter().map(|r| cell(r, 0)).collect())
}

#[derive(Debug, Clone, Serialize)]
pub struct ColumnDesc {
    pub name: String,
    /// 사람이 읽는 형식: `VARCHAR2(30 CHAR)`, `NUMBER(10,2)`, `DATE`
    pub data_type: String,
    pub nullable: bool,
    pub default: Option<String>,
    pub comment: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct IndexDesc {
    pub name: String,
    pub unique: bool,
    pub columns: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TableDesc {
    pub owner: String,
    pub name: String,
    pub object_type: String,
    pub comment: Option<String>,
    pub num_rows: Option<String>,
    pub last_analyzed: Option<String>,
    pub columns: Vec<ColumnDesc>,
    pub primary_key: Vec<String>,
    pub indexes: Vec<IndexDesc>,
}

/// 테이블/뷰 구조. 동의어면 따라가서 원본을 보여 준다.
pub async fn describe(s: &Session, qualified: &str) -> Result<TableDesc> {
    let (mut owner, mut name) = split_qualified(qualified, &s.info().user);

    // 대상 확인 — 없으면 동의어(개인 → PUBLIC 순)를 따라간다
    let found = s
        .query(
            "SELECT object_type FROM all_objects WHERE owner = :o AND object_name = :n \
             AND object_type IN ('TABLE','VIEW','MATERIALIZED VIEW','SYNONYM')",
            vec![("O".into(), Some(owner.clone())), ("N".into(), Some(name.clone()))],
            1,
        )
        .await?;
    let mut otype = found.rows.first().map(|r| cell(r, 0));
    if otype.is_none() || otype.as_deref() == Some("SYNONYM") {
        let syn = s
            .query(
                "SELECT table_owner, table_name FROM all_synonyms \
                 WHERE synonym_name = :n AND owner IN (:o, 'PUBLIC') \
                 ORDER BY DECODE(owner, 'PUBLIC', 2, 1)",
                vec![("N".into(), Some(name.clone())), ("O".into(), Some(owner.clone()))],
                1,
            )
            .await?;
        if let Some(r) = syn.rows.first() {
            owner = cell(r, 0);
            name = cell(r, 1);
            let t = s
                .query(
                    "SELECT object_type FROM all_objects WHERE owner = :o AND object_name = :n \
                     AND object_type IN ('TABLE','VIEW','MATERIALIZED VIEW')",
                    vec![("O".into(), Some(owner.clone())), ("N".into(), Some(name.clone()))],
                    1,
                )
                .await?;
            otype = t.rows.first().map(|r| cell(r, 0));
        }
    }
    let object_type = otype.ok_or_else(|| {
        Error::Invalid(format!("{owner}.{name} 을(를) 찾을 수 없거나 조회 권한이 없습니다"))
    })?;

    let ob = || vec![("O".to_string(), Some(owner.clone())), ("N".to_string(), Some(name.clone()))];

    let cols = s
        .query(
            "SELECT c.column_name, c.data_type, c.data_length, c.data_precision, c.data_scale, \
                    c.char_used, c.char_length, c.nullable, c.data_default, cc.comments \
             FROM all_tab_columns c \
             LEFT JOIN all_col_comments cc \
               ON cc.owner = c.owner AND cc.table_name = c.table_name AND cc.column_name = c.column_name \
             WHERE c.owner = :o AND c.table_name = :n \
             ORDER BY c.column_id",
            ob(),
            5_000,
        )
        .await?;
    let columns = cols
        .rows
        .iter()
        .map(|r| ColumnDesc {
            name: cell(r, 0),
            data_type: format_type(&cell(r, 1), &cell(r, 2), &cell(r, 3), &cell(r, 4), &cell(r, 5), &cell(r, 6)),
            nullable: cell(r, 7) == "Y",
            default: r.get(8).cloned().flatten().map(|d| d.trim().to_string()),
            comment: r.get(9).cloned().flatten(),
        })
        .collect();

    let info = s
        .query(
            "SELECT (SELECT comments FROM all_tab_comments WHERE owner = :o AND table_name = :n), \
                    (SELECT TO_CHAR(num_rows) FROM all_tables WHERE owner = :o AND table_name = :n), \
                    (SELECT TO_CHAR(last_analyzed, 'YYYY-MM-DD HH24:MI') FROM all_tables WHERE owner = :o AND table_name = :n) \
             FROM dual",
            ob(),
            1,
        )
        .await?;
    let (comment, num_rows, last_analyzed) = info
        .rows
        .first()
        .map(|r| (r[0].clone(), r[1].clone(), r[2].clone()))
        .unwrap_or_default();

    let pk = s
        .query(
            "SELECT cc.column_name FROM all_constraints c \
             JOIN all_cons_columns cc ON cc.owner = c.owner AND cc.constraint_name = c.constraint_name \
             WHERE c.owner = :o AND c.table_name = :n AND c.constraint_type = 'P' \
             ORDER BY cc.position",
            ob(),
            100,
        )
        .await?;

    let idx = s
        .query(
            "SELECT i.index_name, i.uniqueness, ic.column_name \
             FROM all_indexes i \
             JOIN all_ind_columns ic ON ic.index_owner = i.owner AND ic.index_name = i.index_name \
             WHERE i.table_owner = :o AND i.table_name = :n \
             ORDER BY i.index_name, ic.column_position",
            ob(),
            2_000,
        )
        .await?;
    let mut indexes: Vec<IndexDesc> = Vec::new();
    for r in &idx.rows {
        let iname = cell(r, 0);
        match indexes.last_mut() {
            Some(last) if last.name == iname => last.columns.push(cell(r, 2)),
            _ => indexes.push(IndexDesc {
                name: iname,
                unique: cell(r, 1) == "UNIQUE",
                columns: vec![cell(r, 2)],
            }),
        }
    }

    Ok(TableDesc {
        owner,
        name,
        object_type,
        comment,
        num_rows,
        last_analyzed,
        columns,
        primary_key: pk.rows.iter().map(|r| cell(r, 0)).collect(),
        indexes,
    })
}

pub(crate) fn format_type(t: &str, len: &str, prec: &str, scale: &str, char_used: &str, char_len: &str) -> String {
    match t {
        "VARCHAR2" | "NVARCHAR2" | "CHAR" | "NCHAR" => {
            if char_used == "C" {
                format!("{t}({char_len} CHAR)")
            } else {
                format!("{t}({len})")
            }
        }
        "RAW" => format!("{t}({len})"),
        "NUMBER" => match (prec.is_empty(), scale) {
            (true, "0") => "INTEGER".into(),
            (true, _) => "NUMBER".into(),
            (false, "0") | (false, "") => format!("NUMBER({prec})"),
            (false, sc) => format!("NUMBER({prec},{sc})"),
        },
        _ => t.to_string(),
    }
}

/// DDL 을 만든다. DBMS_METADATA 가 우선이고, 그것이 깨진 DB(ORA-39212 등 —
/// XDB/XSL 이 빠진 설치에서 실제로 난다)에서는 사전 정보로 만든다.
pub async fn get_ddl(s: &Session, object_type: &str, qualified: &str) -> Result<String> {
    let (owner, name) = split_qualified(qualified, &s.info().user);
    let otype = object_type.trim().to_uppercase();
    // DBMS_METADATA 형식 이름: 'PACKAGE BODY' → 'PACKAGE_BODY'
    let mtype = otype.replace(' ', "_");
    let r = s
        .query(
            "SELECT DBMS_METADATA.GET_DDL(:t, :n, :o) FROM dual",
            vec![
                ("T".into(), Some(mtype)),
                ("N".into(), Some(name.clone())),
                ("O".into(), Some(owner.clone())),
            ],
            1,
        )
        .await;
    match r {
        Ok(page) => Ok(page.rows.first().map(|r| cell(r, 0)).unwrap_or_default().trim().to_string()),
        // 31603: 객체가 없음 — 대체 경로로 가도 없다
        Err(Error::Db { code, .. }) if code != 31603 => {
            tracing::warn!("DBMS_METADATA 실패(ORA-{code:05}) — 사전 정보로 DDL 을 만든다");
            ddl_fallback(s, &otype, &owner, &name).await
        }
        Err(e) => Err(e),
    }
}

const SOURCE_TYPES: &[&str] = &[
    "PROCEDURE", "FUNCTION", "PACKAGE", "PACKAGE BODY", "TRIGGER", "TYPE", "TYPE BODY",
];

async fn ddl_fallback(s: &Session, otype: &str, owner: &str, name: &str) -> Result<String> {
    let note = "-- DBMS_METADATA 를 쓸 수 없어 데이터 사전으로 만든 DDL 입니다 (저장 옵션·권한 제외)\n";
    if SOURCE_TYPES.contains(&otype) {
        let page = s
            .query(
                "SELECT text FROM all_source WHERE owner = :o AND name = :n AND type = :t ORDER BY line",
                vec![
                    ("O".into(), Some(owner.to_string())),
                    ("N".into(), Some(name.to_string())),
                    ("T".into(), Some(otype.to_string())),
                ],
                1_000_000,
            )
            .await?;
        if page.rows.is_empty() {
            return Err(Error::Invalid(format!("{owner}.{name} ({otype}) 소스를 찾을 수 없습니다")));
        }
        let body: String = page.rows.iter().map(|r| cell(r, 0)).collect();
        return Ok(format!("{note}CREATE OR REPLACE {}\n/", body.trim_end()));
    }
    match otype {
        "VIEW" => {
            let page = s
                .query(
                    "SELECT text FROM all_views WHERE owner = :o AND view_name = :n",
                    vec![("O".into(), Some(owner.to_string())), ("N".into(), Some(name.to_string()))],
                    1,
                )
                .await?;
            let text = page.rows.first().map(|r| cell(r, 0)).ok_or_else(|| {
                Error::Invalid(format!("{owner}.{name} 뷰를 찾을 수 없습니다"))
            })?;
            Ok(format!("{note}CREATE OR REPLACE VIEW {owner}.{name} AS\n{};", text.trim_end()))
        }
        "TABLE" => {
            let d = describe(s, &format!("\"{owner}\".\"{name}\"")).await?;
            Ok(format!("{note}{}", table_ddl(&d)))
        }
        _ => Err(Error::Invalid(format!(
            "DBMS_METADATA 없이 {otype} 의 DDL 을 만들 수 없습니다"
        ))),
    }
}

/// 사전 정보로 만든 CREATE TABLE (+ 인덱스, 주석)
pub fn table_ddl(d: &TableDesc) -> String {
    let mut out = format!("CREATE TABLE {}.{} (\n", d.owner, d.name);
    let mut lines: Vec<String> = d
        .columns
        .iter()
        .map(|c| {
            let mut l = format!("  {} {}", c.name, c.data_type);
            if let Some(def) = c.default.as_deref().filter(|x| !x.is_empty()) {
                l.push_str(&format!(" DEFAULT {def}"));
            }
            if !c.nullable {
                l.push_str(" NOT NULL");
            }
            l
        })
        .collect();
    if !d.primary_key.is_empty() {
        lines.push(format!("  PRIMARY KEY ({})", d.primary_key.join(", ")));
    }
    out.push_str(&lines.join(",\n"));
    out.push_str("\n);\n");
    for i in &d.indexes {
        // PK 를 받치는 인덱스는 위의 PRIMARY KEY 가 만든다
        if i.unique && i.columns == d.primary_key {
            continue;
        }
        out.push_str(&format!(
            "CREATE {}INDEX {}.{} ON {}.{} ({});\n",
            if i.unique { "UNIQUE " } else { "" },
            d.owner, i.name, d.owner, d.name, i.columns.join(", ")
        ));
    }
    let q = |t: &str| t.replace('\'', "''");
    if let Some(c) = &d.comment {
        out.push_str(&format!("COMMENT ON TABLE {}.{} IS '{}';\n", d.owner, d.name, q(c)));
    }
    for c in &d.columns {
        if let Some(cm) = &c.comment {
            out.push_str(&format!("COMMENT ON COLUMN {}.{}.{} IS '{}';\n", d.owner, d.name, c.name, q(cm)));
        }
    }
    out
}

/// 컴파일 오류 (ALL_ERRORS) — PL/SQL 단위를 컴파일한 뒤 보여 준다.
#[derive(Debug, Clone, Serialize)]
pub struct CompileError {
    pub line: u32,
    pub position: u32,
    pub text: String,
    pub attribute: String,
}

pub async fn compile_errors(s: &Session, object_type: &str, qualified: &str) -> Result<Vec<CompileError>> {
    let (owner, name) = split_qualified(qualified, &s.info().user);
    let page = s
        .query(
            "SELECT line, position, text, attribute FROM all_errors \
             WHERE owner = :o AND name = :n AND type = :t ORDER BY sequence",
            vec![
                ("O".into(), Some(owner)),
                ("N".into(), Some(name)),
                ("T".into(), Some(object_type.to_uppercase())),
            ],
            1_000,
        )
        .await?;
    Ok(page
        .rows
        .iter()
        .map(|r| CompileError {
            line: cell(r, 0).parse().unwrap_or(0),
            position: cell(r, 1).parse().unwrap_or(0),
            text: cell(r, 2),
            attribute: cell(r, 3),
        })
        .collect())
}

/// LLM 문맥용 스키마 요약. 짧게 — 토큰은 로컬 모델에서 곧 시간이다.
///
/// ```text
/// TABLE SCOTT.EMP  -- 사원 (rows≈14)
///   EMPNO NUMBER(4) NOT NULL PK
///   ENAME VARCHAR2(10)  -- 이름
///   INDEX EMP_IX1(DEPTNO, HIREDATE)
/// ```
pub fn schema_brief(t: &TableDesc) -> String {
    let mut out = format!("{} {}.{}", t.object_type, t.owner, t.name);
    let mut tail = Vec::new();
    if let Some(c) = &t.comment {
        tail.push(c.clone());
    }
    if let Some(n) = &t.num_rows {
        tail.push(format!("rows≈{n}"));
    }
    if !tail.is_empty() {
        out.push_str(&format!("  -- {}", tail.join(" ")));
    }
    out.push('\n');
    for c in &t.columns {
        out.push_str(&format!("  {} {}", c.name, c.data_type));
        if !c.nullable {
            out.push_str(" NOT NULL");
        }
        if t.primary_key.contains(&c.name) {
            out.push_str(" PK");
        }
        if let Some(cm) = &c.comment {
            out.push_str(&format!("  -- {cm}"));
        }
        out.push('\n');
    }
    for i in &t.indexes {
        out.push_str(&format!(
            "  {}INDEX {}({})\n",
            if i.unique { "UNIQUE " } else { "" },
            i.name,
            i.columns.join(", ")
        ));
    }
    out
}

/// SQL 에 나온 테이블 이름 후보 (FROM / JOIN / INTO / UPDATE 뒤의 식별자).
/// AI 에 넘길 스키마 문맥을 고를 때 쓴다. 완벽할 필요는 없다 — 없는 이름은 조회에서 걸러진다.
pub fn referenced_tables(sql_text: &str) -> Vec<String> {
    let upper = sql_text.to_uppercase();
    let toks: Vec<&str> = upper
        .split(|c: char| c.is_whitespace() || c == ',' || c == '(' || c == ')' || c == ';')
        .filter(|t| !t.is_empty())
        .collect();
    let mut out: Vec<String> = Vec::new();
    for w in toks.windows(2) {
        if matches!(w[0], "FROM" | "JOIN" | "INTO" | "UPDATE" | "TABLE") {
            let cand = w[1].trim_matches('"');
            let ok = cand
                .chars()
                .all(|c| c.is_alphanumeric() || matches!(c, '_' | '$' | '#' | '.'))
                && !matches!(cand, "SELECT" | "DUAL" | "TABLE" | "LATERAL")
                && !cand.is_empty();
            if ok && !out.iter().any(|x| x == cand) {
                out.push(cand.to_string());
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idents() {
        assert_eq!(norm_ident("emp"), "EMP");
        assert_eq!(norm_ident("\"MixedCase\""), "MixedCase");
        assert_eq!(split_qualified("scott.emp", "X"), ("SCOTT".into(), "EMP".into()));
        assert_eq!(split_qualified("emp", "scott"), ("SCOTT".into(), "EMP".into()));
    }

    #[test]
    fn types() {
        assert_eq!(format_type("VARCHAR2", "40", "", "", "C", "10"), "VARCHAR2(10 CHAR)");
        assert_eq!(format_type("VARCHAR2", "40", "", "", "B", "40"), "VARCHAR2(40)");
        assert_eq!(format_type("NUMBER", "22", "10", "2", "", ""), "NUMBER(10,2)");
        assert_eq!(format_type("NUMBER", "22", "", "0", "", ""), "INTEGER");
        assert_eq!(format_type("NUMBER", "22", "", "", "", ""), "NUMBER");
        assert_eq!(format_type("DATE", "7", "", "", "", ""), "DATE");
    }

    #[test]
    fn tables_in_sql() {
        let t = referenced_tables(
            "select e.ename from scott.emp e join dept d on d.deptno = e.deptno where exists (select 1 from bonus)",
        );
        assert_eq!(t, vec!["SCOTT.EMP", "DEPT", "BONUS"]);
        assert!(referenced_tables("select sysdate from dual").is_empty());
    }

    #[test]
    fn brief() {
        let t = TableDesc {
            owner: "SCOTT".into(),
            name: "EMP".into(),
            object_type: "TABLE".into(),
            comment: Some("사원".into()),
            num_rows: Some("14".into()),
            last_analyzed: None,
            columns: vec![ColumnDesc {
                name: "EMPNO".into(),
                data_type: "NUMBER(4)".into(),
                nullable: false,
                default: None,
                comment: None,
            }],
            primary_key: vec!["EMPNO".into()],
            indexes: vec![],
        };
        assert_eq!(schema_brief(&t), "TABLE SCOTT.EMP  -- 사원 rows≈14\n  EMPNO NUMBER(4) NOT NULL PK\n");
    }
}
