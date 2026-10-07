//! 실제 Oracle 에서: 사전으로 단위 찾기 → 소스 읽기 → 정적 분석 → 통합.
//!   SQLS_TEST_DSN=system/oracle@host:1521/XE cargo test -p sqls-analyze --test live -- --ignored --test-threads=1

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use sqls_analyze::run::{analyze_unit, Options};
use sqls_analyze::source;
use sqls_analyze::store::Store;
use sqls_core::{ConnectSpec, ExecOptions, Session};

fn spec() -> ConnectSpec {
    let dsn = std::env::var("SQLS_TEST_DSN").expect("SQLS_TEST_DSN");
    let (cred, cs) = dsn.split_once('@').unwrap();
    let (u, p) = cred.split_once('/').unwrap();
    ConnectSpec::new(u, p, cs)
}

const TABLES: &[&str] = &[
    "orders(id number, status varchar2(10), amount number)",
    "order_lines(order_id number, product_id number, qty number, price number)",
    "products(id number)",
    "order_hist(id number, amount number, ts date)",
    "daily_sum(dt date, amt number)",
    "audit_log(kind varchar2(20), ref_id number, ts date)",
    "stats_0(k number, runs number)",
    "stats_1(k number, runs number)",
    "stats_2(k number, runs number)",
];

#[tokio::test]
#[ignore]
async fn analyze_from_dictionary() {
    let s = Session::connect(spec()).await.unwrap();
    for t in TABLES {
        let _ = s.execute(&format!("CREATE TABLE {t}"), ExecOptions::default()).await;
    }
    s.execute("CREATE OR REPLACE PACKAGE audit_pkg IS PROCEDURE log(p_kind VARCHAR2, p_id NUMBER); END;", ExecOptions::default()).await.unwrap();
    s.execute(
        "CREATE OR REPLACE PACKAGE BODY audit_pkg IS\n  PROCEDURE log(p_kind VARCHAR2, p_id NUMBER) IS\n    PRAGMA AUTONOMOUS_TRANSACTION;\n  BEGIN\n    INSERT INTO audit_log (kind, ref_id, ts) VALUES (p_kind, p_id, SYSDATE);\n    COMMIT;\n  END log;\nEND audit_pkg;",
        ExecOptions::default(),
    )
    .await
    .unwrap();
    s.execute(&format!("CREATE OR REPLACE {}", include_str!("data/order_pkg.pks")), ExecOptions::default()).await.unwrap();
    s.execute(&format!("CREATE OR REPLACE {}", include_str!("data/order_pkg.pkb")), ExecOptions::default()).await.unwrap();
    s.execute("CREATE OR REPLACE PROCEDURE run_nightly AS BEGIN order_pkg.nightly; END;", ExecOptions::default()).await.unwrap();

    let owner = s.info().user.clone();
    let mut refs = source::list_units(&s, &owner, &[], Some("%ORDER_PKG%")).await.unwrap();
    refs.extend(source::list_units(&s, &owner, &[], Some("AUDIT_PKG")).await.unwrap());
    refs.extend(source::list_units(&s, &owner, &["PROCEDURE".into()], Some("RUN_NIGHTLY")).await.unwrap());
    let types: Vec<&str> = refs.iter().map(|r| r.unit_type.as_str()).collect();
    assert_eq!(types, vec!["PACKAGE BODY", "PACKAGE BODY", "PROCEDURE"], "{refs:?}");

    let dir = std::env::temp_dir().join(format!("sqls-analyze-live-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let store = Store::open(&dir).unwrap();
    for r in &refs {
        let (src, spec) = source::fetch(&s, r).await.unwrap();
        if r.name == "ORDER_PKG" {
            assert!(spec.as_deref().unwrap().contains("밤 배치"), "명세도 같이 읽는다");
            // ALL_SOURCE 는 "PACKAGE BODY" 로 시작하고 줄 번호가 파일과 같다
            assert!(src.text.starts_with("PACKAGE BODY"));
        }
        let u = analyze_unit(&store, &src, spec.as_deref(), None, &Options::default(), Arc::new(|_| {}), Arc::new(AtomicBool::new(false))).await.unwrap();
        assert!(u.warning.is_none(), "{:?}", u.warning);
    }
    let (g, _) = sqls_analyze::integrate::write_all(&store, None).unwrap();
    let id = |p: &str| format!("{owner}.{p}");
    assert!(g.edges.iter().any(|e| e.from == id("RUN_NIGHTLY") && e.to == id("ORDER_PKG.NIGHTLY") && e.resolved));
    assert!(g.edges.iter().any(|e| e.from == id("ORDER_PKG.CLOSE_ORDER") && e.to == id("AUDIT_PKG.LOG") && e.resolved));
    assert!(g.entries.contains(&id("RUN_NIGHTLY")));
    assert!(!g.entries.contains(&id("ORDER_PKG.NIGHTLY")), "RUN_NIGHTLY 가 부르므로 시작점이 아니다");
    let orders = g.tables.iter().find(|t| t.table == id("ORDERS")).unwrap();
    assert_eq!(orders.impacted_entries, vec![id("RUN_NIGHTLY")]);
    s.close();
    std::fs::remove_dir_all(&dir).unwrap();
}
