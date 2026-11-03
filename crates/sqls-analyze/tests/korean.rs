//! 한국어 분석에 한자·가나가 섞이면 마지막에 한국어로 다시 쓰게 한다 (가짜 Ollama).

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use serde_json::{json, Value};
use sqls_analyze::llm::{ask_summary, foreign_chars, rollup_system, Lang};
use sqls_llm::{Client, ProviderConfig, ProviderKind};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// 처음 답은 중국어가 섞이고, 언어를 고쳐 달라면 한국어로
fn answer(system: &str, user: &str) -> String {
    if system.contains("fix the language") {
        return json!({"summary": "주문을 마감하고 재고를 줄인다", "steps": ["재고 확인", "주문 상태 변경"], "rules": ["재고 부족이면 '库存不足' 오류"], "risks": []}).to_string();
    }
    if user.contains("QUOTED") {
        return json!({"summary": "재고가 모자라면 '库存不足' 오류를 낸다", "steps": [], "rules": [], "risks": []}).to_string();
    }
    json!({"summary": "주문을 마감하고 库存을 줄인다", "steps": ["库存 확인", "주문 상태 변경"], "rules": ["재고 부족이면 '库存不足' 오류"], "risks": []}).to_string()
}

async fn serve(listener: tokio::net::TcpListener, hits: Arc<AtomicU32>) {
    loop {
        let Ok((mut sock, _)) = listener.accept().await else { return };
        let hits = hits.clone();
        tokio::spawn(async move {
            let mut buf = Vec::new();
            let mut tmp = [0u8; 8192];
            let (head_end, len) = loop {
                let n = sock.read(&mut tmp).await.unwrap();
                if n == 0 {
                    return;
                }
                buf.extend_from_slice(&tmp[..n]);
                if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                    let head = String::from_utf8_lossy(&buf[..p]).to_lowercase();
                    let len = head.lines().find_map(|l| l.strip_prefix("content-length:").map(|v| v.trim().parse::<usize>().unwrap())).unwrap_or(0);
                    break (p + 4, len);
                }
            };
            while buf.len() < head_end + len {
                let n = sock.read(&mut tmp).await.unwrap();
                buf.extend_from_slice(&tmp[..n]);
            }
            let body: Value = serde_json::from_slice(&buf[head_end..head_end + len]).unwrap();
            hits.fetch_add(1, Ordering::SeqCst);
            let msgs = body["messages"].as_array().unwrap();
            let system = msgs.iter().find(|m| m["role"] == "system").and_then(|m| m["content"].as_str()).unwrap_or("").to_string();
            let user = msgs.iter().rfind(|m| m["role"] == "user").unwrap()["content"].as_str().unwrap().to_string();
            let l1 = json!({"model": "tiny", "message": {"role": "assistant", "content": answer(&system, &user)}, "done": false});
            let l2 = json!({"model": "tiny", "message": {"role": "assistant", "content": ""}, "done": true, "done_reason": "stop"});
            let payload = format!("{l1}\n{l2}\n");
            let resp = format!("HTTP/1.1 200 OK\r\ncontent-type: application/x-ndjson\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{payload}", payload.len());
            sock.write_all(resp.as_bytes()).await.unwrap();
            let _ = sock.shutdown().await;
        });
    }
}

#[tokio::test]
async fn chinese_in_korean_answer_is_rewritten() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let hits = Arc::new(AtomicU32::new(0));
    tokio::spawn(serve(listener, hits.clone()));
    let mut cfg = ProviderConfig::new("mock", ProviderKind::Ollama, "tiny");
    cfg.base_url = Some(format!("http://127.0.0.1:{port}"));
    let client = Client::new();

    // 한국어: 섞이면 한 번 더 물어 고친다. 따옴표 안(코드의 글)은 그대로
    let (ins, meta) = ask_summary(&client, &cfg, rollup_system(Lang::Ko, "a procedure"), "Unit: X".into()).await.unwrap();
    assert_eq!(hits.load(Ordering::SeqCst), 2);
    assert_eq!(ins.summary, "주문을 마감하고 재고를 줄인다");
    assert_eq!(foreign_chars(&ins), 0);
    assert!(ins.rules[0].contains("'库存不足'"), "{:?}", ins.rules);
    assert!(meta.repaired.iter().any(|r| r.contains("한국어로 다시 씀")), "{:?}", meta.repaired);

    // 따옴표 안에만 있으면 다시 묻지 않는다
    let (_, meta) = ask_summary(&client, &cfg, rollup_system(Lang::Ko, "a procedure"), "QUOTED".into()).await.unwrap();
    assert_eq!(hits.load(Ordering::SeqCst), 3);
    assert!(meta.repaired.is_empty(), "{:?}", meta.repaired);

    // 영어로 물었으면 손대지 않는다
    let (ins, _) = ask_summary(&client, &cfg, rollup_system(Lang::En, "a procedure"), "Unit: X".into()).await.unwrap();
    assert_eq!(hits.load(Ordering::SeqCst), 4);
    assert!(foreign_chars(&ins) > 0);
}
