//! 자동완성 — 프로필마다 메타 세션 하나와 스키마 캐시 하나.
//!
//! 키 하나마다 `complete` 가 불린다. 캐시만 보고 답하므로 DB 왕복이 없다 (보통 0.1ms 안쪽).
//! 캐시에 없는 것(다른 스키마의 컬럼 등)이 필요하면 메타 세션으로 채우되, 300ms 까지만 기다린다.
//! 늦으면 지금 있는 후보로 먼저 답하고, 채운 결과는 다음 키 입력부터 쓰인다.
//!
//! 메타 세션은 작업 세션과 따로다. 사용자의 긴 쿼리가 자동완성을 막지 않고,
//! 자동완성 조회가 사용자의 트랜잭션에 끼어들지 않는다. 항상 읽기 전용이다.
//!
//! 메타 세션이 끊기면(방화벽 유휴 끊기, DB 재시작) 알아채는 대로 뒤에서 다시 접속한다.
//! 캐시는 그대로 두므로 자동완성은 끊긴 동안에도 있는 것으로 계속 답한다.

use sqls_core::sync::{MutexExt, RwLockExt};
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use serde::Serialize;
use sqls_core::complete::{self, load, Completion, Missing, SchemaCache};
use sqls_core::config::Profile;
use sqls_core::Session;
use tauri::{AppHandle, Emitter, State};

use crate::state::{AppState, ErrView};

const FILL_WAIT: Duration = Duration::from_millis(300);
/// 다시 접속 시도 사이 (실패하면 다음 요청 때까지 이만큼은 다시 시도하지 않는다)
const RETRY_GAP: Duration = Duration::from_secs(20);
const LIMIT: usize = 200;

#[derive(Clone, Serialize)]
pub struct HubStatus {
    profile: String,
    /// connecting | objects | columns | synonyms | keys | ready | reconnecting | error
    pub(crate) phase: &'static str,
    objects: usize,
    columns: usize,
    error: Option<String>,
}

pub struct MetaHub {
    profile: String,
    pub cache: RwLock<SchemaCache>,
    session: RwLock<Option<Session>>,
    /// 다시 접속할 때 쓴다 (비밀번호는 메모리에만)
    spec: sqls_core::ConnectSpec,
    /// 다시 접속은 하나씩
    reconnecting: tokio::sync::Mutex<()>,
    last_try: Mutex<Option<std::time::Instant>>,
    status: Mutex<HubStatus>,
    inflight: Mutex<HashSet<Missing>>,
    reload_pending: AtomicBool,
    app: AppHandle,
    /// 테이블 구조 조회 결과 (이름 → 언제, 결과). DDL 뒤 다시 읽을 때 비운다. 컬럼 팝업·우클릭·상세를 다시 열 때 바로
    pub describes: Mutex<std::collections::HashMap<String, (std::time::Instant, sqls_core::meta::TableDesc)>>,
}

impl MetaHub {
    fn set_phase(&self, phase: &'static str, error: Option<String>) {
        let (objects, columns) = self.cache.rd().stats();
        let st = HubStatus { profile: self.profile.clone(), phase, objects, columns, error };
        *self.status.lk() = st.clone();
        let _ = self.app.emit("completion-status", st);
    }

    /// 읽기 전용 메타 세션 (사전 조회용). 아직 접속 중이거나 끊겼으면 None — 끊겼으면 뒤에서 다시 접속을 시작한다
    pub fn session(self: &Arc<Self>) -> Option<Session> {
        let cur = self.session.rd().clone();
        match cur {
            Some(s) if !s.is_lost() => Some(s),
            _ => {
                if cur.is_some() || self.status().phase == "error" {
                    self.kick();
                }
                None
            }
        }
    }

    /// 뒤에서 다시 접속 (방금 실패했으면 잠시 쉬었다가)
    fn kick(self: &Arc<Self>) {
        if self.last_try.lk().is_some_and(|t| t.elapsed() < RETRY_GAP) {
            return;
        }
        let hub = self.clone();
        tauri::async_runtime::spawn(async move {
            hub.reconnect().await;
        });
    }

    /// 세션을 쓰기 시작한다 — 끊김 신호를 지켜보다가 다시 접속한다
    fn install(self: &Arc<Self>, s: Session) {
        let mut lost = s.lost_signal();
        *self.session.wr() = Some(s);
        let hub = self.clone();
        tauri::async_runtime::spawn(async move {
            if lost.wait_for(|v| *v).await.is_ok() {
                tracing::warn!("{}: 메타 세션이 끊겼습니다 — 다시 접속합니다", hub.profile);
                hub.reconnect().await;
            }
        });
    }

    /// 끊긴 메타 세션을 새로 연다. 이미 누가 열었으면 그것을 돌려준다. 몇 번 해 보고 안 되면 None
    pub async fn reconnect(self: &Arc<Self>) -> Option<Session> {
        let _g = self.reconnecting.lock().await;
        if let Some(s) = self.session.rd().clone().filter(|s| !s.is_lost()) {
            return Some(s);
        }
        if let Some(old) = self.session.wr().take() {
            old.abandon();
        }
        let had_cache = self.cache.rd().stats().0 > 0;
        self.set_phase("reconnecting", None);
        let mut last_err = String::new();
        for wait in [0u64, 2, 5] {
            tokio::time::sleep(Duration::from_secs(wait)).await;
            *self.last_try.lk() = Some(std::time::Instant::now());
            match tokio::time::timeout(Duration::from_secs(45), Session::connect(self.spec.clone())).await {
                Ok(Ok(s)) => {
                    tracing::info!("{}: 메타 세션 다시 접속", self.profile);
                    sqls_core::connlog::record(sqls_core::connlog::Ev { kind: "meta_ok".into(), who: "사전 조회".into(), ..Default::default() });
                    self.install(s.clone());
                    if had_cache {
                        self.set_phase("ready", None);
                    } else {
                        self.load(true).await;
                    }
                    return Some(s);
                }
                Ok(Err(e)) => last_err = e.to_string(),
                Err(_) => last_err = "45초 안에 응답 없음".into(),
            }
        }
        tracing::warn!("{}: 메타 세션 다시 접속 실패: {last_err}", self.profile);
        sqls_core::connlog::record(sqls_core::connlog::Ev {
            kind: "meta_fail".into(), who: "사전 조회".into(), code: sqls_core::connlog::code_in(&last_err), msg: last_err.clone(), ..Default::default()
        });
        self.set_phase("error", Some(format!("다시 접속하지 못했습니다: {last_err}")));
        None
    }

    /// 세션을 기다린다 (접속 중·다시 접속 중이면 그 결과까지). 끝내 없으면 None
    pub async fn wait_session(self: &Arc<Self>, limit: Duration) -> Option<Session> {
        let until = std::time::Instant::now() + limit;
        loop {
            if let Some(s) = self.session() {
                return Some(s);
            }
            if std::time::Instant::now() >= until {
                return None;
            }
            let phase = self.status().phase;
            if phase == "error" && self.last_try.lk().is_some_and(|t| t.elapsed() < RETRY_GAP) {
                return None;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    pub fn status(&self) -> HubStatus {
        self.status.lk().clone()
    }

    async fn load(self: &Arc<Self>, progressive: bool) {
        let Some(s) = self.session() else { return };
        let hub = self.clone();
        let r = load::load_user_schema(&s, &self.cache, progressive, move |p| {
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
    /// 기억해 둔 구조 (10분 안의 것)
    pub fn cached_describe(&self, name: &str) -> Option<sqls_core::meta::TableDesc> {
        let m = self.describes.lk();
        m.get(&name.to_uppercase()).filter(|(t, _)| t.elapsed() < Duration::from_secs(600)).map(|(_, d)| d.clone())
    }

    pub fn remember_describe(&self, name: &str, d: &sqls_core::meta::TableDesc) {
        let mut m = self.describes.lk();
        if m.len() > 500 {
            m.clear();
        }
        m.insert(name.to_uppercase(), (std::time::Instant::now(), d.clone()));
        // 찾은 실제 이름으로도 (동의어·별칭으로 열었어도 다음엔 바로)
        m.insert(format!("{}.{}", d.owner, d.name), (std::time::Instant::now(), d.clone()));
    }

    pub fn schedule_reload(self: &Arc<Self>) {
        self.describes.lk().clear();
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
    let mut hubs = st.hubs.lk();
    if let Some(h) = hubs.get(&key) {
        if h.status().phase != "error" {
            return h.clone();
        }
    }
    let mut spec = profile.to_spec(password.to_string());
    spec.read_only = true;
    spec.dbms_output = false;
    spec.module = "SQLStudio-Meta".into();
    spec.call_timeout = Some(Duration::from_secs(120));
    let hub = Arc::new(MetaHub {
        profile: profile.name.clone(),
        cache: RwLock::new(SchemaCache::new(&profile.user)),
        session: RwLock::new(None),
        spec: spec.clone(),
        reconnecting: tokio::sync::Mutex::new(()),
        last_try: Mutex::new(None),
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
        describes: Mutex::new(std::collections::HashMap::new()),
    });
    hubs.insert(key, hub.clone());
    let h = hub.clone();
    tauri::async_runtime::spawn(async move {
        *h.last_try.lk() = Some(std::time::Instant::now());
        match Session::connect(spec).await {
            Ok(s) => {
                h.install(s);
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

pub(crate) fn hub_for(st: &AppState, id: u64) -> Result<Arc<MetaHub>, ErrView> {
    let profile = {
        let map = st.sessions.lk();
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
            let cache = hub.cache.rd();
            complete::complete(&text, cursor, &cache, LIMIT)
        };
        if c.missing.is_empty() {
            return Ok(c);
        }
        let Some(s) = hub.session() else { return Ok(c) };
        // 이미 채우는 중인 것은 다시 묻지 않는다
        let todo: Vec<Missing> = {
            let mut inflight = hub.inflight.lk();
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
                        let mut cache = h.cache.wr();
                        for f in fills {
                            cache.apply(f);
                        }
                    }
                    Err(e) => tracing::debug!("자동완성 채우기 실패 {m:?}: {e}"),
                }
                h.inflight.lk().remove(m);
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

/// 조회 결과 컬럼 → 설명(COMMENT). 자동완성 캐시로만 (DB 왕복 없음) — 결과 그리드 머리에
#[tauri::command]
pub fn column_notes(st: State<'_, AppState>, id: u64, sql: String) -> Result<std::collections::BTreeMap<String, String>, ErrView> {
    let hub = hub_for(&st, id)?;
    let cache = hub.cache.rd();
    Ok(complete::column_notes(&sql, &cache))
}

/// 결과 열이 만들어진 길 (테이블.열까지). 캐시로 풀고, 모자라면 메타 세션으로 그 테이블들의 열을 채워 한 번 더 (우클릭할 때만)
pub async fn column_lineage(st: &AppState, id: u64, sql: &str, pos: Option<usize>, name: &str) -> Result<complete::Lineage, ErrView> {
    let hub = hub_for(st, id)?;
    let (mut best, mut missing) = complete::column_lineage(sql, pos, name, &hub.cache.rd());
    let Some(s) = hub.session() else { return Ok(best) };
    // 캐시에 열이 없는 테이블을 채우고 다시 — 채운 뒤 더 안쪽에서 또 나올 수 있어 몇 번까지 (모두 합쳐 5초)
    let mut tried: Vec<Missing> = Vec::new();
    let rounds = async {
        for _ in 0..3 {
            let todo: Vec<Missing> = missing.iter().filter(|m| !tried.contains(m)).fold(Vec::new(), |mut v, m| {
                if !v.contains(m) {
                    v.push(m.clone());
                }
                v
            });
            if todo.is_empty() {
                break;
            }
            for m in todo.iter().take(8) {
                if let Ok(fills) = load::fill(&s, m).await {
                    let mut cache = hub.cache.wr();
                    for f in fills {
                        cache.apply(f);
                    }
                }
            }
            tried.extend(todo);
            (best, missing) = complete::column_lineage(sql, pos, name, &hub.cache.rd());
        }
    };
    if tokio::time::timeout(Duration::from_secs(5), rounds).await.is_err() {
        tracing::debug!("열 출처: 열 채우기가 늦어 있는 것으로 답함");
        return Ok(complete::column_lineage(sql, pos, name, &hub.cache.rd()).0);
    }
    Ok(best)
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
