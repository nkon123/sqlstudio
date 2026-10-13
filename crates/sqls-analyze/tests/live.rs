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
    "order_log(id number, amt number)",
    "order_tmp(ts date)",
    "stock(id number, qty number)",
    "customers(id number, name varchar2(30), vip char(1))",
    "vip_sum(id number, cnt number)",
    "daily(dt date, amt number)",
    "accounts(id number, checked char(1))",
    "audit_x(a number)",
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
    s.execute("CREATE OR REPLACE PACKAGE flow_pkg IS PROCEDURE run; END;", ExecOptions::default()).await.unwrap();
    s.execute(&format!("CREATE OR REPLACE {}", include_str!("data/flow_pkg.pkb")), ExecOptions::default()).await.unwrap();
    s.execute("CREATE OR REPLACE PROCEDURE run_nightly AS BEGIN order_pkg.nightly; END;", ExecOptions::default()).await.unwrap();

    let owner = s.info().user.clone();
    let mut refs = source::list_units(&s, &owner, &[], Some("%ORDER_PKG%")).await.unwrap();
    refs.extend(source::list_units(&s, &owner, &[], Some("AUDIT_PKG")).await.unwrap());
    refs.extend(source::list_units(&s, &owner, &["PROCEDURE".into()], Some("RUN_NIGHTLY")).await.unwrap());
    refs.extend(source::list_units(&s, &owner, &[], Some("FLOW_PKG")).await.unwrap());
    let types: Vec<&str> = refs.iter().map(|r| r.unit_type.as_str()).collect();
    assert_eq!(types, vec!["PACKAGE BODY", "PACKAGE BODY", "PROCEDURE", "PACKAGE BODY"], "{refs:?}");
    // 컴파일 오류 없이 들어갔는지 (테스트 소스가 진짜 PL/SQL 인지)
    let errs = s.query("SELECT COUNT(*) FROM user_errors WHERE name IN ('FLOW_PKG', 'ORDER_PKG')", vec![], 1).await.unwrap();
    assert_eq!(errs.rows[0][0].as_deref(), Some("0"));

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
    // 커서 흐름: ORDERS → ORDER_LOG (전역 커서 C_OPEN), CUSTOMERS → VIP_SUM, ORDER_LINES → STOCK
    let has = |cur: &str, from: &str, to: &str, via: &str| g.flows.iter().any(|f| f.cursor == cur && f.from.contains(&id(from)) && f.to == id(to) && f.via == via);
    assert!(has("C_OPEN", "ORDERS", "ORDER_LOG", "record R"), "{:#?}", g.flows);
    assert!(has("C_LINES", "ORDER_LINES", "STOCK", "variable V_IDS"));
    assert!(has("X@20", "CUSTOMERS", "VIP_SUM", "record X"));
    assert!(has("C_LOCK", "ACCOUNTS", "ACCOUNTS", "CURRENT OF"));
    let order_log = g.tables.iter().find(|t| t.table == id("ORDER_LOG")).unwrap();
    assert_eq!(order_log.fed_from, vec![id("ORDERS")]);
    s.close();
    std::fs::remove_dir_all(&dir).unwrap();
}

/// 사전 의존성: 동의어를 실제 테이블로 풀고, 자기 명세·STANDARD 는 빼고, 파서와 맞대어 근거를 붙인다
#[tokio::test]
#[ignore]
async fn dictionary_dependencies() {
    let s = Session::connect(spec()).await.unwrap();
    for t in ["dep_base(id number)", "dep_other(id number)"] {
        let _ = s.execute(&format!("CREATE TABLE {t}"), ExecOptions::default()).await;
    }
    s.execute("CREATE OR REPLACE SYNONYM dep_syn FOR dep_base", ExecOptions::default()).await.unwrap();
    // 동의어의 동의어
    s.execute("CREATE OR REPLACE SYNONYM dep_syn2 FOR dep_syn", ExecOptions::default()).await.unwrap();
    let _ = s.execute("CREATE SEQUENCE dep_seq", ExecOptions::default()).await;
    s.execute("CREATE OR REPLACE PACKAGE dep_pkg IS PROCEDURE run; END;", ExecOptions::default()).await.unwrap();
    s.execute(
        "CREATE OR REPLACE PACKAGE BODY dep_pkg IS\n  PROCEDURE run IS\n    v dep_other%ROWTYPE;\n    n NUMBER;\n  BEGIN\n    INSERT INTO dep_syn (id) SELECT dep_seq.NEXTVAL FROM dual;\n    SELECT COUNT(*) INTO n FROM dep_base;\n    DELETE FROM dep_syn2;\n  END run;\nEND dep_pkg;",
        ExecOptions::default(),
    )
    .await
    .unwrap();
    let owner = s.info().user.clone();
    let deps = sqls_analyze::deps::fetch(&s, &owner, "DEP_PKG", "PACKAGE BODY").await.unwrap();
    let has = |n: &str, k: &str, via: Option<String>| deps.iter().any(|d| d.owner == owner && d.name == n && d.kind == k && d.via == via);
    assert!(has("DEP_BASE", "TABLE", Some(format!("{owner}.DEP_SYN"))), "동의어를 풀어야 한다: {deps:?}");
    assert!(has("DEP_BASE", "TABLE", Some(format!("{owner}.DEP_SYN2"))), "동의어의 동의어도 끝까지: {deps:?}");
    assert!(has("DEP_BASE", "TABLE", None) && has("DEP_OTHER", "TABLE", None) && has("DEP_SEQ", "SEQUENCE", None), "{deps:?}");
    assert!(!deps.iter().any(|d| d.name == "DEP_PKG" || d.name == "STANDARD"), "자기 명세·STANDARD 는 뺀다: {deps:?}");

    let dir = std::env::temp_dir().join(format!("sqls-analyze-live-dict-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let store = Store::open(&dir).unwrap();
    let r = source::list_units(&s, &owner, &[], Some("DEP_PKG")).await.unwrap().remove(0);
    let (src, spec) = source::fetch(&s, &r).await.unwrap();
    assert!(sqls_analyze::deps::Fetcher::default().fetch_into(&s, &store, &r.owner, &r.name, &r.unit_type).await.is_none());
    analyze_unit(&store, &src, spec.as_deref(), None, &Options::default(), Arc::new(|_| {}), Arc::new(AtomicBool::new(false))).await.unwrap();
    let (g, _) = sqls_analyze::integrate::write_all(&store, None).unwrap();
    let base = g.tables.iter().find(|t| t.table == format!("{owner}.DEP_BASE")).expect("동의어로 쓴 것도 실제 테이블로 모인다");
    let run = format!("{owner}.DEP_PKG.RUN");
    assert_eq!((base.by[&run].as_str(), base.conf[&run].as_str()), ("CRD", "ok"));
    assert!(!g.tables.iter().any(|t| t.table.ends_with(".DEP_SYN")));
    let other = g.tables.iter().find(|t| t.table == format!("{owner}.DEP_OTHER")).expect("%ROWTYPE 만 쓴 테이블은 사전에서");
    assert_eq!(other.conf.values().next().map(String::as_str), Some("dict"));
    assert_eq!(g.dict.as_ref().map(|d| (d.checked, d.dict_only)), Some((1, 1)), "{:?}", g.gaps);
    s.close();
    std::fs::remove_dir_all(&dir).unwrap();
}
