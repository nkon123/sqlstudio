//! ACP(Agent Client Protocol) 클라이언트 — opencode 같은 코딩 에이전트를 AI 패널의 두뇌로 쓴다.
//!
//! 에이전트를 자식 프로세스로 띄우고 stdin/stdout 으로 줄 단위 JSON-RPC 2.0 을 주고받는다 (이 앱이 클라이언트).
//! 에이전트는 이름마다 하나만 띄워 두고, AI 패널의 대화마다 세션을 연다.
//! 파일·터미널은 빌려 주지 않는다 (clientCapabilities 를 끈다) — 에이전트가 묻는 권한만 화면에 넘긴다.
//! 새 세션에는 sqlstudio-mcp 를 MCP 서버로 넘겨, 허용한 접속의 사전(스키마)을 에이전트가 직접 찾아보게 한다.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::{json, Value};
use sqls_core::config::{config_path, data_dir, AcpAgentConfig};
use sqls_core::sync::{MutexExt, RwLockExt};
use tauri::ipc::Channel;
use tauri::State;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin};
use tokio::sync::oneshot;

use crate::ai::Step;
use crate::state::{AppState, ErrView};

type R<T> = Result<T, ErrView>;

/// 띄우고 initialize 에 답하기까지 (opencode 는 처음에 플러그인·모델 목록을 읽느라 몇 초, 느린 PC 에선 1분 넘게 걸린다)
const START_TIMEOUT: Duration = Duration::from_secs(180);
/// session/new · 설정 바꾸기 (session/new 도 처음엔 MCP 서버를 띄우느라 오래 걸린다)
const CALL_TIMEOUT: Duration = Duration::from_secs(120);
/// 오류 문구에 붙일 stderr 끝 줄 수
const STDERR_TAIL: usize = 12;

/// 화면으로 보내는 일 — 한 번의 질문(session/prompt) 동안
#[derive(Serialize, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AcpEvent {
    /// 답 글자
    Text { text: String },
    /// 생각 글자 (보이지 않고 '생각하는 중' 으로만)
    Thought { text: String },
    /// 도구 호출 — 처음(tool_call)과 바뀔 때(tool_call_update). 바뀔 때는 바뀐 칸만 온다
    Tool { id: String, title: Option<String>, kind: Option<String>, status: Option<String> },
    /// 에이전트의 할 일 목록 [{content, status}]
    Plan { entries: Value },
    /// 권한 요청 — 화면이 고른 뒤 acp_permission(key, option_id) 로 답한다
    Permission { key: u64, title: String, detail: String, options: Vec<PermOption> },
    /// 문맥 사용량 (토큰)
    Usage { used: u64, size: u64 },
}

#[derive(Serialize, Clone)]
pub struct PermOption {
    id: String,
    name: String,
    /// allow_once · allow_always · reject_once · reject_always
    kind: String,
}

struct RpcError {
    code: i64,
    message: String,
}

/// 보낸 요청의 답을 받을 곳
type Reply = oneshot::Sender<Result<Value, RpcError>>;
/// 권한 요청에 고른 선택지를 받을 곳 (None = 취소)
type PermReply = oneshot::Sender<Option<String>>;

/// 띄운 에이전트 하나
struct Agent {
    cfg: AcpAgentConfig,
    stdin: tokio::sync::Mutex<Option<ChildStdin>>,
    child: Mutex<Option<Child>>,
    alive: AtomicBool,
    next: AtomicU64,
    pending: Mutex<HashMap<u64, Reply>>,
    /// 세션 → 지금 질문의 화면 채널
    sinks: Mutex<HashMap<String, Sink>>,
    /// 권한 요청 키 → (세션, 답할 곳). 답이 없이 버려지면 cancelled 로 답한다
    perms: Mutex<HashMap<u64, (String, PermReply)>>,
    stderr: Mutex<VecDeque<String>>,
    /// initialize 의 답 (agentInfo · authMethods)
    info: Mutex<Value>,
}

/// 세션의 일을 받는 쪽 — 화면(채널), 또는 분석(답 글만 모은다 · 권한은 묻지 않고 취소)
#[derive(Clone)]
enum Sink {
    Ui(Channel<AcpEvent>),
    Collect(Arc<Mutex<String>>),
}

impl Agent {
    /// 띄우고 initialize 까지 — 단계마다 `on` 으로 알린다 (띄우기 · 초기화)
    async fn spawn(cfg: &AcpAgentConfig, on: &(impl Fn(Step) + Sync)) -> R<Arc<Self>> {
        let t0 = Instant::now();
        on(Step::new("spawn", "띄우기", "run", "", vec![]));
        let path = resolve(&cfg.command).ok_or_else(|| {
            let hint = if cfg.command == "opencode" { " — npm i -g opencode-ai 로 설치하세요" } else { "" };
            let e = format!("'{}' 을(를) 찾을 수 없습니다 (PATH){hint}", cfg.command);
            on(Step::new("spawn", "띄우기", "fail", "못 찾음", vec![e.clone()]));
            ErrView::msg("acp", e)
        })?;
        let cwd = work_dir(cfg)?;
        let args = launch_args(&path, &cfg.args, cfg.pure);
        let mut c = tokio::process::Command::new(&path);
        c.args(&args)
            .current_dir(&cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(windows)]
        c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW — 콘솔 창이 뜨지 않게
        let mut child = c.spawn().map_err(|e| {
            let e = format!("{} 을(를) 실행하지 못했습니다: {e}", path.display());
            on(Step::new("spawn", "띄우기", "fail", "실패", vec![e.clone()]));
            ErrView::msg("acp", e)
        })?;
        on(Step::new("spawn", "띄우기", "ok", secs(t0), vec![format!("{} {}", path.display(), args.join(" ")), format!("작업 폴더: {}", cwd.display())]));
        let t1 = Instant::now();
        on(Step::new("init", "초기화", "run", "", vec![]));
        let (stdin, stdout, stderr) = (child.stdin.take(), child.stdout.take(), child.stderr.take());
        let a = Arc::new(Self {
            cfg: cfg.clone(),
            stdin: tokio::sync::Mutex::new(stdin),
            child: Mutex::new(Some(child)),
            alive: AtomicBool::new(true),
            next: AtomicU64::new(1),
            pending: Mutex::new(HashMap::new()),
            sinks: Mutex::new(HashMap::new()),
            perms: Mutex::new(HashMap::new()),
            stderr: Mutex::new(VecDeque::new()),
            info: Mutex::new(Value::Null),
        });
        if let Some(out) = stdout {
            tokio::spawn(read_loop(a.clone(), out));
        }
        if let Some(err) = stderr {
            let me = a.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(err).lines();
                while let Ok(Some(l)) = lines.next_line().await {
                    tracing::debug!("ACP {} stderr: {l}", me.cfg.name);
                    let mut t = me.stderr.lk();
                    if t.len() >= STDERR_TAIL {
                        t.pop_front();
                    }
                    t.push_back(l);
                }
            });
        }
        let init = json!({
            "protocolVersion": 1,
            "clientCapabilities": { "fs": { "readTextFile": false, "writeTextFile": false }, "terminal": false },
            "clientInfo": { "name": "sqlstudio", "version": env!("CARGO_PKG_VERSION") },
        });
        match a.request("initialize", init, Some(START_TIMEOUT)).await {
            Ok(info) => {
                tracing::info!("ACP {} 시작: {}", cfg.name, info.get("agentInfo").map(|v| v.to_string()).unwrap_or_default());
                let ai = &info["agentInfo"];
                let who = format!("{} {}", ai["name"].as_str().unwrap_or(&cfg.name), ai["version"].as_str().unwrap_or("")).trim().to_string();
                on(Step::new("init", "초기화", "ok", secs(t1), vec![who, format!("프로토콜 {}", info["protocolVersion"])]));
                *a.info.lk() = info;
                Ok(a)
            }
            Err(e) => {
                on(Step::new("init", "초기화", "fail", "실패", vec![e.message.clone()]));
                a.shutdown();
                Err(e)
            }
        }
    }

    fn alive(&self) -> bool {
        self.alive.load(Ordering::Relaxed)
    }

    async fn write(&self, msg: Value) -> R<()> {
        let mut line = msg.to_string();
        line.push('\n');
        let mut g = self.stdin.lock().await;
        let w = g.as_mut().ok_or_else(|| self.died())?;
        w.write_all(line.as_bytes()).await.map_err(|_| self.died())?;
        w.flush().await.map_err(|_| self.died())
    }

    async fn request(&self, method: &str, params: Value, timeout: Option<Duration>) -> R<Value> {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lk().insert(id, tx);
        if let Err(e) = self.write(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })).await {
            self.pending.lk().remove(&id);
            return Err(e);
        }
        let got = match timeout {
            Some(t) => match tokio::time::timeout(t, rx).await {
                Ok(r) => r,
                Err(_) => {
                    self.pending.lk().remove(&id);
                    return Err(ErrView::msg("acp", format!("{} 이(가) {method} 에 {}초 안에 답하지 않았습니다", self.cfg.name, t.as_secs())));
                }
            },
            None => rx.await,
        };
        match got {
            Ok(Ok(v)) => Ok(v),
            Ok(Err(e)) => Err(self.rpc_error(method, e)),
            Err(_) => Err(self.died()),
        }
    }

    async fn notify(&self, method: &str, params: Value) -> R<()> {
        self.write(json!({ "jsonrpc": "2.0", "method": method, "params": params })).await
    }

    fn rpc_error(&self, method: &str, e: RpcError) -> ErrView {
        // -32000: 로그인이 필요하다 (ACP 의 auth_required)
        if e.code == -32000 || e.message.to_ascii_lowercase().contains("auth") {
            let how: Vec<String> = self.info.lk()["authMethods"]
                .as_array()
                .map(|v| v.iter().filter_map(|m| m["description"].as_str().or(m["name"].as_str()).map(String::from)).collect())
                .unwrap_or_default();
            let how = if how.is_empty() { String::new() } else { format!(" — {}", how.join(" / ")) };
            return ErrView::msg("acp_auth", format!("{} 에 로그인이 필요합니다{how}\n({})", self.cfg.name, e.message));
        }
        ErrView::msg("acp", format!("{} ({method}): {}", self.cfg.name, e.message))
    }

    fn died(&self) -> ErrView {
        let tail: Vec<String> = self.stderr.lk().iter().cloned().collect();
        let tail = if tail.is_empty() { String::new() } else { format!("\n\n{}", tail.join("\n")) };
        ErrView::msg("acp_closed", format!("{} 이(가) 끝났습니다{tail}", self.cfg.name))
    }

    /// 끝낸다 — stdin 을 닫고, 프로세스를 (Windows 는 .cmd 가 띄운 node 까지) 끝낸다
    fn shutdown(&self) {
        self.alive.store(false, Ordering::Relaxed);
        if let Ok(mut g) = self.stdin.try_lock() {
            g.take();
        }
        if let Some(mut c) = self.child.lk().take() {
            #[cfg(windows)]
            if let Some(pid) = c.id() {
                use std::os::windows::process::CommandExt;
                let _ = std::process::Command::new("taskkill")
                    .args(["/T", "/F", "/PID", &pid.to_string()])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .creation_flags(0x0800_0000)
                    .status();
            }
            let _ = c.start_kill();
        }
        self.pending.lk().clear();
        self.perms.lk().clear();
    }

    async fn reply(&self, id: Value, result: Value) {
        let _ = self.write(json!({ "jsonrpc": "2.0", "id": id, "result": result })).await;
    }

    /// 에이전트가 묻는 것 — 권한만 화면에 넘기고, 나머지(fs·terminal)는 빌려 주지 않았으니 없는 메서드라 답한다
    fn on_request(self: &Arc<Self>, method: &str, id: Value, params: &Value) {
        if method != "session/request_permission" {
            let me = self.clone();
            let method = method.to_string();
            tokio::spawn(async move {
                let msg = json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32601, "message": format!("지원하지 않습니다: {method}") } });
                let _ = me.write(msg).await;
            });
            return;
        }
        let sid = params["sessionId"].as_str().unwrap_or_default().to_string();
        let sink = self.sinks.lk().get(&sid).cloned();
        let tool = &params["toolCall"];
        let options: Vec<PermOption> = params["options"]
            .as_array()
            .map(|v| {
                v.iter()
                    .map(|o| PermOption {
                        id: o["optionId"].as_str().unwrap_or_default().into(),
                        name: o["name"].as_str().unwrap_or_default().into(),
                        kind: o["kind"].as_str().unwrap_or_default().into(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        let (tx, rx) = oneshot::channel();
        let key = self.next.fetch_add(1, Ordering::Relaxed);
        match sink {
            Some(Sink::Ui(sink)) => {
                self.perms.lk().insert(key, (sid, tx));
                let detail = match &tool["rawInput"] {
                    Value::Null => String::new(),
                    v => clip(&serde_json::to_string_pretty(v).unwrap_or_default(), 2000),
                };
                let title = tool["title"].as_str().unwrap_or("도구 실행").to_string();
                let _ = sink.send(AcpEvent::Permission { key, title, detail, options });
            }
            // 화면이 듣고 있지 않은 세션(분석 등) — 묻지 않고 취소
            _ => drop(tx),
        }
        let me = self.clone();
        tokio::spawn(async move {
            let outcome = match rx.await.ok().flatten() {
                Some(option_id) => json!({ "outcome": "selected", "optionId": option_id }),
                None => json!({ "outcome": "cancelled" }),
            };
            me.reply(id, json!({ "outcome": outcome })).await;
        });
    }

    fn on_update(&self, params: &Value) {
        let sid = params["sessionId"].as_str().unwrap_or_default();
        let Some(sink) = self.sinks.lk().get(sid).cloned() else { return };
        let u = &params["update"];
        let s = |k: &str| u[k].as_str().map(String::from);
        let ev = match u["sessionUpdate"].as_str().unwrap_or_default() {
            "agent_message_chunk" => u["content"]["text"].as_str().map(|t| AcpEvent::Text { text: t.into() }),
            "agent_thought_chunk" => u["content"]["text"].as_str().map(|t| AcpEvent::Thought { text: t.into() }),
            "tool_call" | "tool_call_update" => Some(AcpEvent::Tool {
                id: s("toolCallId").unwrap_or_default(),
                title: s("title"),
                kind: s("kind"),
                status: s("status"),
            }),
            "plan" => Some(AcpEvent::Plan { entries: u["entries"].clone() }),
            "usage_update" => Some(AcpEvent::Usage { used: u["used"].as_u64().unwrap_or(0), size: u["size"].as_u64().unwrap_or(0) }),
            _ => None,
        };
        match (sink, ev) {
            (Sink::Ui(ch), Some(ev)) => {
                let _ = ch.send(ev);
            }
            (Sink::Collect(buf), Some(AcpEvent::Text { text })) => buf.lk().push_str(&text),
            _ => {}
        }
    }
}

async fn read_loop(a: Arc<Agent>, out: tokio::process::ChildStdout) {
    let mut lines = BufReader::new(out).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(msg) = serde_json::from_str::<Value>(line) else {
            tracing::debug!("ACP {}: JSON 이 아닌 줄: {}", a.cfg.name, clip(line, 200));
            continue;
        };
        let method = msg["method"].as_str();
        let id = msg.get("id").filter(|v| !v.is_null()).cloned();
        match (method, id) {
            (Some(m), Some(id)) => a.on_request(m, id, &msg["params"]),
            (Some("session/update"), None) => a.on_update(&msg["params"]),
            (Some(_), None) => {}
            (None, Some(id)) => {
                let Some(tx) = id.as_u64().and_then(|i| a.pending.lk().remove(&i)) else { continue };
                let r = match msg.get("error") {
                    Some(e) => Err(RpcError { code: e["code"].as_i64().unwrap_or(0), message: e["message"].as_str().unwrap_or("오류").to_string() }),
                    None => Ok(msg.get("result").cloned().unwrap_or(Value::Null)),
                };
                let _ = tx.send(r);
            }
            (None, None) => {}
        }
    }
    tracing::info!("ACP {} 끝남", a.cfg.name);
    a.alive.store(false, Ordering::Relaxed);
    // 기다리던 요청은 보낼 곳이 사라져 died() 로 끝난다
    a.pending.lk().clear();
    a.perms.lk().clear();
}

/// 띄운 에이전트들 (앱 상태)
#[derive(Default)]
pub struct Acp {
    agents: Mutex<HashMap<String, Arc<Agent>>>,
}

impl Acp {
    /// 살아 있는 에이전트, 없거나 실행 설정이 바뀌었으면 새로 띄운다
    async fn get(&self, cfg: &AcpAgentConfig, on: &(impl Fn(Step) + Sync)) -> R<Arc<Agent>> {
        let old = {
            let mut m = self.agents.lk();
            if let Some(a) = m.get(&cfg.name).filter(|a| a.alive() && same_process(&a.cfg, cfg)) {
                on(Step::new("spawn", "띄우기", "ok", "재사용", vec!["이미 떠 있는 에이전트를 그대로 씁니다".into()]));
                return Ok(a.clone());
            }
            m.remove(&cfg.name)
        };
        if let Some(o) = old {
            o.shutdown();
        }
        let a = Agent::spawn(cfg, on).await?;
        self.agents.lk().insert(cfg.name.clone(), a.clone());
        Ok(a)
    }

    /// 세션이 있는 에이전트 — 끝났으면 세션도 사라졌다
    fn running(&self, name: &str) -> R<Arc<Agent>> {
        self.agents
            .lk()
            .get(name)
            .filter(|a| a.alive())
            .cloned()
            .ok_or_else(|| ErrView::msg("acp_session_lost", format!("{name} 이(가) 다시 시작되어 대화가 끊겼습니다 — 새 대화로 이어 묻습니다")))
    }

    fn stop(&self, name: &str) {
        if let Some(a) = self.agents.lk().remove(name) {
            a.shutdown();
        }
    }

    pub fn shutdown_all(&self) {
        for (_, a) in self.agents.lk().drain() {
            a.shutdown();
        }
    }
}

fn same_process(a: &AcpAgentConfig, b: &AcpAgentConfig) -> bool {
    a.command == b.command && a.args == b.args && a.cwd == b.cwd && a.pure == b.pure
}

/// 띄울 때의 인자 — `pure` 면 opencode acp 에 `--pure` 를 붙인다 (인자에 없어도).
/// 외부 플러그인을 읽느라 시작이 크게 늦어지고 (느린 PC 에선 수십 초), SQL 도우미로 쓰는 데엔 대개 플러그인이 필요 없다.
/// omo 같은 플러그인의 에이전트를 쓰려면 끈다 — 그때는 인자에 적은 `--pure` 도 뺀다
fn launch_args(path: &std::path::Path, args: &[String], pure: bool) -> Vec<String> {
    let opencode = path.file_stem().is_some_and(|s| s.eq_ignore_ascii_case("opencode")) && args.first().is_some_and(|a| a == "acp");
    let mut v: Vec<String> = args.iter().filter(|a| !(opencode && !pure && *a == "--pure")).cloned().collect();
    if opencode && pure && !v.iter().any(|a| a == "--pure") {
        v.push("--pure".into());
    }
    v
}

/// 실행 파일 찾기 — 경로면 그대로, 이름이면 PATH 에서 (Windows 는 .exe·.cmd·.bat. npm 의 확장자 없는 sh 스크립트는 건너뛴다)
fn resolve(cmd: &str) -> Option<PathBuf> {
    let p = Path::new(cmd);
    if p.is_absolute() || p.components().count() > 1 {
        return p.is_file().then(|| p.to_path_buf());
    }
    let exts: &[&str] = if cfg!(windows) { &[".exe", ".cmd", ".bat"] } else { &[""] };
    let has_ext = cfg!(windows) && p.extension().is_some_and(|e| ["exe", "cmd", "bat"].iter().any(|x| e.eq_ignore_ascii_case(x)));
    for dir in std::env::split_paths(&std::env::var_os("PATH")?) {
        if has_ext {
            if dir.join(cmd).is_file() {
                return Some(dir.join(cmd));
            }
            continue;
        }
        for e in exts {
            let f = dir.join(format!("{cmd}{e}"));
            if f.is_file() {
                return Some(f);
            }
        }
    }
    None
}

fn work_dir(cfg: &AcpAgentConfig) -> R<PathBuf> {
    let d = match cfg.cwd.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(c) => PathBuf::from(c),
        None => data_dir().join("acp-work"),
    };
    std::fs::create_dir_all(&d).map_err(|e| ErrView::msg("io", format!("{}: {e}", d.display())))?;
    // ACP 는 절대 경로를 요구한다
    Ok(std::path::absolute(&d).unwrap_or(d))
}

fn clip(s: &str, n: usize) -> String {
    match s.char_indices().nth(n) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_string(),
    }
}

/// 설정의 에이전트 — 하나도 없으면, PATH 에 opencode 가 있을 때 기본 하나
fn agents(st: &AppState) -> Vec<AcpAgentConfig> {
    let v = st.cfg.rd().acp_agents.clone();
    if v.is_empty() && resolve("opencode").is_some() {
        return vec![AcpAgentConfig::opencode()];
    }
    v
}

pub(crate) fn agent_cfg(st: &AppState, name: &str) -> R<AcpAgentConfig> {
    agents(st).into_iter().find(|a| a.name == name).ok_or_else(|| ErrView::msg("invalid", format!("ACP 에이전트가 없습니다: {name}")))
}

/// 설정 파일의 목록을 바꾼다 — 보이기만 하던 기본값(opencode)도 이때 파일에 들어간다
fn edit_agents(st: &AppState, f: impl FnOnce(&mut Vec<AcpAgentConfig>) -> R<()>) -> R<()> {
    let mut list = agents(st);
    f(&mut list)?;
    st.cfg.wr().acp_agents = list;
    st.save_config()
}

/// 새 세션에 넘길 MCP 서버 — sqlstudio-mcp 가 앱 옆에 있고 MCP 설정에서 허용한 접속이 있을 때
fn mcp_servers(st: &AppState, cfg: &AcpAgentConfig) -> (Value, Vec<String>) {
    let allowed = st.cfg.rd().mcp.allowed_connections.clone();
    let exe = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join(if cfg!(windows) { "sqlstudio-mcp.exe" } else { "sqlstudio-mcp" })));
    match exe {
        Some(exe) if cfg.mcp && !allowed.is_empty() && exe.is_file() => (
            json!([{ "name": "sqlstudio", "command": exe, "args": ["--config", config_path()], "env": [] }]),
            allowed,
        ),
        _ => (json!([]), vec![]),
    }
}

#[derive(Serialize)]
pub struct AgentView {
    #[serde(flatten)]
    cfg: AcpAgentConfig,
    /// 실행 파일을 찾았는지
    available: bool,
    running: bool,
}

#[tauri::command]
pub fn acp_agents(st: State<'_, AppState>) -> Vec<AgentView> {
    let running = st.acp.agents.lk().iter().filter(|(_, a)| a.alive()).map(|(n, _)| n.clone()).collect::<Vec<_>>();
    agents(&st)
        .into_iter()
        .map(|cfg| AgentView { available: resolve(&cfg.command).is_some(), running: running.contains(&cfg.name), cfg })
        .collect()
}

#[tauri::command]
pub fn acp_save_agent(st: State<'_, AppState>, agent: AcpAgentConfig, original_name: Option<String>) -> R<()> {
    let mut agent = agent;
    agent.name = agent.name.trim().to_string();
    agent.command = agent.command.trim().to_string();
    if agent.name.is_empty() || agent.command.is_empty() {
        return Err(ErrView::msg("invalid", "이름과 실행 파일은 비울 수 없습니다"));
    }
    let key = original_name.unwrap_or_else(|| agent.name.clone());
    edit_agents(&st, |list| {
        // 동의는 화면이 보낸 값을 믿지 않는다 — 같은 실행 파일·인자일 때만 이어 간다
        let old = list.iter().find(|a| a.name == key);
        agent.remote_ack = old.is_some_and(|o| o.remote_ack && o.command == agent.command && o.args == agent.args);
        if key != agent.name && list.iter().any(|a| a.name == agent.name) {
            return Err(ErrView::msg("invalid", format!("같은 이름이 있습니다: {}", agent.name)));
        }
        match list.iter_mut().find(|a| a.name == key) {
            Some(a) => *a = agent.clone(),
            None => list.push(agent.clone()),
        }
        Ok(())
    })?;
    if key != agent.name {
        st.acp.stop(&key);
    }
    Ok(())
}

#[tauri::command]
pub fn acp_delete_agent(st: State<'_, AppState>, name: String) -> R<()> {
    edit_agents(&st, |list| {
        list.retain(|a| a.name != name);
        Ok(())
    })?;
    st.acp.stop(&name);
    Ok(())
}

/// 외부 전송 동의 — AI 패널의 확인 창에서 "계속"
#[tauri::command]
pub fn acp_ack_remote(st: State<'_, AppState>, name: String) -> R<()> {
    edit_agents(&st, |list| {
        let a = list.iter_mut().find(|a| a.name == name).ok_or_else(|| ErrView::msg("invalid", format!("ACP 에이전트가 없습니다: {name}")))?;
        a.remote_ack = true;
        Ok(())
    })
}

#[derive(Serialize)]
pub struct TestResult {
    agent: Value,
    auth: Vec<String>,
    path: String,
}

/// 연결 확인 — 저장하지 않은 설정으로 따로 띄워 initialize 만 하고 끝낸다
#[tauri::command]
pub async fn acp_test_agent(agent: AcpAgentConfig) -> R<TestResult> {
    let path = resolve(&agent.command).map(|p| p.display().to_string()).unwrap_or_default();
    let a = Agent::spawn(&agent, &|_| {}).await?;
    let info = a.info.lk().clone();
    a.shutdown();
    let auth = info["authMethods"]
        .as_array()
        .map(|v| v.iter().filter_map(|m| m["description"].as_str().or(m["name"].as_str()).map(String::from)).collect())
        .unwrap_or_default();
    Ok(TestResult { agent: info["agentInfo"].clone(), auth, path })
}

#[derive(Serialize)]
pub struct NewSession {
    session_id: String,
    /// 에이전트가 고르게 하는 것 (opencode: model · mode) — [{id, name, currentValue, options:[{value,name}]}]
    options: Value,
    /// sqlstudio-mcp 로 넘긴 접속 — 비면 MCP 없이
    mcp_profiles: Vec<String>,
    agent: Value,
}

/// 새 세션의 고르기 하나(model · mode)를 설정값으로 맞춘 결과
#[derive(Debug, PartialEq)]
enum Applied {
    /// 에이전트가 이 고르기를 내놓지 않는다 (예전 방식일 수 있다)
    Absent,
    /// 이미 그 값
    Same,
    Set,
    /// 에이전트가 내놓는 값이 아니다 (로그아웃 등) — 기본값으로 둔다
    Unavailable,
    Failed(String),
}

/// 새 세션의 고르기 하나(model · mode)를 설정값으로 — 에이전트가 그 값을 내놓을 때만
async fn apply_option(a: &Agent, session_id: &str, options: &mut Value, id: &str, value: &str) -> Applied {
    let Some(o) = options.as_array().and_then(|v| v.iter().find(|o| o["id"] == id)).cloned() else {
        return Applied::Absent;
    };
    if o["currentValue"] == value {
        return Applied::Same;
    }
    if !o["options"].as_array().is_some_and(|v| v.iter().any(|x| x["value"] == value)) {
        tracing::warn!("ACP {}: {id} {value} 은(는) 고를 수 없는 값이라 기본값으로 둡니다", a.cfg.name);
        return Applied::Unavailable;
    }
    match a.request("session/set_config_option", json!({ "sessionId": session_id, "configId": id, "value": value }), Some(CALL_TIMEOUT)).await {
        Ok(v) => {
            *options = v.get("configOptions").cloned().unwrap_or(options.clone());
            Applied::Set
        }
        Err(e) => {
            tracing::warn!("ACP {}: {id} 을(를) {value} 로 바꾸지 못했습니다: {}", a.cfg.name, e.message);
            Applied::Failed(e.message)
        }
    }
}

/// 고르기의 지금 값 이름 (opencode: "Big Pickle")
fn current_name(options: &Value, id: &str) -> Option<String> {
    let o = options.as_array()?.iter().find(|o| o["id"] == id)?;
    let cur = &o["currentValue"];
    let name = o["options"].as_array().and_then(|v| v.iter().find(|x| x["value"] == *cur)).and_then(|x| x["name"].as_str());
    name.or(cur.as_str()).map(String::from)
}

/// 설정값을 맞춘 결과 한 줄
fn applied_line(what: &str, value: &str, r: &Applied) -> String {
    match r {
        Applied::Absent => format!("{what}: 에이전트가 고르기를 내놓지 않아 기본값"),
        Applied::Same => format!("{what}: 설정값 {value} (이미 그 값)"),
        Applied::Set => format!("{what}: 설정값 {value} 로 바꿈"),
        Applied::Unavailable => {
            format!("{what}: 설정값 {value} 를 에이전트가 내놓지 않아 기본값으로 둠")
        }
        Applied::Failed(e) => format!("{what}: {value} 로 바꾸지 못함 — {e}"),
    }
}

/// MCP 를 넘기지 않은 까닭 — 넘겼으면 None
fn mcp_skip_reason(st: &AppState, cfg: &AcpAgentConfig) -> Option<&'static str> {
    if !cfg.mcp {
        Some("이 에이전트 설정에서 MCP 넘기기를 껐습니다")
    } else if st.cfg.rd().mcp.allowed_connections.is_empty() {
        Some("설정 → MCP 에서 허용한 접속이 없습니다")
    } else {
        Some("앱 옆에 sqlstudio-mcp 가 없습니다")
    }
}

fn secs(t: Instant) -> String {
    format!("{:.1}s", t.elapsed().as_secs_f32())
}

/// 새 대화 — 필요하면 에이전트를 띄우고 세션을 연다. 설정의 모델·처음 모드로 바꿔 둔다.
/// 단계(띄우기 · 초기화 · 세션 · 모델·모드 · MCP)는 `on_step` 으로 알린다 — 느린 PC 에서 어디서 기다리는지 보이게
#[tauri::command]
pub async fn acp_new_session(st: State<'_, AppState>, agent: String, on_step: Channel<Step>) -> R<NewSession> {
    let on = |s: Step| {
        let _ = on_step.send(s);
    };
    let cfg = agent_cfg(&st, &agent)?;
    let a = st.acp.get(&cfg, &on).await?;
    let (mcp, mcp_profiles) = mcp_servers(&st, &cfg);
    let cwd = work_dir(&cfg)?;
    let t = Instant::now();
    on(Step::new("session", "세션", "run", "", vec![]));
    let r = match a.request("session/new", json!({ "cwd": cwd, "mcpServers": mcp }), Some(CALL_TIMEOUT)).await {
        Ok(r) => r,
        Err(e) => {
            on(Step::new("session", "세션", "fail", "실패", vec![e.message.clone()]));
            return Err(e);
        }
    };
    let session_id = r["sessionId"].as_str().ok_or_else(|| ErrView::msg("acp", format!("{agent}: session/new 답에 sessionId 가 없습니다")))?.to_string();
    on(Step::new("session", "세션", "ok", secs(t), vec![format!("세션 {session_id}"), format!("작업 폴더: {}", cwd.display())]));

    let mut options = r.get("configOptions").cloned().unwrap_or(Value::Array(vec![]));
    let mut lines = Vec::new();
    let mut warn = false;
    if cfg.model.is_some() || cfg.mode.is_some() {
        on(Step::new("options", "모델·모드", "run", "", vec![]));
    }
    if let Some(model) = cfg.model.as_deref().filter(|m| !m.is_empty()) {
        let res = apply_option(&a, &session_id, &mut options, "model", model).await;
        warn |= matches!(res, Applied::Unavailable | Applied::Failed(_));
        lines.push(applied_line("모델", model, &res));
    } else {
        lines.push("모델: 설정값 없음 — 에이전트 기본값".into());
    }
    if let Some(mode) = cfg.mode.as_deref().filter(|m| !m.is_empty()) {
        let res = apply_option(&a, &session_id, &mut options, "mode", mode).await;
        // configOptions 에 mode 가 없으면 예전 방식 (modes · session/set_mode)
        if res == Applied::Absent && r["modes"]["availableModes"].as_array().is_some_and(|v| v.iter().any(|m| m["id"] == mode)) {
            match a.request("session/set_mode", json!({ "sessionId": session_id, "modeId": mode }), Some(CALL_TIMEOUT)).await {
                Ok(_) => lines.push(format!("모드: 처음 모드 {mode} 로 바꿈")),
                Err(e) => {
                    tracing::warn!("ACP {agent}: 처음 모드 {mode} 로 바꾸지 못했습니다: {}", e.message);
                    warn = true;
                    lines.push(format!("모드: {mode} 로 바꾸지 못함 — {}", e.message));
                }
            }
        } else {
            warn |= matches!(res, Applied::Unavailable | Applied::Failed(_));
            lines.push(applied_line("모드", mode, &res));
        }
    } else {
        lines.push("모드: 처음 모드 없음 — 에이전트 기본값".into());
    }
    let summary = [current_name(&options, "model"), current_name(&options, "mode")].into_iter().flatten().collect::<Vec<_>>().join(" · ");
    on(Step::new("options", "모델·모드", if warn { "warn" } else { "ok" }, summary, lines));

    let explain = st.cfg.rd().mcp.allow_explain;
    on(if mcp_profiles.is_empty() {
        Step::new("mcp", "MCP", "skip", "안 넘김", vec![mcp_skip_reason(&st, &cfg).unwrap_or_default().into(), "에이전트가 DB 사전을 볼 수 없습니다".into()])
    } else {
        Step::new(
            "mcp",
            "MCP",
            "ok",
            mcp_profiles.join(", "),
            vec![
                format!("sqlstudio-mcp 로 넘긴 접속: {}", mcp_profiles.join(", ")),
                format!("explain_plan 도구: {}", if explain { "켜짐" } else { "꺼짐 (설정 → MCP 에서 켬)" }),
                "사전 조회만 — SQL 실행·데이터 반환 없음".into(),
            ],
        )
    });
    let agent_info = a.info.lk()["agentInfo"].clone();
    Ok(NewSession { session_id, options, mcp_profiles, agent: agent_info })
}

/// 분석이 ACP 에이전트에게 묻는 길 (주소가 `acp://<이름>` 인 공급자) — 조각마다 새 세션이라 앞 조각의 답이 섞이지 않고,
/// MCP 없이 plan 모드로 글만 보고 답하게 한다. 권한을 물으면 취소한다
pub struct AnalysisChat {
    pub app: tauri::AppHandle,
}

impl sqls_llm::ExternalChat for AnalysisChat {
    fn chat<'a>(
        &'a self,
        cfg: &'a sqls_llm::ProviderConfig,
        req: &'a sqls_llm::ChatRequest,
    ) -> sqls_llm::BoxFuture<'a, sqls_llm::Result<sqls_llm::ChatResponse>> {
        Box::pin(async move {
            let name = cfg.acp_agent().unwrap_or_default();
            let st = tauri::Manager::state::<AppState>(&self.app);
            let acfg = agent_cfg(&st, name).map_err(|e| sqls_llm::Error::Protocol(format!("ACP {name}: {}", e.message)))?;
            ask_agent(&st.acp, &acfg, cfg, req).await
        })
    }
}

/// 에이전트에게 한 번 묻고 답 글을 받는다 — 새 세션 · MCP 없음 · plan 모드
async fn ask_agent(acp: &Acp, acfg: &AcpAgentConfig, cfg: &sqls_llm::ProviderConfig, req: &sqls_llm::ChatRequest) -> sqls_llm::Result<sqls_llm::ChatResponse> {
    let name = acfg.name.as_str();
    let err = |e: ErrView| sqls_llm::Error::Protocol(format!("ACP {name}: {}", e.message));
    let a = acp.get(acfg, &|_| {}).await.map_err(err)?;
    let r = a.request("session/new", json!({ "cwd": work_dir(acfg).map_err(err)?, "mcpServers": [] }), Some(CALL_TIMEOUT)).await.map_err(err)?;
    let sid = r["sessionId"].as_str().ok_or_else(|| sqls_llm::Error::Protocol(format!("ACP {name}: session/new 답에 sessionId 가 없습니다")))?.to_string();
    let mut options = r.get("configOptions").cloned().unwrap_or(Value::Array(vec![]));
    // 분석 화면에서 고른 모델 (cfg.model), 아니면 설정의 모델
    let model = Some(cfg.model.as_str()).filter(|m| !m.is_empty() && *m != DEFAULT_MODEL).or(acfg.model.as_deref().filter(|m| !m.is_empty()));
    if let Some(m) = model {
        apply_option(&a, &sid, &mut options, "model", m).await;
    }
    // 분석은 읽기만 — 편집 도구를 막는 plan (설정의 처음 모드가 있으면 그것)
    apply_option(&a, &sid, &mut options, "mode", acfg.mode.as_deref().filter(|m| !m.is_empty()).unwrap_or("plan")).await;
    let buf = Arc::new(Mutex::new(String::new()));
    a.sinks.lk().insert(sid.clone(), Sink::Collect(buf.clone()));
    let limit = Duration::from_secs(cfg.idle_timeout_secs.max(60) * 2);
    let done = a.request("session/prompt", json!({ "sessionId": sid, "prompt": [{ "type": "text", "text": analysis_text(req) }] }), Some(limit)).await;
    a.sinks.lk().remove(&sid);
    if done.is_err() {
        let _ = a.notify("session/cancel", json!({ "sessionId": sid })).await;
    }
    let done = done.map_err(err)?;
    let text = std::mem::take(&mut *buf.lk());
    Ok(sqls_llm::ChatResponse {
        text,
        stop_reason: Some(done["stopReason"].as_str().unwrap_or("end_turn").to_string()),
        model: current_name(&options, "model").map(|m| format!("{name} · {m}")),
        input_tokens: done["usage"]["inputTokens"].as_u64(),
        output_tokens: done["usage"]["outputTokens"].as_u64(),
        thinking_chars: 0,
    })
}

/// 분석 요청을 에이전트에게 보낼 글 하나로 — 지시문 · (형식을 고쳐 다시 물을 때) 앞의 답 · 요청
fn analysis_text(req: &sqls_llm::ChatRequest) -> String {
    let mut s = String::from(
        "[SQLStudio 코드 분석 — 파일을 읽거나 도구를 쓰지 말고, 아래 글만 보고 답한다]

",
    );
    s.push_str(&req.system);
    for m in &req.messages {
        s.push_str(
            "

",
        );
        if m.role == sqls_llm::Role::Assistant {
            s.push_str(
                "[앞의 답]
",
            );
        }
        s.push_str(&m.content);
    }
    if req.json_schema.is_some() {
        s.push_str(
            "

Reply with ONLY the JSON object — no code fences, no other text.",
        );
    }
    s
}

/// 모델·모드 바꾸기 — 바뀐 전체 목록을 돌려준다. 모델은 설정에도 남겨 다음 대화·다음 실행에서 그대로 쓴다
/// (모드는 남기지 않는다 — build 로 바꾼 것이 굳으면 편집 도구가 늘 열린다. 처음 모드는 설정 화면에서)
#[tauri::command]
pub async fn acp_set_option(st: State<'_, AppState>, agent: String, session_id: String, config_id: String, value: String) -> R<Value> {
    let a = st.acp.running(&agent)?;
    let r = a
        .request("session/set_config_option", json!({ "sessionId": session_id, "configId": config_id, "value": value }), Some(CALL_TIMEOUT))
        .await?;
    if config_id == "model" {
        let saved = edit_agents(&st, |list| {
            if let Some(c) = list.iter_mut().find(|c| c.name == agent) {
                c.model = Some(value.clone());
            }
            Ok(())
        });
        if let Err(e) = saved {
            tracing::warn!("ACP {agent}: 고른 모델을 설정에 남기지 못했습니다: {}", e.message);
        }
    }
    Ok(r.get("configOptions").cloned().unwrap_or(Value::Array(vec![])))
}

/// 에이전트 모델의 컨텍스트 크기와 거기서 정한 분석 조각 줄 수 (모르면 None)
#[derive(Serialize)]
pub struct ModelContext {
    model: Option<String>,
    context: Option<u32>,
    lines: Option<u32>,
}

/// 모델 칸에 아무것도 고르지 않았을 때 — 에이전트가 정한 모델
pub(crate) const DEFAULT_MODEL: &str = "에이전트 기본 모델";

/// 에이전트가 내놓는 모델 목록 — 새 세션의 configOptions 에서 (분석 화면의 모델 고르기)
#[derive(Serialize)]
pub struct AgentModels {
    /// 설정에 적은 모델, 없으면 에이전트의 기본 모델
    current: Option<String>,
    /// (값, 이름)
    options: Vec<(String, String)>,
}

/// 에이전트를 띄우고 세션을 하나 열어 설정 고르기(configOptions)를 본다 — 분석이 곧 쓸 프로세스를 미리 띄우는 셈
async fn session_options(st: &AppState, acfg: &AcpAgentConfig) -> R<Value> {
    let a = st.acp.get(acfg, &|_| {}).await?;
    let r = a.request("session/new", json!({ "cwd": work_dir(acfg)?, "mcpServers": [] }), Some(CALL_TIMEOUT)).await?;
    Ok(r.get("configOptions").cloned().unwrap_or(Value::Array(vec![])))
}

fn model_option(options: &Value) -> Option<&Value> {
    options.as_array()?.iter().find(|o| o["id"] == "model")
}

#[tauri::command]
pub async fn acp_models(st: State<'_, AppState>, agent: String) -> R<AgentModels> {
    let acfg = agent_cfg(&st, &agent)?;
    let options = session_options(&st, &acfg).await?;
    let Some(o) = model_option(&options) else { return Ok(AgentModels { current: acfg.model.clone(), options: vec![] }) };
    let list = o["options"].as_array().into_iter().flatten().filter_map(|x| {
        let v = x["value"].as_str()?;
        Some((v.to_string(), x["name"].as_str().unwrap_or(v).to_string()))
    });
    let current = acfg.model.clone().filter(|m| !m.is_empty()).or_else(|| o["currentValue"].as_str().map(String::from));
    Ok(AgentModels { current, options: list.collect() })
}

/// 분석 조각 크기를 정하려고 — ACP 는 모델의 컨텍스트 크기를 알려 주지 않아, opencode 는 `opencode models <공급자> --verbose` 로 읽는다.
/// `model` 이 없으면 설정의 모델, 그것도 없으면 에이전트의 기본 모델. 다른 에이전트는 모른다
#[tauri::command]
pub async fn acp_model_context(st: State<'_, AppState>, agent: String, model: Option<String>) -> R<ModelContext> {
    let acfg = agent_cfg(&st, &agent)?;
    let model = model.filter(|m| !m.is_empty() && m != DEFAULT_MODEL).or_else(|| acfg.model.clone().filter(|m| !m.is_empty()));
    let unknown = |model: Option<String>| ModelContext { model, context: None, lines: None };
    let Some(path) = resolve(&acfg.command).filter(|p| p.file_stem().is_some_and(|s| s.eq_ignore_ascii_case("opencode"))) else { return Ok(unknown(model)) };
    let model = match model {
        Some(m) => m,
        None => match model_option(&session_options(&st, &acfg).await?).and_then(|o| o["currentValue"].as_str()) {
            Some(m) => m.to_string(),
            None => return Ok(unknown(None)),
        },
    };
    let context = opencode_context(&path, &acfg, &model).await;
    Ok(ModelContext { lines: context.map(auto_lines), model: Some(model), context })
}

/// 모델 → 컨텍스트 크기 (한 번 읽으면 앱을 끌 때까지)
static CONTEXTS: std::sync::LazyLock<Mutex<HashMap<String, u32>>> = std::sync::LazyLock::new(Default::default);

async fn opencode_context(path: &Path, acfg: &AcpAgentConfig, model: &str) -> Option<u32> {
    if let Some(n) = CONTEXTS.lk().get(model) {
        return Some(*n);
    }
    let (provider, _) = model.split_once('/')?;
    let mut c = tokio::process::Command::new(path);
    c.args(["models", provider, "--verbose"]);
    if acfg.pure {
        c.arg("--pure");
    }
    c.current_dir(work_dir(acfg).ok()?).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).kill_on_drop(true);
    #[cfg(windows)]
    c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    let out = tokio::time::timeout(Duration::from_secs(90), c.output()).await.ok()?.ok()?;
    let n = parse_context(&String::from_utf8_lossy(&out.stdout), model)?;
    CONTEXTS.lk().insert(model.to_string(), n);
    Some(n)
}

/// `opencode models --verbose` 의 출력 — "공급자/모델" 한 줄 뒤에 JSON 객체 (닫는 `}` 가 맨 앞에 오는 줄까지)
fn parse_context(out: &str, model: &str) -> Option<u32> {
    let mut it = out.lines().skip_while(|l| l.trim() != model).skip(1);
    let mut body = String::new();
    for l in it.by_ref() {
        body.push_str(l);
        body.push('\n');
        if l.starts_with('}') {
            break;
        }
    }
    let v: Value = serde_json::from_str(&body).ok()?;
    v["limit"]["context"].as_u64().filter(|&n| n > 0).map(|n| n.min(u32::MAX as u64) as u32)
}

/// 컨텍스트에서 조각 줄 수 — 에이전트 자신의 지시문·도구 설명(약 1만 토큰)을 빼고 남은 것의 1/4 을 조각에, 한 줄 ≈ 15 토큰.
/// 20~400 줄 (화면에서 고르는 범위) — 큰 모델이라도 조각이 너무 크면 요약이 거칠어진다
fn auto_lines(context: u32) -> u32 {
    (context.saturating_sub(10_000) / 4 / 15).clamp(20, 400)
}

#[derive(Serialize)]
pub struct PromptDone {
    /// end_turn · max_tokens · max_turn_requests · refusal · cancelled
    stop_reason: String,
    usage: Value,
}

/// 질문 — 답이 끝날 때까지 일을 채널로 보낸다. 외부 전송 동의는 여기서 확인한다
#[tauri::command]
pub async fn acp_prompt(st: State<'_, AppState>, agent: String, session_id: String, text: String, on_event: Channel<AcpEvent>) -> R<PromptDone> {
    let cfg = agent_cfg(&st, &agent)?;
    if !cfg.remote_ack {
        return Err(ErrView::msg("remote_consent", format!("'{agent}' 은 받은 내용을 에이전트의 모델(대개 외부 서버)로 보냅니다. 전송 동의가 필요합니다.")));
    }
    let a = st.acp.running(&agent)?;
    a.sinks.lk().insert(session_id.clone(), Sink::Ui(on_event));
    let r = a.request("session/prompt", json!({ "sessionId": session_id, "prompt": [{ "type": "text", "text": text }] }), None).await;
    a.sinks.lk().remove(&session_id);
    a.perms.lk().retain(|_, (s, _)| *s != session_id);
    let r = r?;
    Ok(PromptDone { stop_reason: r["stopReason"].as_str().unwrap_or("end_turn").to_string(), usage: r["usage"].clone() })
}

/// 멈추기 — 에이전트가 cancelled 로 질문을 끝낸다. 기다리던 권한 요청도 취소로 답한다
#[tauri::command]
pub async fn acp_cancel(st: State<'_, AppState>, agent: String, session_id: String) -> R<()> {
    let a = st.acp.running(&agent)?;
    a.perms.lk().retain(|_, (s, _)| *s != session_id);
    a.notify("session/cancel", json!({ "sessionId": session_id })).await
}

/// 권한 요청에 답한다 — option_id 가 없으면 취소
#[tauri::command]
pub fn acp_permission(st: State<'_, AppState>, agent: String, key: u64, option_id: Option<String>) -> R<()> {
    let a = st.acp.running(&agent)?;
    if let Some((_, tx)) = a.perms.lk().remove(&key) {
        let _ = tx.send(option_id);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clip_counts_chars() {
        assert_eq!(clip("가나다라", 2), "가나…");
        assert_eq!(clip("ab", 5), "ab");
    }

    #[test]
    fn model_context_from_opencode_models() {
        let out = "opencode/a-free\n{\n  \"id\": \"a-free\",\n  \"limit\": {\n    \"context\": 1048576,\n    \"output\": 131072\n  }\n}\nopencode/b\n{\n  \"id\": \"b\",\n  \"limit\": {\n    \"context\": 32000\n  }\n}\n";
        assert_eq!(parse_context(out, "opencode/b"), Some(32000));
        assert_eq!(parse_context(out, "opencode/a-free"), Some(1048576));
        assert_eq!(parse_context(out, "opencode/none"), None);
        assert_eq!(auto_lines(8192), 20);
        assert_eq!(auto_lines(32000), 366);
        assert_eq!(auto_lines(1048576), 400);
    }

    /// 실제 opencode 의 기본 모델 컨텍스트 — `cargo test -p sqlstudio acp_live_context -- --ignored --nocapture`
    #[tokio::test]
    #[ignore]
    async fn acp_live_context() {
        let acfg = AcpAgentConfig::opencode();
        let path = resolve(&acfg.command).expect("opencode");
        let n = opencode_context(&path, &acfg, "opencode/big-pickle").await;
        eprintln!("opencode/big-pickle: {n:?} → {:?}줄", n.map(auto_lines));
        assert!(n.is_some_and(|n| n > 0));
    }

    #[test]
    fn opencode_always_pure() {
        let args = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        // 구분자 없는 이름으로 — CI(Linux) 는 \ 를 경로 구분자로 보지 않아 C:\npm\opencode.cmd 의 이름이 opencode 가 아니게 된다
        let oc = std::path::Path::new("opencode.cmd");
        assert_eq!(launch_args(oc, &args(&["acp"]), true), args(&["acp", "--pure"]));
        assert_eq!(launch_args(oc, &args(&["acp", "--pure", "--log-level", "WARN"]), true), args(&["acp", "--pure", "--log-level", "WARN"]));
        assert_eq!(launch_args(oc, &args(&["--version"]), true), args(&["--version"]));
        assert_eq!(launch_args(std::path::Path::new("gemini.exe"), &args(&["acp"]), true), args(&["acp"]));
        // 플러그인(omo 등)을 쓰려고 끄면 인자에 적은 --pure 도 뺀다
        assert_eq!(launch_args(oc, &args(&["acp", "--pure"]), false), args(&["acp"]));
        assert_eq!(launch_args(std::path::Path::new("gemini.exe"), &args(&["x", "--pure"]), false), args(&["x", "--pure"]));
    }

    #[test]
    fn resolve_rejects_missing_paths() {
        assert!(resolve(r"C:\없는\폴더\agent.exe").is_none());
        assert!(resolve("sqlstudio-없는-에이전트").is_none());
    }

    /// 분석이 ACP 에이전트로 묻는 길 — 실제 opencode 에게 분석 요청(JSON 답)을 보내 하네스가 읽을 수 있는지.
    /// `cargo test -p sqlstudio acp_live_analysis -- --ignored --nocapture`
    #[tokio::test]
    #[ignore]
    async fn acp_live_analysis() {
        struct Direct(Acp, AcpAgentConfig);
        impl sqls_llm::ExternalChat for Direct {
            fn chat<'a>(&'a self, cfg: &'a sqls_llm::ProviderConfig, req: &'a sqls_llm::ChatRequest) -> sqls_llm::BoxFuture<'a, sqls_llm::Result<sqls_llm::ChatResponse>> {
                Box::pin(ask_agent(&self.0, &self.1, cfg, req))
            }
        }
        let acfg = AcpAgentConfig::opencode();
        let client = sqls_llm::Client::new();
        let d = Arc::new(Direct(Acp::default(), acfg.clone()));
        client.set_external(d.clone());
        let mut cfg = sqls_llm::ProviderConfig::new("acp:opencode", sqls_llm::ProviderKind::OpenaiCompatible, DEFAULT_MODEL);
        cfg.base_url = Some("acp://opencode".into());
        assert!(cfg.is_remote(), "ACP 공급자는 외부로 본다 (분석이 전송 동의를 묻게)");
        let body = "Unit: PROCEDURE APP.P_CLOSE_DAY
Lines 1-6:
BEGIN
  UPDATE orders SET status = 'CLOSED' WHERE order_dt < TRUNC(SYSDATE);
  INSERT INTO close_log(dt) VALUES (SYSDATE);
  COMMIT;
END;";
        let r = sqls_analyze::llm::ask_summary(&client, &cfg, sqls_analyze::llm::rollup_system(sqls_analyze::llm::Lang::Ko, "a procedure"), body.into()).await;
        d.0.shutdown_all();
        match r {
            Ok((ins, meta)) => {
                eprintln!("요약: {}
단계: {:?}
모델: {:?} · 시도 {} · {}ms", ins.summary, ins.steps, meta.answered_by, meta.attempts, meta.elapsed_ms);
                assert!(!ins.summary.is_empty());
            }
            Err(e) => panic!("분석 답을 읽지 못함: {e:?}"),
        }
    }

    /// 분석 화면에서 고른 모델(cfg.model)이 설정의 모델보다 먼저 — `cargo test -p sqlstudio acp_live_pick_model -- --ignored --nocapture`
    #[tokio::test]
    #[ignore]
    async fn acp_live_pick_model() {
        let acfg = AcpAgentConfig::opencode();
        let acp = Acp::default();
        let mut cfg = sqls_llm::ProviderConfig::new("acp:opencode", sqls_llm::ProviderKind::OpenaiCompatible, "opencode/beam-free");
        cfg.base_url = Some("acp://opencode".into());
        let req = sqls_llm::ChatRequest { system: "Answer with only the number.".into(), messages: vec![sqls_llm::Message::user("What is 2 + 2?")], json_schema: None };
        let r = ask_agent(&acp, &acfg, &cfg, &req).await;
        acp.shutdown_all();
        let r = r.unwrap_or_else(|e| panic!("{e}"));
        eprintln!("{:?} → {:?}", r.model, r.text);
        assert!(r.model.as_deref().is_some_and(|m| m.contains("Beam")), "{:?}", r.model);
    }

    /// 분석의 사전 대조처럼 한 opencode 에 여러 질문을 같이 — 세션마다 답이 섞이지 않는지.
    /// `cargo test -p sqlstudio acp_live_parallel -- --ignored --nocapture`
    #[tokio::test]
    #[ignore]
    async fn acp_live_parallel() {
        let acfg = AcpAgentConfig::opencode();
        let acp = Acp::default();
        let mut cfg = sqls_llm::ProviderConfig::new("acp:opencode", sqls_llm::ProviderKind::OpenaiCompatible, DEFAULT_MODEL);
        cfg.base_url = Some("acp://opencode".into());
        let ask = |n: u32| {
            let req = sqls_llm::ChatRequest { system: "Answer with only the number.".into(), messages: vec![sqls_llm::Message::user(format!("What is {n} + 100?"))], json_schema: None };
            let (acp, acfg, cfg) = (&acp, &acfg, &cfg);
            async move { ask_agent(acp, acfg, cfg, &req).await }
        };
        let t = std::time::Instant::now();
        let (a, b, c, d) = tokio::join!(ask(1), ask(2), ask(3), ask(4));
        eprintln!("4개 같이: {}ms", t.elapsed().as_millis());
        acp.shutdown_all();
        for (n, r) in [(1, a), (2, b), (3, c), (4, d)] {
            let text = r.unwrap_or_else(|e| panic!("{n}: {e:?}")).text;
            eprintln!("{n} → {text:?}");
            assert!(text.contains(&format!("{}", n + 100)), "{n} 의 답이 섞였거나 비었다: {text:?}");
        }
    }

    /// 실제 opencode 로 끝까지 — 띄우기 · 세션 · plan 모드 · 질문 · 글자 이벤트 · 끝내기.
    /// `cargo test -p sqlstudio acp_live -- --ignored --nocapture`.
    /// SQLSTUDIO_ACP_LIVE_MCP=1 이면 sqlstudio-mcp 도 넘겨 도구가 보이는지 본다
    /// (`cargo build --release -p sqls-mcp` 와 Oracle Client 가 있어야 MCP 서버가 뜬다)
    #[tokio::test]
    #[ignore]
    async fn acp_live_opencode() {
        let mut cfg = AcpAgentConfig::opencode();
        let dir = std::env::temp_dir().join("sqlstudio-acp-live");
        std::fs::create_dir_all(&dir).unwrap();
        cfg.cwd = Some(dir.display().to_string());
        let started = Mutex::new(Vec::<Step>::new());
        let a = Agent::spawn(&cfg, &|s| started.lk().push(s)).await.expect("opencode acp 를 띄우지 못함");
        // 띄우기 · 초기화 단계가 run → ok 로 왔다
        let got: Vec<(&str, &str)> = started.lk().iter().map(|s| (s.id, s.state)).collect();
        assert_eq!(got, [("spawn", "run"), ("spawn", "ok"), ("init", "run"), ("init", "ok")]);
        assert!(a.info.lk()["agentInfo"]["name"].is_string());

        let mcp_exe = std::env::current_dir().unwrap().join("../../target/release/sqlstudio-mcp.exe");
        let mcp_cfg = dir.join("config.toml");
        std::fs::write(&mcp_cfg, "").unwrap();
        let with_mcp = std::env::var("SQLSTUDIO_ACP_LIVE_MCP").is_ok_and(|v| v == "1") && mcp_exe.is_file();
        let mcp = if with_mcp {
            json!([{ "name": "sqlstudio", "command": std::path::absolute(&mcp_exe).unwrap(), "args": ["--config", mcp_cfg], "env": [] }])
        } else {
            json!([])
        };
        let r = a.request("session/new", json!({ "cwd": work_dir(&cfg).unwrap(), "mcpServers": mcp }), Some(CALL_TIMEOUT)).await.unwrap();
        let sid = r["sessionId"].as_str().unwrap().to_string();
        let mut options = r["configOptions"].clone();
        let current = |o: &Value, id: &str| o.as_array().unwrap().iter().find(|x| x["id"] == id).map(|x| x["currentValue"].clone()).unwrap();
        // 설정의 모델 — 없는 값은 건너뛰고, 있는 값은 그대로
        let model = current(&options, "model").as_str().unwrap().to_string();
        assert_eq!(apply_option(&a, &sid, &mut options, "model", "없는/모델").await, Applied::Unavailable);
        assert_eq!(current(&options, "model"), model.as_str());
        assert_eq!(apply_option(&a, &sid, &mut options, "model", &model).await, Applied::Same);
        assert_ne!(apply_option(&a, &sid, &mut options, "mode", "plan").await, Applied::Absent);
        assert_eq!(current(&options, "mode"), "plan");

        let events = Arc::new(Mutex::new(Vec::<Value>::new()));
        let ev = events.clone();
        let ch: Channel<AcpEvent> = Channel::new(move |body| {
            if let tauri::ipc::InvokeResponseBody::Json(s) = body {
                ev.lk().push(serde_json::from_str(&s).unwrap());
            }
            Ok(())
        });
        a.sinks.lk().insert(sid.clone(), Sink::Ui(ch));
        // MCP 없이 도구 이름을 물으면 모델이 설정 파일을 뒤지러 나선다 — 그때는 짧은 답만
        let ask = if with_mcp { "MCP 서버 'sqlstudio' 의 도구 이름을 쉼표로 나열하라. 도구는 부르지 말고 이름만. 없으면 NONE 이라고만." } else { "도구를 쓰지 말고 정확히 pong 이라고만 답하라." };
        let done = a.request("session/prompt", json!({ "sessionId": sid, "prompt": [{ "type": "text", "text": ask }] }), Some(Duration::from_secs(180))).await;
        let evs = events.lk().clone();
        let tail: Vec<String> = a.stderr.lk().iter().cloned().collect();
        a.shutdown();
        let done = done.unwrap_or_else(|e| panic!("{}\n이벤트: {evs:?}\nstderr: {tail:?}", e.message));
        let text: String = evs.iter().filter(|e| e["type"] == "text").filter_map(|e| e["text"].as_str()).collect();
        eprintln!("stop={} 이벤트 {}개\n답: {text}", done["stopReason"], evs.len());
        assert_eq!(done["stopReason"], "end_turn");
        assert!(!text.trim().is_empty(), "답이 비었다 — 이벤트: {evs:?}");
        if with_mcp {
            assert!(!text.contains("NONE"), "sqlstudio-mcp 를 넘겼는데 도구가 없다고 함: {text}");
        }
        assert!(!a.alive());
    }
}
