//! 자동완성 캐시 채우기 — DB 에서 사전 정보를 한꺼번에 읽는다.
//!
//! 객체마다 묻지 않는다. 접속 사용자의 스키마는 쿼리 몇 개로 통째로 읽고(백그라운드),
//! 다른 스키마·공개 동의어 대상은 처음 쓸 때 그것만 읽는다.
//! USER_* / ALL_* 뷰만 쓰고, 11g 에 있는 컬럼만 쓴다.

use std::time::Instant;

use super::{ColInfo, ForeignKey, Missing, ObjInfo, ObjKind, SchemaCache};
use crate::error::Result;
use crate::meta::format_type;
use crate::session::Session;

const KINDS: &str = "'TABLE','VIEW','MATERIALIZED VIEW','SYNONYM','SEQUENCE','PACKAGE','PROCEDURE','FUNCTION','TYPE'";
const ALL: usize = 2_000_000;

fn cell(r: &[Option<String>], i: usize) -> String {
    r.get(i).cloned().flatten().unwrap_or_default()
}

fn bind(n: &str, v: &str) -> (String, Option<String>) {
    (n.to_string(), Some(v.to_string()))
}

/// 적재 단계 — 단계가 끝날 때마다 캐시에 반영된다 (테이블 이름은 접속 직후부터 된다)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Objects,
    Columns,
    Synonyms,
    Keys,
    Done,
}

/// 접속 사용자의 스키마 전체를 읽어 `target` 에 넣는다.
///
/// `progressive` 면 단계마다 바로 반영한다 (첫 적재). 아니면 다 읽은 뒤 한 번에 바꾼다
/// (DDL 뒤 다시 읽기 — 읽는 동안 옛 캐시가 계속 쓰인다).
/// 사전 뷰는 11g 에서 느리다. 무거운 조인(제약 × 제약 컬럼)은 DB 에서 하지 않고 여기서 한다.
pub async fn load_user_schema(
    s: &Session,
    target: &std::sync::RwLock<SchemaCache>,
    progressive: bool,
    mut on_phase: impl FnMut(Phase),
) -> Result<SchemaCache> {
    let started = Instant::now();
    let mut c = SchemaCache::new(&s.info().user);
    let user = c.user.clone();
    let publish = |c: &SchemaCache, phase: Phase, on_phase: &mut dyn FnMut(Phase)| {
        if progressive || phase == Phase::Done {
            *target.write().unwrap() = c.clone();
        }
        on_phase(phase);
    };

    // 1) 객체 이름
    let objs = s
        .query(
            &format!(
                "SELECT o.object_name, o.object_type, t.comments FROM user_objects o \
                 LEFT JOIN user_tab_comments t ON t.table_name = o.object_name \
                 WHERE o.object_type IN ({KINDS}) AND o.object_name NOT LIKE 'BIN$%'"
            ),
            vec![],
            ALL,
        )
        .await?;
    c.set_objects(
        objs.rows
            .iter()
            .filter_map(|r| {
                Some(ObjInfo {
                    owner: user.clone(),
                    name: cell(r, 0),
                    kind: ObjKind::from_oracle(&cell(r, 1))?,
                    comment: r.get(2).cloned().flatten(),
                })
            })
            .collect(),
    );
    publish(&c, Phase::Objects, &mut on_phase);

    // 2) 컬럼 — 테이블 순서대로 오므로 묶어서 넣는다
    let cols = s
        .query(
            "SELECT c.table_name, c.column_name, c.data_type, c.data_length, c.data_precision, \
                    c.data_scale, c.char_used, c.char_length, c.nullable, cc.comments \
             FROM user_tab_columns c \
             LEFT JOIN user_col_comments cc ON cc.table_name = c.table_name AND cc.column_name = c.column_name \
             WHERE c.table_name NOT LIKE 'BIN$%' \
             ORDER BY c.table_name, c.column_id",
            vec![],
            ALL,
        )
        .await?;
    let mut cur = String::new();
    let mut buf: Vec<ColInfo> = Vec::new();
    for r in &cols.rows {
        let t = cell(r, 0);
        if t != cur && !cur.is_empty() {
            c.set_columns(&user, &cur, std::mem::take(&mut buf));
        }
        cur = t;
        buf.push(col_from(r, 1));
    }
    if !cur.is_empty() {
        c.set_columns(&user, &cur, buf);
    }
    publish(&c, Phase::Columns, &mut on_phase);

    // 3) 동의어: 개인 → 공개, 스키마 목록
    let syn = s.query("SELECT synonym_name, table_owner, table_name FROM user_synonyms", vec![], ALL).await?;
    for r in &syn.rows {
        c.add_synonym(&cell(r, 0), &cell(r, 1), &cell(r, 2), false);
    }
    let pubs = s
        .query(
            "SELECT synonym_name, table_owner, table_name FROM all_synonyms \
             WHERE owner = 'PUBLIC' AND db_link IS NULL",
            vec![],
            ALL,
        )
        .await?;
    for r in &pubs.rows {
        c.add_synonym(&cell(r, 0), &cell(r, 1), &cell(r, 2), true);
    }
    let users = s.query("SELECT username FROM all_users ORDER BY username", vec![], ALL).await?;
    c.set_schemas(users.rows.iter().map(|r| cell(r, 0)).collect());
    publish(&c, Phase::Synonyms, &mut on_phase);

    // 4) PK / FK — 제약과 제약 컬럼을 따로 읽어 여기서 잇는다 (DB 쪽 조인은 몇 배 느리다)
    let cons = s
        .query(
            "SELECT constraint_name, constraint_type, table_name, r_owner, r_constraint_name \
             FROM user_constraints WHERE constraint_type IN ('P', 'R')",
            vec![],
            ALL,
        )
        .await?;
    let ccols = s
        .query(
            "SELECT constraint_name, column_name, position FROM user_cons_columns",
            vec![],
            ALL,
        )
        .await?;
    let mut cols_of: std::collections::HashMap<String, Vec<(i64, String)>> = Default::default();
    for r in &ccols.rows {
        cols_of
            .entry(cell(r, 0))
            .or_default()
            .push((cell(r, 2).parse().unwrap_or(0), cell(r, 1)));
    }
    for v in cols_of.values_mut() {
        v.sort();
    }
    let names = |k: &str| -> Vec<String> {
        cols_of.get(k).map(|v| v.iter().map(|x| x.1.clone()).collect()).unwrap_or_default()
    };
    // 제약 이름 → 테이블 (참조 대상 찾기용)
    let mut cons_table: std::collections::HashMap<String, String> = Default::default();
    for r in &cons.rows {
        if cell(r, 1) == "P" {
            let (name, table) = (cell(r, 0), cell(r, 2));
            c.set_pk(&user, &table, names(&name));
            cons_table.insert(name, table);
        }
    }
    // 다른 스키마의 PK 를 참조하는 FK 는 그 제약만 따로 읽는다 (드물다)
    let mut foreign: Vec<(String, String)> = Vec::new();
    for r in &cons.rows {
        if cell(r, 1) == "R" {
            let r_owner = cell(r, 3);
            if r_owner != user {
                foreign.push((r_owner, cell(r, 4)));
            }
        }
    }
    let mut foreign_cons: std::collections::HashMap<(String, String), (String, Vec<String>)> = Default::default();
    for (o, cn) in foreign.iter().take(200) {
        if foreign_cons.contains_key(&(o.clone(), cn.clone())) {
            continue;
        }
        if let Ok(rows) = s
            .query(
                "SELECT c.table_name, cc.column_name FROM all_constraints c \
                 JOIN all_cons_columns cc ON cc.owner = c.owner AND cc.constraint_name = c.constraint_name \
                 WHERE c.owner = :o AND c.constraint_name = :n ORDER BY cc.position",
                vec![bind("O", o), bind("N", cn)],
                100,
            )
            .await
        {
            if let Some(first) = rows.rows.first() {
                foreign_cons.insert(
                    (o.clone(), cn.clone()),
                    (cell(first, 0), rows.rows.iter().map(|r| cell(r, 1)).collect()),
                );
            }
        }
    }
    for r in &cons.rows {
        if cell(r, 1) != "R" {
            continue;
        }
        let (name, table, r_owner, r_cons) = (cell(r, 0), cell(r, 2), cell(r, 3), cell(r, 4));
        let target = if r_owner == user {
            cons_table.get(&r_cons).map(|t| (t.clone(), names(&r_cons)))
        } else {
            foreign_cons.get(&(r_owner.clone(), r_cons.clone())).cloned()
        };
        if let Some((r_table, r_cols)) = target {
            c.add_fk(ForeignKey { owner: user.clone(), table, cols: names(&name), r_owner, r_table, r_cols });
        }
    }

    // 패키지 멤버
    let members = s
        .query(
            "SELECT object_name, procedure_name FROM user_procedures \
             WHERE object_type = 'PACKAGE' AND procedure_name IS NOT NULL \
             ORDER BY object_name, subprogram_id",
            vec![],
            ALL,
        )
        .await?;
    let mut pkg = String::new();
    let mut list: Vec<String> = Vec::new();
    for r in &members.rows {
        let p = cell(r, 0);
        if p != pkg && !pkg.is_empty() {
            list.dedup();
            c.set_package_members(&user, &pkg, std::mem::take(&mut list));
        }
        pkg = p;
        list.push(cell(r, 1));
    }
    if !pkg.is_empty() {
        list.dedup();
        c.set_package_members(&user, &pkg, list);
    }
    publish(&c, Phase::Keys, &mut on_phase);
    publish(&c, Phase::Done, &mut on_phase);

    let (o, n) = c.stats();
    tracing::info!(
        "자동완성 캐시: 객체 {o}개, 컬럼 {n}개, 공개 동의어 {}개, FK {}개 ({:?})",
        pubs.rows.len(),
        c.fks.len(),
        started.elapsed()
    );
    Ok(c)
}

fn col_from(r: &[Option<String>], o: usize) -> ColInfo {
    ColInfo {
        name: cell(r, o),
        data_type: format_type(&cell(r, o + 1), &cell(r, o + 2), &cell(r, o + 3), &cell(r, o + 4), &cell(r, o + 5), &cell(r, o + 6)),
        nullable: cell(r, o + 7) == "Y",
        comment: r.get(o + 8).cloned().flatten(),
    }
}

#[allow(dead_code)]
fn group_pk(c: &mut SchemaCache, owner: &str, rows: &[Vec<Option<String>>]) {
    let mut cur = String::new();
    let mut buf = Vec::new();
    for r in rows {
        let t = cell(r, 0);
        if t != cur && !cur.is_empty() {
            c.set_pk(owner, &cur, std::mem::take(&mut buf));
        }
        cur = t;
        buf.push(cell(r, 1));
    }
    if !cur.is_empty() {
        c.set_pk(owner, &cur, buf);
    }
}

/// 자동완성이 모자라다고 한 것을 채워 온다. 결과는 캐시에 넣을 수 있는 형태로 돌려준다
/// (캐시 잠금을 DB 를 기다리는 동안 잡고 있지 않게).
pub enum Filled {
    Columns { owner: String, table: String, cols: Vec<ColInfo>, pk: Vec<String> },
    Schema { owner: String, objs: Vec<ObjInfo> },
    Package { owner: String, name: String, kind: Option<ObjKind>, members: Vec<String> },
}

pub async fn fill(s: &Session, m: &Missing) -> Result<Vec<Filled>> {
    match m {
        Missing::Columns { owner, table } => Ok(vec![columns(s, owner, table).await?]),
        Missing::Schema { owner } => {
            let rows = s
                .query(
                    &format!(
                        "SELECT object_name, object_type FROM all_objects \
                         WHERE owner = :o AND object_type IN ({KINDS}) AND object_name NOT LIKE 'BIN$%'"
                    ),
                    vec![bind("O", owner)],
                    ALL,
                )
                .await?;
            let objs = rows
                .rows
                .iter()
                .filter_map(|r| {
                    Some(ObjInfo { owner: owner.clone(), name: cell(r, 0), kind: ObjKind::from_oracle(&cell(r, 1))?, comment: None })
                })
                .collect();
            Ok(vec![Filled::Schema { owner: owner.clone(), objs }])
        }
        Missing::Package { owner, name } => {
            // 점 앞의 이름이 무엇인지부터 (동의어 대상은 테이블일 수도 있다)
            let k = s
                .query(
                    "SELECT object_type FROM all_objects WHERE owner = :o AND object_name = :n \
                     AND object_type NOT IN ('PACKAGE BODY', 'TYPE BODY')",
                    vec![bind("O", owner), bind("N", name)],
                    1,
                )
                .await?;
            let kind = k.rows.first().and_then(|r| ObjKind::from_oracle(&cell(r, 0)));
            let mut out = Vec::new();
            let mut members = Vec::new();
            match kind {
                Some(ObjKind::Package) => {
                    let r = s
                        .query(
                            "SELECT DISTINCT procedure_name FROM all_procedures \
                             WHERE owner = :o AND object_name = :n AND procedure_name IS NOT NULL",
                            vec![bind("O", owner), bind("N", name)],
                            10_000,
                        )
                        .await?;
                    members = r.rows.iter().map(|r| cell(r, 0)).collect();
                    members.sort();
                }
                Some(ObjKind::Table) | Some(ObjKind::View) => out.push(columns(s, owner, name).await?),
                _ => {}
            }
            out.push(Filled::Package { owner: owner.clone(), name: name.clone(), kind, members });
            Ok(out)
        }
    }
}

async fn columns(s: &Session, owner: &str, table: &str) -> Result<Filled> {
    let r = s
        .query(
            "SELECT c.column_name, c.data_type, c.data_length, c.data_precision, c.data_scale, \
                    c.char_used, c.char_length, c.nullable, cc.comments \
             FROM all_tab_columns c \
             LEFT JOIN all_col_comments cc ON cc.owner = c.owner AND cc.table_name = c.table_name \
                                         AND cc.column_name = c.column_name \
             WHERE c.owner = :o AND c.table_name = :t ORDER BY c.column_id",
            vec![bind("O", owner), bind("T", table)],
            5_000,
        )
        .await?;
    let pk = s
        .query(
            "SELECT cc.column_name FROM all_constraints c \
             JOIN all_cons_columns cc ON cc.owner = c.owner AND cc.constraint_name = c.constraint_name \
             WHERE c.owner = :o AND c.table_name = :t AND c.constraint_type = 'P' ORDER BY cc.position",
            vec![bind("O", owner), bind("T", table)],
            100,
        )
        .await?;
    Ok(Filled::Columns {
        owner: owner.to_string(),
        table: table.to_string(),
        cols: r.rows.iter().map(|r| col_from(r, 0)).collect(),
        pk: pk.rows.iter().map(|r| cell(r, 0)).collect(),
    })
}

impl SchemaCache {
    pub fn apply(&mut self, f: Filled) {
        match f {
            Filled::Columns { owner, table, cols, pk } => {
                // 빈 목록도 넣는다 — 없는 테이블을 계속 묻지 않게
                self.set_columns(&owner, &table, cols);
                if !pk.is_empty() {
                    self.set_pk(&owner, &table, pk);
                }
            }
            Filled::Schema { owner, objs } => self.set_schema_objects(&owner, objs),
            Filled::Package { owner, name, kind, members } => {
                if let Some(k) = kind {
                    self.set_kind(&owner, &name, k);
                }
                if kind == Some(ObjKind::Package) || kind.is_none() {
                    self.set_package_members(&owner, &name, members);
                }
            }
        }
    }
}
