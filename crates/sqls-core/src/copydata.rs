//! 접속 간 데이터 복사 — 원본 테이블의 행을 다른 접속(다른 DB)의 테이블로 옮기는 SQL 을 만든다.
//!
//! - 열은 이름으로 맞춘다. 값은 문자열로 옮기되, 날짜·시각은 정해진 형식으로 바꿔 읽고 같은 형식으로 넣는다
//!   (두 DB 의 NLS 설정이 달라도 값이 바뀌지 않게). RAW 는 16진수로.
//! - BLOB·LONG RAW·BFILE·XMLTYPE·객체 형식은 문자열로 옮길 수 없어 뺀다 (화면에 알린다).
//! - 넣는 방식: 추가(INSERT) / 지우고 넣기(같은 WHERE 로 DELETE 후 INSERT) / 키로 MERGE.
//! - 커밋하지 않는다 — 대상 탭에서 결과를 보고 커밋·롤백한다.

use serde::{Deserialize, Serialize};

use crate::meta::ColumnDesc;

/// 값을 옮기는 방식 (원본 열의 형식으로 정한다 — 넣을 때는 그 형식의 값을 Oracle 이 대상 열 형식으로 바꾼다)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ValueKind {
    Text,
    Number,
    Date,
    Timestamp,
    TimestampTz,
    IntervalDs,
    IntervalYm,
    Raw,
    /// CLOB·NCLOB·LONG — 문자열로 옮긴다 (32K 넘는 값은 넣을 때 실패할 수 있다)
    Lob,
    Unsupported,
}

pub fn kind_of(data_type: &str) -> ValueKind {
    let t = data_type.trim().to_uppercase();
    if t.starts_with("TIMESTAMP") {
        if t.contains("WITH TIME ZONE") && !t.contains("LOCAL") {
            return ValueKind::TimestampTz;
        }
        return ValueKind::Timestamp;
    }
    if t.starts_with("INTERVAL DAY") {
        return ValueKind::IntervalDs;
    }
    if t.starts_with("INTERVAL YEAR") {
        return ValueKind::IntervalYm;
    }
    let base = t.split(['(', ' ']).next().unwrap_or("");
    match base {
        "DATE" => ValueKind::Date,
        "NUMBER" | "FLOAT" | "INTEGER" | "BINARY_FLOAT" | "BINARY_DOUBLE" | "DECIMAL" | "NUMERIC" | "SMALLINT" | "INT" | "REAL" => ValueKind::Number,
        "VARCHAR2" | "VARCHAR" | "NVARCHAR2" | "CHAR" | "NCHAR" => ValueKind::Text,
        "RAW" => ValueKind::Raw,
        "CLOB" | "NCLOB" | "LONG" => ValueKind::Lob,
        _ => ValueKind::Unsupported,
    }
}

const DATE_FMT: &str = "YYYY-MM-DD HH24:MI:SS";
const TS_FMT: &str = "YYYY-MM-DD HH24:MI:SS.FF9";
const TSTZ_FMT: &str = "YYYY-MM-DD HH24:MI:SS.FF9 TZH:TZM";

/// 원본에서 읽는 식
fn read_expr(col: &str, k: ValueKind) -> String {
    let c = q(col);
    match k {
        ValueKind::Date => format!("TO_CHAR({c}, '{DATE_FMT}')"),
        ValueKind::Timestamp => format!("TO_CHAR({c}, '{TS_FMT}')"),
        ValueKind::TimestampTz => format!("TO_CHAR({c}, '{TSTZ_FMT}')"),
        ValueKind::Raw => format!("RAWTOHEX({c})"),
        _ => c,
    }
}

/// 넣는 식 (바인드 하나)
fn write_expr(bind: &str, k: ValueKind) -> String {
    match k {
        ValueKind::Date => format!("TO_DATE({bind}, '{DATE_FMT}')"),
        ValueKind::Timestamp => format!("TO_TIMESTAMP({bind}, '{TS_FMT}')"),
        ValueKind::TimestampTz => format!("TO_TIMESTAMP_TZ({bind}, '{TSTZ_FMT}')"),
        ValueKind::IntervalDs => format!("TO_DSINTERVAL({bind})"),
        ValueKind::IntervalYm => format!("TO_YMINTERVAL({bind})"),
        ValueKind::Raw => format!("HEXTORAW({bind})"),
        _ => bind.to_string(),
    }
}

/// "식별자" (사전의 이름 그대로 — 따옴표는 뺀다)
pub fn q(name: &str) -> String {
    format!("\"{}\"", name.replace('"', ""))
}

pub fn qualified(owner: &str, table: &str) -> String {
    format!("{}.{}", q(owner), q(table))
}

/// 열 하나를 어떻게 하는지
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ColPlan {
    pub name: String,
    pub src_type: Option<String>,
    pub dst_type: Option<String>,
    /// copy — 옮김 / src_only — 원본에만 / dst_only — 대상에만 (NULL 이나 기본값이 들어간다) / unsupported — 옮길 수 없는 형식
    pub status: String,
    pub kind: ValueKind,
    /// 알릴 것 (형식이 다름, 대상이 NOT NULL 인데 원본에 없음 …)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// 원본·대상 열을 이름으로 맞춘다 (대상 열 순서, 원본에만 있는 것은 뒤에)
pub fn plan_columns(src: &[ColumnDesc], dst: &[ColumnDesc]) -> Vec<ColPlan> {
    let mut out = Vec::new();
    for d in dst {
        let s = src.iter().find(|s| s.name == d.name);
        let plan = match s {
            None => ColPlan {
                name: d.name.clone(),
                src_type: None,
                dst_type: Some(d.data_type.clone()),
                status: "dst_only".into(),
                kind: kind_of(&d.data_type),
                note: (!d.nullable && d.default.is_none()).then(|| "대상은 NOT NULL 인데 원본에 이 열이 없습니다 — 넣을 때 실패합니다".to_string()),
            },
            Some(s) => {
                let (ks, kd) = (kind_of(&s.data_type), kind_of(&d.data_type));
                let unsupported = ks == ValueKind::Unsupported || kd == ValueKind::Unsupported;
                let note = if unsupported {
                    Some(format!("{} 형식은 옮길 수 없습니다", if ks == ValueKind::Unsupported { &s.data_type } else { &d.data_type }))
                } else if ks == ValueKind::Lob {
                    Some("32K 를 넘는 값은 넣을 때 실패할 수 있습니다".into())
                } else if family(ks) != family(kd) {
                    Some(format!("형식이 다릅니다 ({} → {}) — Oracle 이 바꿔 넣습니다", s.data_type, d.data_type))
                } else if s.data_type != d.data_type && ks == ValueKind::Text {
                    Some(format!("길이가 다를 수 있습니다 ({} → {})", s.data_type, d.data_type))
                } else {
                    None
                };
                ColPlan {
                    name: d.name.clone(),
                    src_type: Some(s.data_type.clone()),
                    dst_type: Some(d.data_type.clone()),
                    status: if unsupported { "unsupported" } else { "copy" }.into(),
                    kind: ks,
                    note,
                }
            }
        };
        out.push(plan);
    }
    for s in src.iter().filter(|s| !dst.iter().any(|d| d.name == s.name)) {
        out.push(ColPlan { name: s.name.clone(), src_type: Some(s.data_type.clone()), dst_type: None, status: "src_only".into(), kind: kind_of(&s.data_type), note: None });
    }
    out
}

fn family(k: ValueKind) -> u8 {
    match k {
        ValueKind::Text | ValueKind::Lob => 0,
        ValueKind::Number => 1,
        ValueKind::Date | ValueKind::Timestamp | ValueKind::TimestampTz => 2,
        ValueKind::IntervalDs | ValueKind::IntervalYm => 3,
        ValueKind::Raw => 4,
        ValueKind::Unsupported => 5,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// INSERT 만
    Append,
    /// 대상에서 같은 WHERE 의 행을 지우고 INSERT
    Replace,
    /// 키가 같으면 UPDATE, 없으면 INSERT
    Merge,
}

fn where_clause(cond: Option<&str>) -> String {
    match cond.map(str::trim).filter(|c| !c.is_empty()) {
        Some(c) => format!(" WHERE {}", c.trim_end_matches(';')),
        None => String::new(),
    }
}

/// 옮길 열만 (copy)
pub fn copied<'a>(cols: &'a [ColPlan], picked: &[String]) -> Vec<&'a ColPlan> {
    cols.iter().filter(|c| c.status == "copy" && (picked.is_empty() || picked.contains(&c.name))).collect()
}

/// 원본에서 읽는 SELECT
pub fn select_sql(owner: &str, table: &str, cols: &[&ColPlan], cond: Option<&str>) -> String {
    let list: Vec<String> = cols.iter().map(|c| format!("{} {}", read_expr(&c.name, c.kind), q(&c.name))).collect();
    format!("SELECT {} FROM {}{}", list.join(", "), qualified(owner, table), where_clause(cond))
}

pub fn count_sql(owner: &str, table: &str, cond: Option<&str>) -> String {
    format!("SELECT COUNT(*) FROM {}{}", qualified(owner, table), where_clause(cond))
}

pub fn delete_sql(owner: &str, table: &str, cond: Option<&str>) -> String {
    format!("DELETE FROM {}{}", qualified(owner, table), where_clause(cond))
}

pub fn insert_sql(owner: &str, table: &str, cols: &[&ColPlan]) -> String {
    let names: Vec<String> = cols.iter().map(|c| q(&c.name)).collect();
    let vals: Vec<String> = cols.iter().enumerate().map(|(i, c)| write_expr(&format!(":{}", i + 1), c.kind)).collect();
    format!("INSERT INTO {} ({}) VALUES ({})", qualified(owner, table), names.join(", "), vals.join(", "))
}

/// 키가 같으면 나머지 열을 고치고, 없으면 넣는다. 열이 모두 키면 넣기만
pub fn merge_sql(owner: &str, table: &str, cols: &[&ColPlan], keys: &[String]) -> String {
    let using: Vec<String> = cols.iter().enumerate().map(|(i, c)| format!("{} {}", write_expr(&format!(":{}", i + 1), c.kind), q(&c.name))).collect();
    let on: Vec<String> = keys.iter().map(|k| format!("t.{} = s.{}", q(k), q(k))).collect();
    let set: Vec<String> = cols.iter().filter(|c| !keys.contains(&c.name)).map(|c| format!("t.{} = s.{}", q(&c.name), q(&c.name))).collect();
    let names: Vec<String> = cols.iter().map(|c| q(&c.name)).collect();
    let vals: Vec<String> = cols.iter().map(|c| format!("s.{}", q(&c.name))).collect();
    let mut sql = format!("MERGE INTO {} t USING (SELECT {} FROM dual) s ON ({})", qualified(owner, table), using.join(", "), on.join(" AND "));
    if !set.is_empty() {
        sql.push_str(&format!(" WHEN MATCHED THEN UPDATE SET {}", set.join(", ")));
    }
    sql.push_str(&format!(" WHEN NOT MATCHED THEN INSERT ({}) VALUES ({})", names.join(", "), vals.join(", ")));
    sql
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(n: &str, t: &str, nullable: bool) -> ColumnDesc {
        ColumnDesc { name: n.into(), data_type: t.into(), nullable, default: None, comment: None }
    }

    #[test]
    fn kinds() {
        assert_eq!(kind_of("NUMBER(10,2)"), ValueKind::Number);
        assert_eq!(kind_of("VARCHAR2(30 CHAR)"), ValueKind::Text);
        assert_eq!(kind_of("DATE"), ValueKind::Date);
        assert_eq!(kind_of("TIMESTAMP(6)"), ValueKind::Timestamp);
        assert_eq!(kind_of("TIMESTAMP(6) WITH TIME ZONE"), ValueKind::TimestampTz);
        assert_eq!(kind_of("TIMESTAMP(6) WITH LOCAL TIME ZONE"), ValueKind::Timestamp);
        assert_eq!(kind_of("INTERVAL DAY(2) TO SECOND(6)"), ValueKind::IntervalDs);
        assert_eq!(kind_of("RAW(16)"), ValueKind::Raw);
        assert_eq!(kind_of("CLOB"), ValueKind::Lob);
        assert_eq!(kind_of("BLOB"), ValueKind::Unsupported);
        assert_eq!(kind_of("XMLTYPE"), ValueKind::Unsupported);
    }

    #[test]
    fn plan_and_sql() {
        let src = vec![c("ID", "NUMBER(10)", false), c("NM", "VARCHAR2(30)", true), c("HIRED", "DATE", true), c("PIC", "BLOB", true), c("OLD", "CHAR(1)", true)];
        let dst = vec![c("ID", "NUMBER(10)", false), c("NM", "VARCHAR2(10)", true), c("HIRED", "TIMESTAMP(6)", true), c("PIC", "BLOB", true), c("NEWC", "VARCHAR2(1)", false)];
        let p = plan_columns(&src, &dst);
        let st: Vec<(&str, &str)> = p.iter().map(|x| (x.name.as_str(), x.status.as_str())).collect();
        assert_eq!(st, vec![("ID", "copy"), ("NM", "copy"), ("HIRED", "copy"), ("PIC", "unsupported"), ("NEWC", "dst_only"), ("OLD", "src_only")]);
        assert!(p[1].note.as_deref().unwrap().contains("길이"));
        assert!(p[4].note.as_deref().unwrap().contains("NOT NULL"));
        // 같은 계열(DATE → TIMESTAMP)은 알릴 것 없음
        assert_eq!(p[2].note, None);
        let cols = copied(&p, &[]);
        assert_eq!(cols.len(), 3);
        assert_eq!(select_sql("QA", "EMP", &cols, Some("deptno = 10;")),
            "SELECT \"ID\" \"ID\", \"NM\" \"NM\", TO_CHAR(\"HIRED\", 'YYYY-MM-DD HH24:MI:SS') \"HIRED\" FROM \"QA\".\"EMP\" WHERE deptno = 10");
        assert_eq!(insert_sql("DEV", "EMP", &cols),
            "INSERT INTO \"DEV\".\"EMP\" (\"ID\", \"NM\", \"HIRED\") VALUES (:1, :2, TO_DATE(:3, 'YYYY-MM-DD HH24:MI:SS'))");
        assert_eq!(delete_sql("DEV", "EMP", Some("  ")), "DELETE FROM \"DEV\".\"EMP\"");
        assert_eq!(merge_sql("DEV", "EMP", &cols, &["ID".into()]),
            "MERGE INTO \"DEV\".\"EMP\" t USING (SELECT :1 \"ID\", :2 \"NM\", TO_DATE(:3, 'YYYY-MM-DD HH24:MI:SS') \"HIRED\" FROM dual) s ON (t.\"ID\" = s.\"ID\") \
WHEN MATCHED THEN UPDATE SET t.\"NM\" = s.\"NM\", t.\"HIRED\" = s.\"HIRED\" WHEN NOT MATCHED THEN INSERT (\"ID\", \"NM\", \"HIRED\") VALUES (s.\"ID\", s.\"NM\", s.\"HIRED\")");
        // 고른 열만
        assert_eq!(copied(&p, &["ID".into(), "PIC".into()]).len(), 1);
        // 열이 모두 키면 넣기만
        let only_id = copied(&p, &["ID".into()]);
        assert!(!merge_sql("DEV", "EMP", &only_id, &["ID".into()]).contains("WHEN MATCHED"));
    }
}
