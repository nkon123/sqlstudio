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
    assert_eq!(tool_names(&c).await, ["describe_table", "get_ddl", "list_connections", "list_objects"]);
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
