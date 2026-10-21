//! 가짜 HTTP 서버로 세 공급자의 스트리밍·재시도·오류 처리를 확인한다.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use super::*;

/// 요청마다 다음 응답을 순서대로 돌려주는 서버. 받은 요청 원문을 기록한다.
struct Mock {
    base: String,
    seen: Arc<Mutex<Vec<String>>>,
}

enum Reply {
    /// 상태, content-type, 본문 조각들 (조각 사이에 잠깐 쉰다)
    Stream(u16, &'static str, Vec<String>),
    /// 헤더만 보내고 멈춘다
    Stall,
}

async fn mock(replies: Vec<Reply>) -> Mock {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let seen2 = seen.clone();
    tokio::spawn(async move {
        for r in replies {
            let (mut sock, _) = l.accept().await.unwrap();
            // 요청 읽기: 헤더 + content-length 만큼
            let mut buf = Vec::new();
            let mut tmp = [0u8; 4096];
            loop {
                let n = sock.read(&mut tmp).await.unwrap();
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&tmp[..n]);
                let s = String::from_utf8_lossy(&buf).to_string();
                if let Some(h) = s.find("\r\n\r\n") {
                    let len = s[..h]
                        .lines()
                        .find_map(|l| l.to_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse::<usize>().unwrap()))
                        .unwrap_or(0);
                    if buf.len() >= h + 4 + len {
                        break;
                    }
                }
            }
            seen2.lock().unwrap().push(String::from_utf8_lossy(&buf).to_string());
            match r {
                Reply::Stream(status, ctype, parts) => {
                    let head = format!(
                        "HTTP/1.1 {status} X\r\ncontent-type: {ctype}\r\nconnection: close\r\n\r\n"
                    );
                    sock.write_all(head.as_bytes()).await.unwrap();
                    for p in parts {
                        sock.write_all(p.as_bytes()).await.unwrap();
                        sock.flush().await.unwrap();
                        tokio::time::sleep(Duration::from_millis(5)).await;
                    }
                    let _ = sock.shutdown().await;
                }
                Reply::Stall => {
                    sock.write_all(b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\r\n").await.unwrap();
                    tokio::time::sleep(Duration::from_secs(10)).await;
                }
            }
        }
    });
    Mock { base, seen }
}

fn cfg(kind: ProviderKind, base: &str) -> ProviderConfig {
    let mut c = ProviderConfig::new("t", kind, "m");
    c.base_url = Some(base.into());
    c.api_key = Some("k-test".into());
    c.idle_timeout_secs = 2;
    c.connect_timeout_secs = 5;
    c
}

fn req() -> ChatRequest {
    ChatRequest { system: "sys".into(), messages: vec![Message::user("안녕")], json_schema: None }
}

#[tokio::test]
async fn anthropic_stream() {
    let ev = |e: &str, d: &str| format!("event: {e}\ndata: {d}\n\n");
    let m = mock(vec![Reply::Stream(200, "text/event-stream", vec![
        ev("message_start", r#"{"type":"message_start","message":{"model":"claude-opus-5-5","usage":{"input_tokens":10}}}"#),
        ev("content_block_start", r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}"#),
        ev("content_block_delta", r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"숨김"}}"#),
        ev("content_block_delta", r#"{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"SELECT "}}"#),
        // 이벤트 중간에서 끊어 보낸다
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"text_delta\",\"text\":\"1 FROM du".into(),
        "al\"}}\n\n".into(),
        ev("message_delta", r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":7}}"#),
        ev("message_stop", r#"{"type":"message_stop"}"#),
    ])]).await;
    let mut c = cfg(ProviderKind::Anthropic, &m.base);
    c.effort = Some("low".into());
    let mut deltas = Vec::new();
    let r = Client::new().chat(&c, &req(), |d| deltas.push(d.to_string())).await.unwrap();
    assert_eq!(r.text, "SELECT 1 FROM dual");
    assert_eq!(deltas.concat(), "SELECT 1 FROM dual");
    assert_eq!(r.stop_reason.as_deref(), Some("end_turn"));
    assert_eq!(r.model.as_deref(), Some("claude-opus-5-5"));
    assert_eq!(r.output_tokens, Some(7));

    let raw = m.seen.lock().unwrap()[0].clone();
    let lower = raw.to_lowercase();
    assert!(raw.starts_with("POST /v1/messages "));
    assert!(lower.contains("x-api-key: k-test"));
    assert!(lower.contains("anthropic-version: 2023-06-01"));
    assert!(lower.contains("anthropic-beta: server-side-fallback-2026-07-01"));
    let body: Value = serde_json::from_str(&raw[raw.find("\r\n\r\n").unwrap() + 4..]).unwrap();
    assert_eq!(body["fallbacks"], "default");
    assert_eq!(body["output_config"]["effort"], "low");
    assert_eq!(body["system"], "sys");
    assert_eq!(body["stream"], true);
}

#[tokio::test]
async fn anthropic_refusal_is_reported() {
    let m = mock(vec![Reply::Stream(200, "text/event-stream", vec![
        "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"refusal\"}}\n\n".into(),
        "data: {\"type\":\"message_stop\"}\n\n".into(),
    ])]).await;
    let r = Client::new().chat(&cfg(ProviderKind::Anthropic, &m.base), &req(), |_| {}).await.unwrap();
    assert!(r.refused());
}

#[tokio::test]
async fn openai_compatible_stream() {
    let d = |t: &str| format!("data: {{\"model\":\"qwen\",\"choices\":[{{\"delta\":{{\"content\":\"{t}\"}},\"finish_reason\":null}}]}}\n\n");
    let m = mock(vec![Reply::Stream(200, "text/event-stream", vec![
        d("가"), d("나"),
        "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n".into(),
        "data: [DONE]\n\n".into(),
    ])]).await;
    let r = Client::new().chat(&cfg(ProviderKind::OpenaiCompatible, &m.base), &req(), |_| {}).await.unwrap();
    assert_eq!(r.text, "가나");
    assert_eq!(r.stop_reason.as_deref(), Some("stop"));
    let raw = m.seen.lock().unwrap()[0].clone();
    assert!(raw.starts_with("POST /v1/chat/completions "), "경로 없는 주소에는 /v1 을 붙인다");
    assert!(raw.to_lowercase().contains("authorization: bearer k-test"));
    let body: Value = serde_json::from_str(&raw[raw.find("\r\n\r\n").unwrap() + 4..]).unwrap();
    assert_eq!(body["messages"][0]["role"], "system");
    assert_eq!(body["messages"][1]["content"], "안녕");
}

#[tokio::test]
async fn ollama_ndjson_with_num_ctx() {
    let m = mock(vec![Reply::Stream(200, "application/x-ndjson", vec![
        "{\"model\":\"gemma\",\"message\":{\"content\":\"SEL\"},\"done\":false}\n".into(),
        "{\"model\":\"gemma\",\"message\":{\"content\":\"ECT\"},\"done\":false}\n{\"done\":true,\"done_reason\":\"stop\",\"eval_count\":3}\n".into(),
    ])]).await;
    let mut c = cfg(ProviderKind::Ollama, &m.base);
    c.num_ctx = Some(32768);
    let r = Client::new().chat(&c, &req(), |_| {}).await.unwrap();
    assert_eq!(r.text, "SELECT");
    assert_eq!(r.output_tokens, Some(3));
    let raw = m.seen.lock().unwrap()[0].clone();
    assert!(raw.starts_with("POST /api/chat "));
    let body: Value = serde_json::from_str(&raw[raw.find("\r\n\r\n").unwrap() + 4..]).unwrap();
    assert_eq!(body["options"]["num_ctx"], 32768);
    assert_eq!(body["keep_alive"], "30m");
}

#[tokio::test]
async fn retries_5xx_then_succeeds() {
    let m = mock(vec![
        Reply::Stream(503, "application/json", vec!["{\"error\":{\"message\":\"busy\"}}".into()]),
        Reply::Stream(200, "text/event-stream", vec!["data: {\"choices\":[{\"delta\":{\"content\":\"ok\"}}]}\n\ndata: [DONE]\n\n".into()]),
    ]).await;
    let r = Client::new().chat(&cfg(ProviderKind::OpenaiCompatible, &m.base), &req(), |_| {}).await.unwrap();
    assert_eq!(r.text, "ok");
    assert_eq!(m.seen.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn client_error_is_not_retried() {
    let m = mock(vec![Reply::Stream(400, "application/json", vec!["{\"error\":{\"message\":\"model not found\"}}".into()])]).await;
    let e = Client::new().chat(&cfg(ProviderKind::OpenaiCompatible, &m.base), &req(), |_| {}).await.unwrap_err();
    match e {
        Error::Api { status, message } => {
            assert_eq!(status, 400);
            assert_eq!(message, "model not found");
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(m.seen.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn stalled_stream_times_out() {
    let m = mock(vec![Reply::Stall]).await;
    let mut c = cfg(ProviderKind::OpenaiCompatible, &m.base);
    c.idle_timeout_secs = 1;
    let e = Client::new().chat(&c, &req(), |_| {}).await.unwrap_err();
    assert!(matches!(e, Error::Idle(1)), "{e:?}");
}

#[test]
fn missing_anthropic_key_is_config_error() {
    let mut c = ProviderConfig::new("a", ProviderKind::Anthropic, "claude-opus-5-5");
    c.api_key_env = Some("SQLS_TEST_NO_SUCH_KEY".into());
    assert!(matches!(build_request(&c, &req()), Err(Error::Config(_))));
}

#[test]
fn remote_detection() {
    let mut c = ProviderConfig::new("o", ProviderKind::Ollama, "x");
    assert!(!c.is_remote());
    c.base_url = Some("http://gpu-server:11434".into());
    assert!(c.is_remote());
    assert!(ProviderConfig::new("a", ProviderKind::Anthropic, "x").is_remote());
    // 글자 포함으로 속이는 주소는 외부
    for u in ["http://localhost.evil.com:11434", "http://127.0.0.1.nip.io", "http://127.0.0.1@evil.com", "http://evil.com/?x=://localhost", "not a url"] {
        c.base_url = Some(u.into());
        assert!(c.is_remote(), "{u}");
    }
    for u in ["http://localhost:11434", "http://LOCALHOST:8080/v1", "http://127.0.0.1:11434", "http://127.1.2.3", "http://[::1]:11434"] {
        c.base_url = Some(u.into());
        assert!(!c.is_remote(), "{u}");
    }
}

#[test]
fn remote_consent_follows_the_address() {
    let mut c = ProviderConfig::new("o", ProviderKind::Ollama, "x");
    assert!(c.remote_ok(), "로컬은 동의가 필요 없다");
    c.base_url = Some("http://gpu.corp.local:11434".into());
    assert!(!c.remote_ok());
    c.remote_ack = Some("http://gpu.corp.local:11434".into());
    assert!(c.remote_ok());
    c.base_url = Some("http://other.example.com:11434".into());
    assert!(!c.remote_ok(), "주소가 바뀌면 다시 묻는다");
}

#[test]
fn base_url_normalized() {
    use crate::normalize_base as n;
    assert_eq!(n(ProviderKind::Ollama, "gpu-server:11434"), "http://gpu-server:11434", "스킴이 없으면 http");
    assert_eq!(n(ProviderKind::Ollama, " http://10.1.2.3:11434/ "), "http://10.1.2.3:11434");
    assert_eq!(n(ProviderKind::Ollama, "http://h:11434/api/chat"), "http://h:11434");
    assert_eq!(n(ProviderKind::Ollama, "http://h:11434/v1"), "http://h:11434");
    assert_eq!(n(ProviderKind::OpenaiCompatible, "http://h:1234"), "http://h:1234/v1", "경로가 없으면 /v1");
    assert_eq!(n(ProviderKind::OpenaiCompatible, "http://h:1234/v1/chat/completions"), "http://h:1234/v1");
    assert_eq!(n(ProviderKind::OpenaiCompatible, "https://gw.corp/openai/v1"), "https://gw.corp/openai/v1", "경로가 있으면 그대로");
    assert_eq!(n(ProviderKind::Anthropic, "https://api.anthropic.com/v1"), "https://api.anthropic.com");
}

#[test]
fn internal_hosts_go_direct() {
    use crate::is_internal_host as i;
    for h in ["localhost", "127.0.0.1", "[::1]", "10.20.30.40", "172.16.0.5", "172.31.255.1", "192.168.0.10", "169.254.1.1", "gpu-server", "llm.corp", "ai.internal", "box.local", "fd00::1"] {
        assert!(i(h), "{h}");
    }
    for h in ["api.openai.com", "8.8.8.8", "172.32.0.1", "llm.example.com", ""] {
        assert!(!i(h), "{h}");
    }
    let mut c = ProviderConfig::new("o", ProviderKind::Ollama, "x");
    c.base_url = Some("gpu-server:11434".into());
    assert!(!c.use_proxy(), "자동: 사내 주소는 직접");
    c.proxy = Some("system".into());
    assert!(c.use_proxy());
    c.base_url = Some("https://llm.example.com".into());
    c.proxy = None;
    assert!(c.use_proxy(), "자동: 바깥 주소는 시스템 프록시");
    c.proxy = Some("none".into());
    assert!(!c.use_proxy());
}

#[test]
fn missing_model_is_explained() {
    let c = ProviderConfig::new("o", ProviderKind::Ollama, "gemma4:e2b");
    let e = crate::api_error(404, r#"{"error":"model 'gemma4:e2b' not found"}"#, &c).to_string();
    assert!(e.contains("ollama pull gemma4:e2b") && !e.contains("경로"), "{e}");
    let e = crate::api_error(404, "404 page not found", &c).to_string();
    assert!(e.contains("경로"), "{e}");
}

#[tokio::test]
async fn connect_error_explains() {
    // 아무도 듣지 않는 포트 → 연결 거부 안내 + 경로
    let mut c = ProviderConfig::new("o", ProviderKind::Ollama, "x");
    c.base_url = Some("http://127.0.0.1:9".into());
    let e = crate::Client::new().list_models(&c).await.unwrap_err().to_string();
    assert!(e.contains("연결: 프록시 없이 직접"), "{e}");
    assert!(e.contains("→"), "안내가 있어야 한다: {e}");
}

#[test]
fn think_setting_maps_to_each_server() {
    let r = req();
    let build = |kind: ProviderKind, think: Option<&str>| {
        let mut c = cfg(kind, "http://x");
        c.think = think.map(String::from);
        super::build_request(&c, &r).unwrap().1
    };
    // Ollama: 비우면 보내지 않고, 끄기·켜기는 불리언, 수준은 문자열
    assert!(build(ProviderKind::Ollama, None).get("think").is_none());
    assert_eq!(build(ProviderKind::Ollama, Some("off"))["think"], serde_json::json!(false));
    assert_eq!(build(ProviderKind::Ollama, Some("on"))["think"], serde_json::json!(true));
    assert_eq!(build(ProviderKind::Ollama, Some("high"))["think"], serde_json::json!("high"));
    assert!(build(ProviderKind::Ollama, Some("???")).get("think").is_none());
    // OpenAI 호환: 끄기 → "none", 수준 그대로, 켜기·비움은 보내지 않는다
    assert_eq!(build(ProviderKind::OpenaiCompatible, Some("off"))["reasoning_effort"], serde_json::json!("none"));
    assert_eq!(build(ProviderKind::OpenaiCompatible, Some("medium"))["reasoning_effort"], serde_json::json!("medium"));
    assert!(build(ProviderKind::OpenaiCompatible, Some("on")).get("reasoning_effort").is_none());
    assert!(build(ProviderKind::OpenaiCompatible, None).get("reasoning_effort").is_none());
    // Anthropic 은 effort 를 쓴다 — think 는 무시
    assert!(build(ProviderKind::Anthropic, Some("off")).get("think").is_none());
}

/// 로컬 Ollama 로 문법 카드 고르기를 실제로 돌려 본다 (수동: SQLS_ROUTE_MODEL=qwen3.5:4b cargo test -p sqls-llm route_live -- --ignored --nocapture)
#[tokio::test]
#[ignore]
async fn route_live() {
    let model = std::env::var("SQLS_ROUTE_MODEL").unwrap_or_else(|_| "qwen3.5:4b".into());
    let mut cfg = ProviderConfig::new("local", ProviderKind::Ollama, &model);
    cfg.num_ctx = Some(16384);
    cfg.think = Some("off".into());
    cfg.max_tokens = 1024;
    let client = Client::new();
    let all = guides::load(None);
    let qs: Vec<String> = match std::env::var("SQLS_ROUTE_QS") {
        Ok(v) => v.split('|').map(String::from).collect(),
        Err(_) => ["앞에서 다섯 개만 보여줘", "부서별로 급여 제일 많은 사람 한 명씩", "주문 테이블에 있으면 고치고 없으면 넣어줘", "사원 이름을 쉼표로 이어서 부서마다 한 줄로", "조직도처럼 상하 관계로 보여줘", "어제 하루치 주문만"].map(String::from).to_vec(),
    };
    for q in qs.iter().map(String::as_str) {
        let req = guides::route_request(&all, prompts::Task::Generate, q, Some(19)).unwrap();
        let t = std::time::Instant::now();
        let r = client.chat(&cfg, &req, |_| {}).await;
        let el = t.elapsed();
        match r {
            Ok(r) => {
                let ids = guides::parse_route(&r.text, &all);
                let kw = guides::keyword_strong(&all, prompts::Task::Generate, q, Some(19));
                println!("{:>5.1}s  낱말충분={kw:<5} {q}  →  {ids:?}   (원문: {})", el.as_secs_f32(), r.text.replace('\n', " "));
            }
            Err(e) => println!("{:>5.1}s  {q}  →  오류 {e}", el.as_secs_f32()),
        }
    }
}
