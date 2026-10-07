//! MCP 프로토콜 수준 테스트 — 메모리 파이프로 실제 클라이언트를 붙인다.

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
max_rows = 50
call_timeout_secs = 20
"#;

/// 실제 DB 가 필요 없는 테스트용: 접속하려 하면 실패
fn no_db() -> Connector {
    Arc::new(|_p| Box::pin(async { Err(Error::Driver("테스트: DB 없음".into())) }))
}

async fn client(server: SqlStudioMcp) -> rmcp::service::RunningService<rmcp::RoleClient, ClientConfig> {
    let (a, b) = tokio::io::duplex(1 << 20);
    tokio::spawn(async move {
        let running = server.serve(b).await.unwrap();
        let _ = running.waiting().await;
    });
    ClientConfig::default().serve(a).await.unwrap()
}

async fn call(c: &rmcp::service::RunningService<rmcp::RoleClient, ClientConfig>, name: &str, args: Value) -> (bool, String) {
    let args = args.as_object().cloned().unwrap_or_default();
    let r = c
        .call_tool(CallToolRequestParams::new(name.to_string()).with_arguments(args))
        .await
        .unwrap();
    let text = r
        .content
        .iter()
        .filter_map(|c| c.as_text().map(|t| t.text.clone()))
        .collect::<Vec<_>>()
        .join("");
    (r.is_error == Some(true), text)
}

#[tokio::test]
async fn tools_are_listed_and_read_only() {
    let cfg = Config::parse(CFG).unwrap();
    let c = client(SqlStudioMcp::new(&cfg, no_db())).await;
    let tools = c.list_all_tools().await.unwrap();
    let mut names: Vec<_> = tools.iter().map(|t| t.name.to_string()).collect();
    names.sort();
    assert_eq!(names, ["describe_table", "explain_plan", "get_ddl", "list_connections", "list_objects", "run_query"]);
    for t in &tools {
        assert_eq!(t.annotations.as_ref().and_then(|a| a.read_only_hint), Some(true), "{}", t.name);
    }
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
    assert_eq!(v["read_only"], true);

    // 허용하지 않은 프로필은 접속도 시도하지 않는다
    let (err, text) = call(&c, "run_query", json!({"connection": "PROD", "sql": "select 1 from dual"})).await;
    assert!(err);
    assert!(text.contains("Unknown connection 'PROD'"), "{text}");
}

#[tokio::test]
async fn writes_are_rejected_before_connecting() {
    let cfg = Config::parse(CFG).unwrap();
    let c = client(SqlStudioMcp::new(&cfg, no_db())).await;
    for sql in [
        "delete from emp",
        "update emp set sal = 0",
        "select * from emp for update",
        "begin execute immediate 'drop table emp'; end;",
        "drop table emp",
    ] {
        let (err, text) = call(&c, "run_query", json!({"connection": "XE", "sql": sql})).await;
        assert!(err, "{sql}");
        assert!(text.contains("read-only"), "{sql}: {text}");
    }
}

#[tokio::test]
async fn connect_failure_is_a_tool_error_not_a_crash() {
    let cfg = Config::parse(CFG).unwrap();
    let c = client(SqlStudioMcp::new(&cfg, no_db())).await;
    let (err, text) = call(&c, "run_query", json!({"connection": "xe", "sql": "select 1 from dual"})).await;
    assert!(err);
    assert!(text.contains("DB 없음"), "{text}");
    // 서버는 계속 응답한다
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
    let text = CFG.replace("\"system\"", &format!("\"{u}\"")).replace("SQLS_TEST", cs);
    std::env::set_var("SQLSTUDIO_PW_XE", p);
    let cfg = Config::parse(&text).unwrap();
    let c = client(SqlStudioMcp::new(&cfg, oracle_connector())).await;

    let (err, text) = call(&c, "run_query", json!({
        "connection": "XE",
        "sql": "select level n, 'r' || level label from dual where level <= :lim connect by level <= 1000",
        "binds": {"lim": "120"},
    })).await;
    assert!(!err, "{text}");
    let v: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["row_count"], 50, "max_rows 상한");
    assert_eq!(v["truncated"], true);
    assert_eq!(v["columns"][0]["name"], "N");

    let (err, plan) = call(&c, "explain_plan", json!({"connection": "XE", "sql": "select * from dual where dummy = 'X'"})).await;
    assert!(!err, "{plan}");
    assert!(plan.contains("DUAL"), "{plan}");

    let (err, d) = call(&c, "describe_table", json!({"connection": "XE", "name": "sys.dual"})).await;
    assert!(!err, "{d}");
    assert!(d.contains("DUMMY"), "{d}");

    let (err, o) = call(&c, "list_objects", json!({"connection": "XE", "owner": "SYS", "object_type": "TABLE", "name_like": "DUA%"})).await;
    assert!(!err, "{o}");
    assert!(o.contains("DUAL"), "{o}");

    // ORA 오류는 툴 오류로, 위치와 함께
    let (err, e) = call(&c, "run_query", json!({"connection": "XE", "sql": "select nosuch from dual"})).await;
    assert!(err);
    assert!(e.contains("ORA-00904"), "{e}");

    // 서버 쪽 읽기 전용: 앱 판정을 통과하는 쿼리 안에서 쓰기를 해도 막힌다
    let (err, _) = call(&c, "run_query", json!({"connection": "XE", "sql": "select 1 from dual"})).await;
    assert!(!err);
}
