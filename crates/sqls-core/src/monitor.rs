//! 세션·락 모니터 — V$SESSION, V$LOCK, V$LOCKED_OBJECT, V$TRANSACTION, V$SQL.
//!
//! 조회는 읽기 전용 세션이면 충분하다. 권한: `SELECT_CATALOG_ROLE` (또는 V_$SESSION 등 각각의 SELECT).
//! 세션 종료(`ALTER SYSTEM KILL SESSION`)는 문장만 만들어 주고, 실행은 사람이 확인한 뒤 앱이 작업 세션으로 한다.

use serde::Serialize;

use crate::error::{Error, Result};
use crate::session::Session;

fn cell(row: &[Option<String>], i: usize) -> String {
    row.get(i).cloned().flatten().unwrap_or_default()
}
fn opt(row: &[Option<String>], i: usize) -> Option<String> {
    row.get(i).cloned().flatten().filter(|s| !s.is_empty())
}
fn num(row: &[Option<String>], i: usize) -> Option<i64> {
    opt(row, i).and_then(|s| s.parse().ok())
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionRow {
    pub sid: i64,
    pub serial: i64,
    pub username: Option<String>,
    /// ACTIVE / INACTIVE / KILLED / SNIPED
    pub status: String,
    /// USER / BACKGROUND
    pub kind: String,
    pub osuser: Option<String>,
    pub machine: Option<String>,
    pub program: Option<String>,
    pub module: Option<String>,
    pub action: Option<String>,
    pub sql_id: Option<String>,
    pub prev_sql_id: Option<String>,
    pub event: Option<String>,
    pub wait_class: Option<String>,
    /// 지금 대기의 경과 초 (대기 중이 아니면 마지막 대기 시간)
    pub seconds_in_wait: Option<i64>,
    /// 마지막 호출 뒤 경과 초 (ACTIVE 면 지금 호출이 돈 시간)
    pub last_call_et: Option<i64>,
    pub blocking_session: Option<i64>,
    pub logon_time: String,
    /// 이 세션이 막고 있는 세션 수
    pub blocks: u32,
    /// 열린 트랜잭션이 있는지
    pub in_transaction: bool,
}

/// 권한이 없을 때 사람이 알아들을 말로
fn privilege_hint(e: Error) -> Error {
    match &e {
        Error::Db { code: 942, .. } | Error::Db { code: 1031, .. } => Error::Invalid(format!(
            "세션 정보를 볼 권한이 없습니다 ({e}). DBA 에게 'GRANT SELECT_CATALOG_ROLE TO 사용자' 를 요청하세요."
        )),
        _ => e,
    }
}

const SESSIONS_SQL: &str = "\
SELECT s.sid, s.serial#, s.username, s.status, s.type, s.osuser, s.machine, s.program, s.module, s.action,
       s.sql_id, s.prev_sql_id, s.event, s.wait_class, s.seconds_in_wait, s.last_call_et, s.blocking_session,
       TO_CHAR(s.logon_time, 'YYYY-MM-DD HH24:MI:SS'),
       (SELECT COUNT(*) FROM v$session b WHERE b.blocking_session = s.sid),
       CASE WHEN s.taddr IS NULL THEN 0 ELSE 1 END
  FROM v$session s
 WHERE (:bg = 'Y' OR s.type = 'USER')
   AND (:act = 'N' OR s.status = 'ACTIVE' OR EXISTS (SELECT 1 FROM v$session b WHERE b.blocking_session = s.sid))
 ORDER BY CASE WHEN s.blocking_session IS NOT NULL THEN 0 WHEN s.status = 'ACTIVE' THEN 1 ELSE 2 END,
          s.last_call_et DESC";

/// 세션 목록. `active_only` 면 ACTIVE 이거나 누군가를 막고 있는 세션만.
pub async fn sessions(s: &Session, active_only: bool, background: bool) -> Result<Vec<SessionRow>> {
    let page = s
        .query(
            SESSIONS_SQL,
            vec![
                ("BG".into(), Some(if background { "Y" } else { "N" }.into())),
                ("ACT".into(), Some(if active_only { "Y" } else { "N" }.into())),
            ],
            5000,
        )
        .await
        .map_err(privilege_hint)?;
    Ok(page
        .rows
        .iter()
        .map(|r| SessionRow {
            sid: num(r, 0).unwrap_or(0),
            serial: num(r, 1).unwrap_or(0),
            username: opt(r, 2),
            status: cell(r, 3),
            kind: cell(r, 4),
            osuser: opt(r, 5),
            machine: opt(r, 6),
            program: opt(r, 7),
            module: opt(r, 8),
            action: opt(r, 9),
            sql_id: opt(r, 10),
            prev_sql_id: opt(r, 11),
            event: opt(r, 12),
            wait_class: opt(r, 13),
            seconds_in_wait: num(r, 14),
            last_call_et: num(r, 15),
            blocking_session: num(r, 16),
            logon_time: cell(r, 17),
            blocks: num(r, 18).unwrap_or(0) as u32,
            in_transaction: num(r, 19) == Some(1),
        })
        .collect())
}

#[derive(Debug, Clone, Serialize)]
pub struct Wait {
    /// 기다리는 세션
    pub sid: i64,
    pub serial: i64,
    pub username: Option<String>,
    /// 막고 있는 세션
    pub blocker: i64,
    pub blocker_serial: Option<i64>,
    pub blocker_username: Option<String>,
    pub blocker_status: Option<String>,
    pub event: Option<String>,
    pub seconds: Option<i64>,
    /// 기다리는 행이 있는 객체 (ROW_WAIT_OBJ#)
    pub object: Option<String>,
    /// 기다리는 행의 ROWID (알 수 있으면)
    pub rowid: Option<String>,
    pub sql_id: Option<String>,
}

const WAITS_SQL: &str = "\
SELECT w.sid, w.serial#, w.username, w.blocking_session, b.serial#, b.username, b.status, w.event, w.seconds_in_wait,
       (SELECT o.owner || '.' || o.object_name FROM all_objects o WHERE o.object_id = w.row_wait_obj#),
       CASE WHEN w.row_wait_obj# > 0 AND w.row_wait_file# > 0 THEN
         (SELECT DBMS_ROWID.ROWID_CREATE(1, o.data_object_id, w.row_wait_file#, w.row_wait_block#, w.row_wait_row#)
            FROM all_objects o WHERE o.object_id = w.row_wait_obj#) END,
       w.sql_id
  FROM v$session w
  LEFT JOIN v$session b ON b.sid = w.blocking_session
 WHERE w.blocking_session IS NOT NULL
 ORDER BY w.seconds_in_wait DESC";

/// 락 대기 (누가 누구를 기다리는지)
pub async fn waits(s: &Session) -> Result<Vec<Wait>> {
    let page = s.query(WAITS_SQL, vec![], 5000).await.map_err(privilege_hint)?;
    Ok(page
        .rows
        .iter()
        .map(|r| Wait {
            sid: num(r, 0).unwrap_or(0),
            serial: num(r, 1).unwrap_or(0),
            username: opt(r, 2),
            blocker: num(r, 3).unwrap_or(0),
            blocker_serial: num(r, 4),
            blocker_username: opt(r, 5),
            blocker_status: opt(r, 6),
            event: opt(r, 7),
            seconds: num(r, 8),
            object: opt(r, 9),
            rowid: opt(r, 10),
            sql_id: opt(r, 11),
        })
        .collect())
}

#[derive(Debug, Clone, Serialize)]
pub struct LockedObject {
    pub object: String,
    pub object_type: String,
    /// 2 Row-S … 6 Exclusive
    pub mode: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionDetail {
    pub sid: i64,
    pub sql_text: Option<String>,
    pub prev_sql_text: Option<String>,
    /// 트랜잭션 시작 시각 (없으면 None)
    pub tx_start: Option<String>,
    /// 트랜잭션이 쓴 undo 블록
    pub tx_undo_blocks: Option<i64>,
    pub locked: Vec<LockedObject>,
    pub open_cursors: Option<i64>,
}

fn lock_mode(m: &str) -> String {
    match m {
        "1" => "Null",
        "2" => "Row-S (SS)",
        "3" => "Row-X (SX)",
        "4" => "Share",
        "5" => "S/Row-X (SSX)",
        "6" => "Exclusive",
        other => other,
    }
    .to_string()
}

async fn sql_text(s: &Session, sql_id: &str) -> Result<Option<String>> {
    let page = s
        .query(
            "SELECT DBMS_LOB.SUBSTR(sql_fulltext, 4000, 1) FROM v$sql WHERE sql_id = :id AND ROWNUM = 1",
            vec![("ID".into(), Some(sql_id.to_string()))],
            1,
        )
        .await
        .map_err(privilege_hint)?;
    Ok(page.rows.first().and_then(|r| opt(r, 0)))
}

/// 세션 하나의 상세: 지금·이전 SQL, 트랜잭션, 락을 잡은 객체
pub async fn detail(s: &Session, sid: i64) -> Result<SessionDetail> {
    let sid_s = sid.to_string();
    let page = s
        .query(
            "SELECT s.sql_id, s.prev_sql_id, TO_CHAR(t.start_date, 'YYYY-MM-DD HH24:MI:SS'), t.used_ublk,
                    (SELECT COUNT(*) FROM v$open_cursor c WHERE c.sid = s.sid)
               FROM v$session s LEFT JOIN v$transaction t ON t.addr = s.taddr
              WHERE s.sid = :sid",
            vec![("SID".into(), Some(sid_s.clone()))],
            1,
        )
        .await
        .map_err(privilege_hint)?;
    let Some(r) = page.rows.first() else {
        return Err(Error::Invalid(format!("세션 {sid} 가 없습니다 (이미 끝났을 수 있습니다)")));
    };
    let (sql_id, prev_id) = (opt(r, 0), opt(r, 1));
    let tx_start = opt(r, 2);
    let tx_undo_blocks = num(r, 3);
    let open_cursors = num(r, 4);
    let sql_text_v = match &sql_id {
        Some(id) => sql_text(s, id).await?,
        None => None,
    };
    let prev_sql_text = match &prev_id {
        Some(id) if Some(id) != sql_id.as_ref() => sql_text(s, id).await?,
        _ => None,
    };
    let locks = s
        .query(
            "SELECT o.owner || '.' || o.object_name, o.object_type, l.locked_mode
               FROM v$locked_object l JOIN all_objects o ON o.object_id = l.object_id
              WHERE l.session_id = :sid ORDER BY 1",
            vec![("SID".into(), Some(sid_s))],
            500,
        )
        .await
        .map_err(privilege_hint)?;
    Ok(SessionDetail {
        sid,
        sql_text: sql_text_v,
        prev_sql_text,
        tx_start,
        tx_undo_blocks,
        locked: locks.rows.iter().map(|r| LockedObject { object: cell(r, 0), object_type: cell(r, 1), mode: lock_mode(&cell(r, 2)) }).collect(),
        open_cursors,
    })
}

/// 세션을 종료한다. 사람이 확인한 뒤에만 부른다.
///
/// **잠깐 쓰는 별도 세션**으로 실행한다 — 서버가 ORA-00031(종료 표시됨)을 돌려주면 드라이버가 실행한 쪽 접속을
/// 닫아 버리므로(DPI-1080), 사용자의 작업 세션에서 실행하면 그 세션이 날아간다. ORA-00031 은 성공으로 본다.
pub async fn kill(spec: crate::session::ConnectSpec, sid: i64, serial: i64, immediate: bool) -> Result<&'static str> {
    let mut spec = spec;
    spec.read_only = false;
    spec.dbms_output = false;
    spec.module = "SQLStudio-Kill".into();
    let s = Session::connect(spec).await?;
    let r = s.execute(&kill_sql(sid, serial, immediate), crate::session::ExecOptions::default()).await;
    let out = match r {
        Ok(_) => Ok("세션을 종료했습니다"),
        Err(e) if e.to_string().contains("ORA-00031") => Ok("종료 표시됨 — 서버가 정리하는 중입니다 (진행 중이던 작업을 롤백하느라 시간이 걸릴 수 있습니다)"),
        Err(Error::Db { code: 30, .. }) => Err(Error::Invalid(format!("세션 {sid},{serial} 이 없습니다 (이미 끝났을 수 있습니다)"))),
        Err(Error::Db { code: 1031, .. }) => Err(Error::Invalid("세션을 종료할 권한이 없습니다 (ALTER SYSTEM 권한 필요)".into())),
        Err(e) => Err(e),
    };
    s.abandon();
    out
}

/// 세션 종료 문장. 실행은 사람이 확인한 뒤.
pub fn kill_sql(sid: i64, serial: i64, immediate: bool) -> String {
    format!("ALTER SYSTEM KILL SESSION '{sid},{serial}'{}", if immediate { " IMMEDIATE" } else { "" })
}

#[cfg(test)]
mod tests {
    #[test]
    fn kill() {
        assert_eq!(super::kill_sql(12, 345, true), "ALTER SYSTEM KILL SESSION '12,345' IMMEDIATE");
        assert_eq!(super::lock_mode("3"), "Row-X (SX)");
    }
}
