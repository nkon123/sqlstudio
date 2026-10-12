//! 스키마 브라우저의 상세 탭 — 객체 하나의 인덱스·제약조건·트리거·권한·의존 관계 등.
//!
//! 탭마다 SQL 하나. 결과는 그리드와 같은 RowPage 로 돌려준다 (화면은 표로 그린다).
//! 모두 ALL_ 사전 뷰라 접속 계정이 볼 수 있는 것만 나온다. 11g(11.2) 에서 돈다 (LISTAGG 는 11.2+).

use serde::Serialize;

use crate::error::{Error, Result};
use crate::meta::norm_ident;
use crate::session::{RowPage, Session};

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Section {
    pub key: &'static str,
    pub label: &'static str,
    /// 한 행짜리(정보) — 화면이 세로로 펼친다
    pub single: bool,
}

const fn sec(key: &'static str, label: &'static str) -> Section {
    Section { key, label, single: false }
}
const fn one(key: &'static str, label: &'static str) -> Section {
    Section { key, label, single: true }
}

/// 객체 종류별 탭 (컬럼·소스는 화면이 따로 그린다)
pub fn sections_for(object_type: &str) -> Vec<Section> {
    match object_type.to_uppercase().as_str() {
        "TABLE" => vec![
            sec("indexes", "인덱스"), sec("constraints", "제약조건"), sec("referenced_by", "참조하는 테이블"),
            sec("triggers", "트리거"), sec("grants", "권한"), sec("used_by", "사용처"), sec("synonyms", "동의어"),
            sec("partitions", "파티션"), one("stats", "통계"),
        ],
        "VIEW" => vec![sec("uses", "쓰는 객체"), sec("used_by", "사용처"), sec("grants", "권한"), sec("synonyms", "동의어"), sec("triggers", "트리거")],
        "MATERIALIZED VIEW" => vec![
            one("mview", "갱신"), sec("indexes", "인덱스"), sec("uses", "쓰는 객체"), sec("used_by", "사용처"), sec("grants", "권한"), one("stats", "통계"),
        ],
        "PACKAGE" | "PACKAGE BODY" | "PROCEDURE" | "FUNCTION" | "TYPE" | "TYPE BODY" => {
            vec![sec("errors", "컴파일 오류"), sec("used_by", "사용처"), sec("uses", "쓰는 객체"), sec("grants", "권한"), sec("synonyms", "동의어")]
        }
        "TRIGGER" => vec![one("trigger", "트리거 정보"), sec("errors", "컴파일 오류"), sec("uses", "쓰는 객체")],
        "SEQUENCE" => vec![one("sequence", "시퀀스 정보"), sec("used_by", "사용처"), sec("grants", "권한"), sec("synonyms", "동의어")],
        "SYNONYM" => vec![one("synonym", "동의어 정보")],
        "INDEX" => vec![one("index", "인덱스 정보")],
        _ => vec![sec("used_by", "사용처"), sec("grants", "권한")],
    }
}

/// 제약조건·인덱스의 컬럼 목록 (11.2 LISTAGG)
const CONS_COLS: &str = "(SELECT LISTAGG(cc.column_name, ', ') WITHIN GROUP (ORDER BY cc.position) \
     FROM all_cons_columns cc WHERE cc.owner = c.owner AND cc.constraint_name = c.constraint_name)";

fn sql_of(section: &str) -> Option<&'static str> {
    Some(match section {
        "indexes" => "SELECT i.index_name \"이름\", DECODE(i.uniqueness, 'UNIQUE', '유일', '') \"유일\", \
             (SELECT LISTAGG(ic.column_name || DECODE(ic.descend, 'DESC', ' DESC'), ', ') WITHIN GROUP (ORDER BY ic.column_position) \
                FROM all_ind_columns ic WHERE ic.index_owner = i.owner AND ic.index_name = i.index_name) \"컬럼\", \
             i.index_type \"형식\", i.status \"상태\", i.tablespace_name \"테이블스페이스\", i.distinct_keys \"고유 값\", \
             TO_CHAR(i.last_analyzed, 'YYYY-MM-DD') \"통계\" \
             FROM all_indexes i WHERE i.table_owner = :o AND i.table_name = :n ORDER BY i.index_name",
        "constraints" => "SELECT c.constraint_name \"이름\", \
             DECODE(c.constraint_type, 'P', '기본 키', 'U', '유일', 'R', '외래 키', 'C', '검사', 'V', '뷰 검사', 'O', '읽기 전용', c.constraint_type) \"종류\", \
             CONS_COLS \"컬럼\", \
             (SELECT r.owner || '.' || r.table_name FROM all_constraints r WHERE r.owner = c.r_owner AND r.constraint_name = c.r_constraint_name) \"참조\", \
             c.delete_rule \"삭제 규칙\", c.status \"상태\", c.validated \"검증\", c.search_condition \"조건\" \
             FROM all_constraints c WHERE c.owner = :o AND c.table_name = :n \
             ORDER BY DECODE(c.constraint_type, 'P', 1, 'U', 2, 'R', 3, 4), c.constraint_name",
        "referenced_by" => "SELECT c.owner || '.' || c.table_name \"테이블\", c.constraint_name \"외래 키\", CONS_COLS \"컬럼\", \
             c.delete_rule \"삭제 규칙\", c.status \"상태\" \
             FROM all_constraints c JOIN all_constraints p ON p.owner = c.r_owner AND p.constraint_name = c.r_constraint_name \
             WHERE c.constraint_type = 'R' AND p.owner = :o AND p.table_name = :n ORDER BY 1, 2",
        "triggers" => "SELECT t.owner || '.' || t.trigger_name \"트리거\", t.trigger_type \"시점\", t.triggering_event \"사건\", t.status \"상태\", \
             t.when_clause \"조건\" FROM all_triggers t WHERE t.table_owner = :o AND t.table_name = :n ORDER BY 1",
        "grants" => "SELECT grantee \"받은 사람\", privilege \"권한\", grantable \"재부여\", grantor \"준 사람\" \
             FROM all_tab_privs WHERE table_schema = :o AND table_name = :n ORDER BY grantee, privilege",
        "used_by" => "SELECT owner || '.' || name \"객체\", type \"종류\" FROM all_dependencies \
             WHERE referenced_owner = :o AND referenced_name = :n AND NOT (owner = :o AND name = :n) ORDER BY type, owner, name",
        "uses" => "SELECT referenced_owner || '.' || referenced_name \"객체\", referenced_type \"종류\", referenced_link_name \"DB 링크\" \
             FROM all_dependencies WHERE owner = :o AND name = :n \
             AND NOT (referenced_owner = 'SYS' AND referenced_name IN ('STANDARD', 'DBMS_STANDARD')) \
             AND NOT (referenced_owner = :o AND referenced_name = :n) ORDER BY referenced_type, 1",
        "synonyms" => "SELECT owner || '.' || synonym_name \"동의어\", db_link \"DB 링크\" FROM all_synonyms \
             WHERE table_owner = :o AND table_name = :n ORDER BY 1",
        "partitions" => "SELECT partition_position \"#\", partition_name \"파티션\", high_value \"상한\", num_rows \"행 수\", \
             tablespace_name \"테이블스페이스\", TO_CHAR(last_analyzed, 'YYYY-MM-DD') \"통계\" \
             FROM all_tab_partitions WHERE table_owner = :o AND table_name = :n ORDER BY partition_position",
        "stats" => "SELECT num_rows \"행 수\", blocks \"블록\", avg_row_len \"평균 행 길이\", TO_CHAR(last_analyzed, 'YYYY-MM-DD HH24:MI') \"통계 수집\", \
             tablespace_name \"테이블스페이스\", partitioned \"파티션\", temporary \"임시\", logging \"로깅\", compression \"압축\", \
             TRIM(degree) \"병렬도\", iot_type \"IOT\" FROM all_tables WHERE owner = :o AND table_name = :n",
        "errors" => "SELECT type \"단위\", line \"줄\", position \"칸\", text \"오류\", attribute \"종류\" \
             FROM all_errors WHERE owner = :o AND name = :n ORDER BY type, sequence",
        "trigger" => "SELECT table_owner || '.' || table_name \"테이블\", base_object_type \"대상\", trigger_type \"시점\", \
             triggering_event \"사건\", when_clause \"조건\", referencing_names \"참조 이름\", status \"상태\" \
             FROM all_triggers WHERE owner = :o AND trigger_name = :n",
        "sequence" => "SELECT min_value \"최소\", max_value \"최대\", increment_by \"증가\", cycle_flag \"순환\", \
             order_flag \"순서 보장\", cache_size \"캐시\", last_number \"다음 값(캐시 기준)\" \
             FROM all_sequences WHERE sequence_owner = :o AND sequence_name = :n",
        "synonym" => "SELECT table_owner || '.' || table_name \"대상\", db_link \"DB 링크\" FROM all_synonyms WHERE owner = :o AND synonym_name = :n",
        "index" => "SELECT table_owner || '.' || table_name \"테이블\", uniqueness \"유일\", index_type \"형식\", status \"상태\", \
             tablespace_name \"테이블스페이스\", blevel \"높이\", leaf_blocks \"리프 블록\", distinct_keys \"고유 값\", \
             clustering_factor \"클러스터링 팩터\", TO_CHAR(last_analyzed, 'YYYY-MM-DD') \"통계\" \
             FROM all_indexes WHERE owner = :o AND index_name = :n",
        "mview" => "SELECT refresh_mode \"갱신 방식\", refresh_method \"갱신 방법\", build_mode \"생성\", \
             TO_CHAR(last_refresh_date, 'YYYY-MM-DD HH24:MI') \"마지막 갱신\", staleness \"최신 여부\", compile_state \"컴파일\" \
             FROM all_mviews WHERE owner = :o AND mview_name = :n",
        _ => return None,
    })
}

/// 탭 하나의 내용
pub async fn section(s: &Session, owner: &str, name: &str, section: &str) -> Result<RowPage> {
    let sql = sql_of(section).ok_or_else(|| Error::Driver(format!("알 수 없는 탭: {section}")))?;
    let sql = sql.replace("CONS_COLS", CONS_COLS);
    let binds = vec![("O".to_string(), Some(norm_ident(owner))), ("N".to_string(), Some(norm_ident(name)))];
    let mut page = s.query(&sql, binds, 2000).await?;
    // 통계: 세그먼트 크기 (DBA 권한이 없으면 자기 것만, 그것도 안 되면 빼고)
    if section == "stats" && !page.rows.is_empty() {
        let mb = segment_mb(s, owner, name).await;
        page.columns.push(crate::session::Column { name: "크기(MB)".into(), type_name: "NUMBER".into(), nullable: true });
        page.rows[0].push(mb);
    }
    Ok(page)
}

async fn segment_mb(s: &Session, owner: &str, name: &str) -> Option<String> {
    let (o, n) = (norm_ident(owner), norm_ident(name));
    let b = || vec![("O".to_string(), Some(o.clone())), ("N".to_string(), Some(n.clone()))];
    let q = "SELECT TO_CHAR(ROUND(SUM(bytes) / 1048576, 1)) FROM dba_segments WHERE owner = :o AND segment_name = :n";
    if let Ok(p) = s.query(q, b(), 1).await {
        return p.rows.first().and_then(|r| r[0].clone());
    }
    if o == s.info().user.to_uppercase() {
        let q = "SELECT TO_CHAR(ROUND(SUM(bytes) / 1048576, 1)) FROM user_segments WHERE segment_name = :n";
        if let Ok(p) = s.query(q, vec![("N".into(), Some(n))], 1).await {
            return p.rows.first().and_then(|r| r[0].clone());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_section_has_sql() {
        for t in ["TABLE", "VIEW", "MATERIALIZED VIEW", "PACKAGE", "PACKAGE BODY", "PROCEDURE", "FUNCTION", "TRIGGER", "SEQUENCE", "SYNONYM", "INDEX", "TYPE", "DIRECTORY"] {
            for s in sections_for(t) {
                let sql = sql_of(s.key).unwrap_or_else(|| panic!("{t}: {} 에 SQL 이 없다", s.key));
                assert!(sql.contains(":o") && sql.contains(":n"), "{}", s.key);
            }
        }
    }
}
