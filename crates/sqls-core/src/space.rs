//! 용량 — 테이블스페이스별 사용량, 스키마의 테이블별 크기 (테이블 + 인덱스 + LOB).
//!
//! 조회는 읽기 전용 세션이면 충분하다. 권한이 있으면(`SELECT_CATALOG_ROLE` 등) DBA_ 뷰로 어느 스키마든 보고,
//! 없으면 USER_ 뷰로 접속 계정의 것만 보여 준다 (그 사실을 `note` 로 알린다).

use serde::Serialize;

use crate::error::{Error, Result};
use crate::session::Session;

/// 화면 그리드에 그대로 넣는 표
#[derive(Debug, Clone, Serialize)]
pub struct SpaceGrid {
    /// (이름, 숫자인지) — 숫자 열은 오른쪽 정렬·숫자 순 정렬
    pub columns: Vec<(String, bool)>,
    pub rows: Vec<Vec<Option<String>>>,
    /// DBA 권한이 없어 범위가 좁아졌다는 등의 안내
    pub note: Option<String>,
}

fn cols(c: &[(&str, bool)]) -> Vec<(String, bool)> {
    c.iter().map(|(n, num)| (n.to_string(), *num)).collect()
}

/// 권한 부족·뷰 없음 같은 오류면 true (그러면 USER_ 뷰로 물러난다). 접속이 끊긴 것 등은 그대로 올린다
fn is_privilege_error(e: &Error) -> bool {
    // 942 테이블·뷰 없음(권한 없음 포함), 1031 권한 부족, 1039 뷰 기초 객체 권한 부족, 904 11g 이전 열 없음
    matches!(e, Error::Db { code, .. } if matches!(*code, 942 | 1031 | 1039 | 904))
}

/// 테이블스페이스별 사용량. 영구·UNDO 는 데이터 파일과 빈 공간, TEMP 는 임시 파일로.
/// 자동 확장 파일은 최대 크기도 함께 (사용률이 낮아 보여도 한도가 가까울 수 있다)
pub async fn tablespaces(s: &Session) -> Result<SpaceGrid> {
    let sql = "SELECT t.tablespace_name, t.contents, t.status, \
                      ROUND(d.total / 1048576, 1), \
                      ROUND((d.total - NVL(f.free, 0)) / 1048576, 1), \
                      ROUND(NVL(f.free, 0) / 1048576, 1), \
                      ROUND((d.total - NVL(f.free, 0)) / NULLIF(d.total, 0) * 100, 1), \
                      ROUND(d.maxb / 1048576, 1), \
                      ROUND((d.total - NVL(f.free, 0)) / NULLIF(d.maxb, 0) * 100, 1), \
                      d.files, d.autoext \
               FROM dba_tablespaces t \
               JOIN (SELECT tablespace_name, SUM(bytes) total, \
                            SUM(GREATEST(bytes, DECODE(autoextensible, 'YES', maxbytes, bytes))) maxb, \
                            COUNT(*) files, MAX(autoextensible) autoext \
                       FROM dba_data_files GROUP BY tablespace_name) d ON d.tablespace_name = t.tablespace_name \
               LEFT JOIN (SELECT tablespace_name, SUM(bytes) free FROM dba_free_space GROUP BY tablespace_name) f \
                      ON f.tablespace_name = t.tablespace_name \
               ORDER BY 7 DESC NULLS LAST, 1";
    let columns = cols(&[
        ("테이블스페이스", false), ("종류", false), ("상태", false), ("전체(MB)", true), ("사용(MB)", true), ("남음(MB)", true),
        ("사용률(%)", true), ("최대(MB)", true), ("최대 대비(%)", true), ("파일 수", true), ("자동 확장", false),
    ]);
    match s.query(sql, vec![], 5_000).await {
        Ok(page) => {
            let mut rows = page.rows;
            // 임시 테이블스페이스 — 11g 부터의 DBA_TEMP_FREE_SPACE. 못 보면 건너뛴다
            let temp = "SELECT f.tablespace_name, 'TEMPORARY', t.status, \
                               ROUND(f.tablespace_size / 1048576, 1), ROUND(f.allocated_space / 1048576, 1), ROUND(f.free_space / 1048576, 1), \
                               ROUND(f.allocated_space / NULLIF(f.tablespace_size, 0) * 100, 1), \
                               ROUND(m.maxb / 1048576, 1), ROUND(f.allocated_space / NULLIF(m.maxb, 0) * 100, 1), m.files, m.autoext \
                        FROM dba_temp_free_space f \
                        JOIN dba_tablespaces t ON t.tablespace_name = f.tablespace_name \
                        JOIN (SELECT tablespace_name, SUM(GREATEST(bytes, DECODE(autoextensible, 'YES', maxbytes, bytes))) maxb, COUNT(*) files, MAX(autoextensible) autoext \
                                FROM dba_temp_files GROUP BY tablespace_name) m ON m.tablespace_name = f.tablespace_name";
            if let Ok(t) = s.query(temp, vec![], 1_000).await {
                rows.extend(t.rows);
            }
            Ok(SpaceGrid { columns, rows, note: None })
        }
        Err(e) if is_privilege_error(&e) => user_quotas(s).await,
        Err(e) => Err(e),
    }
}

/// DBA 권한이 없을 때 — 접속 계정에 허용된 테이블스페이스의 쿼터와 사용량
async fn user_quotas(s: &Session) -> Result<SpaceGrid> {
    let sql = "SELECT tablespace_name, \
                      ROUND(bytes / 1048576, 1), \
                      CASE WHEN max_bytes < 0 THEN NULL ELSE ROUND(max_bytes / 1048576, 1) END, \
                      CASE WHEN max_bytes < 0 THEN '무제한' ELSE TO_CHAR(ROUND(bytes / NULLIF(max_bytes, 0) * 100, 1)) END \
               FROM user_ts_quotas ORDER BY bytes DESC";
    let page = s.query(sql, vec![], 1_000).await?;
    Ok(SpaceGrid {
        columns: cols(&[("테이블스페이스", false), ("내가 쓴 크기(MB)", true), ("쿼터(MB)", true), ("쿼터 대비", false)]),
        rows: page.rows,
        note: Some("DBA 권한이 없어 접속 계정의 테이블스페이스 쿼터만 보여 줍니다 (전체 용량은 DBA_ 뷰 조회 권한이 필요합니다)".into()),
    })
}

/// 스키마의 테이블별 크기 — 테이블·인덱스·LOB 세그먼트를 테이블로 모아 합친다. 큰 것부터 `limit` 개.
/// 세그먼트가 아직 없는 테이블(11g 의 지연 생성)은 0. 파티션은 합쳐서 한 줄
pub async fn table_sizes(s: &Session, owner: Option<&str>, name_like: Option<&str>, limit: usize) -> Result<SpaceGrid> {
    let me = s.info().user.clone();
    let owner = owner.map(crate::meta::norm_ident).filter(|o| !o.is_empty()).unwrap_or_else(|| me.clone());
    let pat = name_like.map(|p| p.trim().to_uppercase()).filter(|p| !p.is_empty());
    let build = |seg_from: &str| {
        format!(
            "WITH seg AS (SELECT segment_name, segment_type, tablespace_name, bytes FROM {seg_from} \
                            AND (segment_type LIKE 'TABLE%' OR segment_type LIKE 'INDEX%' OR segment_type LIKE 'LOB%' OR segment_type = 'NESTED TABLE')) \
             SELECT t.table_name, \
                    ROUND(NVL(tb.b, 0) / 1048576, 2), ROUND(NVL(ix.b, 0) / 1048576, 2), ROUND(NVL(lb.b, 0) / 1048576, 2), \
                    ROUND((NVL(tb.b, 0) + NVL(ix.b, 0) + NVL(lb.b, 0)) / 1048576, 2), \
                    t.num_rows, TO_CHAR(t.last_analyzed, 'YYYY-MM-DD'), NVL(tb.ts, t.tablespace_name), t.partitioned \
             FROM all_tables t \
             LEFT JOIN (SELECT segment_name n, SUM(bytes) b, MAX(tablespace_name) ts FROM seg WHERE segment_type LIKE 'TABLE%' OR segment_type = 'NESTED TABLE' GROUP BY segment_name) tb \
                    ON tb.n = t.table_name \
             LEFT JOIN (SELECT i.table_name n, SUM(s.bytes) b FROM seg s JOIN all_indexes i ON i.owner = :o AND i.index_name = s.segment_name \
                         WHERE s.segment_type LIKE 'INDEX%' GROUP BY i.table_name) ix ON ix.n = t.table_name \
             LEFT JOIN (SELECT l.table_name n, SUM(s.bytes) b FROM seg s JOIN all_lobs l ON l.owner = :o AND (l.segment_name = s.segment_name OR l.index_name = s.segment_name) \
                         WHERE s.segment_type LIKE 'LOB%' GROUP BY l.table_name) lb ON lb.n = t.table_name \
             WHERE t.owner = :o AND t.table_name NOT LIKE 'BIN$%' AND (:pat IS NULL OR t.table_name LIKE :pat ESCAPE '\\') \
             ORDER BY 5 DESC NULLS LAST, 1"
        )
    };
    let binds = || vec![("O".to_string(), Some(owner.clone())), ("PAT".to_string(), pat.clone())];
    let columns = cols(&[
        ("테이블", false), ("테이블(MB)", true), ("인덱스(MB)", true), ("LOB(MB)", true), ("합계(MB)", true),
        ("행 수(통계)", true), ("통계 시각", false), ("테이블스페이스", false), ("파티션", false),
    ]);
    match s.query(&build("dba_segments WHERE owner = :o"), binds(), limit).await {
        Ok(p) => Ok(SpaceGrid { columns, rows: p.rows, note: None }),
        Err(e) if is_privilege_error(&e) => {
            // DBA 권한이 없다 — 자기 스키마는 USER_SEGMENTS 로
            if owner != me {
                return Err(Error::Invalid(format!("DBA 권한이 없어 접속 계정({me})의 테이블만 볼 수 있습니다 — 스키마를 '{me}' 로 하세요")));
            }
            let p = s.query(&build("user_segments WHERE 1 = 1"), binds(), limit).await?;
            Ok(SpaceGrid { columns, rows: p.rows, note: Some("DBA 권한이 없어 접속 계정의 스키마만 보여 줍니다".into()) })
        }
        Err(e) => Err(e),
    }
}
