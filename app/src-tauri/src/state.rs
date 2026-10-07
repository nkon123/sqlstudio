//! 앱 상태 — 설정, 열린 세션, 진행 중인 AI 요청.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use serde::Serialize;
use sqls_core::config::Config;
use sqls_core::{Error, ErrorInfo, Session};
use sqls_llm::ProviderConfig;

/// 화면에서 연 세션 하나 (에디터 탭 하나에 붙는다)
pub struct OpenSession {
    pub session: Session,
    /// 접속 프로필 이름 — 자동완성 캐시를 같은 프로필의 탭끼리 나눠 쓴다
    pub profile: String,
    /// 커밋/롤백하지 않은 DML 이 있는지 — 탭을 닫을 때 묻는다
    pub txn_pending: Arc<AtomicBool>,
    /// 스크립트 실행을 멈추라는 신호
    pub stop_script: Arc<AtomicBool>,
}

pub struct AppState {
    pub cfg: RwLock<Config>,
    pub config_error: Option<String>,
    pub sessions: Mutex<HashMap<u64, OpenSession>>,
    pub next_id: AtomicU64,
    pub llm: sqls_llm::Client,
    pub providers: RwLock<Vec<ProviderConfig>>,
    /// 공급자 이름 → API 키 (메모리에만)
    pub api_keys: RwLock<HashMap<String, String>>,
    /// 프로필 이름(대문자) → 비밀번호. 같은 프로필로 새 탭을 열 때 다시 묻지 않는다. 메모리에만.
    pub passwords: RwLock<HashMap<String, String>>,
    pub ai_tasks: Mutex<HashMap<u64, tokio::task::AbortHandle>>,
    /// 프로필(대문자) → 자동완성 허브 (메타 세션 + 스키마 캐시)
    pub hubs: Mutex<HashMap<String, Arc<crate::complete::MetaHub>>>,
    /// 진행 중인 디버그
    pub debuggers: Mutex<HashMap<u64, Arc<crate::debug::DebugRun>>>,
    /// 진행 중인 PL/SQL 분석 → 멈춤 신호
    pub analyses: Mutex<HashMap<u64, Arc<AtomicBool>>>,
}

impl AppState {
    pub fn load() -> Self {
        let (cfg, config_error) = match Config::load() {
            Ok(c) => (c, None),
            // 설정이 깨져도 앱은 뜬다 — 화면에 오류를 보여 주고 고치게 한다
            Err(e) => (Config::default(), Some(e.to_string())),
        };
        if let Err(e) = sqls_core::session::init_client(cfg.oracle.client_lib_dir.clone()) {
            tracing::error!("Oracle Client 초기화 실패: {e}");
        }
        let providers = providers_from(&cfg);
        Self {
            cfg: RwLock::new(cfg),
            config_error,
            sessions: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
            llm: sqls_llm::Client::new(),
            providers: RwLock::new(providers),
            api_keys: RwLock::new(HashMap::new()),
            passwords: RwLock::new(HashMap::new()),
            hubs: Mutex::new(HashMap::new()),
            debuggers: Mutex::new(HashMap::new()),
            analyses: Mutex::new(HashMap::new()),
            ai_tasks: Mutex::new(HashMap::new()),
        }
    }

    pub fn session(&self, id: u64) -> Result<(Session, Arc<AtomicBool>, Arc<AtomicBool>), ErrView> {
        let map = self.sessions.lock().unwrap();
        let s = map.get(&id).ok_or_else(|| ErrView::msg("session_closed", "세션이 닫혀 있습니다"))?;
        Ok((s.session.clone(), s.txn_pending.clone(), s.stop_script.clone()))
    }

    pub fn next_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    pub fn close_all(&self) {
        let mut map = self.sessions.lock().unwrap();
        for (_, s) in map.drain() {
            s.session.close();
        }
    }

    pub fn save_config(&self) -> Result<(), ErrView> {
        let mut cfg = self.cfg.write().unwrap();
        cfg.llm_providers = self
            .providers
            .read()
            .unwrap()
            .iter()
            .filter_map(|p| toml::Value::try_from(p).ok())
            .collect();
        cfg.save().map_err(ErrView::from)
    }
}

/// 설정의 [[llm]] → 공급자. 하나도 없으면 로컬 Ollama 와 Claude 를 기본으로 넣는다.
fn providers_from(cfg: &Config) -> Vec<ProviderConfig> {
    let mut v: Vec<ProviderConfig> = cfg
        .llm_providers
        .iter()
        .filter_map(|t| match t.clone().try_into::<ProviderConfig>() {
            Ok(p) => Some(p),
            Err(e) => {
                tracing::warn!("[[llm]] 항목을 읽을 수 없습니다: {e}");
                None
            }
        })
        .collect();
    if v.is_empty() {
        let mut ollama = ProviderConfig::new("로컬 (Ollama)", sqls_llm::ProviderKind::Ollama, "gemma4:e2b");
        ollama.num_ctx = Some(16384);
        let mut claude = ProviderConfig::new("Claude", sqls_llm::ProviderKind::Anthropic, "claude-opus-5-5");
        claude.effort = Some("medium".into());
        v.push(ollama);
        v.push(claude);
    }
    v
}

/// 화면으로 보내는 오류
#[derive(Debug, Clone, Serialize)]
pub struct ErrView {
    pub kind: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ora_code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub offset: Option<u32>,
}

impl ErrView {
    pub fn msg(kind: &str, message: impl Into<String>) -> Self {
        Self { kind: kind.into(), message: message.into(), ora_code: None, offset: None }
    }
}

impl From<Error> for ErrView {
    fn from(e: Error) -> Self {
        let ErrorInfo { kind, message, ora_code, offset } = e.info();
        Self { kind: kind.into(), message, ora_code, offset }
    }
}

impl From<sqls_llm::Error> for ErrView {
    fn from(e: sqls_llm::Error) -> Self {
        Self::msg("llm", e.to_string())
    }
}
