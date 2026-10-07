//! 자동완성 — 프로필마다 메타 세션 하나와 스키마 캐시 하나.
//!
//! 키 하나마다 `complete` 가 불린다. 캐시만 보고 답하므로 DB 왕복이 없다 (보통 0.1ms 안쪽).
//! 캐시에 없는 것(다른 스키마의 컬럼 등)이 필요하면 메타 세션으로 채우되, 300ms 까지만 기다린다.
//! 늦으면 지금 있는 후보로 먼저 답하고, 채운 결과는 다음 키 입력부터 쓰인다.
//!
//! 메타 세션은 작업 세션과 따로다. 사용자의 긴 쿼리가 자동완성을 막지 않고,
//! 자동완성 조회가 사용자의 트랜잭션에 끼어들지 않는다. 항상 읽기 전용이다.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use serde::Serialize;
use sqls_core::complete::{self, load, Completion, Missing, SchemaCache};
use sqls_core::config::Profile;
use sqls_core::Session;
use tauri::{AppHandle, Emitter, State};
use tokio::sync::OnceCell;

use crate::state::{AppState, ErrView};

const FILL_WAIT: Duration = Duration::from_millis(300);
const LIMIT: usize = 200;

#[derive(Clone, Serialize)]
pub struct HubStatus {
    profile: String,
    /// connecting | objects | columns | synonyms | keys | ready | error
    phase: &'static str,
    objects: usize,
    columns: usize,
    error: Option<String>,
}

pub struct MetaHub {
    profile: String,
    pub cache: RwLock<SchemaCache>,
    session: OnceCell<Session>,
    status: Mutex<HubStatus>,
    inflight: Mutex<HashSet<Missing>>,
    reload_pending: AtomicBool,
    app: AppHandle,
}

impl MetaHub {
    fn set_phase(&self, phase: &'static str, error: Option<String>) {
        let (objects, columns) = self.cache.read().unwrap().stats();
        let st = HubStatus { profile: self.profile.clone(), phase, objects, columns, error };
        *self.status.lock().unwrap() = st.clone();
        let _ = self.app.emit("completion-status", st);
    }

    pub fn status(&self) -> HubStatus {
        self.status.lock().unwrap().clone()
    }

    async fn load(self: &Arc<Self>, progressive: bool) {
        let Some(s) = self.session.get() else { return };
        let hub = self.clone();
        let r = load::load_user_schema(s, &self.cache, progressive, move |p| {
            let name = match p {
                load::Phase::Objects => "objects",
                load::Phase::Columns => "columns",
                load::Phase::Synonyms => "synonyms",
                load::Phase::Keys => "keys",
                load::Phase::Done => "ready",
            };
            hub.set_phase(name, None);
        })
        .await;
        if let Err(e) = r {
            tracing::warn!("{}: 자동완성 캐시 적재 실패: {e}", self.profile);
            self.set_phase("error", Some(e.to_string()));
        }
    }

    /// DDL 뒤에 다시 읽는다. 몰아서 한 번만 (연속 DDL 스크립트).
    pub fn schedule_reload(self: &Arc<Self>) {
        if self.reload_pending.swap(true, Ordering::SeqCst) {
            return;
        }
        let hub = self.clone();
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(Duration::from_millis(1500)).await;
            hub.reload_pending.store(false, Ordering::SeqCst);
            hub.load(false).await;
        });
    }
}

/// 프로필의 허브를 돌려준다. 처음이면 메타 세션을 열고 백그라운드로 적재를 시작한다.
pub fn ensure_hub(st: &AppState, app: &AppHandle, profile: &Profile, password: &str) -> Arc<MetaHub> {
    let key = profile.name.to_uppercase();
    let mut hubs = st.hubs.lock().unwrap();
    if let Some(h) = hubs.get(&key) {
        if h.status().phase != "error" {
            return h.clone();
        }
    }
    let hub = Arc::new(MetaHub {
        profile: profile.name.clone(),
        cache: RwLock::new(SchemaCache::new(&profile.user)),
        session: OnceCell::new(),
        status: Mutex::new(HubStatus {
            profile: profile.name.clone(),
            phase: "connecting",
            objects: 0,
            columns: 0,
            error: None,
        }),
        inflight: Mutex::new(HashSet::new()),
        reload_pending: AtomicBool::new(false),
        app: app.clone(),
    });
    hubs.insert(key, hub.clone());
    let mut spec = profile.to_spec(password.to_string());
    spec.read_only = true;
    spec.dbms_output = false;
    spec.module = "SQLStudio-Meta".into();
    spec.call_timeout = Some(Duration::from_secs(120));
    let h = hub.clone();
    tauri::async_runtime::spawn(async move {
        match Session::connect(spec).await {
            Ok(s) => {
                let _ = h.session.set(s);
                h.load(true).await;
            }
            Err(e) => {
                tracing::warn!("{}: 메타 세션 접속 실패: {e}", h.profile);
                h.set_phase("error", Some(e.to_string()));
            }
        }
    });
    hub
}

fn hub_for(st: &AppState, id: u64) -> Result<Arc<MetaHub>, ErrView> {
    let profile = {
        let map = st.sessions.lock().unwrap();
        map.get(&id)
            .map(|s| s.profile.to_uppercase())
            .ok_or_else(|| ErrView::msg("session_closed", "세션이 닫혀 있습니다"))?
    };
    st.hubs
        .lock()
        .unwrap()
        .get(&profile)
        .cloned()
        .ok_or_else(|| ErrView::msg("invalid", "자동완성 정보가 없습니다"))
}

/// 자동완성. `text` 는 에디터의 일부(커서 앞뒤 수만 자)여도 되고, `cursor` 는 그 안의 바이트 위치다.
#[tauri::command]
pub async fn complete(st: State<'_, AppState>, id: u64, text: String, cursor: usize) -> Result<Completion, ErrView> {
    let hub = hub_for(&st, id)?;
    let cursor = cursor.min(text.len());
    let mut last = None;
    for _ in 0..3 {
        let c = {
            let cache = hub.cache.read().unwrap();
            complete::complete(&text, cursor, &cache, LIMIT)
        };
        if c.missing.is_empty() {
            return Ok(c);
        }
        let Some(s) = hub.session.get().cloned() else { return Ok(c) };
        // 이미 채우는 중인 것은 다시 묻지 않는다
        let todo: Vec<Missing> = {
            let mut inflight = hub.inflight.lock().unwrap();
            c.missing.iter().filter(|m| inflight.insert((*m).clone())).take(3).cloned().collect()
        };
        if todo.is_empty() {
            return Ok(c);
        }
        let h = hub.clone();
        let task = tauri::async_runtime::spawn(async move {
            for m in &todo {
                match load::fill(&s, m).await {
                    Ok(fills) => {
                        let mut cache = h.cache.write().unwrap();
                        for f in fills {
                            cache.apply(f);
                        }
                    }
                    Err(e) => tracing::debug!("자동완성 채우기 실패 {m:?}: {e}"),
                }
                h.inflight.lock().unwrap().remove(m);
            }
        });
        // 늦으면 있는 것으로 먼저 답한다 — 채우기는 뒤에서 계속된다
        if tokio::time::timeout(FILL_WAIT, task).await.is_err() {
            return Ok(c);
        }
        last = Some(c);
    }
    Ok(last.unwrap())
}

#[tauri::command]
pub fn completion_status(st: State<'_, AppState>, id: u64) -> Result<HubStatus, ErrView> {
    Ok(hub_for(&st, id)?.status())
}

/// 수동 새로 고침 (설정 화면·단축키)
#[tauri::command]
pub fn refresh_completion(st: State<'_, AppState>, id: u64) -> Result<(), ErrView> {
    hub_for(&st, id)?.schedule_reload();
    Ok(())
}

/// DDL 을 실행한 뒤 db.rs 가 부른다
pub fn after_ddl(st: &AppState, id: u64) {
    if let Ok(h) = hub_for(st, id) {
        h.schedule_reload();
    }
}
