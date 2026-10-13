//! LLM 공급자 — 로컬(Ollama, OpenAI 호환 서버)과 프론티어(Anthropic, OpenAI 등)를
//! 같은 모양으로 부른다.
//!
//! 모두 스트리밍이다. 로컬 모델은 답 하나에 수십 초가 걸리므로 첫 글자가 바로
//! 보여야 멈춘 것과 구분된다. 취소는 반환된 future 를 drop 하면 된다
//! (HTTP 연결이 끊기고 서버도 생성을 멈춘다).

pub mod guides;
pub mod prompts;
pub mod sse;

use std::time::Duration;

use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::sse::{LineParser, SseParser};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("LLM 서버에 연결할 수 없습니다 ({url}): {source}{hint}")]
    Connect { url: String, source: reqwest::Error, hint: String },
    #[error("LLM API 오류 {status}: {message}")]
    Api { status: u16, message: String },
    #[error("응답을 해석할 수 없습니다: {0}")]
    Protocol(String),
    #[error("설정 오류: {0}")]
    Config(String),
    #[error("응답 시간이 초과되었습니다 ({0}초 동안 아무것도 오지 않음)")]
    Idle(u64),
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    /// Ollama 네이티브 API (`/api/chat`) — num_ctx, keep_alive 를 지정할 수 있다
    Ollama,
    /// OpenAI Chat Completions 호환 (`/chat/completions`) — OpenAI, LM Studio, vLLM, llama.cpp, OpenRouter
    OpenaiCompatible,
    /// Anthropic Messages API (`/v1/messages`)
    Anthropic,
}

/// 공급자 설정 — config.toml 의 `[[llm]]` 하나
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    pub name: String,
    pub kind: ProviderKind,
    /// 비우면 종류별 기본값
    #[serde(default)]
    pub base_url: Option<String>,
    pub model: String,
    /// API 키를 읽을 환경변수 이름 (키를 파일에 쓰지 않는다)
    #[serde(default)]
    pub api_key_env: Option<String>,
    /// 직접 지정한 키 (앱이 OS 자격 증명 저장소에서 읽어 넣는다). 직렬화하지 않는다.
    #[serde(skip)]
    pub api_key: Option<String>,
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u32,
    #[serde(default)]
    pub temperature: Option<f32>,
    /// 연결 + 첫 응답까지 상한(초)
    #[serde(default = "default_connect_timeout")]
    pub connect_timeout_secs: u64,
    /// 스트림 중 아무것도 오지 않는 시간의 상한(초). 로컬 모델 로딩을 감안해 넉넉히.
    #[serde(default = "default_idle_timeout")]
    pub idle_timeout_secs: u64,
    /// Ollama: 컨텍스트 길이. 기본값(2048~4096)은 스키마 문맥을 조용히 잘라 버린다.
    #[serde(default)]
    pub num_ctx: Option<u32>,
    /// Ollama: 모델을 메모리에 붙잡아 둘 시간 (예: "30m")
    #[serde(default)]
    pub keep_alive: Option<String>,
    /// Anthropic: 생각 깊이 (low | medium | high | xhigh | max)
    #[serde(default)]
    pub effort: Option<String>,
    /// Anthropic: 안전 분류기가 거절하면 서버가 다른 모델로 이어서 답하게 한다.
    /// Claude API 직통에서만 동작한다 (사내 게이트웨이/클라우드 경유면 끈다).
    #[serde(default = "default_true")]
    pub fallbacks: bool,
    /// 외부 전송에 동의한 주소 — 사용자가 확인 창에서 "계속"을 누른 그때의 base_url.
    /// 주소가 바뀌면 다시 묻는다. 앱(백엔드)이 보내기 직전에 확인한다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_ack: Option<String>,
    /// 프록시: "auto"(기본 — 사내 주소는 직접, 그 밖은 시스템 프록시) | "system" | "none"
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy: Option<String>,
}

fn default_max_tokens() -> u32 {
    8192
}
fn default_connect_timeout() -> u64 {
    120
}
fn default_idle_timeout() -> u64 {
    300
}
fn default_true() -> bool {
    true
}

impl ProviderConfig {
    /// 종류별 기본값으로 만든다 (설정 화면의 "추가" 버튼)
    pub fn new(name: &str, kind: ProviderKind, model: &str) -> Self {
        Self {
            name: name.into(),
            kind,
            base_url: None,
            model: model.into(),
            api_key_env: None,
            api_key: None,
            max_tokens: default_max_tokens(),
            temperature: None,
            connect_timeout_secs: default_connect_timeout(),
            idle_timeout_secs: default_idle_timeout(),
            num_ctx: None,
            keep_alive: None,
            effort: None,
            fallbacks: true,
            remote_ack: None,
            proxy: None,
        }
    }

    pub fn base_url(&self) -> String {
        let def = match self.kind {
            ProviderKind::Ollama => "http://127.0.0.1:11434",
            ProviderKind::OpenaiCompatible => "https://api.openai.com/v1",
            ProviderKind::Anthropic => "https://api.anthropic.com",
        };
        let raw = self.base_url.clone().filter(|s| !s.trim().is_empty()).unwrap_or_else(|| def.to_string());
        normalize_base(self.kind, &raw)
    }

    /// 이 공급자에 시스템 프록시를 쓸지
    pub fn use_proxy(&self) -> bool {
        match self.proxy.as_deref() {
            Some("none") => false,
            Some("system") => true,
            _ => {
                // 자동: 사내 주소(루프백·사설 IP·점 없는 이름·사내 도메인)는 직접. 브라우저의 PAC 처럼
                let host = reqwest::Url::parse(&self.base_url()).ok().and_then(|u| u.host_str().map(str::to_string)).unwrap_or_default();
                !is_internal_host(&host)
            }
        }
    }

    fn key(&self) -> Option<String> {
        if let Some(k) = &self.api_key {
            return Some(k.clone());
        }
        let env = self.api_key_env.clone().or_else(|| match self.kind {
            ProviderKind::Anthropic => Some("ANTHROPIC_API_KEY".into()),
            ProviderKind::OpenaiCompatible => Some("OPENAI_API_KEY".into()),
            ProviderKind::Ollama => None,
        })?;
        std::env::var(env).ok().filter(|k| !k.is_empty())
    }

    /// 데이터가 이 PC 밖으로 나가는 공급자인지. 주소를 해석해 호스트가 루프백(127.0.0.0/8, ::1, localhost)일 때만 로컬.
    /// 글자 포함으로 보지 않는다 — "http://localhost.evil.com", "http://127.0.0.1@evil.com" 은 외부다. 못 읽는 주소도 외부로 본다.
    pub fn is_remote(&self) -> bool {
        let Ok(u) = reqwest::Url::parse(&self.base_url()) else { return true };
        let Some(host) = u.host_str() else { return true };
        if host.eq_ignore_ascii_case("localhost") {
            return false;
        }
        match host.trim_start_matches('[').trim_end_matches(']').parse::<std::net::IpAddr>() {
            Ok(ip) => !ip.is_loopback(),
            Err(_) => true,
        }
    }

    /// 외부 공급자면 지금 주소로 동의를 받았는지
    pub fn remote_ok(&self) -> bool {
        !self.is_remote() || self.remote_ack.as_deref() == Some(self.base_url().as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: String,
}

impl Message {
    pub fn user(s: impl Into<String>) -> Self {
        Self { role: Role::User, content: s.into() }
    }
    pub fn assistant(s: impl Into<String>) -> Self {
        Self { role: Role::Assistant, content: s.into() }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChatRequest {
    pub system: String,
    pub messages: Vec<Message>,
    /// 답을 이 JSON 스키마로 묶는다 (Ollama `format`, OpenAI `response_format`, Anthropic `output_config.format`).
    /// 서버가 지원하지 않으면 400 이 온다 — 부르는 쪽이 스키마 없이 다시 보낸다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub json_schema: Option<Value>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct ChatResponse {
    pub text: String,
    /// end_turn / stop / length / max_tokens / refusal ...
    pub stop_reason: Option<String>,
    /// 실제로 답한 모델 (Anthropic fallback 이 일어나면 요청한 것과 다르다)
    pub model: Option<String>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
}

impl ChatResponse {
    pub fn refused(&self) -> bool {
        self.stop_reason.as_deref() == Some("refusal")
    }
    pub fn truncated(&self) -> bool {
        matches!(self.stop_reason.as_deref(), Some("max_tokens") | Some("length"))
    }
}

/// HTTP 클라이언트 하나를 재사용한다 (연결 풀).
#[derive(Clone)]
pub struct Client {
    /// 시스템 프록시를 따르는 것
    http: reqwest::Client,
    /// 프록시를 거치지 않는 것 (사내 LLM 서버)
    direct: reqwest::Client,
}

impl Default for Client {
    fn default() -> Self {
        Self::new()
    }
}

impl Client {
    pub fn new() -> Self {
        let builder = || {
            reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(15))
                .pool_idle_timeout(Duration::from_secs(90))
                .user_agent(concat!("sqlstudio/", env!("CARGO_PKG_VERSION")))
        };
        let http = builder().build().expect("reqwest client");
        let direct = builder().no_proxy().build().expect("reqwest client");
        Self { http, direct }
    }

    fn client_for(&self, cfg: &ProviderConfig) -> &reqwest::Client {
        if cfg.use_proxy() {
            &self.http
        } else {
            &self.direct
        }
    }

    /// 스트리밍 채팅. 글자가 올 때마다 `on_delta` 가 불린다.
    pub async fn chat(
        &self,
        cfg: &ProviderConfig,
        req: &ChatRequest,
        mut on_delta: impl FnMut(&str) + Send,
    ) -> Result<ChatResponse> {
        let (url, body, headers) = build_request(cfg, req)?;
        let resp = self.send_with_retry(cfg, &url, &body, &headers).await?;
        let idle = Duration::from_secs(cfg.idle_timeout_secs.max(1));
        let mut stream = resp.bytes_stream();
        let mut out = ChatResponse::default();
        let mut sse = SseParser::new();
        let mut lines = LineParser::default();

        loop {
            let chunk = match tokio::time::timeout(idle, stream.next()).await {
                Err(_) => return Err(Error::Idle(idle.as_secs())),
                Ok(None) => break,
                Ok(Some(Err(e))) => {
                    return Err(Error::Protocol(format!("스트림이 끊겼습니다: {e}")))
                }
                Ok(Some(Ok(c))) => c,
            };
            match cfg.kind {
                ProviderKind::Ollama => {
                    for line in lines.push(&chunk) {
                        if handle_ollama(&line, &mut out, &mut on_delta)? {
                            return Ok(out);
                        }
                    }
                }
                _ => {
                    for ev in sse.push(&chunk) {
                        if handle_sse(cfg.kind, &ev, &mut out, &mut on_delta)? {
                            return Ok(out);
                        }
                    }
                }
            }
        }
        // 마지막 빈 줄 없이 끝난 서버
        match cfg.kind {
            ProviderKind::Ollama => {
                if let Some(line) = lines.finish() {
                    handle_ollama(&line, &mut out, &mut on_delta)?;
                }
            }
            _ => {
                if let Some(ev) = sse.finish() {
                    handle_sse(cfg.kind, &ev, &mut out, &mut on_delta)?;
                }
            }
        }
        Ok(out)
    }

    /// 접속 확인 + 모델 목록 (설정 화면의 "연결 테스트")
    pub async fn list_models(&self, cfg: &ProviderConfig) -> Result<Vec<String>> {
        let base = cfg.base_url();
        let (url, list_key, id_key) = match cfg.kind {
            ProviderKind::Ollama => (format!("{base}/api/tags"), "models", "name"),
            ProviderKind::OpenaiCompatible => (format!("{base}/models"), "data", "id"),
            ProviderKind::Anthropic => (format!("{base}/v1/models"), "data", "id"),
        };
        let mut rb = self.client_for(cfg).get(&url).timeout(Duration::from_secs(30));
        for (k, v) in auth_headers(cfg)? {
            rb = rb.header(k, v);
        }
        let resp = rb.send().await.map_err(|e| connect_error(&url, e, cfg.use_proxy()))?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(api_error(status.as_u16(), &text, cfg));
        }
        let v: Value = serde_json::from_str(&text).map_err(|e| Error::Protocol(e.to_string()))?;
        Ok(v[list_key]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|m| m[id_key].as_str().map(String::from))
            .collect())
    }

    /// 연결 실패·408·429·5xx 는 스트림이 시작되기 전까지만 다시 시도한다 (최대 2번).
    /// 스트림이 시작된 뒤 재시도하면 같은 글자가 두 번 나간다.
    async fn send_with_retry(
        &self,
        cfg: &ProviderConfig,
        url: &str,
        body: &Value,
        headers: &[(&'static str, String)],
    ) -> Result<reqwest::Response> {
        let first_byte = Duration::from_secs(cfg.connect_timeout_secs.max(1));
        let mut attempt = 0u32;
        loop {
            let mut rb = self
                .client_for(cfg)
                .post(url)
                .json(body)
                .header("accept", "text/event-stream, application/x-ndjson, application/json");
            for (k, v) in headers {
                rb = rb.header(*k, v);
            }
            // 전체 시간 제한은 두지 않는다 (긴 답이 잘린다). 응답 헤더까지만 제한한다.
            match tokio::time::timeout(first_byte, rb.send()).await {
                Err(_) => {
                    if attempt >= 2 {
                        return Err(Error::Idle(first_byte.as_secs()));
                    }
                }
                Ok(Err(e)) => {
                    if !(e.is_connect() || e.is_timeout()) || attempt >= 2 {
                        return Err(connect_error(url, e, cfg.use_proxy()));
                    }
                }
                Ok(Ok(resp)) => {
                    let st = resp.status();
                    if st.is_success() {
                        return Ok(resp);
                    }
                    let code = st.as_u16();
                    let retryable = code == 429 || code == 408 || code >= 500;
                    let retry_after = resp
                        .headers()
                        .get("retry-after")
                        .and_then(|v| v.to_str().ok())
                        .and_then(|v| v.parse::<u64>().ok());
                    let text = resp.text().await.unwrap_or_default();
                    if !retryable || attempt >= 2 {
                        return Err(api_error(code, &text, cfg));
                    }
                    if let Some(s) = retry_after {
                        tokio::time::sleep(Duration::from_secs(s.min(30))).await;
                    }
                }
            }
            attempt += 1;
            tracing::warn!("LLM 요청 재시도 {attempt}/2 ({url})");
            tokio::time::sleep(Duration::from_millis(500 * 2u64.pow(attempt))).await;
        }
    }
}

/// 주소 다듬기: 스킴이 없으면 http://, 끝의 / 제거. Ollama 에 붙여 넣은 /api·/v1, OpenAI 호환의 /chat/completions 를 뗀다.
/// OpenAI 호환은 경로가 없으면 /v1 (LM Studio·vLLM·Ollama 의 OpenAI 호환 주소)
pub fn normalize_base(kind: ProviderKind, raw: &str) -> String {
    let t = raw.trim().trim_end_matches('/');
    let mut u = if t.contains("://") { t.to_string() } else { format!("http://{t}") };
    let strip = |u: &mut String, sufs: &[&str]| {
        for s in sufs {
            if u.to_lowercase().ends_with(s) {
                u.truncate(u.len() - s.len());
                break;
            }
        }
    };
    match kind {
        ProviderKind::Ollama => strip(&mut u, &["/api/chat", "/api/generate", "/api", "/v1/chat/completions", "/v1"]),
        ProviderKind::Anthropic => strip(&mut u, &["/v1/messages", "/v1"]),
        ProviderKind::OpenaiCompatible => {
            strip(&mut u, &["/chat/completions"]);
            if reqwest::Url::parse(&u).is_ok_and(|p| p.path() == "/" || p.path().is_empty()) {
                u.push_str("/v1");
            }
        }
    }
    u.trim_end_matches('/').to_string()
}

/// 사내 주소인지 — 프록시 없이 직접 연결할 곳 (브라우저 PAC 의 흔한 규칙과 같게)
pub fn is_internal_host(host: &str) -> bool {
    let h = host.trim_start_matches('[').trim_end_matches(']').to_lowercase();
    if h.is_empty() {
        return false;
    }
    if let Ok(ip) = h.parse::<std::net::IpAddr>() {
        return match ip {
            std::net::IpAddr::V4(v) => v.is_loopback() || v.is_private() || v.is_link_local(),
            std::net::IpAddr::V6(v) => v.is_loopback() || (v.segments()[0] & 0xfe00) == 0xfc00 || (v.segments()[0] & 0xffc0) == 0xfe80,
        };
    }
    h == "localhost"
        || !h.contains('.')
        || [".local", ".localhost", ".lan", ".internal", ".intra", ".intranet", ".corp", ".home", ".home.arpa"].iter().any(|s| h.ends_with(s))
}

/// 연결 실패 안내 — 무엇을 보면 되는지
fn connect_error(url: &str, e: reqwest::Error, proxied: bool) -> Error {
    let chain = {
        let mut s = format!("{e}");
        let mut src = std::error::Error::source(&e);
        while let Some(x) = src {
            s.push_str(&format!(" / {x}"));
            src = x.source();
        }
        s.to_lowercase()
    };
    let route = if proxied { "시스템 프록시를 거쳐" } else { "프록시 없이 직접" };
    let hint = if chain.contains("certificate") || chain.contains("tls") || chain.contains("ssl") || chain.contains("schannel") {
        "https 인증서를 Windows 가 믿지 못합니다 — 사내 루트 인증서를 설치하거나, 서버가 http 를 받으면 http:// 주소를 쓰세요."
    } else if chain.contains("dns") || chain.contains("lookup") || chain.contains("no such host") || chain.contains("11001") || chain.contains("name or service") {
        "서버 이름을 찾지 못했습니다 — 주소의 이름(오타·사내 DNS)을 확인하거나 IP 주소로 넣어 보세요."
    } else if chain.contains("proxy") || chain.contains("407") {
        "프록시가 연결을 막았습니다 — 사내 서버라면 공급자 설정의 '프록시'를 '사용 안 함'으로 바꾸세요."
    } else if chain.contains("refused") || chain.contains("10061") {
        "서버가 그 포트에서 연결을 받지 않습니다 — 포트를 확인하세요. Ollama 를 다른 PC 에서 쓰려면 서버에서 OLLAMA_HOST=0.0.0.0 으로 띄워야 합니다."
    } else if e.is_timeout() || chain.contains("timed out") || chain.contains("10060") {
        if proxied {
            "응답이 없습니다 — 브라우저에서는 되는데 여기서만 안 되면 프록시 때문일 수 있습니다. 공급자 설정의 '프록시'를 '사용 안 함'으로 바꿔 보세요."
        } else {
            "응답이 없습니다 — 방화벽·주소·포트를 확인하세요."
        }
    } else if proxied {
        "브라우저에서는 되는데 여기서만 안 되면 프록시 때문일 수 있습니다 — 공급자 설정의 '프록시'를 '사용 안 함'으로 바꿔 보세요."
    } else {
        ""
    };
    let hint = if hint.is_empty() { format!("\n(연결: {route})") } else { format!("\n→ {hint}\n(연결: {route})") };
    Error::Connect { url: url.to_string(), source: e, hint }
}

/// API 오류 안내 — 404 는 주소 경로·종류, 407/프록시 차단 페이지는 프록시
fn api_error(status: u16, text: &str, cfg: &ProviderConfig) -> Error {
    let mut message = api_message(text);
    let lower = text.to_lowercase();
    let tip = match status {
        404 | 405 => Some(match cfg.kind {
            ProviderKind::Ollama => "주소의 경로가 맞지 않습니다 — 종류가 Ollama 면 http://서버:11434 처럼 경로 없이 넣으세요. OpenAI 호환 서버면 종류를 'OpenAI 호환'으로.",
            ProviderKind::OpenaiCompatible => "주소의 경로가 맞지 않습니다 — OpenAI 호환 서버는 보통 http://서버:포트/v1 입니다. Ollama 서버면 종류를 'Ollama'로 바꾸세요. 모델 이름도 확인하세요.",
            ProviderKind::Anthropic => "주소의 경로가 맞지 않습니다 — 사내 게이트웨이 주소를 확인하세요.",
        }),
        407 => Some("프록시 인증이 필요합니다 — 사내 서버라면 공급자 설정의 '프록시'를 '사용 안 함'으로."),
        403 | 502 | 503 if cfg.use_proxy() && (lower.contains("<html") || lower.contains("proxy")) => {
            Some("프록시가 막은 것 같습니다 (응답이 프록시의 안내 페이지) — 사내 서버라면 '프록시'를 '사용 안 함'으로.")
        }
        _ => None,
    };
    if let Some(t) = tip {
        message.push_str(&format!("\n→ {t}"));
    }
    Error::Api { status, message }
}

fn auth_headers(cfg: &ProviderConfig) -> Result<Vec<(&'static str, String)>> {
    let mut h = Vec::new();
    match cfg.kind {
        ProviderKind::Anthropic => {
            let key = cfg.key().ok_or_else(|| {
                Error::Config(format!(
                    "{}: API 키가 없습니다 (ANTHROPIC_API_KEY 또는 api_key_env)",
                    cfg.name
                ))
            })?;
            h.push(("x-api-key", key));
            h.push(("anthropic-version", "2023-06-01".to_string()));
        }
        ProviderKind::OpenaiCompatible => {
            // 로컬 서버(LM Studio 등)는 키가 없어도 된다
            if let Some(k) = cfg.key() {
                h.push(("authorization", format!("Bearer {k}")));
            }
        }
        ProviderKind::Ollama => {}
    }
    Ok(h)
}

type Built = (String, Value, Vec<(&'static str, String)>);

fn build_request(cfg: &ProviderConfig, req: &ChatRequest) -> Result<Built> {
    let base = cfg.base_url();
    let mut headers = auth_headers(cfg)?;
    let msgs: Vec<Value> = req
        .messages
        .iter()
        .map(|m| json!({ "role": m.role, "content": m.content }))
        .collect();
    match cfg.kind {
        ProviderKind::Ollama => {
            let mut all = vec![json!({ "role": "system", "content": req.system })];
            all.extend(msgs);
            let mut options = json!({
                "num_ctx": cfg.num_ctx.unwrap_or(16384),
                "num_predict": cfg.max_tokens,
            });
            if let Some(t) = cfg.temperature {
                options["temperature"] = json!(t);
            }
            let body = json!({
                "model": cfg.model,
                "messages": all,
                "stream": true,
                "options": options,
                "keep_alive": cfg.keep_alive.clone().unwrap_or_else(|| "30m".into()),
            });
            let mut body = body;
            if let Some(schema) = &req.json_schema {
                body["format"] = schema.clone();
            }
            Ok((format!("{base}/api/chat"), body, headers))
        }
        ProviderKind::OpenaiCompatible => {
            let mut all = vec![json!({ "role": "system", "content": req.system })];
            all.extend(msgs);
            let mut body = json!({
                "model": cfg.model,
                "messages": all,
                "stream": true,
                "max_tokens": cfg.max_tokens,
            });
            if let Some(t) = cfg.temperature {
                body["temperature"] = json!(t);
            }
            if let Some(schema) = &req.json_schema {
                body["response_format"] = json!({
                    "type": "json_schema",
                    "json_schema": { "name": "answer", "schema": schema },
                });
            }
            Ok((format!("{base}/chat/completions"), body, headers))
        }
        ProviderKind::Anthropic => {
            let mut body = json!({
                "model": cfg.model,
                "max_tokens": cfg.max_tokens,
                "system": req.system,
                "messages": msgs,
                "stream": true,
            });
            if let Some(e) = &cfg.effort {
                body["output_config"] = json!({ "effort": e });
            }
            if let Some(schema) = &req.json_schema {
                body["output_config"]["format"] = json!({ "type": "json_schema", "schema": schema });
            }
            if cfg.fallbacks {
                body["fallbacks"] = json!("default");
                headers.push(("anthropic-beta", "server-side-fallback-2026-07-01".to_string()));
            }
            Ok((format!("{base}/v1/messages"), body, headers))
        }
    }
}

/// SSE 이벤트 하나 처리. 스트림 끝이면 true.
fn handle_sse(
    kind: ProviderKind,
    ev: &crate::sse::SseEvent,
    out: &mut ChatResponse,
    on_delta: &mut impl FnMut(&str),
) -> Result<bool> {
    if ev.data == "[DONE]" {
        return Ok(true);
    }
    if ev.data.is_empty() {
        return Ok(false);
    }
    let v: Value = serde_json::from_str(&ev.data)
        .map_err(|e| Error::Protocol(format!("{e}: {}", truncate(&ev.data, 200))))?;
    match kind {
        ProviderKind::Anthropic => match v["type"].as_str().unwrap_or("") {
            "message_start" => {
                out.model = v["message"]["model"].as_str().map(String::from);
                out.input_tokens = v["message"]["usage"]["input_tokens"].as_u64();
            }
            "content_block_delta" => {
                // thinking 등 다른 블록은 화면에 내지 않는다
                if v["delta"]["type"] == "text_delta" {
                    if let Some(t) = v["delta"]["text"].as_str() {
                        out.text.push_str(t);
                        on_delta(t);
                    }
                }
            }
            "message_delta" => {
                if let Some(s) = v["delta"]["stop_reason"].as_str() {
                    out.stop_reason = Some(s.to_string());
                }
                if let Some(n) = v["usage"]["output_tokens"].as_u64() {
                    out.output_tokens = Some(n);
                }
            }
            "message_stop" => return Ok(true),
            "error" => {
                return Err(Error::Api {
                    status: 0,
                    message: v["error"]["message"]
                        .as_str()
                        .unwrap_or("알 수 없는 오류")
                        .to_string(),
                })
            }
            _ => {}
        },
        _ => {
            if let Some(err) = v.get("error") {
                return Err(Error::Api { status: 0, message: api_message(&json!({ "error": err }).to_string()) });
            }
            if out.model.is_none() {
                out.model = v["model"].as_str().map(String::from);
            }
            if let Some(ch) = v["choices"].get(0) {
                if let Some(t) = ch["delta"]["content"].as_str() {
                    if !t.is_empty() {
                        out.text.push_str(t);
                        on_delta(t);
                    }
                }
                if let Some(r) = ch["finish_reason"].as_str() {
                    out.stop_reason = Some(r.to_string());
                }
            }
            if let Some(u) = v.get("usage").filter(|u| !u.is_null()) {
                out.input_tokens = u["prompt_tokens"].as_u64();
                out.output_tokens = u["completion_tokens"].as_u64();
            }
        }
    }
    Ok(false)
}

/// Ollama 줄 하나 처리. 끝이면 true.
fn handle_ollama(line: &str, out: &mut ChatResponse, on_delta: &mut impl FnMut(&str)) -> Result<bool> {
    let v: Value = serde_json::from_str(line)
        .map_err(|e| Error::Protocol(format!("{e}: {}", truncate(line, 200))))?;
    if let Some(err) = v["error"].as_str() {
        return Err(Error::Api { status: 0, message: err.to_string() });
    }
    if out.model.is_none() {
        out.model = v["model"].as_str().map(String::from);
    }
    if let Some(t) = v["message"]["content"].as_str() {
        if !t.is_empty() {
            out.text.push_str(t);
            on_delta(t);
        }
    }
    if v["done"].as_bool() == Some(true) {
        out.stop_reason = v["done_reason"].as_str().map(String::from).or(Some("stop".into()));
        out.input_tokens = v["prompt_eval_count"].as_u64();
        out.output_tokens = v["eval_count"].as_u64();
        return Ok(true);
    }
    Ok(false)
}

/// 오류 본문에서 사람이 읽을 메시지를 꺼낸다 (`{"error":{"message":..}}` 등)
fn api_message(body: &str) -> String {
    if let Ok(v) = serde_json::from_str::<Value>(body) {
        for p in ["/error/message", "/error", "/message", "/detail"] {
            if let Some(s) = v.pointer(p).and_then(Value::as_str) {
                return s.to_string();
            }
        }
    }
    truncate(body, 500)
}

fn truncate(s: &str, n: usize) -> String {
    match s.char_indices().nth(n) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_string(),
    }
}

#[cfg(test)]
mod tests;
