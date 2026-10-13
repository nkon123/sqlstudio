//! 스키마 비교 — 두 스키마(다른 DB 여도 된다)의 구조를 맞대고, 대상을 원본에 맞추는 스크립트를 만든다.
//!
//! 속도: 객체마다 묻지 않는다. 사전 뷰를 종류별로 한 번씩 크게 읽는다 (한쪽에 열 번 남짓).
//!       PL/SQL 소스는 DB 안에서 줄마다 해시해 더한 값으로 먼저 맞대고, 다른 것만 소스를 가져온다.
//! 안정성: 읽기만 한다 (부르는 쪽이 읽기 전용 세션을 준다). 스크립트는 실행하지 않고 돌려준다.
//!         지우는 것(DROP·DROP COLUMN)은 주석으로만 넣는다.
//!
//! 비교하지 않는 것: 저장 옵션(테이블스페이스·파티션·STORAGE), 권한, 동의어, 통계.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::session::Session;

pub const CODE_TYPES: &[&str] = &["PACKAGE", "PACKAGE BODY", "PROCEDURE", "FUNCTION", "TRIGGER", "TYPE", "TYPE BODY"];
const MAX_ROWS: usize = 5_000_000;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Col {
    pub name: String,
    /// VARCHAR2(30 CHAR) · NUMBER(10,2) …
    pub ty: String,
    pub nullable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Cons {
    pub name: String,
    /// P U R C
    pub kind: String,
    pub cols: Vec<String>,
    /// FK: 참조 테이블 (OWNER.TABLE) 과 컬럼
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r_table: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub r_cols: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delete_rule: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cond: Option<String>,
    /// 이름을 시스템이 지었다 (SYS_C…) — 이름 대신 정의로 맞춘다
    pub generated: bool,
    pub enabled: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Idx {
    pub name: String,
    pub unique: bool,
    /// 컬럼 (함수 기반이면 식, 내림차순이면 " DESC")
    pub cols: Vec<String>,
    pub kind: String,
    pub generated: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Table {
    pub name: String,
    pub temporary: bool,
    pub cols: Vec<Col>,
    pub cons: Vec<Cons>,
    pub idx: Vec<Idx>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Seq {
    pub name: String,
    pub increment: String,
    pub min: String,
    pub max: String,
    pub cycle: bool,
    pub order: bool,
    pub cache: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Code {
    pub kind: String,
    pub name: String,
    pub lines: u64,
    /// 줄 번호와 줄(끝 공백 뺌)의 ORA_HASH 를 더한 값 — 같으면 같은 소스
    pub hash: String,
    pub valid: bool,
}

/// 한 스키마의 구조
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Snapshot {
    pub owner: String,
    pub tables: BTreeMap<String, Table>,
    pub views: BTreeMap<String, String>,
    pub seqs: BTreeMap<String, Seq>,
    /// "KIND NAME" → 코드
    pub code: BTreeMap<String, Code>,
    pub ms: u64,
}

/// 읽을 종류
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Kinds {
    pub tables: bool,
    pub views: bool,
    pub seqs: bool,
    pub code: bool,
}

impl Default for Kinds {
    fn default() -> Self {
        Self { tables: true, views: true, seqs: true, code: true }
    }
}

fn cell(r: &[Option<String>], i: usize) -> String {
    r.get(i).cloned().flatten().unwrap_or_default()
}

fn opt(r: &[Option<String>], i: usize) -> Option<String> {
    r.get(i).cloned().flatten().filter(|s| !s.trim().is_empty())
}

/// 컬럼 형식을 DDL 에 쓰는 모양으로
pub fn col_type(dt: &str, len: &str, prec: &str, scale: &str, char_used: &str, char_len: &str) -> String {
    match dt {
        "VARCHAR2" | "CHAR" => {
            if char_used == "C" {
                format!("{dt}({char_len} CHAR)")
            } else {
                format!("{dt}({len})")
            }
        }
        "NVARCHAR2" | "NCHAR" => format!("{dt}({char_len})"),
        "NUMBER" => match (prec.is_empty(), scale) {
            (true, "") => "NUMBER".into(),
            (true, "0") => "NUMBER(*,0)".into(),
            (true, s) => format!("NUMBER(*,{s})"),
            (false, "" | "0") => format!("NUMBER({prec})"),
            (false, s) => format!("NUMBER({prec},{s})"),
        },
        "FLOAT" => if prec.is_empty() { "FLOAT".into() } else { format!("FLOAT({prec})") },
        "RAW" => format!("RAW({len})"),
        "UROWID" if !len.is_empty() => format!("UROWID({len})"),
        _ => dt.to_string(),
    }
}

/// 기본값 비교용: 앞뒤 공백·줄바꿈을 걷고, 따옴표 밖이면 대소문자를 가리지 않는다
fn norm_default(d: &Option<String>) -> Option<String> {
    let s = d.as_deref()?.split_whitespace().collect::<Vec<_>>().join(" ");
    if s.is_empty() || s.eq_ignore_ascii_case("NULL") {
        return None;
    }
    Some(if s.contains('\'') { s } else { s.to_uppercase() })
}

fn norm_text(s: &str) -> String {
    s.lines().map(|l| l.trim_end()).collect::<Vec<_>>().join("\n").trim().to_string()
}

/// "COL" IS NOT NULL — 시스템이 만든 NOT NULL 검사 (컬럼의 NULL 여부로 이미 맞댄다)
fn is_not_null_check(c: &Cons) -> bool {
    c.kind == "C" && c.generated && c.cond.as_deref().is_some_and(|s| {
        let t = s.trim().to_uppercase();
        t.ends_with("IS NOT NULL") && !t.contains(" AND ") && !t.contains(" OR ")
    })
}

/// 모든 조회가 :o (스키마) 와 :p (이름 LIKE — 조건이 없으면 '%') 를 받는다. 이름 조건을 DB 안에서 걸어야 큰 스키마에서 빠르다
async fn q(s: &Session, sql: &str, owner: &str, like: &str) -> Result<Vec<Vec<Option<String>>>> {
    Ok(s.query(sql, vec![("O".into(), Some(owner.to_string())), ("P".into(), Some(like.to_string()))], MAX_ROWS).await?.rows)
}

/// 스키마 하나를 읽는다. name_like 가 있으면 그 이름만 (LIKE). progress 는 단계 이름
pub async fn snapshot(s: &Session, owner: &str, kinds: &Kinds, name_like: Option<&str>, progress: &(dyn Fn(&str) + Sync)) -> Result<Snapshot> {
    let started = std::time::Instant::now();
    let owner = owner.to_uppercase();
    let like = name_like.map(|p| p.trim().to_uppercase()).filter(|p| !p.is_empty() && p != "%");
    let keep = |name: &str| like.as_deref().is_none_or(|p| like_match(name, p));
    let like_sql = like.clone().unwrap_or_else(|| "%".into());
    let mut snap = Snapshot { owner: owner.clone(), ..Default::default() };

    if kinds.tables {
        progress("테이블");
        for r in q(s, "SELECT table_name, temporary FROM all_tables WHERE owner = :o AND table_name LIKE :p AND nested = 'NO' AND table_name NOT LIKE 'BIN$%' \
                        AND (iot_type IS NULL OR iot_type = 'IOT') AND secondary = 'N'", &owner, &like_sql).await? {
            let name = cell(&r, 0);
            if keep(&name) {
                snap.tables.insert(name.clone(), Table { name, temporary: cell(&r, 1) == "Y", ..Default::default() });
            }
        }
        progress("컬럼");
        for r in q(s, "SELECT c.table_name, c.column_name, c.data_type, c.data_length, c.data_precision, c.data_scale, c.char_used, c.char_length, c.nullable, c.data_default \
                         FROM all_tab_columns c WHERE c.owner = :o AND c.table_name LIKE :p ORDER BY c.table_name, c.column_id", &owner, &like_sql).await? {
            if let Some(t) = snap.tables.get_mut(&cell(&r, 0)) {
                t.cols.push(Col {
                    name: cell(&r, 1),
                    ty: col_type(&cell(&r, 2), &cell(&r, 3), &cell(&r, 4), &cell(&r, 5), &cell(&r, 6), &cell(&r, 7)),
                    nullable: cell(&r, 8) == "Y",
                    default: opt(&r, 9).map(|d| d.trim().to_string()),
                });
            }
        }
        progress("제약조건");
        let mut cons: BTreeMap<String, (String, Cons)> = BTreeMap::new();
        for r in q(s, "SELECT table_name, constraint_name, constraint_type, search_condition, generated, status, delete_rule \
                         FROM all_constraints WHERE owner = :o AND table_name LIKE :p AND constraint_type IN ('P','U','R','C') AND table_name NOT LIKE 'BIN$%'", &owner, &like_sql).await? {
            let table = cell(&r, 0);
            if !snap.tables.contains_key(&table) {
                continue;
            }
            let c = Cons {
                name: cell(&r, 1),
                kind: cell(&r, 2),
                cond: opt(&r, 3).map(|x| x.trim().to_string()),
                generated: cell(&r, 4).starts_with("GENERATED"),
                enabled: cell(&r, 5) == "ENABLED",
                delete_rule: opt(&r, 6).filter(|d| d != "NO ACTION"),
                ..Default::default()
            };
            cons.insert(c.name.clone(), (table, c));
        }
        for r in q(s, "SELECT constraint_name, column_name FROM all_cons_columns WHERE owner = :o AND table_name LIKE :p ORDER BY constraint_name, position", &owner, &like_sql).await? {
            if let Some((_, c)) = cons.get_mut(&cell(&r, 0)) {
                c.cols.push(cell(&r, 1));
            }
        }
        for r in q(s, "SELECT c.constraint_name, rc.owner, rc.table_name, cc.column_name \
                         FROM all_constraints c \
                         JOIN all_constraints rc ON rc.owner = c.r_owner AND rc.constraint_name = c.r_constraint_name \
                         JOIN all_cons_columns cc ON cc.owner = rc.owner AND cc.constraint_name = rc.constraint_name \
                        WHERE c.owner = :o AND c.table_name LIKE :p AND c.constraint_type = 'R' ORDER BY c.constraint_name, cc.position", &owner, &like_sql).await? {
            if let Some((_, c)) = cons.get_mut(&cell(&r, 0)) {
                // 같은 스키마를 가리키면 스키마를 떼어 둔다 (다른 스키마와 비교할 때 같게 보이게)
                let ro = cell(&r, 1);
                c.r_table = Some(if ro == owner { cell(&r, 2) } else { format!("{ro}.{}", cell(&r, 2)) });
                c.r_cols.push(cell(&r, 3));
            }
        }
        for (_, (table, c)) in cons {
            if is_not_null_check(&c) {
                continue;
            }
            if let Some(t) = snap.tables.get_mut(&table) {
                t.cons.push(c);
            }
        }
        progress("인덱스");
        let mut idx: BTreeMap<String, (String, Idx)> = BTreeMap::new();
        for r in q(s, "SELECT index_name, table_name, uniqueness, index_type, generated FROM all_indexes \
                        WHERE owner = :o AND table_owner = :o AND table_name LIKE :p AND index_type <> 'LOB' AND table_name NOT LIKE 'BIN$%'", &owner, &like_sql).await? {
            let table = cell(&r, 1);
            if snap.tables.contains_key(&table) {
                let i = Idx { name: cell(&r, 0), unique: cell(&r, 2) == "UNIQUE", kind: cell(&r, 3), generated: cell(&r, 4) == "Y", cols: vec![] };
                idx.insert(i.name.clone(), (table, i));
            }
        }
        let mut exprs: BTreeMap<(String, String), String> = BTreeMap::new();
        for r in q(s, "SELECT index_name, column_position, column_expression FROM all_ind_expressions WHERE index_owner = :o AND table_name LIKE :p", &owner, &like_sql).await? {
            exprs.insert((cell(&r, 0), cell(&r, 1)), cell(&r, 2).trim().to_string());
        }
        for r in q(s, "SELECT index_name, column_name, descend, column_position FROM all_ind_columns WHERE index_owner = :o AND table_name LIKE :p ORDER BY index_name, column_position", &owner, &like_sql).await? {
            if let Some((_, i)) = idx.get_mut(&cell(&r, 0)) {
                // 내림차순 인덱스는 컬럼이 식("ID")으로 저장된다 — 식이어도 DESC 를 붙인다
                let mut c = exprs.get(&(cell(&r, 0), cell(&r, 3))).cloned().unwrap_or_else(|| cell(&r, 1));
                if cell(&r, 2) == "DESC" {
                    c.push_str(" DESC");
                }
                i.cols.push(c);
            }
        }
        for (_, (table, i)) in idx {
            if let Some(t) = snap.tables.get_mut(&table) {
                t.idx.push(i);
            }
        }
    }
    if kinds.views {
        progress("뷰");
        for r in q(s, "SELECT view_name, text FROM all_views WHERE owner = :o AND view_name LIKE :p", &owner, &like_sql).await? {
            let name = cell(&r, 0);
            if keep(&name) {
                snap.views.insert(name, cell(&r, 1));
            }
        }
    }
    if kinds.seqs {
        progress("시퀀스");
        for r in q(s, "SELECT sequence_name, increment_by, min_value, max_value, cycle_flag, order_flag, cache_size FROM all_sequences WHERE sequence_owner = :o AND sequence_name LIKE :p", &owner, &like_sql).await? {
            let name = cell(&r, 0);
            if keep(&name) {
                snap.seqs.insert(name.clone(), Seq { name, increment: cell(&r, 1), min: cell(&r, 2), max: cell(&r, 3), cycle: cell(&r, 4) == "Y", order: cell(&r, 5) == "Y", cache: cell(&r, 6) });
            }
        }
    }
    if kinds.code {
        progress("소스 해시");
        let types = CODE_TYPES.iter().map(|t| format!("'{t}'")).collect::<Vec<_>>().join(",");
        // 줄 끝 공백·CR 은 빼고 (개발 PC 마다 다르다). 줄 번호를 넣어 순서가 바뀐 것도 잡는다.
        // 첫 줄(머리)은 "PROCEDURE app.x" / "procedure X" / "PACKAGE  BODY \"X\"" 처럼 적은 대로 저장되므로
        // 대문자로, 따옴표·스키마. 를 빼고, 공백을 하나로 맞춰 비교한다
        let sql = format!(
            "SELECT type, name, COUNT(*), SUM(ORA_HASH(line || ':' || \
                    CASE WHEN line = 1 THEN REGEXP_REPLACE(REPLACE(REPLACE(UPPER(RTRIM(REPLACE(text, CHR(13)), ' ' || CHR(9) || CHR(10))), '\"'), :o || '.'), '[[:space:]]+', ' ') \
                         ELSE RTRIM(REPLACE(text, CHR(13)), ' ' || CHR(9) || CHR(10)) END)) \
               FROM all_source WHERE owner = :o AND name LIKE :p AND type IN ({types}) GROUP BY type, name"
        );
        for r in q(s, &sql, &owner, &like_sql).await? {
            let (kind, name) = (cell(&r, 0), cell(&r, 1));
            if keep(&name) {
                snap.code.insert(format!("{kind} {name}"), Code { kind, name, lines: cell(&r, 2).parse().unwrap_or(0), hash: cell(&r, 3), valid: true });
            }
        }
        for r in q(s, &format!("SELECT object_type, object_name FROM all_objects WHERE owner = :o AND object_name LIKE :p AND status <> 'VALID' AND object_type IN ({types})"), &owner, &like_sql).await? {
            if let Some(c) = snap.code.get_mut(&format!("{} {}", cell(&r, 0), cell(&r, 1))) {
                c.valid = false;
            }
        }
    }
    snap.ms = started.elapsed().as_millis() as u64;
    Ok(snap)
}

/// 소스 여러 개 — "KIND NAME" → 본문 (CREATE OR REPLACE 없이). 묶어서 읽는다
pub async fn sources(s: &Session, owner: &str, keys: &[String]) -> Result<BTreeMap<String, String>> {
    let mut out: BTreeMap<String, String> = BTreeMap::new();
    for chunk in keys.chunks(100) {
        let mut binds = vec![("O".to_string(), Some(owner.to_uppercase()))];
        let mut ors = Vec::new();
        for (i, k) in chunk.iter().enumerate() {
            let (kind, name) = split_key(k);
            ors.push(format!("(type = :T{i} AND name = :N{i})"));
            binds.push((format!("T{i}"), Some(kind.to_string())));
            binds.push((format!("N{i}"), Some(name.to_string())));
        }
        let sql = format!("SELECT type, name, text FROM all_source WHERE owner = :o AND ({}) ORDER BY type, name, line", ors.join(" OR "));
        for r in s.query(&sql, binds, MAX_ROWS).await?.rows {
            out.entry(format!("{} {}", cell(&r, 0), cell(&r, 1))).or_default().push_str(&cell(&r, 2));
        }
    }
    Ok(out)
}

/// "PACKAGE BODY X" → ("PACKAGE BODY", "X")
pub fn split_key(k: &str) -> (&str, &str) {
    for t in ["PACKAGE BODY", "TYPE BODY"] {
        if let Some(n) = k.strip_prefix(t).and_then(|x| x.strip_prefix(' ')) {
            return (t, n);
        }
    }
    k.split_once(' ').unwrap_or((k, ""))
}

fn like_match(name: &str, pat: &str) -> bool {
    // % 와 _ 만 (대문자로 맞춘 뒤)
    fn go(n: &[char], p: &[char]) -> bool {
        match p.first() {
            None => n.is_empty(),
            Some('%') => (0..=n.len()).any(|i| go(&n[i..], &p[1..])),
            Some('_') => !n.is_empty() && go(&n[1..], &p[1..]),
            Some(c) => n.first() == Some(c) && go(&n[1..], &p[1..]),
        }
    }
    go(&name.chars().collect::<Vec<_>>(), &pat.chars().collect::<Vec<_>>())
}

// ── 맞대기 ─────────────────────────────────

/// 원본에만 있음(대상에 만들어야 함) · 대상에만 있음 · 다름
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    OnlySource,
    OnlyTarget,
    Changed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Item {
    /// TABLE · VIEW · SEQUENCE · PACKAGE …
    pub kind: String,
    pub name: String,
    pub status: Status,
    /// 다른 점 (사람이 읽을 줄)
    pub details: Vec<String>,
}

impl Item {
    pub fn key(&self) -> String {
        format!("{} {}", self.kind, self.name)
    }
}

/// 제약조건을 맞출 열쇠 — 시스템 이름이면 정의로
fn cons_key(c: &Cons) -> String {
    if c.generated {
        format!("{}({}){}", c.kind, c.cols.join(","), c.r_table.as_deref().map(|t| format!("→{t}({})", c.r_cols.join(","))).unwrap_or_default())
    } else {
        c.name.clone()
    }
}

fn idx_key(i: &Idx) -> String {
    if i.generated || i.name.starts_with("SYS_") {
        format!("{}({})", if i.unique { "U" } else { "N" }, i.cols.join(","))
    } else {
        i.name.clone()
    }
}

fn cons_desc(c: &Cons) -> String {
    let kind = match c.kind.as_str() {
        "P" => "PK",
        "U" => "UNIQUE",
        "R" => "FK",
        _ => "CHECK",
    };
    let mut s = format!("{kind} {}", if c.generated { "(이름 자동)".to_string() } else { c.name.clone() });
    if !c.cols.is_empty() {
        s.push_str(&format!(" ({})", c.cols.join(", ")));
    }
    if let Some(t) = &c.r_table {
        s.push_str(&format!(" → {t}({})", c.r_cols.join(", ")));
    }
    if let Some(cond) = &c.cond {
        s.push_str(&format!(" {}", cond.split_whitespace().collect::<Vec<_>>().join(" ")));
    }
    if !c.enabled {
        s.push_str(" DISABLED");
    }
    s
}

fn cons_same(a: &Cons, b: &Cons) -> bool {
    a.kind == b.kind && a.cols == b.cols && a.r_table == b.r_table && a.r_cols == b.r_cols && a.delete_rule == b.delete_rule && a.enabled == b.enabled
        && a.cond.as_deref().map(|x| x.split_whitespace().collect::<Vec<_>>().join(" ").to_uppercase())
            == b.cond.as_deref().map(|x| x.split_whitespace().collect::<Vec<_>>().join(" ").to_uppercase())
}

fn col_desc(c: &Col) -> String {
    format!("{} {}{}{}", c.name, c.ty, c.default.as_deref().map(|d| format!(" DEFAULT {}", d.trim())).unwrap_or_default(), if c.nullable { "" } else { " NOT NULL" })
}

fn table_diff(a: &Table, b: &Table) -> Vec<String> {
    let mut d = Vec::new();
    let bc: BTreeMap<&str, &Col> = b.cols.iter().map(|c| (c.name.as_str(), c)).collect();
    let ac: BTreeMap<&str, &Col> = a.cols.iter().map(|c| (c.name.as_str(), c)).collect();
    for c in &a.cols {
        match bc.get(c.name.as_str()) {
            None => d.push(format!("컬럼 추가: {}", col_desc(c))),
            Some(o) => {
                if c.ty != o.ty {
                    d.push(format!("컬럼 형식: {} {} ← {}", c.name, c.ty, o.ty));
                }
                if c.nullable != o.nullable {
                    d.push(format!("컬럼 NULL: {} {} ← {}", c.name, if c.nullable { "NULL" } else { "NOT NULL" }, if o.nullable { "NULL" } else { "NOT NULL" }));
                }
                if norm_default(&c.default) != norm_default(&o.default) {
                    d.push(format!("컬럼 기본값: {} {} ← {}", c.name, c.default.as_deref().unwrap_or("(없음)"), o.default.as_deref().unwrap_or("(없음)")));
                }
            }
        }
    }
    for c in &b.cols {
        if !ac.contains_key(c.name.as_str()) {
            d.push(format!("컬럼 대상에만: {}", col_desc(c)));
        }
    }
    let a_order: Vec<&str> = a.cols.iter().map(|c| c.name.as_str()).filter(|n| bc.contains_key(n)).collect();
    let b_order: Vec<&str> = b.cols.iter().map(|c| c.name.as_str()).filter(|n| ac.contains_key(n)).collect();
    if a_order != b_order {
        d.push("컬럼 순서가 다릅니다 (스크립트로 맞추지 않음)".into());
    }
    let bk: BTreeMap<String, &Cons> = b.cons.iter().map(|c| (cons_key(c), c)).collect();
    let ak: BTreeMap<String, &Cons> = a.cons.iter().map(|c| (cons_key(c), c)).collect();
    for (k, c) in &ak {
        match bk.get(k) {
            None => d.push(format!("제약 추가: {}", cons_desc(c))),
            Some(o) if !cons_same(c, o) => d.push(format!("제약 다름: {} ← {}", cons_desc(c), cons_desc(o))),
            _ => {}
        }
    }
    for (k, c) in &bk {
        if !ak.contains_key(k) {
            d.push(format!("제약 대상에만: {}", cons_desc(c)));
        }
    }
    // 제약이 쓰는 인덱스(PK·UNIQUE 와 이름이 같은 것)는 제약으로 맞댄다
    let cons_names: BTreeSet<&str> = a.cons.iter().chain(b.cons.iter()).filter(|c| c.kind == "P" || c.kind == "U").map(|c| c.name.as_str()).collect();
    let ib: BTreeMap<String, &Idx> = b.idx.iter().filter(|i| !cons_names.contains(i.name.as_str())).map(|i| (idx_key(i), i)).collect();
    let ia: BTreeMap<String, &Idx> = a.idx.iter().filter(|i| !cons_names.contains(i.name.as_str())).map(|i| (idx_key(i), i)).collect();
    let idesc = |i: &Idx| format!("{}{} ({})", if i.unique { "UNIQUE " } else { "" }, i.name, i.cols.join(", "));
    for (k, i) in &ia {
        match ib.get(k) {
            None => d.push(format!("인덱스 추가: {}", idesc(i))),
            Some(o) if o.unique != i.unique || o.cols != i.cols => d.push(format!("인덱스 다름: {} ← {}", idesc(i), idesc(o))),
            _ => {}
        }
    }
    for (k, i) in &ib {
        if !ia.contains_key(k) {
            d.push(format!("인덱스 대상에만: {}", idesc(i)));
        }
    }
    if a.temporary != b.temporary {
        d.push(format!("임시 테이블 여부: {} ← {}", a.temporary, b.temporary));
    }
    d
}

/// 원본(a)과 대상(b)을 맞댄다
pub fn compare(a: &Snapshot, b: &Snapshot) -> Vec<Item> {
    let mut out = Vec::new();
    let item = |kind: &str, name: &str, status, details| Item { kind: kind.into(), name: name.into(), status, details };
    for (n, t) in &a.tables {
        match b.tables.get(n) {
            None => out.push(item("TABLE", n, Status::OnlySource, vec![format!("컬럼 {}개", t.cols.len())])),
            Some(o) => {
                let d = table_diff(t, o);
                if !d.is_empty() {
                    out.push(item("TABLE", n, Status::Changed, d));
                }
            }
        }
    }
    for n in b.tables.keys().filter(|n| !a.tables.contains_key(*n)) {
        out.push(item("TABLE", n, Status::OnlyTarget, vec![]));
    }
    for (n, v) in &a.views {
        match b.views.get(n) {
            None => out.push(item("VIEW", n, Status::OnlySource, vec![])),
            Some(o) if norm_text(o).to_uppercase() != norm_text(v).to_uppercase() => out.push(item("VIEW", n, Status::Changed, vec!["정의가 다릅니다".into()])),
            _ => {}
        }
    }
    for n in b.views.keys().filter(|n| !a.views.contains_key(*n)) {
        out.push(item("VIEW", n, Status::OnlyTarget, vec![]));
    }
    for (n, s) in &a.seqs {
        match b.seqs.get(n) {
            None => out.push(item("SEQUENCE", n, Status::OnlySource, vec![])),
            Some(o) => {
                let mut d = Vec::new();
                for (what, x, y) in [("INCREMENT BY", &s.increment, &o.increment), ("MINVALUE", &s.min, &o.min), ("MAXVALUE", &s.max, &o.max), ("CACHE", &s.cache, &o.cache)] {
                    if x != y {
                        d.push(format!("{what}: {x} ← {y}"));
                    }
                }
                if s.cycle != o.cycle {
                    d.push(format!("CYCLE: {} ← {}", s.cycle, o.cycle));
                }
                if s.order != o.order {
                    d.push(format!("ORDER: {} ← {}", s.order, o.order));
                }
                if !d.is_empty() {
                    out.push(item("SEQUENCE", n, Status::Changed, d));
                }
            }
        }
    }
    for n in b.seqs.keys().filter(|n| !a.seqs.contains_key(*n)) {
        out.push(item("SEQUENCE", n, Status::OnlyTarget, vec![]));
    }
    for (k, c) in &a.code {
        match b.code.get(k) {
            None => out.push(item(&c.kind, &c.name, Status::OnlySource, vec![format!("{}줄", c.lines)])),
            Some(o) if o.hash != c.hash => out.push(item(&c.kind, &c.name, Status::Changed, vec![format!("소스가 다릅니다 ({}줄 ← {}줄){}", c.lines, o.lines, if o.valid { "" } else { " · 대상은 INVALID" })])),
            Some(o) if c.valid != o.valid => out.push(item(&c.kind, &c.name, Status::Changed, vec![format!("소스는 같고 상태만 다름: {} ← {}", if c.valid { "VALID" } else { "INVALID" }, if o.valid { "VALID" } else { "INVALID" })])),
            _ => {}
        }
    }
    for (k, c) in &b.code {
        if !a.code.contains_key(k) {
            out.push(item(&c.kind, &c.name, Status::OnlyTarget, vec![]));
        }
    }
    out
}

// ── 스크립트 ─────────────────────────────────

/// 이름에 따옴표가 필요하면
fn qn(n: &str) -> String {
    let plain = n.chars().next().is_some_and(|c| c.is_ascii_uppercase()) && n.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || matches!(c, '_' | '$' | '#'));
    if plain { n.to_string() } else { format!("\"{}\"", n.replace('"', "\"\"")) }
}

fn col_def(c: &Col) -> String {
    let mut s = format!("{} {}", qn(&c.name), c.ty);
    if let Some(d) = &c.default {
        s.push_str(&format!(" DEFAULT {}", d.trim()));
    }
    if !c.nullable {
        s.push_str(" NOT NULL");
    }
    s
}

fn cons_def(c: &Cons, owner: &str) -> String {
    let name = if c.generated { String::new() } else { format!("CONSTRAINT {} ", qn(&c.name)) };
    let cols = c.cols.iter().map(|x| qn(x)).collect::<Vec<_>>().join(", ");
    match c.kind.as_str() {
        "P" => format!("{name}PRIMARY KEY ({cols})"),
        "U" => format!("{name}UNIQUE ({cols})"),
        "R" => {
            let rt = c.r_table.clone().unwrap_or_default();
            let rt = if rt.contains('.') { rt } else { format!("{owner}.{}", qn(&rt)) };
            let del = c.delete_rule.as_deref().map(|d| format!(" ON DELETE {d}")).unwrap_or_default();
            format!("{name}FOREIGN KEY ({cols}) REFERENCES {rt} ({}){del}", c.r_cols.iter().map(|x| qn(x)).collect::<Vec<_>>().join(", "))
        }
        _ => format!("{name}CHECK ({})", c.cond.as_deref().unwrap_or("")),
    }
}

fn index_def(i: &Idx, table: &str, owner: &str) -> String {
    format!("CREATE {}INDEX {owner}.{} ON {owner}.{} ({});", if i.unique { "UNIQUE " } else { "" }, qn(&i.name), qn(table), i.cols.join(", "))
}

/// 원본에서 가져온 소스 (CREATE OR REPLACE 없이) → 대상 스키마에 만드는 문장.
/// 머리의 객체 이름에 대상 스키마를 붙이고 (원본 스키마가 적혀 있으면 바꾼다), 머리 몇 줄의 "원본스키마." 도 대상으로
/// (트리거의 ON APP.T 등). 본문은 건드리지 않는다.
fn code_ddl(kind: &str, name: &str, text: &str, src_owner: &str, owner: &str) -> String {
    let body = text.trim_end();
    let kw: Vec<&str> = kind.split(' ').collect();
    // 머리: 종류 낱말들 → 이름 (스키마. 이 붙어 있을 수 있다, 따옴표일 수 있다)
    let mut i = 0;
    let bytes: Vec<char> = body.chars().collect();
    let skip_ws = |i: &mut usize| while *i < bytes.len() && bytes[*i].is_whitespace() { *i += 1 };
    let mut ok = true;
    for w in &kw {
        skip_ws(&mut i);
        let end = (i + w.len()).min(bytes.len());
        let word: String = bytes[i..end].iter().collect();
        if !word.eq_ignore_ascii_case(w) {
            ok = false;
            break;
        }
        i = end;
    }
    let ident = |i: &mut usize| -> String {
        let start = *i;
        if bytes.get(*i) == Some(&'"') {
            *i += 1;
            while *i < bytes.len() && bytes[*i] != '"' { *i += 1 }
            *i = (*i + 1).min(bytes.len());
        } else {
            while *i < bytes.len() && (bytes[*i].is_alphanumeric() || "_$#".contains(bytes[*i])) { *i += 1 }
        }
        bytes[start..*i].iter().collect()
    };
    let mut rest = String::new();
    if ok {
        skip_ws(&mut i);
        let first = ident(&mut i);
        let mut j = i;
        skip_ws(&mut j);
        if bytes.get(j) == Some(&'.') {
            j += 1;
            skip_ws(&mut j);
            let _ = ident(&mut j);
            i = j; // 스키마.이름 → 이름만 다시 붙인다
        } else if first.is_empty() {
            ok = false;
        }
        rest = bytes[i..].iter().collect();
    }
    let mut out = if ok { format!("{kind} {owner}.{}{rest}", qn(name)) } else { body.to_string() };
    if src_owner != owner {
        // 머리(앞 다섯 줄)의 원본 스키마 참조만
        let mut lines: Vec<String> = out.lines().map(String::from).collect();
        for l in lines.iter_mut().take(5) {
            *l = replace_owner(l, src_owner, owner);
        }
        out = lines.join("\n");
    }
    format!("CREATE OR REPLACE {out}\n/\n")
}

/// 낱말 경계의 SRC. / "SRC". 를 대상 스키마로 (대소문자 무시)
fn replace_owner(line: &str, src: &str, dst: &str) -> String {
    let up = line.to_uppercase();
    let src = src.to_uppercase();
    let word = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || matches!(c, '_' | '$' | '#'));
    let mut out = String::new();
    let mut last = 0;
    let mut k = 0;
    while k < up.len() {
        let plain = format!("{src}.");
        let quoted = format!("\"{src}\".");
        let (at, len) = match (up[k..].find(&plain), up[k..].find(&quoted)) {
            (Some(a), Some(b)) if b < a => (k + b, quoted.len()),
            (Some(a), _) => (k + a, plain.len()),
            (None, Some(b)) => (k + b, quoted.len()),
            (None, None) => break,
        };
        let is_quoted = len == quoted.len() && up[at..].starts_with('"');
        let before = up[..at].chars().next_back();
        if is_quoted || (!word(before) && before != Some('"')) {
            out.push_str(&line[last..at]);
            out.push_str(&if is_quoted { format!("\"{dst}\".") } else { format!("{dst}.") });
            last = at + len;
        }
        k = at + len;
    }
    out.push_str(&line[last..]);
    out
}

/// 대상(b)을 원본(a)에 맞추는 스크립트. keys 는 고른 항목 ("KIND NAME"), sources 는 원본 소스 (코드 항목)
pub fn script(a: &Snapshot, b: &Snapshot, items: &[Item], keys: &BTreeSet<String>, src_code: &BTreeMap<String, String>, header: &str) -> String {
    let o = &b.owner;
    let mut s = format!(
        "-- {header}\n-- 대상 {o} 을 원본 {} 에 맞추는 스크립트 — 실행 전에 꼭 검토하세요.\n\
         -- 지우는 문장(DROP, DROP COLUMN)은 주석으로만 넣었습니다. 필요하면 직접 푸세요.\n\
         -- 저장 옵션(테이블스페이스·파티션)·권한·동의어는 비교하지 않았습니다.\n\n",
        a.owner
    );
    let pick: Vec<&Item> = items.iter().filter(|i| keys.contains(&i.key())).collect();
    let order = |k: &str| match k {
        "SEQUENCE" => 0,
        "TABLE" => 1,
        "TYPE" => 2,
        "VIEW" => 3,
        "PACKAGE" => 4,
        "PROCEDURE" | "FUNCTION" => 5,
        "TYPE BODY" => 6,
        "PACKAGE BODY" => 7,
        "TRIGGER" => 8,
        _ => 9,
    };
    let mut pick = pick;
    pick.sort_by_key(|i| (order(&i.kind), i.name.clone()));
    let mut fks: Vec<String> = Vec::new();
    for it in pick {
        let n = &it.name;
        s.push_str(&format!("-- ── {} {} ({}) ──\n", it.kind, n, match it.status { Status::OnlySource => "대상에 없음", Status::OnlyTarget => "대상에만 있음", Status::Changed => "다름" }));
        match (it.kind.as_str(), it.status) {
            (_, Status::OnlyTarget) => {
                s.push_str(&format!("-- DROP {} {o}.{};\n\n", it.kind, qn(n)));
            }
            ("TABLE", Status::OnlySource) => {
                let t = &a.tables[n];
                let mut lines: Vec<String> = t.cols.iter().map(col_def).collect();
                for c in t.cons.iter().filter(|c| c.kind != "R") {
                    lines.push(cons_def(c, o));
                }
                s.push_str(&format!("CREATE {}TABLE {o}.{} (\n  {}\n){};\n", if t.temporary { "GLOBAL TEMPORARY " } else { "" }, qn(n), lines.join(",\n  "), if t.temporary { " ON COMMIT DELETE ROWS" } else { "" }));
                let cn: BTreeSet<&str> = t.cons.iter().map(|c| c.name.as_str()).collect();
                for i in t.idx.iter().filter(|i| !cn.contains(i.name.as_str()) && !i.generated) {
                    s.push_str(&index_def(i, n, o));
                    s.push('\n');
                }
                for c in t.cons.iter().filter(|c| c.kind == "R") {
                    fks.push(format!("ALTER TABLE {o}.{} ADD {};", qn(n), cons_def(c, o)));
                }
                s.push('\n');
            }
            ("TABLE", Status::Changed) => {
                let (t, x) = (&a.tables[n], &b.tables[n]);
                let tq = format!("{o}.{}", qn(n));
                let xc: BTreeMap<&str, &Col> = x.cols.iter().map(|c| (c.name.as_str(), c)).collect();
                let tc: BTreeSet<&str> = t.cols.iter().map(|c| c.name.as_str()).collect();
                for c in &t.cols {
                    match xc.get(c.name.as_str()) {
                        None => {
                            if !c.nullable && c.default.is_none() {
                                s.push_str("-- 행이 있으면 NOT NULL 컬럼은 기본값 없이 추가할 수 없습니다\n");
                            }
                            s.push_str(&format!("ALTER TABLE {tq} ADD ({});\n", col_def(c)));
                        }
                        Some(y) => {
                            let mut m = Vec::new();
                            if c.ty != y.ty {
                                m.push(c.ty.clone());
                            }
                            if norm_default(&c.default) != norm_default(&y.default) {
                                m.push(format!("DEFAULT {}", c.default.as_deref().map(str::trim).unwrap_or("NULL")));
                            }
                            if c.nullable != y.nullable {
                                m.push(if c.nullable { "NULL".into() } else { "NOT NULL".into() });
                            }
                            if !m.is_empty() {
                                s.push_str(&format!("ALTER TABLE {tq} MODIFY ({} {});\n", qn(&c.name), m.join(" ")));
                            }
                        }
                    }
                }
                for c in x.cols.iter().filter(|c| !tc.contains(c.name.as_str())) {
                    s.push_str(&format!("-- ALTER TABLE {tq} DROP COLUMN {};\n", qn(&c.name)));
                }
                let xk: BTreeMap<String, &Cons> = x.cons.iter().map(|c| (cons_key(c), c)).collect();
                let tk: BTreeMap<String, &Cons> = t.cons.iter().map(|c| (cons_key(c), c)).collect();
                for (k, c) in &tk {
                    match xk.get(k) {
                        None => {
                            let line = format!("ALTER TABLE {tq} ADD {};", cons_def(c, o));
                            if c.kind == "R" { fks.push(line) } else { s.push_str(&format!("{line}\n")) }
                        }
                        Some(y) if !cons_same(c, y) => {
                            s.push_str(&format!("-- ALTER TABLE {tq} DROP CONSTRAINT {};\nALTER TABLE {tq} ADD {};\n", qn(&y.name), cons_def(c, o)));
                        }
                        _ => {}
                    }
                }
                for (k, y) in &xk {
                    if !tk.contains_key(k) {
                        s.push_str(&format!("-- ALTER TABLE {tq} DROP CONSTRAINT {};\n", qn(&y.name)));
                    }
                }
                let cons_names: BTreeSet<&str> = t.cons.iter().chain(x.cons.iter()).filter(|c| c.kind == "P" || c.kind == "U").map(|c| c.name.as_str()).collect();
                let xi: BTreeMap<String, &Idx> = x.idx.iter().filter(|i| !cons_names.contains(i.name.as_str())).map(|i| (idx_key(i), i)).collect();
                let ti: BTreeMap<String, &Idx> = t.idx.iter().filter(|i| !cons_names.contains(i.name.as_str())).map(|i| (idx_key(i), i)).collect();
                for (k, i) in &ti {
                    match xi.get(k) {
                        None if !i.generated => s.push_str(&format!("{}\n", index_def(i, n, o))),
                        Some(y) if y.unique != i.unique || y.cols != i.cols => {
                            s.push_str(&format!("DROP INDEX {o}.{};\n{}\n", qn(&y.name), index_def(i, n, o)));
                        }
                        _ => {}
                    }
                }
                for (k, y) in &xi {
                    if !ti.contains_key(k) {
                        s.push_str(&format!("-- DROP INDEX {o}.{};\n", qn(&y.name)));
                    }
                }
                if t.cols.iter().map(|c| &c.name).filter(|n| xc.contains_key(n.as_str())).ne(x.cols.iter().map(|c| &c.name).filter(|n| tc.contains(n.as_str()))) {
                    s.push_str("-- 컬럼 순서가 다릅니다 — 맞추려면 테이블을 다시 만들어야 해 스크립트에 넣지 않았습니다\n");
                }
                s.push('\n');
            }
            ("VIEW", _) => {
                s.push_str(&format!("CREATE OR REPLACE VIEW {o}.{} AS\n{};\n\n", qn(n), a.views[n].trim_end().trim_end_matches(';')));
            }
            ("SEQUENCE", Status::OnlySource) => {
                let q = &a.seqs[n];
                s.push_str(&format!(
                    "CREATE SEQUENCE {o}.{} INCREMENT BY {} MINVALUE {} MAXVALUE {} {} {} {};\n\n",
                    qn(n), q.increment, q.min, q.max,
                    if q.cache == "0" { "NOCACHE".into() } else { format!("CACHE {}", q.cache) },
                    if q.cycle { "CYCLE" } else { "NOCYCLE" }, if q.order { "ORDER" } else { "NOORDER" }
                ));
            }
            ("SEQUENCE", Status::Changed) => {
                let q = &a.seqs[n];
                s.push_str(&format!(
                    "ALTER SEQUENCE {o}.{} INCREMENT BY {} MINVALUE {} MAXVALUE {} {} {} {};\n\n",
                    qn(n), q.increment, q.min, q.max,
                    if q.cache == "0" { "NOCACHE".into() } else { format!("CACHE {}", q.cache) },
                    if q.cycle { "CYCLE" } else { "NOCYCLE" }, if q.order { "ORDER" } else { "NOORDER" }
                ));
            }
            (kind, _) => match src_code.get(&it.key()) {
                Some(text) => s.push_str(&code_ddl(kind, n, text, &a.owner, o)),
                None => s.push_str("-- 원본 소스를 읽지 못했습니다\n\n"),
            },
        }
    }
    if !fks.is_empty() {
        s.push_str("-- ── 외래 키 (테이블을 다 만든 뒤) ──\n");
        for f in fks {
            s.push_str(&f);
            s.push('\n');
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn col(n: &str, ty: &str, nullable: bool, d: Option<&str>) -> Col {
        Col { name: n.into(), ty: ty.into(), nullable, default: d.map(String::from) }
    }

    fn snaps() -> (Snapshot, Snapshot) {
        let mut a = Snapshot { owner: "DEV".into(), ..Default::default() };
        let mut b = Snapshot { owner: "PRD".into(), ..Default::default() };
        let pk = |name: &str, gen| Cons { name: name.into(), kind: "P".into(), cols: vec!["ID".into()], generated: gen, enabled: true, ..Default::default() };
        a.tables.insert("ORDERS".into(), Table {
            name: "ORDERS".into(),
            cols: vec![col("ID", "NUMBER(10)", false, None), col("STATUS", "VARCHAR2(20 CHAR)", false, Some("'NEW' ")), col("MEMO", "VARCHAR2(400)", true, None)],
            cons: vec![pk("SYS_C001", true), Cons { name: "ORD_CUST_FK".into(), kind: "R".into(), cols: vec!["CUST_ID".into()], r_table: Some("CUSTOMERS".into()), r_cols: vec!["ID".into()], enabled: true, ..Default::default() }],
            idx: vec![Idx { name: "SYS_C001".into(), unique: true, cols: vec!["ID".into()], kind: "NORMAL".into(), generated: true }, Idx { name: "ORD_STATUS_IX".into(), unique: false, cols: vec!["STATUS".into()], kind: "NORMAL".into(), generated: false }],
            ..Default::default()
        });
        b.tables.insert("ORDERS".into(), Table {
            name: "ORDERS".into(),
            cols: vec![col("ID", "NUMBER(10)", false, None), col("STATUS", "VARCHAR2(10 CHAR)", true, Some("'NEW'")), col("OLD_FLAG", "CHAR(1)", true, None)],
            cons: vec![pk("SYS_C777", true)],
            idx: vec![Idx { name: "SYS_C777".into(), unique: true, cols: vec!["ID".into()], kind: "NORMAL".into(), generated: true }],
            ..Default::default()
        });
        a.tables.insert("NEW_T".into(), Table { name: "NEW_T".into(), cols: vec![col("ID", "NUMBER", false, None)], cons: vec![pk("NEW_T_PK", false)], ..Default::default() });
        b.tables.insert("GONE_T".into(), Table { name: "GONE_T".into(), ..Default::default() });
        a.views.insert("V1".into(), "select id\n  from orders  ".into());
        b.views.insert("V1".into(), "SELECT ID\n  FROM ORDERS".into());
        a.seqs.insert("S1".into(), Seq { name: "S1".into(), increment: "1".into(), min: "1".into(), max: "999".into(), cache: "20".into(), ..Default::default() });
        a.code.insert("PACKAGE BODY P".into(), Code { kind: "PACKAGE BODY".into(), name: "P".into(), lines: 10, hash: "123".into(), valid: true });
        b.code.insert("PACKAGE BODY P".into(), Code { kind: "PACKAGE BODY".into(), name: "P".into(), lines: 9, hash: "456".into(), valid: false });
        (a, b)
    }

    #[test]
    fn compare_finds_what_differs() {
        let (a, b) = snaps();
        let items = compare(&a, &b);
        let get = |k: &str, n: &str| items.iter().find(|i| i.kind == k && i.name == n);
        let o = get("TABLE", "ORDERS").unwrap();
        assert_eq!(o.status, Status::Changed);
        let d = o.details.join("\n");
        assert!(d.contains("컬럼 형식: STATUS VARCHAR2(20 CHAR) ← VARCHAR2(10 CHAR)"), "{d}");
        assert!(d.contains("컬럼 NULL: STATUS NOT NULL ← NULL"), "{d}");
        assert!(!d.contains("기본값"), "공백만 다른 기본값은 같다: {d}");
        assert!(d.contains("컬럼 추가: MEMO") && d.contains("컬럼 대상에만: OLD_FLAG"), "{d}");
        assert!(!d.contains("PK"), "이름이 시스템 이름이면 정의로 맞춘다: {d}");
        assert!(d.contains("제약 추가: FK ORD_CUST_FK"), "{d}");
        assert!(d.contains("인덱스 추가: ORD_STATUS_IX"), "{d}");
        assert_eq!(get("TABLE", "NEW_T").unwrap().status, Status::OnlySource);
        assert_eq!(get("TABLE", "GONE_T").unwrap().status, Status::OnlyTarget);
        assert!(get("VIEW", "V1").is_none(), "대소문자·끝 공백만 다른 뷰는 같다");
        assert_eq!(get("SEQUENCE", "S1").unwrap().status, Status::OnlySource);
        assert!(get("PACKAGE BODY", "P").unwrap().details[0].contains("대상은 INVALID"));
    }

    #[test]
    fn script_aligns_target_safely() {
        let (a, b) = snaps();
        let items = compare(&a, &b);
        let keys: BTreeSet<String> = items.iter().map(|i| i.key()).collect();
        let mut src = BTreeMap::new();
        src.insert("PACKAGE BODY P".to_string(), "PACKAGE BODY DEV.P IS\nBEGIN NULL; END;\n".to_string());
        let s = script(&a, &b, &items, &keys, &src, "테스트");
        assert!(s.contains("ALTER TABLE PRD.ORDERS MODIFY (STATUS VARCHAR2(20 CHAR) NOT NULL);"), "{s}");
        assert!(s.contains("ALTER TABLE PRD.ORDERS ADD (MEMO VARCHAR2(400));"), "{s}");
        assert!(s.contains("-- ALTER TABLE PRD.ORDERS DROP COLUMN OLD_FLAG;"), "지우기는 주석: {s}");
        assert!(s.contains("CREATE INDEX PRD.ORD_STATUS_IX ON PRD.ORDERS (STATUS);"), "{s}");
        assert!(s.contains("CREATE TABLE PRD.NEW_T (\n  ID NUMBER NOT NULL,\n  CONSTRAINT NEW_T_PK PRIMARY KEY (ID)\n);"), "{s}");
        assert!(s.contains("-- DROP TABLE PRD.GONE_T;"), "{s}");
        assert!(s.contains("CREATE SEQUENCE PRD.S1 INCREMENT BY 1 MINVALUE 1 MAXVALUE 999 CACHE 20 NOCYCLE NOORDER;"), "{s}");
        assert!(s.contains("CREATE OR REPLACE PACKAGE BODY PRD.P IS"), "원본 스키마를 대상으로: {s}");
        assert_eq!(code_ddl("PROCEDURE", "X", "procedure x is\nbegin null; end;", "DEV", "PRD"), "CREATE OR REPLACE PROCEDURE PRD.X is\nbegin null; end;\n/\n", "스키마 없는 머리에 붙인다");
        assert_eq!(code_ddl("TRIGGER", "T", "TRIGGER dev.t BEFORE INSERT ON dev.orders\nFOR EACH ROW BEGIN NULL; END;", "DEV", "PRD"),
            "CREATE OR REPLACE TRIGGER PRD.T BEFORE INSERT ON PRD.orders\nFOR EACH ROW BEGIN NULL; END;\n/\n");
        assert_eq!(replace_owner("ON \"DEV\".\"T\" x_dev.y DEV.Z", "DEV", "PRD"), "ON \"PRD\".\"T\" x_dev.y PRD.Z");
        assert_eq!(code_ddl("PACKAGE BODY", "P", "PACKAGE  BODY \"DEV\".\"P\" AS\nEND;", "DEV", "PRD"), "CREATE OR REPLACE PACKAGE BODY PRD.P AS\nEND;\n/\n");
        let fk = s.find("ADD CONSTRAINT ORD_CUST_FK FOREIGN KEY (CUST_ID) REFERENCES PRD.CUSTOMERS (ID);").expect("FK");
        assert!(fk > s.find("CREATE TABLE PRD.NEW_T").unwrap(), "외래 키는 맨 뒤");
        assert!(s.find("SEQUENCE PRD.S1").unwrap() < s.find("TABLE PRD.NEW_T").unwrap());
    }

    #[test]
    fn helpers() {
        assert_eq!(col_type("VARCHAR2", "80", "", "", "C", "20"), "VARCHAR2(20 CHAR)");
        assert_eq!(col_type("VARCHAR2", "80", "", "", "B", "80"), "VARCHAR2(80)");
        assert_eq!(col_type("NUMBER", "22", "10", "2", "", ""), "NUMBER(10,2)");
        assert_eq!(col_type("NUMBER", "22", "", "0", "", ""), "NUMBER(*,0)");
        assert_eq!(col_type("NUMBER", "22", "", "", "", ""), "NUMBER");
        assert!(like_match("ORDER_PKG", "ORD%") && like_match("ORDERS", "ORDER_") && !like_match("XORD", "ORD%"));
        assert_eq!(split_key("PACKAGE BODY X"), ("PACKAGE BODY", "X"));
        assert_eq!(split_key("TABLE T"), ("TABLE", "T"));
        assert_eq!(qn("ORDERS"), "ORDERS");
        assert_eq!(qn("Mixed"), "\"Mixed\"");
    }
}
