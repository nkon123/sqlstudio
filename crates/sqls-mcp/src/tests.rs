//! MCP 프로토콜 수준 테스트 — 메모리 파이프로 실제 클라이언트를 붙인다.
//!
//! 가장 중요한 성질: **AI 가 SQL 을 실행할 수 있는 툴이 없다.**

use rmcp::model::{CallToolRequestParams, ClientConfig};
use rmcp::ServiceExt;
use serde_json::{json, Value};

use super::*;

const CFG: &str = r#"
[[connection]]
name = "XE"
user = "system"
connect_string = "SQLS_TEST"

[[connection]]
name = "PROD"
user = "erp"
connect_string = "PRODTNS"

[mcp]
allowed_connections = ["xe"]
call_timeout_secs = 20
"#;

/// 실제 DB 가 필요 없는 테스트용: 접속하려 하면 실패
fn no_db() -> Connector {
    Arc::new(|_p| Box::pin(async { Err(Error::Driver("테스트: DB 없음".into())) }))
}

type Client = rmcp::service::RunningService<rmcp::RoleClient, ClientConfig>;

async fn client(server: SqlStudioMcp) -> Client {
    let (a, b) = tokio::io::duplex(1 << 20);
    tokio::spawn(async move {
        let running = server.serve(b).await.unwrap();
        let _ = running.waiting().await;
    });
    ClientConfig::default().serve(a).await.unwrap()
}

async fn call(c: &Client, name: &str, args: Value) -> (bool, String) {
    let args = args.as_object().cloned().unwrap_or_default();
    match c.call_tool(CallToolRequestParams::new(name.to_string()).with_arguments(args)).await {
        Ok(r) => {
            let text = r
                .content
                .iter()
                .filter_map(|c| c.as_text().map(|t| t.text.clone()))
                .collect::<Vec<_>>()
                .join("");
            (r.is_error == Some(true), text)
        }
        Err(e) => (true, e.to_string()),
    }
}

async fn tool_names(c: &Client) -> Vec<String> {
    let mut n: Vec<_> = c.list_all_tools().await.unwrap().iter().map(|t| t.name.to_string()).collect();
    n.sort();
    n
}

#[tokio::test]
async fn no_tool_executes_sql() {
    let cfg = Config::parse(CFG).unwrap();
    let c = client(SqlStudioMcp::new(&cfg, no_db())).await;
    assert_eq!(
        tool_names(&c).await,
        [
            "analysis_findings", "analysis_overview", "analysis_unit", "column_writers", "data_lineage", "describe_table",
            "flow_report", "flow_reports", "get_ddl", "list_connections", "list_objects", "subprogram_relations", "subprogram_sql",
            "table_usage",
        ]
    );
    // 기본 설정에서는 SQL 문장을 인자로 받는 툴이 하나도 없다
    for t in c.list_all_tools().await.unwrap() {
        let schema = serde_json::to_string(&t.input_schema).unwrap();
        assert!(!schema.contains("\"sql\""), "{} 가 SQL 을 받는다: {schema}", t.name);
        assert_eq!(t.annotations.as_ref().and_then(|a| a.read_only_hint), Some(true), "{}", t.name);
    }
    // 예전 이름으로 불러도 없다
    let (err, _) = call(&c, "run_query", json!({"connection": "XE", "sql": "select 1 from dual"})).await;
    assert!(err);
}

#[tokio::test]
async fn explain_only_when_enabled() {
    let text = CFG.replace("call_timeout_secs = 20", "call_timeout_secs = 20\nallow_explain = true");
    let cfg = Config::parse(&text).unwrap();
    let c = client(SqlStudioMcp::new(&cfg, no_db())).await;
    assert!(tool_names(&c).await.contains(&"explain_plan".to_string()));
}

/// run_check 는 allow_checks 일 때만 — 그리고 운영 프로필·SELECT 아닌 것은 DB 에 가기 전에 거절
#[tokio::test]
async fn run_check_only_when_enabled_and_guarded() {
    let cfg = Config::parse(CFG).unwrap();
    let c = client(SqlStudioMcp::new(&cfg, no_db())).await;
    assert!(!tool_names(&c).await.contains(&"run_check".to_string()));
    let (err, _) = call(&c, "run_check", json!({"connection": "XE", "sql": "select 1 from dual", "purpose": "x"})).await;
    assert!(err);

    let text = CFG
        .replace("connect_string = \"PRODTNS\"", "connect_string = \"PRODTNS\"\nproduction = true")
        .replace("allowed_connections = [\"xe\"]", "allowed_connections = [\"xe\", \"prod\"]\nallow_checks = true");
    let cfg = Config::parse(&text).unwrap();
    let log = std::env::temp_dir().join(format!("sqls-mcp-checks-{}.jsonl", std::process::id()));
    let _ = std::fs::remove_file(&log);
    let c = client(SqlStudioMcp::new(&cfg, no_db()).with_check_log(&log)).await;
    assert!(tool_names(&c).await.contains(&"run_check".to_string()));

    let (_, t) = call(&c, "list_connections", json!({})).await;
    let v: Value = serde_json::from_str(&t).unwrap();
    let flags: Vec<_> = v["connections"].as_array().unwrap().iter().map(|c| (c["name"].as_str().unwrap().to_string(), c["run_check"].as_bool().unwrap())).collect();
    assert_eq!(flags, [("XE".to_string(), true), ("PROD".to_string(), false)]);

    let (err, t) = call(&c, "run_check", json!({"connection": "PROD", "sql": "select 1 from dual", "purpose": "x"})).await;
    assert!(err && t.contains("production"), "{t}");
    for bad in [
        "delete from orders",
        "update orders set amount = 0",
        "select * from orders for update",
        "begin null; end;",
        "select 1 from dual; delete from orders",
        "drop table orders",
    ] {
        let (err, t) = call(&c, "run_check", json!({"connection": "XE", "sql": bad, "purpose": "x"})).await;
        assert!(err, "{bad}: {t}");
        assert!(!t.contains("DB 없음"), "{bad} 가 DB 까지 갔다: {t}");
    }
    // SELECT 는 접속까지 간다 (여기서는 DB 가 없어 실패)
    let (err, t) = call(&c, "run_check", json!({"connection": "XE", "sql": "with x as (select 1 n from dual) select * from x where n <> 1;", "purpose": "x"})).await;
    assert!(err && t.contains("DB 없음"), "{t}");

    let lines = std::fs::read_to_string(&log).unwrap();
    assert_eq!(lines.lines().count(), 8, "{lines}");
    assert!(lines.lines().all(|l| serde_json::from_str::<Value>(l).is_ok()));
    std::fs::remove_file(&log).unwrap();
}

#[tokio::test]
async fn only_allowed_profiles_are_visible() {
    let cfg = Config::parse(CFG).unwrap();
    let c = client(SqlStudioMcp::new(&cfg, no_db())).await;
    let (err, text) = call(&c, "list_connections", json!({})).await;
    assert!(!err);
    let v: Value = serde_json::from_str(&text).unwrap();
    let names: Vec<_> = v["connections"].as_array().unwrap().iter().map(|c| c["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["XE"]);
    assert_eq!(v["executes_sql"], false);

    // 허용하지 않은 프로필은 접속도 시도하지 않는다
    let (err, text) = call(&c, "describe_table", json!({"connection": "PROD", "name": "emp"})).await;
    assert!(err);
    assert!(text.contains("Unknown connection 'PROD'"), "{text}");
}

#[tokio::test]
async fn connect_failure_is_a_tool_error_not_a_crash() {
    let cfg = Config::parse(CFG).unwrap();
    let c = client(SqlStudioMcp::new(&cfg, no_db())).await;
    let (err, text) = call(&c, "describe_table", json!({"connection": "xe", "name": "emp"})).await;
    assert!(err);
    assert!(text.contains("DB 없음"), "{text}");
    let (err, _) = call(&c, "list_connections", json!({})).await;
    assert!(!err);
}

/// 실제 Oracle: SQLS_TEST_DSN=system/oracle@host:1521/XE cargo test -p sqls-mcp -- --ignored
#[tokio::test]
#[ignore]
async fn live_tools_over_protocol() {
    let dsn = std::env::var("SQLS_TEST_DSN").expect("SQLS_TEST_DSN");
    let (cred, cs) = dsn.split_once('@').unwrap();
    let (u, p) = cred.split_once('/').unwrap();
    let text = CFG
        .replace("\"system\"", &format!("\"{u}\""))
        .replace("SQLS_TEST", cs)
        .replace("call_timeout_secs = 20", "call_timeout_secs = 20\nallow_explain = true");
    std::env::set_var("SQLSTUDIO_PW_XE", p);
    let cfg = Config::parse(&text).unwrap();
    let c = client(SqlStudioMcp::new(&cfg, oracle_connector())).await;

    let (err, d) = call(&c, "describe_table", json!({"connection": "XE", "name": "sys.dual"})).await;
    assert!(!err, "{d}");
    assert!(d.contains("DUMMY"), "{d}");

    let (err, o) = call(&c, "list_objects", json!({"connection": "XE", "owner": "SYS", "object_type": "TABLE", "name_like": "DUA%"})).await;
    assert!(!err, "{o}");
    assert!(o.contains("DUAL"), "{o}");

    let (err, plan) = call(&c, "explain_plan", json!({"connection": "XE", "sql": "select * from dual where dummy = 'X'"})).await;
    assert!(!err, "{plan}");
    assert!(plan.contains("DUAL"), "{plan}");

    // explain 은 실행하지 않는다 — DML 을 넣어도 데이터는 그대로다 (읽기 전용 세션 + EXPLAIN 만)
    let (err, plan) = call(&c, "explain_plan", json!({"connection": "XE", "sql": "delete from sys.dual"})).await;
    let _ = (err, plan);
    let (err, d) = call(&c, "describe_table", json!({"connection": "XE", "name": "sys.dual"})).await;
    assert!(!err, "{d}");
}

/// 분석 결과(JSON)만 읽는 툴 — DB 없이
#[tokio::test]
async fn analysis_tools_read_stored_results() {
    use sqls_analyze::chunk::UnitSource;
    use sqls_analyze::run::{analyze_unit, Options};
    use sqls_analyze::store::Store;
    let root = std::env::temp_dir().join(format!("sqls-mcp-analysis-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    // 프로필 이름 XE 의 결과 폴더
    let store = Store::open(root.join("XE")).unwrap();
    let on: sqls_analyze::run::OnEvent = Arc::new(|_| {});
    let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
    for (name, text, spec) in [
        ("ORDER_PKG", include_str!("../../sqls-analyze/tests/data/order_pkg.pkb"), Some(include_str!("../../sqls-analyze/tests/data/order_pkg.pks"))),
        ("FLOW_PKG", include_str!("../../sqls-analyze/tests/data/flow_pkg.pkb"), None),
    ] {
        let src = UnitSource { owner: "APP".into(), name: name.into(), unit_type: "PACKAGE BODY".into(), text: text.into() };
        analyze_unit(&store, &src, spec, None, &Options::default(), on.clone(), cancel.clone()).await.unwrap();
    }
    sqls_analyze::integrate::write_all(&store, None).unwrap();

    let cfg = Config::parse(CFG).unwrap();
    let c = client(SqlStudioMcp::new(&cfg, no_db()).with_analysis_root(&root)).await;
    let (err, text) = call(&c, "analysis_overview", json!({"connection": "XE"})).await;
    assert!(!err, "{text}");
    let v: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["units"], 2);

    let (err, text) = call(&c, "analysis_unit", json!({"connection": "XE", "name": "order_pkg"})).await;
    assert!(!err, "{text}");
    let v: Value = serde_json::from_str(&text).unwrap();
    let close = v["subprograms"].as_array().unwrap().iter().find(|s| s["name"] == "CLOSE_ORDER").unwrap();
    assert!(close["tables"].as_array().unwrap().iter().any(|t| t == "ORDERS(U)"));

    let (err, text) = call(&c, "table_usage", json!({"connection": "XE", "table": "order_log"})).await;
    assert!(!err, "{text}");
    let v: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v[0]["table"], "APP.ORDER_LOG");
    assert_eq!(v[0]["data_comes_from"][0], "APP.ORDERS");
    assert_eq!(v[0]["cursor_flows_in"][0]["cursor"], "C_OPEN");

    let (err, text) = call(&c, "subprogram_relations", json!({"connection": "XE", "name": "close_order"})).await;
    assert!(!err, "{text}");
    let v: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v[0]["called_by"][0]["caller"], "APP.ORDER_PKG.NIGHTLY");

    let (err, text) = call(&c, "analysis_findings", json!({"connection": "XE", "kind": "예외 삼킴"})).await;
    assert!(!err, "{text}");
    assert!(text.contains("WHEN"), "{text}");

    // 흐름도 — 계보 (ORDER_LOG 는 ORDERS 의 커서로 채워진다)
    let (err, text) = call(&c, "data_lineage", json!({"connection": "XE", "table": "order_log", "direction": "up"})).await;
    assert!(!err, "{text}");
    let v: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["table"], "APP.ORDER_LOG");
    let steps = v["steps"].as_array().unwrap();
    let s0 = steps.iter().find(|s| s["cursor_flows"].as_array().is_some_and(|f| !f.is_empty())).expect("커서 흐름이 있는 단계");
    assert!(s0["reads_from"].as_array().unwrap().iter().any(|t| t == "APP.ORDERS"), "{s0}");
    assert!(v["mermaid"].as_str().unwrap().starts_with("flowchart LR"));
    let (err, text) = call(&c, "data_lineage", json!({"connection": "XE", "table": "orders", "direction": "down"})).await;
    assert!(!err, "{text}");
    assert!(text.contains("APP.ORDER_LOG"), "{text}");
    let (err, _) = call(&c, "data_lineage", json!({"connection": "XE", "table": "orders", "direction": "sideways"})).await;
    assert!(err);

    // 문장 — 서브프로그램, 패키지 전체
    let (err, text) = call(&c, "subprogram_sql", json!({"connection": "XE", "name": "close_order"})).await;
    assert!(!err, "{text}");
    let v: Value = serde_json::from_str(&text).unwrap();
    assert!(v["statements"].as_array().unwrap().iter().any(|s| s["kind"] == "UPDATE" && s["writes"][0].as_str().unwrap().contains("ORDERS")), "{text}");
    let (err, text) = call(&c, "subprogram_sql", json!({"connection": "XE", "name": "app.order_pkg"})).await;
    assert!(!err, "{text}");
    let v: Value = serde_json::from_str(&text).unwrap();
    assert!(v["total"].as_u64().unwrap() > 1);

    // 열에 값을 넣는 문장 — 데이터를 대는 커서 쿼리까지
    let (err, text) = call(&c, "column_writers", json!({"connection": "XE", "table": "app.order_log", "column": "amt"})).await;
    assert!(!err, "{text}");
    let v: Value = serde_json::from_str(&text).unwrap();
    let w = &v["writers"][0];
    assert_eq!(w["kind"], "INSERT", "{text}");
    assert!(w["fed_by"].as_array().unwrap().iter().any(|f| f["cursor"] == "C_OPEN" && f["query"].as_str().is_some_and(|q| q.contains("ORDERS"))), "{text}");

    // 흐름도 보고서 — 숨긴 선·상자는 빼고, 메모·선 설명은 붙여서
    let (err, text) = call(&c, "flow_reports", json!({"connection": "XE"})).await;
    assert!(!err && text.contains("\"reports\":[]"), "{text}");
    let rdir = root.join("XE").join("integrated").join("reports");
    std::fs::create_dir_all(&rdir).unwrap();
    std::fs::write(rdir.join("r1.json"), serde_json::to_string(&json!({
        "format": 1, "id": "r1", "name": "마감", "title": "주문 마감 흐름", "description": "야간 마감", "created_at": 1, "updated_at": 2, "profile": "XE",
        "basis": {"center": "APP.ORDER_LOG", "dir": "up", "depth": 2, "unit": "proc", "calls": false, "flows": false, "hidden": []},
        "nodes": [
            {"id": "a", "ref": "APP.ORDERS", "type": "table", "label": "ORDERS", "x": 0, "y": 0, "w": 1, "h": 1, "lane": 0},
            {"id": "b", "ref": "APP.FLOW_PKG.RUN", "type": "proc", "label": "NIGHTLY", "text": "야간 배치", "x": 0, "y": 0, "w": 1, "h": 1, "lane": 1},
            {"id": "c", "ref": "APP.ORDER_LOG", "type": "table", "label": "ORDER_LOG", "x": 0, "y": 0, "w": 1, "h": 1, "lane": 2},
            {"id": "h", "ref": "APP.SECRET", "type": "table", "label": "HIDDEN_BOX", "hidden": true, "x": 0, "y": 0, "w": 1, "h": 1, "lane": 2},
            {"id": "m", "ref": "", "type": "note", "label": "", "text": "닫힌 주문만 로그로", "attach": "b", "x": 0, "y": 0, "w": 1, "h": 1, "lane": 1}
        ],
        "edges": [
            {"id": "e1", "source": "a", "target": "b", "kind": "read", "label": "R"},
            {"id": "e2", "source": "b", "target": "c", "kind": "write", "label": "C", "text": "주문마다 한 줄"},
            {"id": "e3", "source": "b", "target": "h", "kind": "write", "label": "U", "hidden": true}
        ]
    })).unwrap()).unwrap();
    let (err, text) = call(&c, "flow_reports", json!({"connection": "XE"})).await;
    assert!(!err && text.contains("주문 마감 흐름"), "{text}");
    let (err, text) = call(&c, "flow_report", json!({"connection": "XE", "id": "마감"})).await;
    assert!(!err, "{text}");
    let v: Value = serde_json::from_str(&text).unwrap();
    assert!(!text.contains("HIDDEN_BOX"), "{text}");
    assert_eq!(v["boxes"].as_array().unwrap().len(), 3);
    let batch = v["boxes"].as_array().unwrap().iter().find(|b| b["id"] == "b").unwrap();
    assert_eq!(batch["label"], "야간 배치");
    assert!(batch["tables"].as_array().unwrap().iter().any(|t| t.as_str().unwrap().starts_with("APP.ORDER_LOG")), "{batch}");
    assert_eq!(v["notes"][0]["about"], "야간 배치");
    assert!(v["edges"].as_array().unwrap().iter().any(|e| e["note"] == "주문마다 한 줄" && e["from"] == "야간 배치"));
    assert!(v["mermaid"].as_str().unwrap().contains("주문마다 한 줄"));
    let (err, _) = call(&c, "flow_report", json!({"connection": "XE", "id": "nope"})).await;
    assert!(err);

    // 허용하지 않은 프로필, 분석이 없는 경우
    let (err, text) = call(&c, "analysis_overview", json!({"connection": "PROD"})).await;
    assert!(err && text.contains("Unknown connection"), "{text}");
    let c2 = client(SqlStudioMcp::new(&cfg, no_db()).with_analysis_root(root.join("nothing"))).await;
    let (err, text) = call(&c2, "analysis_overview", json!({"connection": "XE"})).await;
    assert!(err && text.contains("No PL/SQL analysis"), "{text}");
    std::fs::remove_dir_all(&root).unwrap();
}
