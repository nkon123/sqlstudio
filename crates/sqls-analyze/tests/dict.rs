//! 사전 대조 — 사전 의존성과 파서를 맞대고, 다른 곳만 (가짜) 모델에 묻는다.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use sqls_analyze::chunk::UnitSource;
use sqls_analyze::deps::{Dep, UnitDeps};
use sqls_analyze::run::{analyze_unit, Event, Options};
use sqls_analyze::store::Store;
use sqls_llm::{Client, ProviderConfig, ProviderKind};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const BODY: &str = "PACKAGE BODY stock_pkg IS
  PROCEDURE move(p_id NUMBER) IS
    v_sql VARCHAR2(200) := 'DELETE FROM ' || 'move_log WHERE id = :1';
  BEGIN
    UPDATE stock SET qty = qty - 1 WHERE item_id = p_id;
    INSERT INTO cust_syn (id) VALUES (p_id);
    INSERT INTO tmp_work (id) VALUES (p_id);
    EXECUTE IMMEDIATE v_sql USING p_id;
  END move;
  PROCEDURE audit_it(p_id NUMBER) IS
    v item_master%ROWTYPE;
  BEGIN
    NULL;
  END audit_it;
END stock_pkg;";

/// 묻는 글을 보고 답한다
fn answer(user: &str) -> String {
    if user.contains("data dictionary says") {
        assert!(user.contains("item_master%ROWTYPE"), "이름이 나오는 줄을 보내야 한다: {user}");
        json!({"ops": "r", "subprogram": "audit_it", "reason": "행 형식으로만 참조한다"}).to_string()
    } else if user.contains("dynamic SQL") {
        assert!(user.contains("move_log WHERE"), "{user}");
        format!("답: ```json\n{}\n```", json!({"tables": [{"name": "move_log", "ops": "D"}], "reason": "이동 이력을 지운다"}))
    } else {
        panic!("모르는 질문: {user}")
    }
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
            let user = body["messages"].as_array().unwrap().iter().rfind(|m| m["role"] == "user").unwrap()["content"].as_str().unwrap().to_string();
            let text = answer(&user);
            let l1 = json!({"model": "tiny", "message": {"role": "assistant", "content": text}, "done": false});
            let l2 = json!({"model": "tiny", "message": {"role": "assistant", "content": ""}, "done": true, "done_reason": "stop"});
            let payload = format!("{l1}\n{l2}\n");
            let resp = format!("HTTP/1.1 200 OK\r\ncontent-type: application/x-ndjson\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{payload}", payload.len());
            sock.write_all(resp.as_bytes()).await.unwrap();
            let _ = sock.shutdown().await;
        });
    }
}

fn dep(o: &str, n: &str, k: &str, via: Option<&str>) -> Dep {
    Dep { owner: o.into(), name: n.into(), kind: k.into(), via: via.map(String::from), link: None }
}

#[tokio::test]
async fn dictionary_compare_and_supplement() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let hits = Arc::new(AtomicU32::new(0));
    tokio::spawn(serve(listener, hits.clone()));
    let mut cfg = ProviderConfig::new("mock", ProviderKind::Ollama, "tiny");
    cfg.base_url = Some(format!("http://127.0.0.1:{port}"));
    let client = Client::new();

    let dir = std::env::temp_dir().join(format!("sqls-analyze-dict-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let store = Store::open(&dir).unwrap();
    let on: sqls_analyze::run::OnEvent = Arc::new(|_: Event| {});
    let cancel = Arc::new(AtomicBool::new(false));
    let src = UnitSource { owner: "APP".into(), name: "STOCK_PKG".into(), unit_type: "PACKAGE BODY".into(), text: BODY.into() };
    let u = analyze_unit(&store, &src, None, None, &Options::default(), on, cancel.clone()).await.unwrap();

    // 사전 없이: 근거 표시도 대조 숫자도 없다 (동적 SQL 은 확인 필요로 남는다)
    let (g, _) = sqls_analyze::integrate::write_all(&store, None).unwrap();
    assert!(g.tables.iter().all(|t| t.conf.is_empty()));
    assert_eq!(g.dict.as_ref().map(|d| (d.checked, d.dynamic)), Some((0, 1)));
    assert!(g.gaps.iter().any(|x| x.kind == "dynamic" && x.table.is_none()));

    // 사전: STOCK, 동의어 CUST_SYN → ERP.CUSTOMERS, ITEM_MASTER(%ROWTYPE 만), 시퀀스·패키지는 비교하지 않는다
    let deps = vec![
        dep("APP", "STOCK", "TABLE", None),
        dep("ERP", "CUSTOMERS", "TABLE", Some("APP.CUST_SYN")),
        dep("APP", "ITEM_MASTER", "TABLE", None),
        dep("APP", "MOVE_SEQ", "SEQUENCE", None),
        dep("APP", "UTIL_PKG", "PACKAGE", None),
    ];
    store.write_deps(&UnitDeps { key: u.key.clone(), fetched_at: 0, deps }).unwrap();

    let said = Mutex::new(Vec::new());
    let n = sqls_analyze::deps::supplement(&store, &client, &cfg, &cancel, &|w| said.lock().unwrap().push(w)).await;
    assert_eq!(n, 2, "사전만 1 + 동적 SQL 1: {:?}", said.lock().unwrap());
    assert_eq!(hits.load(Ordering::SeqCst), 2);
    // 같은 질문은 다시 묻지 않는다
    assert_eq!(sqls_analyze::deps::supplement(&store, &client, &cfg, &cancel, &|_| {}).await, 0);
    assert_eq!(hits.load(Ordering::SeqCst), 2);

    let (g, _) = sqls_analyze::integrate::write_all(&store, None).unwrap();
    let row = |t: &str| g.tables.iter().find(|r| r.table == t).unwrap_or_else(|| panic!("{t}: {:?}", g.tables.iter().map(|r| &r.table).collect::<Vec<_>>()));
    let mv = "APP.STOCK_PKG.MOVE";
    assert_eq!(row("APP.STOCK").conf[mv], "ok");
    assert_eq!(row("ERP.CUSTOMERS").by[mv], "C", "동의어는 실제 테이블로");
    assert_eq!(row("ERP.CUSTOMERS").conf[mv], "ok");
    assert!(!g.tables.iter().any(|r| r.table == "APP.CUST_SYN"));
    assert_eq!(row("APP.TMP_WORK").conf[mv], "src");
    let im = row("APP.ITEM_MASTER");
    assert_eq!(im.by["APP.STOCK_PKG.AUDIT_IT"], "R", "모델이 짚은 서브프로그램에");
    assert_eq!(im.conf["APP.STOCK_PKG.AUDIT_IT"], "llm");
    let ml = row("APP.MOVE_LOG");
    assert_eq!((ml.by[mv].as_str(), ml.conf[mv].as_str()), ("D", "llm"));
    assert!(ml.impacted_entries.contains(&mv.to_string()), "추정한 쓰기도 영향 범위에 든다");

    let d = g.dict.as_ref().unwrap();
    assert_eq!((d.checked, d.both, d.dict_only, d.source_only, d.dynamic, d.guessed), (1, 2, 1, 1, 1, 2));
    assert!(g.gaps.iter().any(|x| x.kind == "source_only" && x.table.as_deref() == Some("APP.TMP_WORK")));
    assert!(g.gaps.iter().any(|x| x.kind == "dict_only" && x.ops.as_deref() == Some("R") && x.note.contains("행 형식")));
    assert!(g.gaps.iter().any(|x| x.kind == "dynamic" && x.line == Some(8) && x.table.as_deref() == Some("APP.MOVE_LOG")));
    let report = std::fs::read_to_string(dir.join("integrated/report.md")).unwrap();
    assert!(report.contains("## 사전 대조") && report.contains("R (추정)"), "{report}");
    assert!(dir.join("integrated/gaps.json").exists() && dir.join("integrated/supplement.json").exists());
    std::fs::remove_dir_all(&dir).unwrap();
}
