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

/// 질문의 낱말로 찾은 테이블 후보 (AI 가 SQL 을 만들기 전에 고르게 한다)
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct TableHit {
    pub name: String,
    pub comment: String,
    pub score: usize,
}

/// 접속 사용자의 테이블·뷰에서 낱말과 맞는 것 — 이름·주석에 맞으면 3점, 맞는 컬럼(이름·주석)마다 1점 (5점까지).
/// 낱말은 8개까지 (`%낱말%` LIKE). 점수 순으로 `limit` 개
pub async fn find_tables(s: &Session, words: &[String], limit: usize) -> Result<Vec<TableHit>> {
    let words: Vec<&String> = words.iter().take(8).collect();
    if words.is_empty() {
        return Ok(vec![]);
    }
    let mut binds = vec![("O".to_string(), Some(s.info().user.clone()))];
    for (i, w) in words.iter().enumerate() {
        let esc = w.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_");
        binds.push((format!("W{i}"), Some(format!("%{esc}%"))));
    }
    let any = |cols: &[&str]| -> String {
        (0..words.len())
            .flat_map(|i| cols.iter().map(move |c| format!("{c} LIKE :w{i} ESCAPE '\\'")))
            .collect::<Vec<_>>()
            .join(" OR ")
    };
    // 이름·주석: 맞은 낱말 수를 센다
    let per_word: Vec<String> = (0..words.len())
        .map(|i| format!("CASE WHEN UPPER(table_name) LIKE :w{i} ESCAPE '\\' OR comments LIKE :w{i} ESCAPE '\\' THEN 1 ELSE 0 END"))
        .collect();
    let tabs = s
        .query(
            &format!(
                "SELECT table_name, comments, {} FROM all_tab_comments \
                 WHERE owner = :o AND table_type IN ('TABLE','VIEW') AND table_name NOT LIKE 'BIN$%'",
                per_word.join(" + ")
            ),
            binds.clone(),
            20_000,
        )
        .await?;
    let cols = s
        .query(
            &format!(
                "SELECT table_name, COUNT(*) FROM all_col_comments WHERE owner = :o AND ({}) GROUP BY table_name",
                any(&["column_name", "comments"])
            ),
            binds,
            20_000,
        )
        .await?;
    let col_hits: std::collections::HashMap<String, usize> =
        cols.rows.iter().map(|r| (cell(r, 0), cell(r, 1).parse().unwrap_or(0))).collect();
    let mut out: Vec<TableHit> = tabs
        .rows
        .iter()
        .filter_map(|r| {
            let name = cell(r, 0);
            let score = cell(r, 2).parse::<usize>().unwrap_or(0) * 3 + col_hits.get(&name).copied().unwrap_or(0).min(5);
            (score > 0).then(|| TableHit { name, comment: cell(r, 1), score })
        })
        .collect();
    out.sort_by(|a, b| b.score.cmp(&a.score).then(a.name.cmp(&b.name)));
    out.truncate(limit);
    Ok(out)
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
    /// 자동완성 캐시로 바로 만든 것 (기본값·인덱스·통계가 없다 — 화면이 이어서 전체를 묻는다)
    #[serde(default)]
    pub partial: bool,
}

/// 테이블/뷰 구조. 동의어면 따라가서 원본을 보여 준다.
pub async fn describe(s: &Session, qualified: &str) -> Result<TableDesc> {
    let (mut owner, mut name) = split_qualified(qualified, &s.info().user);

    // 대상 확인과 동의어를 한 번에 — 객체가 있으면 그것, 없으면 동의어(개인 → PUBLIC)를 따라간다
    let found = s
        .query(
            "SELECT 1 ord, owner, object_name, object_type FROM all_objects \
              WHERE owner = :o AND object_name = :n AND object_type IN ('TABLE','VIEW','MATERIALIZED VIEW') \
             UNION ALL \
             SELECT DECODE(owner, 'PUBLIC', 3, 2), table_owner, table_name, 'SYNONYM' FROM all_synonyms \
              WHERE synonym_name = :n AND owner IN (:o, 'PUBLIC') AND db_link IS NULL \
             ORDER BY 1",
            vec![("O".into(), Some(owner.clone())), ("N".into(), Some(name.clone()))],
            3,
        )
        .await?;
    let mut otype = None;
    if let Some(r) = found.rows.first() {
        owner = cell(r, 1);
        name = cell(r, 2);
        otype = Some(cell(r, 3));
    }
    if otype.as_deref() == Some("SYNONYM") {
        // 동의어의 대상 종류 (all_tables/all_views 가 all_objects 보다 가볍다)
        let t = s
            .query(
                "SELECT 'TABLE' FROM all_tables WHERE owner = :o AND table_name = :n \
                 UNION ALL SELECT 'VIEW' FROM all_views WHERE owner = :o AND view_name = :n \
                 UNION ALL SELECT 'MATERIALIZED VIEW' FROM all_mviews WHERE owner = :o AND mview_name = :n",
                vec![("O".into(), Some(owner.clone())), ("N".into(), Some(name.clone()))],
                1,
            )
            .await?;
        otype = t.rows.first().map(|r| cell(r, 0));
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

    // 주석·통계·기본 키·인덱스를 한 번에 (종류 글자로 나눈다)
    let extra = s
        .query(
            "SELECT 'C' k, comments a, NULL b, 0 p FROM all_tab_comments WHERE owner = :o AND table_name = :n \
             UNION ALL \
             SELECT 'S', TO_CHAR(num_rows), TO_CHAR(last_analyzed, 'YYYY-MM-DD HH24:MI'), 0 FROM all_tables WHERE owner = :o AND table_name = :n \
             UNION ALL \
             SELECT 'P', cc.column_name, NULL, cc.position FROM all_constraints c \
               JOIN all_cons_columns cc ON cc.owner = c.owner AND cc.constraint_name = c.constraint_name \
              WHERE c.owner = :o AND c.table_name = :n AND c.constraint_type = 'P' \
             UNION ALL \
             SELECT 'I', i.index_name, DECODE(i.uniqueness, 'UNIQUE', 'U', '') || '|' || ic.column_name, ic.column_position FROM all_indexes i \
               JOIN all_ind_columns ic ON ic.index_owner = i.owner AND ic.index_name = i.index_name \
              WHERE i.table_owner = :o AND i.table_name = :n",
            ob(),
            5_000,
        )
        .await?;
    let (mut comment, mut num_rows, mut last_analyzed) = (None, None, None);
    let mut pk: Vec<(i64, String)> = Vec::new();
    let mut idx: Vec<(String, i64, bool, String)> = Vec::new();
    for r in &extra.rows {
        let pos: i64 = cell(r, 3).parse().unwrap_or(0);
        match cell(r, 0).as_str() {
            "C" => comment = r[1].clone(),
            "S" => {
                num_rows = r[1].clone();
                last_analyzed = r[2].clone();
            }
            "P" => pk.push((pos, cell(r, 1))),
            "I" => {
                let b = cell(r, 2);
                let (u, col) = b.split_once('|').unwrap_or(("", b.as_str()));
                idx.push((cell(r, 1), pos, u == "U", col.to_string()));
            }
            _ => {}
        }
    }
    pk.sort();
    idx.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
    let mut indexes: Vec<IndexDesc> = Vec::new();
    for (iname, _, unique, col) in idx {
        match indexes.last_mut() {
            Some(last) if last.name == iname => last.columns.push(col),
            _ => indexes.push(IndexDesc { name: iname, unique, columns: vec![col] }),
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
        primary_key: pk.into_iter().map(|p| p.1).collect(),
        indexes,
        partial: false,
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
    schema_brief_some(t, &|_| true)
}

/// 고른 컬럼만 넣은 요약 — 컬럼이 많은 테이블을 작은 모델 문맥에 맞출 때.
/// PK 컬럼은 늘 넣고, 뺀 컬럼은 "… 외 N개" 로 적는다. 인덱스는 첫 컬럼이 남은 것만.
pub fn schema_brief_some(t: &TableDesc, keep: &dyn Fn(&ColumnDesc) -> bool) -> String {
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
    let mut kept: Vec<&str> = Vec::new();
    for c in &t.columns {
        if !(keep(c) || t.primary_key.contains(&c.name)) {
            continue;
        }
        kept.push(&c.name);
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
    if kept.len() < t.columns.len() {
        out.push_str(&format!("  … 외 {}개 컬럼\n", t.columns.len() - kept.len()));
    }
    for i in &t.indexes {
        if !i.columns.first().is_some_and(|c| kept.contains(&c.as_str())) {
            continue;
        }
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
            partial: false,
        };
        assert_eq!(schema_brief(&t), "TABLE SCOTT.EMP  -- 사원 rows≈14\n  EMPNO NUMBER(4) NOT NULL PK\n");
    }
}

// ── 컬럼 영향도: 이 컬럼을 바꾸면 어디가 깨지나 ─────────────

/// 컬럼이 나오는 줄
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct UsageLine {
    pub line: u32,
    pub text: String,
    /// 주석 안에서만 나온다
    pub in_comment: bool,
}

/// 테이블을 쓰는 객체 하나
#[derive(Debug, Clone, Serialize)]
pub struct UsageObj {
    pub owner: String,
    pub name: String,
    pub kind: String,
    /// 동의어를 거쳐 쓰면 그 동의어
    #[serde(skip_serializing_if = "Option::is_none")]
    pub via: Option<String>,
    pub lines: Vec<UsageLine>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct Usage {
    pub table: String,
    pub column: String,
    /// 소스·뷰 정의에 컬럼 이름이 나오는 객체
    pub hits: Vec<UsageObj>,
    /// 테이블은 쓰지만 컬럼 이름이 안 보이는 객체 — SELECT *, %ROWTYPE, 동적 SQL 이면 그래도 영향을 받는다
    pub silent: Vec<UsageObj>,
    /// 이 컬럼이 든 인덱스·제약조건, 이 컬럼을 가리키는 다른 테이블의 외래 키
    pub indexes: Vec<String>,
    pub constraints: Vec<String>,
    pub referenced_by: Vec<String>,
}

/// 줄에 컬럼 이름이 낱말로 나오는지 — Some(주석 안에서만 나오는지), 안 나오면 None
pub fn word_in_line(text: &str, col: &str) -> Option<bool> {
    let up = text.to_uppercase();
    let is_word = |c: char| c.is_alphanumeric() || matches!(c, '_' | '$' | '#');
    let code_end = up.find("--").unwrap_or(up.len());
    let mut found_code = false;
    let mut found = false;
    for (i, _) in up.match_indices(col) {
        let before = up[..i].chars().next_back();
        let after = up[i + col.len()..].chars().next();
        if before.is_some_and(is_word) || after.is_some_and(is_word) {
            continue;
        }
        found = true;
        if i < code_end {
            found_code = true;
        }
    }
    found.then_some(!found_code)
}

const USAGE_CODE: &[&str] = &["PACKAGE", "PACKAGE BODY", "PROCEDURE", "FUNCTION", "TRIGGER", "TYPE", "TYPE BODY"];

/// 컬럼을 쓰는 곳. 사전 의존성으로 이 테이블을 쓰는 객체를 찾고(동의어를 거친 것도), 그 소스·뷰 정의에서 컬럼 이름이 나오는 줄을 모은다.
/// 묶어서 읽는다 — 객체마다 묻지 않는다. 소스는 컬럼 이름이 든 줄만 DB 에서 걸러 받는다
pub async fn column_usage(s: &Session, qualified: &str, column: &str) -> Result<Usage> {
    let (owner, table) = split_qualified(qualified, &s.info().user);
    let col = column.trim().trim_matches('"').to_uppercase();
    let ot = || vec![("O".to_string(), Some(owner.clone())), ("T".to_string(), Some(table.clone()))];
    let otc = || vec![("O".to_string(), Some(owner.clone())), ("T".to_string(), Some(table.clone())), ("C".to_string(), Some(col.clone()))];
    let mut u = Usage { table: format!("{owner}.{table}"), column: col.clone(), ..Default::default() };

    // 이 테이블에 기대는 객체 (직접, 동의어를 거쳐)
    let deps = s
        .query(
            "SELECT d.owner, d.name, d.type, NULL FROM all_dependencies d \
              WHERE d.referenced_owner = :o AND d.referenced_name = :t AND d.referenced_type IN ('TABLE', 'VIEW', 'MATERIALIZED VIEW') \
             UNION \
             SELECT d.owner, d.name, d.type, sy.owner || '.' || sy.synonym_name FROM all_synonyms sy \
               JOIN all_dependencies d ON d.referenced_owner = sy.owner AND d.referenced_name = sy.synonym_name AND d.referenced_type = 'SYNONYM' \
              WHERE sy.table_owner = :o AND sy.table_name = :t",
            ot(),
            100_000,
        )
        .await?;
    let mut objs: Vec<UsageObj> = Vec::new();
    for r in &deps.rows {
        let (o, n, k, via) = (cell(r, 0), cell(r, 1), cell(r, 2), r.get(3).cloned().flatten());
        if let Some(x) = objs.iter_mut().find(|x| x.owner == o && x.name == n && x.kind == k) {
            if x.via.is_none() {
                x.via = via;
            }
            continue;
        }
        objs.push(UsageObj { owner: o, name: n, kind: k, via, lines: vec![] });
    }
    let code: Vec<usize> = (0..objs.len()).filter(|&i| USAGE_CODE.contains(&objs[i].kind.as_str())).collect();
    for chunk in code.chunks(100) {
        let mut b = vec![("C".to_string(), Some(col.clone()))];
        let mut ors = Vec::new();
        for (j, &i) in chunk.iter().enumerate() {
            ors.push(format!("(owner = :O{j} AND name = :N{j} AND type = :T{j})"));
            b.push((format!("O{j}"), Some(objs[i].owner.clone())));
            b.push((format!("N{j}"), Some(objs[i].name.clone())));
            b.push((format!("T{j}"), Some(objs[i].kind.clone())));
        }
        let sql = format!("SELECT owner, name, type, line, text FROM all_source WHERE ({}) AND INSTR(UPPER(text), :c) > 0 ORDER BY owner, name, type, line", ors.join(" OR "));
        for r in s.query(&sql, b, 1_000_000).await?.rows {
            let text = cell(&r, 4);
            let Some(in_comment) = word_in_line(&text, &col) else { continue };
            if let Some(x) = objs.iter_mut().find(|x| x.owner == cell(&r, 0) && x.name == cell(&r, 1) && x.kind == cell(&r, 2)) {
                x.lines.push(UsageLine { line: cell(&r, 3).parse().unwrap_or(0), text: text.trim_end().to_string(), in_comment });
            }
        }
    }
    // 뷰: 정의(LONG)는 DB 에서 거를 수 없어 받아서 본다
    let views: Vec<usize> = (0..objs.len()).filter(|&i| objs[i].kind == "VIEW").collect();
    for chunk in views.chunks(100) {
        let mut b = Vec::new();
        let mut ors = Vec::new();
        for (j, &i) in chunk.iter().enumerate() {
            ors.push(format!("(owner = :O{j} AND view_name = :N{j})"));
            b.push((format!("O{j}"), Some(objs[i].owner.clone())));
            b.push((format!("N{j}"), Some(objs[i].name.clone())));
        }
        for r in s.query(&format!("SELECT owner, view_name, text FROM all_views WHERE {}", ors.join(" OR ")), b, 100_000).await?.rows {
            if let Some(x) = objs.iter_mut().find(|x| x.kind == "VIEW" && x.owner == cell(&r, 0) && x.name == cell(&r, 1)) {
                for (n, l) in cell(&r, 2).lines().enumerate() {
                    if let Some(in_comment) = word_in_line(l, &col) {
                        x.lines.push(UsageLine { line: n as u32 + 1, text: l.trim_end().to_string(), in_comment });
                    }
                }
            }
        }
    }
    for x in objs {
        if x.lines.is_empty() {
            u.silent.push(x)
        } else {
            u.hits.push(x)
        }
    }
    let key = |a: &UsageObj| (a.kind.clone(), a.owner.clone(), a.name.clone());
    u.hits.sort_by_key(key);
    u.silent.sort_by_key(key);
    for r in s.query("SELECT index_owner || '.' || index_name FROM all_ind_columns WHERE table_owner = :o AND table_name = :t AND column_name = :c", otc(), 1000).await?.rows {
        u.indexes.push(cell(&r, 0));
    }
    let cons = "SELECT c.constraint_name || ' (' || DECODE(c.constraint_type, 'P', 'PK', 'U', 'UNIQUE', 'R', 'FK', 'C', 'CHECK', c.constraint_type) || ')' \
                  FROM all_cons_columns cc JOIN all_constraints c ON c.owner = cc.owner AND c.constraint_name = cc.constraint_name \
                 WHERE cc.owner = :o AND cc.table_name = :t AND cc.column_name = :c";
    for r in s.query(cons, otc(), 1000).await?.rows {
        u.constraints.push(cell(&r, 0));
    }
    let refs = "SELECT c.owner || '.' || c.table_name || ' · ' || c.constraint_name FROM all_constraints c \
                  JOIN all_cons_columns rc ON rc.owner = c.r_owner AND rc.constraint_name = c.r_constraint_name \
                 WHERE c.constraint_type = 'R' AND rc.owner = :o AND rc.table_name = :t AND rc.column_name = :c";
    for r in s.query(refs, otc(), 1000).await?.rows {
        u.referenced_by.push(cell(&r, 0));
    }
    Ok(u)
}

// ── 객체 의존: 이 객체를 쓰는 곳(Used By) · 이 객체가 쓰는 것(Uses) ─────────────

/// 의존 관계의 상대 객체 하나
#[derive(Debug, Clone, Serialize)]
pub struct DepObj {
    pub owner: String,
    pub name: String,
    pub kind: String,
    /// VALID · INVALID (사전에서 못 찾으면 빈칸)
    pub status: String,
    /// 어떻게 이어졌나: "직접" · "동의어 X.Y" · "FK 이름" · "DB 링크 L"
    pub how: String,
    /// Used By 만: 그 객체의 소스·뷰 정의에서 이 객체 이름(동의어를 거치면 동의어 이름)이 나오는 줄
    pub lines: Vec<UsageLine>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct ObjectDeps {
    pub object: String,
    pub used_by: Vec<DepObj>,
    pub uses: Vec<DepObj>,
}

/// 소스에서 이 객체를 찾아 줄을 모을 때, 한 객체에서 많아야 이만큼
const DEP_LINES_MAX: usize = 200;

/// 객체 의존. 사전 의존성(ALL_DEPENDENCIES)과 외래 키로 Used By · Uses 를 모으고,
/// Used By 의 소스·뷰 정의에서 이 객체 이름이 나오는 줄을 붙인다 (어디에서 쓰는지).
pub async fn object_deps(s: &Session, qualified: &str) -> Result<ObjectDeps> {
    let (owner, name) = split_qualified(qualified, &s.info().user);
    let on = || vec![("O".to_string(), Some(owner.clone())), ("N".to_string(), Some(name.clone()))];
    let mut r = ObjectDeps { object: format!("{owner}.{name}"), ..Default::default() };
    let status = "LEFT JOIN all_objects o ON o.owner = d.owner AND o.object_name = d.name AND o.object_type = d.type";

    // Used By: 직접, 동의어를 거쳐, 외래 키로 가리키는 테이블
    // 바인드(:n)를 그대로 고르면 NVARCHAR2 라 다른 갈래의 VARCHAR2 와 UNION 할 때 ORA-12704 — 사전 컬럼을 고른다
    let used_by = format!(
        "SELECT d.owner, d.name, d.type, o.status, '직접', d.referenced_name FROM all_dependencies d {status} \
          WHERE d.referenced_owner = :o AND d.referenced_name = :n AND NOT (d.owner = :o AND d.name = :n) \
         UNION \
         SELECT d.owner, d.name, d.type, o.status, '동의어 ' || sy.owner || '.' || sy.synonym_name, sy.synonym_name FROM all_synonyms sy \
           JOIN all_dependencies d ON d.referenced_owner = sy.owner AND d.referenced_name = sy.synonym_name AND d.referenced_type = 'SYNONYM' {status} \
          WHERE sy.table_owner = :o AND sy.table_name = :n \
         UNION \
         SELECT c.owner, c.table_name, 'TABLE', 'VALID', 'FK ' || c.constraint_name, NULL FROM all_constraints c \
           JOIN all_constraints p ON p.owner = c.r_owner AND p.constraint_name = c.r_constraint_name \
          WHERE c.constraint_type = 'R' AND p.owner = :o AND p.table_name = :n"
    );
    // 줄을 찾을 낱말 (동의어를 거치면 동의어 이름)
    let mut words: Vec<Option<String>> = Vec::new();
    for row in s.query(&used_by, on(), 100_000).await?.rows {
        let (o, n, k, how) = (cell(&row, 0), cell(&row, 1), cell(&row, 2), cell(&row, 4));
        // 같은 객체가 직접·동의어로 둘 다 나오면 하나로 (직접이 먼저)
        if let Some(x) = r.used_by.iter_mut().find(|x| x.owner == o && x.name == n && x.kind == k) {
            if !x.how.contains(&how) {
                x.how = format!("{}, {how}", x.how);
            }
            continue;
        }
        r.used_by.push(DepObj { owner: o, name: n, kind: k, status: cell(&row, 3), how, lines: vec![] });
        words.push(row.get(5).cloned().flatten().map(|w| w.to_uppercase()));
    }

    // 소스: 낱말이 든 줄만 DB 에서 걸러 받는다
    let code: Vec<usize> = (0..r.used_by.len()).filter(|&i| words[i].is_some() && USAGE_CODE.contains(&r.used_by[i].kind.as_str())).collect();
    for chunk in code.chunks(100) {
        let mut b = Vec::new();
        let mut ors = Vec::new();
        for (j, &i) in chunk.iter().enumerate() {
            ors.push(format!("(owner = :O{j} AND name = :N{j} AND type = :T{j} AND INSTR(UPPER(text), :W{j}) > 0)"));
            b.push((format!("O{j}"), Some(r.used_by[i].owner.clone())));
            b.push((format!("N{j}"), Some(r.used_by[i].name.clone())));
            b.push((format!("T{j}"), Some(r.used_by[i].kind.clone())));
            b.push((format!("W{j}"), words[i].clone()));
        }
        let sql = format!("SELECT owner, name, type, line, text FROM all_source WHERE {} ORDER BY owner, name, type, line", ors.join(" OR "));
        for row in s.query(&sql, b, 1_000_000).await?.rows {
            let Some(i) = r.used_by.iter().position(|x| x.owner == cell(&row, 0) && x.name == cell(&row, 1) && x.kind == cell(&row, 2)) else { continue };
            let text = cell(&row, 4);
            let Some(in_comment) = word_in_line(&text, words[i].as_deref().unwrap_or_default()) else { continue };
            if r.used_by[i].lines.len() < DEP_LINES_MAX {
                r.used_by[i].lines.push(UsageLine { line: cell(&row, 3).parse().unwrap_or(0), text: text.trim_end().to_string(), in_comment });
            }
        }
    }
    // 뷰: 정의(LONG)는 DB 에서 거를 수 없어 받아서 본다
    let views: Vec<usize> = (0..r.used_by.len()).filter(|&i| words[i].is_some() && r.used_by[i].kind == "VIEW").collect();
    for chunk in views.chunks(100) {
        let mut b = Vec::new();
        let mut ors = Vec::new();
        for (j, &i) in chunk.iter().enumerate() {
            ors.push(format!("(owner = :O{j} AND view_name = :N{j})"));
            b.push((format!("O{j}"), Some(r.used_by[i].owner.clone())));
            b.push((format!("N{j}"), Some(r.used_by[i].name.clone())));
        }
        for row in s.query(&format!("SELECT owner, view_name, text FROM all_views WHERE {}", ors.join(" OR ")), b, 100_000).await?.rows {
            let Some(i) = r.used_by.iter().position(|x| x.kind == "VIEW" && x.owner == cell(&row, 0) && x.name == cell(&row, 1)) else { continue };
            let word = words[i].clone().unwrap_or_default();
            for (n, l) in cell(&row, 2).lines().enumerate() {
                if let Some(in_comment) = word_in_line(l, &word) {
                    if r.used_by[i].lines.len() < DEP_LINES_MAX {
                        r.used_by[i].lines.push(UsageLine { line: n as u32 + 1, text: l.trim_end().to_string(), in_comment });
                    }
                }
            }
        }
    }

    // Uses: 이 객체가 기대는 것 (STANDARD 같은 기본 패키지는 뺀다), 외래 키로 가리키는 부모 테이블
    let uses = "SELECT d.referenced_owner, d.referenced_name, d.referenced_type, o.status, \
                       NVL2(d.referenced_link_name, 'DB 링크 ' || d.referenced_link_name, '직접') FROM all_dependencies d \
                  LEFT JOIN all_objects o ON o.owner = d.referenced_owner AND o.object_name = d.referenced_name AND o.object_type = d.referenced_type \
                 WHERE d.owner = :o AND d.name = :n AND NOT (d.referenced_owner = :o AND d.referenced_name = :n) \
                   AND NOT (d.referenced_owner = 'SYS' AND d.referenced_name IN ('STANDARD', 'DBMS_STANDARD', 'SYS_STUB_FOR_PURITY_ANALYSIS')) \
                UNION \
                SELECT p.owner, p.table_name, 'TABLE', 'VALID', 'FK ' || c.constraint_name FROM all_constraints c \
                  JOIN all_constraints p ON p.owner = c.r_owner AND p.constraint_name = c.r_constraint_name \
                 WHERE c.constraint_type = 'R' AND c.owner = :o AND c.table_name = :n";
    for row in s.query(uses, on(), 100_000).await?.rows {
        let (o, n, k, how) = (cell(&row, 0), cell(&row, 1), cell(&row, 2), cell(&row, 4));
        if let Some(x) = r.uses.iter_mut().find(|x| x.owner == o && x.name == n && x.kind == k) {
            if !x.how.contains(&how) {
                x.how = format!("{}, {how}", x.how);
            }
            continue;
        }
        r.uses.push(DepObj { owner: o, name: n, kind: k, status: cell(&row, 3), how, lines: vec![] });
    }
    let key = |a: &DepObj| (a.kind.clone(), a.owner.clone(), a.name.clone());
    r.used_by.sort_by_key(key);
    r.uses.sort_by_key(key);
    Ok(r)
}

#[cfg(test)]
mod usage_tests {
    use super::word_in_line;

    #[test]
    fn column_word_in_line() {
        assert_eq!(word_in_line("  UPDATE orders SET status = 'X'", "STATUS"), Some(false));
        assert_eq!(word_in_line("  v_x := 1; -- status 바꿈", "STATUS"), Some(true), "주석에만");
        assert_eq!(word_in_line("  x.order_status := 1;", "STATUS"), None, "다른 낱말의 일부");
        assert_eq!(word_in_line("WHERE o.STATUS='A'", "STATUS"), Some(false));
    }
}
