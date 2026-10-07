//! 세션·락 모니터 명령. 조회는 프로필의 메타 세션(읽기 전용), 종료는 사람이 확인한 뒤 잠깐 쓰는 별도 세션으로.

use serde::Serialize;
use sqls_core::monitor::{self, SessionDetail, SessionRow, Wait};
use tauri::State;

use crate::complete::hub_for;
use crate::state::{AppState, ErrView};

type R<T> = Result<T, ErrView>;

#[derive(Serialize)]
pub struct Snapshot {
    sessions: Vec<SessionRow>,
    waits: Vec<Wait>,
    /// 이 탭 자신의 SID (목록에서 표시용)
    me: Option<i64>,
}

async fn meta(st: &AppState, id: u64) -> R<std::sync::Arc<crate::complete::MetaHub>> {
    let hub = hub_for(st, id)?;
    for _ in 0..50 {
        if hub.session().is_some() {
            return Ok(hub);
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    Err(ErrView::msg("session_closed", "사전 조회 세션이 아직 준비되지 않았습니다"))
}

#[tauri::command]
pub async fn monitor_snapshot(st: State<'_, AppState>, id: u64, active_only: bool, background: bool) -> R<Snapshot> {
    let hub = meta(&st, id).await?;
    let s = hub.session().unwrap();
    let sessions = monitor::sessions(s, active_only, background).await?;
    let waits = monitor::waits(s).await?;
    let me = st.session(id).ok().map(|(sess, _, _)| sess.info().sid).filter(|x| *x > 0);
    Ok(Snapshot { sessions, waits, me })
}

#[tauri::command]
pub async fn monitor_detail(st: State<'_, AppState>, id: u64, sid: i64) -> R<SessionDetail> {
    let hub = meta(&st, id).await?;
    Ok(monitor::detail(hub.session().unwrap(), sid).await?)
}

/// 세션 종료 — 화면이 확인을 받은 뒤 부른다
#[tauri::command]
pub async fn monitor_kill(st: State<'_, AppState>, id: u64, sid: i64, serial: i64, immediate: bool) -> R<String> {
    let profile = st.sessions.lock().unwrap().get(&id).map(|s| s.profile.clone()).unwrap_or_default();
    let p = st
        .cfg
        .read()
        .unwrap()
        .profile(&profile)
        .cloned()
        .ok_or_else(|| ErrView::msg("invalid", "접속 프로필을 찾을 수 없습니다"))?;
    if p.read_only {
        return Err(ErrView::msg("read_only", "읽기 전용 프로필에서는 세션을 종료할 수 없습니다"));
    }
    let pw = st
        .passwords
        .read()
        .unwrap()
        .get(&p.name.to_uppercase())
        .cloned()
        .or_else(|| p.password_from_env())
        .ok_or_else(|| ErrView::msg("password_required", "다시 접속한 뒤 시도하세요"))?;
    // 자기 자신은 막는다
    if let Ok((sess, _, _)) = st.session(id) {
        if sess.info().sid == sid {
            return Err(ErrView::msg("invalid", "지금 이 탭의 세션입니다 — 탭을 닫거나 '끊기'를 쓰세요"));
        }
    }
    Ok(monitor::kill(p.to_spec(pw), sid, serial, immediate).await?.to_string())
}
