//! 가짜 Ollama 서버로 끝에서 끝까지: 조각 → 모델 → 저장 → 다시 돌리면 건너뛰기 → 통합 분석.
//! 작은 모델이 흔히 내는 깨진 답(코드 펜스, 끝 쉼표, 엉뚱한 글)을 섞어 보낸다.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use sqls_analyze::chunk::{Limits, UnitSource};
use sqls_analyze::run::{analyze_unit, Event, Llm, Options};
use sqls_analyze::store::Store;
use sqls_llm::{Client, ProviderConfig, ProviderKind};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const BODY: &str = include_str!("data/order_pkg.pkb");
const SPEC: &str = include_str!("data/order_pkg.pks");
const AUDIT: &str = "PACKAGE BODY audit_pkg IS\n  PROCEDURE log(p_kind VARCHAR2, p_id NUMBER) IS\n    PRAGMA AUTONOMOUS_TRANSACTION;\n  BEGIN\n    INSERT INTO audit_log (kind, ref_id, ts) VALUES (p_kind, p_id, SYSDATE);\n    COMMIT;\n  END log;\nEND audit_pkg;";

struct Mock {
    hits: AtomicU32,
    bodies: Mutex<Vec<Value>>,
    nightly_seen: AtomicBool,
    /// 긴 커서 시험용 답
    long: bool,
}

/// 작은 모델처럼: 조각이 어느 절인지만 보고 짧게 답한다
fn long_answer(user: &str) -> String {
    if user.contains("ONE SQL statement") {
        return json!({"summary": "월별 활성 고객의 상태별 매출을 집계해 순위 1000위 안(또는 VIP·GOLD·프로모션 고객)을 고른다",
            "steps": ["활성·미차단 고객", "상태별 월 매출 합계", "건수 순위", "지역 붙이기"], "rules": ["rnk <= 1000", "VIP/GOLD 는 순위와 무관"], "risks": []}).to_string();
    }
    if user.contains("Describe the whole unit") {
        return json!({"summary": "월별 고객 매출 보고서를 만드는 패키지", "steps": [], "rules": [], "risks": []}).to_string();
    }
    let part = user.lines().find(|l| l.starts_with("긴 SQL 의 일부")).unwrap_or("").to_string();
    let what = part.split("이 조각이다").nth(1).unwrap_or("").trim_matches(|c: char| c == ' ' || c == '(' || c == ')' || c == '.').to_string();
    let summary = if what.is_empty() { "보고서 테이블을 다시 채운다".to_string() } else { format!("{what} 부분") };
    json!({"summary": summary, "steps": [], "rules": [], "risks": []}).to_string()
}

fn answer(user: &str, mock: &Mock) -> String {
    if mock.long {
        return long_answer(user);
    }
    if user.contains("Piece: CLOSE_ORDER") {
        // 코드 펜스 + 끝 쉼표 + 범위 밖 줄
        return "다음은 분석입니다:\n```json\n{\"summary\": \"주문을 마감하고 이력을 남긴다\", \"steps\": [\"합계 계산\", \"상태 변경\",], \"rules\": [\"합계 1000 초과면 감사 로그\"], \"risks\": [{\"line\": 41, \"issue\": \"NO_DATA_FOUND 를 삼킨다\"}, {\"line\": 9999, \"issue\": \"엉뚱한 줄\"}],}\n```".into();
    }
    if user.contains("Piece: NIGHTLY") && !mock.nightly_seen.swap(true, Ordering::SeqCst) {
        return "I think this procedure loops over orders.".into();
    }
    if user.contains("Combine") || user.contains("Describe the whole") {
        return json!({"summary": "주문 처리 패키지", "steps": ["합계", "마감"], "rules": [], "risks": []}).to_string();
    }
    // 객체 모양 단계, 문자열 규칙 — 느슨하게 읽혀야 한다
    json!({"summary": "조각 요약", "steps": [{"step": "한다"}], "rules": "- 규칙 하나\n- 규칙 둘", "risks": []}).to_string()
}

async fn serve(listener: tokio::net::TcpListener, mock: Arc<Mock>) {
    loop {
        let Ok((mut sock, _)) = listener.accept().await else { return };
        let mock = mock.clone();
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
            mock.hits.fetch_add(1, Ordering::SeqCst);
            let user = body["messages"].as_array().unwrap().iter().filter(|m| m["role"] == "user").next_back().map(|m| m["content"].as_str().unwrap().to_string()).unwrap();
            let first_user = body["messages"][1]["content"].as_str().unwrap().to_string();
            let text = if user.starts_with("That was not") { answer("", &mock) } else { answer(&first_user, &mock) };
            mock.bodies.lock().unwrap().push(body);
            // 두 줄로 나눠 스트리밍
            let mid = text.char_indices().nth(text.chars().count() / 2).map(|(i, _)| i).unwrap_or(0);
            let l1 = json!({"model": "tiny", "message": {"role": "assistant", "content": &text[..mid]}, "done": false});
            let l2 = json!({"model": "tiny", "message": {"role": "assistant", "content": &text[mid..]}, "done": false});
            let l3 = json!({"model": "tiny", "message": {"role": "assistant", "content": ""}, "done": true, "done_reason": "stop", "prompt_eval_count": 100, "eval_count": 50});
            let payload = format!("{l1}\n{l2}\n{l3}\n");
            let resp = format!("HTTP/1.1 200 OK\r\ncontent-type: application/x-ndjson\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{payload}", payload.len());
            sock.write_all(resp.as_bytes()).await.unwrap();
            let _ = sock.shutdown().await;
        });
    }
}

#[tokio::test]
async fn end_to_end_with_messy_small_model() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let mock = Arc::new(Mock { hits: AtomicU32::new(0), bodies: Mutex::new(Vec::new()), nightly_seen: AtomicBool::new(false), long: false });
    tokio::spawn(serve(listener, mock.clone()));

    let mut cfg = ProviderConfig::new("mock", ProviderKind::Ollama, "tiny");
    cfg.base_url = Some(format!("http://127.0.0.1:{port}"));
    let client = Client::new();
    let dir = std::env::temp_dir().join(format!("sqls-analyze-e2e-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let store = Store::open(&dir).unwrap();
    let opts = Options { limits: Limits { max_lines: 40, max_chars: 4000 }, ..Default::default() };
    let events = Arc::new(Mutex::new(Vec::new()));
    let ev = events.clone();
    let on: sqls_analyze::run::OnEvent = Arc::new(move |e: Event| ev.lock().unwrap().push(e));
    let cancel = Arc::new(AtomicBool::new(false));

    let body = UnitSource { owner: "APP".into(), name: "ORDER_PKG".into(), unit_type: "PACKAGE BODY".into(), text: BODY.into() };
    let u = analyze_unit(&store, &body, Some(SPEC), Some(Llm { client: &client, cfg: &cfg }), &opts, on.clone(), cancel.clone()).await.unwrap();
    assert_eq!(u.stats.failed, 0, "{:?}", u.stats);
    assert_eq!(u.stats.cached, 0);
    let first_hits = mock.hits.load(Ordering::SeqCst);
    assert!(first_hits >= u.stats.chunks + 1, "조각 + 재시도 + 요약: {first_hits}");

    // 요청에는 스키마와 낮은 temperature 가 들어간다
    {
        let b = mock.bodies.lock().unwrap();
        assert_eq!(b[0]["format"]["type"], "object");
        assert!((b[0]["options"]["temperature"].as_f64().unwrap() - 0.1).abs() < 1e-6);
        let user = b.iter().map(|x| x["messages"][1]["content"].as_str().unwrap().to_string()).find(|m| m.contains("Piece: CLOSE_ORDER")).unwrap();
        assert!(user.contains("tables (C=insert R=select U=update D=delete): DAILY_SUM(CU), ORDERS(U)"), "{user}");
        assert!(user.contains("명세 주석"), "{user}");
    }

    // 짧은 전역 커서 C_OPEN 은 NIGHTLY 가 쓴다 → 커서 뜻을 먼저 묻고, NIGHTLY 프롬프트에 넣는다
    {
        let b = mock.bodies.lock().unwrap();
        let users: Vec<String> = b.iter().map(|x| x["messages"][1]["content"].as_str().unwrap().to_string()).collect();
        let cur = users.iter().position(|m| m.contains("Describe what cursor C_OPEN")).expect("커서 뜻 요청");
        assert!(users[cur].contains("SELECT ID FROM ORDERS WHERE STATUS = 'OPEN'"), "{}", users[cur]);
        let nightly = users.iter().position(|m| m.starts_with("Unit:") && m.contains("Piece: NIGHTLY")).unwrap();
        assert!(cur < nightly);
        assert!(users[nightly].contains("Cursor meanings (analyzed first):\n- cursor C_OPEN: "), "{}", users[nightly]);
        // 커서를 안 쓰는 조각에는 붙지 않는다
        let calc = users.iter().find(|m| m.starts_with("Unit:") && m.contains("Piece: CALC_TOTAL")).unwrap();
        assert!(!calc.contains("Cursor meanings"));
    }
    assert!(u.sql_summaries.iter().any(|q| q.cursor.as_deref() == Some("C_OPEN")));

    // 깨진 답을 고쳐 읽었다
    let close = u.subprograms.iter().find(|s| s.path == "CLOSE_ORDER").unwrap();
    let cid = &close.chunk_ids[0];
    let cr = store.read_chunk(&u.key, cid).unwrap();
    let ins = cr.insight.unwrap();
    assert_eq!(ins.summary, "주문을 마감하고 이력을 남긴다");
    assert_eq!(ins.steps, vec!["합계 계산", "상태 변경"]);
    assert_eq!(ins.risks.len(), 2);
    assert_eq!(ins.risks[0].line, Some(41));
    assert_eq!(ins.risks[1].line, None, "범위 밖 줄은 버린다");
    let meta = cr.llm.unwrap();
    assert!(meta.repaired.iter().any(|r| r.contains("끝 쉼표")), "{:?}", meta.repaired);
    assert_eq!(close.public, Some(true));
    // 엉뚱한 답 → 다시 물어서 받았다
    let nightly = u.subprograms.iter().find(|s| s.path == "NIGHTLY").unwrap();
    let nr = store.read_chunk(&u.key, &nightly.chunk_ids[0]).unwrap();
    assert_eq!(nr.llm.unwrap().attempts, 2);
    // 느슨한 모양
    let calc = u.subprograms.iter().find(|s| s.path == "CALC_TOTAL").unwrap();
    let s = calc.summary.as_ref().unwrap();
    assert!(s.steps == vec!["한다"] || s.summary == "주문 처리 패키지", "{s:?}");
    // 여러 조각으로 나뉜 서브프로그램은 요약이 하나로
    let multi: Vec<_> = u.subprograms.iter().filter(|s| s.chunk_ids.len() > 1).collect();
    assert!(!multi.is_empty(), "한도 40줄이면 나뉘는 서브프로그램이 있어야 한다");
    for m in multi {
        assert_eq!(m.summary.as_ref().unwrap().summary, "주문 처리 패키지");
    }
    assert_eq!(u.summary.as_ref().unwrap().summary, "주문 처리 패키지");
    // 비공개
    assert_eq!(u.subprograms.iter().find(|s| s.path == "VALIDATE").unwrap().public, Some(false));

    // 다시 돌리면 묻지 않는다
    let before = mock.hits.load(Ordering::SeqCst);
    let u2 = analyze_unit(&store, &body, Some(SPEC), Some(Llm { client: &client, cfg: &cfg }), &opts, on.clone(), cancel.clone()).await.unwrap();
    assert_eq!(u2.stats.cached, u2.stats.chunks);
    assert_eq!(mock.hits.load(Ordering::SeqCst), before, "모든 조각과 요약을 재사용해야 한다");

    // 한 서브프로그램만 바꾸면 그 조각만 다시
    let changed = UnitSource { text: BODY.replace("v_amt > 1000", "v_amt > 2000"), ..body.clone() };
    let u3 = analyze_unit(&store, &changed, Some(SPEC), Some(Llm { client: &client, cfg: &cfg }), &opts, on.clone(), cancel.clone()).await.unwrap();
    assert_eq!(u3.stats.asked, 1, "{:?}", u3.stats);

    // 다른 단위는 정적 분석만
    let audit = UnitSource { owner: "APP".into(), name: "AUDIT_PKG".into(), unit_type: "PACKAGE BODY".into(), text: AUDIT.into() };
    analyze_unit(&store, &audit, None, None, &opts, on.clone(), cancel.clone()).await.unwrap();

    // 통합
    let (g, files) = sqls_analyze::integrate::write_all(&store, None).unwrap();
    assert_eq!(files.len(), 7);
    assert!(dir.join("integrated/statements.json").exists() && dir.join("integrated/flows.json").exists());
    let e = g.edges.iter().find(|e| e.from == "APP.ORDER_PKG.CLOSE_ORDER" && e.to == "APP.AUDIT_PKG.LOG").expect("패키지 사이 호출을 풀어야 한다");
    assert!(e.resolved);
    assert!(g.edges.iter().any(|e| e.from == "APP.ORDER_PKG.NIGHTLY" && e.to == "APP.ORDER_PKG.CLOSE_ORDER"));
    assert!(g.edges.iter().any(|e| e.from == "APP.ORDER_PKG.CALC_TOTAL" && e.to == "APP.ORDER_PKG.CALC_TOTAL.LINE_AMT"));
    assert!(g.edges.iter().any(|e| e.to == "DBMS_OUTPUT.PUT_LINE" && e.external.as_deref() == Some("system")));
    // 시작점: 공개이고 아무도 안 부르는 것
    assert!(g.entries.contains(&"APP.ORDER_PKG.NIGHTLY".to_string()), "{:?}", g.entries);
    assert!(!g.entries.contains(&"APP.ORDER_PKG.CLOSE_ORDER".to_string()));
    assert!(g.unused.contains(&"APP.ORDER_PKG.VALIDATE".to_string()), "{:?}", g.unused);
    // NIGHTLY → CLOSE_ORDER(COMMIT), → AUDIT_PKG.LOG(자율 COMMIT)
    let tx = &g.transactions["APP.ORDER_PKG.NIGHTLY"];
    assert!(tx.contains(&"APP.ORDER_PKG.CLOSE_ORDER".to_string()) && tx.contains(&"APP.AUDIT_PKG.LOG".to_string()), "{tx:?}");
    let orders = g.tables.iter().find(|t| t.table == "APP.ORDERS").unwrap();
    assert_eq!(orders.by["APP.ORDER_PKG.CLOSE_ORDER"], "U");
    assert!(orders.impacted_entries.contains(&"APP.ORDER_PKG.NIGHTLY".to_string()));
    let audit_log = g.tables.iter().find(|t| t.table == "APP.AUDIT_LOG").unwrap();
    assert!(audit_log.impacted_entries.contains(&"APP.ORDER_PKG.NIGHTLY".to_string()));
    assert!(g.cycles.iter().any(|c| c.contains(&"APP.ORDER_PKG.RETRY".to_string())), "{:?}", g.cycles);
    assert!(g.findings.iter().any(|f| f.kind == "예외 삼킴" && f.line == Some(41)));
    assert!(g.findings.iter().any(|f| f.source == "llm" && f.message.contains("NO_DATA_FOUND")));
    let report = std::fs::read_to_string(dir.join("integrated/report.md")).unwrap();
    assert!(report.contains("```mermaid"));
    assert!(report.contains("주문 처리 패키지"));

    // 진행 이벤트
    let ev = events.lock().unwrap();
    assert!(ev.iter().any(|e| matches!(e, Event::Chunk { status: "cached", .. })));
    assert!(ev.iter().any(|e| matches!(e, Event::Rollup { .. })));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test]
async fn provider_down_stops_quickly() {
    // 아무도 듣지 않는 포트
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    drop(l);
    let mut cfg = ProviderConfig::new("down", ProviderKind::Ollama, "tiny");
    cfg.base_url = Some(format!("http://127.0.0.1:{port}"));
    cfg.connect_timeout_secs = 2;
    let dir = std::env::temp_dir().join(format!("sqls-analyze-down-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let store = Store::open(&dir).unwrap();
    let client = Client::new();
    let body = UnitSource { owner: "APP".into(), name: "ORDER_PKG".into(), unit_type: "PACKAGE BODY".into(), text: BODY.into() };
    let on: sqls_analyze::run::OnEvent = Arc::new(|_| {});
    let t = std::time::Instant::now();
    let r = analyze_unit(&store, &body, None, Some(Llm { client: &client, cfg: &cfg }), &Options::default(), on, Arc::new(AtomicBool::new(false))).await;
    let e = r.unwrap_err();
    assert!(e.contains("모델 서버"), "{e}");
    assert!(t.elapsed().as_secs() < 30, "{:?}", t.elapsed());
    // 실패한 조각도 파일로 남는다 (다음에 다시 묻는다)
    let u = store.read_unit("APP.ORDER_PKG.PACKAGE_BODY").unwrap();
    assert!(u.stats.failed >= 3);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test]
async fn long_cursor_with_small_model() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let mock = Arc::new(Mock { hits: AtomicU32::new(0), bodies: Mutex::new(Vec::new()), nightly_seen: AtomicBool::new(false), long: true });
    tokio::spawn(serve(listener, mock.clone()));
    let mut cfg = ProviderConfig::new("mock", ProviderKind::Ollama, "tiny");
    cfg.base_url = Some(format!("http://127.0.0.1:{port}"));
    let client = Client::new();
    let dir = std::env::temp_dir().join(format!("sqls-analyze-long-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let store = Store::open(&dir).unwrap();
    // 작은 모델 설정: 조각 40줄
    let opts = Options { limits: Limits { max_lines: 40, max_chars: 2400 }, ..Default::default() };
    let src = UnitSource { owner: "APP".into(), name: "REPORT_PKG".into(), unit_type: "PACKAGE BODY".into(), text: include_str!("data/report_pkg.pkb").into() };
    let on: sqls_analyze::run::OnEvent = Arc::new(|_| {});
    let u = analyze_unit(&store, &src, None, Some(Llm { client: &client, cfg: &cfg }), &opts, on, Arc::new(AtomicBool::new(false))).await.unwrap();
    assert_eq!(u.stats.failed, 0);

    let bodies = mock.bodies.lock().unwrap().clone();
    let users: Vec<String> = bodies.iter().map(|b| b["messages"][1]["content"].as_str().unwrap().to_string()).collect();
    // 커서 조각: 어디서 잘렸고 무엇을 알고 묻는가
    let parts: Vec<&String> = users.iter().filter(|m| m.contains("긴 SQL 의 일부: CURSOR C_REPORT")).collect();
    assert!(parts.len() >= 3, "{}", parts.len());
    for m in &parts {
        assert!(m.contains("SQL 전체 구조: WITH ACTIVE_CUST 4~13 · WITH MONTHLY 14~46 · WITH RANKED 47~51 · SELECT 52~81 · FROM 82~85 · WHERE 86~91 · ORDER BY 92"), "{m}");
        assert!(m.contains("이 SQL 전체가 읽는 테이블: CUSTOMERS, CUST_BLOCK, ORDERS, PROMOTIONS, REGIONS"), "CTE 는 테이블이 아니다: {m}");
        assert!(m.contains("이 커서의 데이터가 들어가는 곳: REPORT_MONTHLY(C)@99 [record R]"), "{m}");
        // 작은 모델 문맥 안: 지시문 + 문맥 + 코드
        let sys = bodies[0]["messages"][0]["content"].as_str().unwrap();
        let chars = m.chars().count() + sys.chars().count();
        assert!(chars < 6000, "프롬프트 {chars}자");
    }
    // SQL 하나로 모은 요약
    let rollup = users.iter().find(|m| m.contains("ONE SQL statement: cursor C_REPORT")).expect("긴 SQL 요약 요청");
    assert!(rollup.contains("Part (lines 1-13)") || rollup.contains("Part (lines 3-13)") || rollup.contains("WITH ACTIVE_CUST 부분"), "{rollup}");
    assert_eq!(u.sql_summaries.len(), 1);
    let q = &u.sql_summaries[0];
    assert_eq!((q.cursor.as_deref(), q.line, q.end_line), (Some("C_REPORT"), 3, 92));
    assert!(q.chunk_ids.len() >= 3);
    assert!(q.summary.summary.starts_with("월별 활성 고객"));
    // 순서: 커서 조각들 → 커서 요약 → 커서를 쓰는 BUILD (커서의 뜻을 알고 읽는다)
    let pos = |pat: &str| users.iter().position(|m| m.contains(pat)).unwrap_or_else(|| panic!("{pat} 요청이 없다"));
    let last_part = users.iter().rposition(|m| m.starts_with("Unit:") && m.contains("긴 SQL 의 일부: CURSOR C_REPORT")).unwrap();
    let sql_req = pos("ONE SQL statement: cursor C_REPORT");
    let build_req = pos("Piece: BUILD");
    assert!(last_part < sql_req && sql_req < build_req, "순서 {last_part} {sql_req} {build_req}");
    assert!(users[build_req].contains("Cursor meanings (analyzed first):\n- cursor C_REPORT: 월별 활성 고객"), "{}", users[build_req]);
    // 단위 요약은 SQL 요약을 입력으로 쓴다
    let unit_req = users.iter().find(|m| m.contains("Describe the whole unit")).unwrap();
    assert!(unit_req.contains("(global) CURSOR C_REPORT: 월별 활성 고객"), "{unit_req}");
    // 통합: 흐름에 커서의 뜻이 붙는다
    let (g, _) = sqls_analyze::integrate::write_all(&store, None).unwrap();
    let f = g.flows.iter().find(|f| f.cursor == "C_REPORT").unwrap();
    assert_eq!(f.to, "APP.REPORT_MONTHLY");
    assert_eq!(f.from, vec!["APP.CUSTOMERS", "APP.CUST_BLOCK", "APP.ORDERS", "APP.PROMOTIONS", "APP.REGIONS"]);
    assert!(f.cursor_summary.as_deref().unwrap().starts_with("월별 활성 고객"));
    // 과정을 보고 싶으면: SQLS_SHOW=1 cargo test -p sqls-analyze --test mock_llm long_cursor -- --nocapture
    if std::env::var("SQLS_SHOW").is_ok() {
        for (i, m) in users.iter().enumerate() {
            println!("──── 요청 {} ({}자) ────\n{m}\n", i + 1, m.chars().count());
        }
        println!("──── 저장된 SQL 요약 ────\n{}", serde_json::to_string_pretty(&u.sql_summaries).unwrap());
    }
    std::fs::remove_dir_all(&dir).unwrap();
}
