//! sqlstudio-mcp — SQLStudio 의 Oracle 툴을 MCP 로 연다.
//!
//! ```text
//! sqlstudio-mcp                      # stdio (Claude Desktop, Claude Code, Cursor 등)
//! sqlstudio-mcp --http 127.0.0.1:8765  # Streamable HTTP (/mcp)
//! sqlstudio-mcp --check              # 설정과 접속을 점검하고 끝낸다
//! sqlstudio-mcp --config D:\cfg\config.toml
//! ```

use std::net::SocketAddr;
use std::process::ExitCode;

use rmcp::ServiceExt;
use sqls_core::config::{config_path, password_env_name, Config};
use sqls_mcp::{oracle_connector, SqlStudioMcp};

fn usage() -> &'static str {
    "사용법: sqlstudio-mcp [--config <파일>] [--http <127.0.0.1:포트>] [--check]"
}

#[tokio::main]
async fn main() -> ExitCode {
    // stdout 은 MCP 프로토콜 전용 — 로그는 stderr 로만
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("SQLSTUDIO_LOG")
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    let mut http: Option<String> = None;
    let mut check = false;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--config" => match args.next() {
                Some(p) => std::env::set_var("SQLSTUDIO_CONFIG", p),
                None => {
                    eprintln!("{}", usage());
                    return ExitCode::from(2);
                }
            },
            "--http" => http = args.next(),
            "--check" => check = true,
            "-h" | "--help" => {
                eprintln!("{}", usage());
                return ExitCode::SUCCESS;
            }
            other => {
                eprintln!("알 수 없는 인자: {other}\n{}", usage());
                return ExitCode::from(2);
            }
        }
    }

    let cfg = match Config::load() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(1);
        }
    };
    if let Err(e) = sqls_core::session::init_client(cfg.oracle.client_lib_dir.clone()) {
        eprintln!("Oracle Client 를 불러올 수 없습니다: {e}\n\
                   Instant Client 19 이상을 설치하고 [oracle] client_lib_dir 을 지정하거나 PATH 에 넣으세요.");
        return ExitCode::from(1);
    }

    eprintln!("config       : {}", config_path().display());
    eprintln!("connections  : {}", cfg.mcp.allowed_connections.join(", "));
    eprintln!("limits       : timeout={}s · SQL 실행 툴 없음 · explain_plan {}", cfg.mcp.call_timeout_secs,
        if cfg.mcp.allow_explain { "켜짐" } else { "꺼짐" });
    if cfg.mcp.allowed_connections.is_empty() {
        eprintln!("경고: [mcp] allowed_connections 가 비어 있어 노출되는 접속이 없습니다.");
    }

    if check {
        return run_check(&cfg).await;
    }

    let server = SqlStudioMcp::new(&cfg, oracle_connector());
    match http {
        None => match server.serve(rmcp::transport::stdio()).await {
            Ok(running) => {
                let _ = running.waiting().await;
                ExitCode::SUCCESS
            }
            Err(e) => {
                tracing::error!("MCP 시작 실패: {e}");
                ExitCode::from(1)
            }
        },
        Some(addr) => serve_http(server, &addr).await,
    }
}

async fn serve_http(server: SqlStudioMcp, addr: &str) -> ExitCode {
    use rmcp::transport::streamable_http_server::{
        session::local::LocalSessionManager, StreamableHttpServerConfig, StreamableHttpService,
    };
    let addr: SocketAddr = match addr.parse() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("주소 형식이 틀렸습니다 ({addr}): {e}");
            return ExitCode::from(2);
        }
    };
    // 인증이 없으므로 이 PC 안에서만 열어 둔다
    if !addr.ip().is_loopback() {
        eprintln!("--http 는 127.0.0.1 / ::1 에만 열 수 있습니다 (인증이 없는 DB 통로를 네트워크에 내놓지 않는다)");
        return ExitCode::from(2);
    }
    let ct = tokio_util::sync::CancellationToken::new();
    let service = StreamableHttpService::new(
        move || Ok(server.clone()),
        LocalSessionManager::default().into(),
        StreamableHttpServerConfig::default().with_cancellation_token(ct.child_token()),
    );
    let router = axum::Router::new().nest_service("/mcp", service);
    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("{addr} 에 열 수 없습니다: {e}");
            return ExitCode::from(1);
        }
    };
    eprintln!("listening    : http://{addr}/mcp");
    let r = axum::serve(listener, router)
        .with_graceful_shutdown(async move {
            let _ = tokio::signal::ctrl_c().await;
            ct.cancel();
        })
        .await;
    if let Err(e) = r {
        tracing::error!("HTTP 서버 오류: {e}");
        return ExitCode::from(1);
    }
    ExitCode::SUCCESS
}

/// 설정한 접속마다 실제로 붙어 본다. "왜 결과가 안 나오지" 를 가르는 첫 단계.
async fn run_check(cfg: &Config) -> ExitCode {
    let connect = oracle_connector();
    let mut ok = true;
    for name in &cfg.mcp.allowed_connections {
        let Some(p) = cfg.profile(name) else { continue };
        if p.stored_password().is_none() {
            eprintln!("FAIL {name}: 비밀번호 환경변수 {} 가 없습니다", password_env_name(&p.name));
            ok = false;
            continue;
        }
        match connect(p.clone()).await {
            Ok(s) => {
                eprintln!("OK   {name}: {} @ {} — {}", s.info().user, p.connect_string, s.info().server_version);
                s.close();
            }
            Err(e) => {
                eprintln!("FAIL {name}: {e}");
                ok = false;
            }
        }
    }
    if ok { ExitCode::SUCCESS } else { ExitCode::from(1) }
}
