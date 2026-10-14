//! DB 프로시저 → 테이블 → XML 쿼리·Pro*C 가 한 흐름도에 이어지는지.

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use sqls_analyze::chunk::UnitSource;
use sqls_analyze::run::{analyze_unit, Event, Options};
use sqls_analyze::store::Store;

const DB_PKG: &str = "PACKAGE BODY load_pkg IS
  PROCEDURE build_hist IS
  BEGIN
    INSERT INTO order_hist (id) SELECT id FROM orders;
  END build_hist;
END load_pkg;";

const XML: &str = r#"<queryservice><queries>
<query id="findHist"><statement><![CDATA[ SELECT id FROM order_hist WHERE id = :id ]]></statement></query>
</queries></queryservice>"#;

const PC: &str = r#"
int main(void)
{
    EXEC SQL INSERT INTO daily_sum (id) SELECT id FROM order_hist;
    EXEC SQL COMMIT WORK;
    return 0;
}
"#;

#[tokio::test]
async fn db_proc_to_screen_query_flow() {
    let dir = std::env::temp_dir().join(format!("sqls-codeflow-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let store = Store::open(&dir).unwrap();
    let mut units = vec![UnitSource { owner: "APP".into(), name: "LOAD_PKG".into(), unit_type: "PACKAGE BODY".into(), text: DB_PKG.into() }];
    units.push(sqls_analyze::xmlquery::translate(XML, "APP", "hist").unwrap().unit);
    units.push(sqls_analyze::proc::translate(PC, "APP", "batch").unwrap().unit);
    for u in &units {
        analyze_unit(&store, u, None, None, &Options::default(), Arc::new(|_: Event| {}), Arc::new(AtomicBool::new(false))).await.unwrap();
    }
    let (g, _) = sqls_analyze::integrate::write_all(&store, None).unwrap();
    let ids: Vec<&str> = g.nodes.iter().map(|n| n.id.as_str()).collect();
    assert!(ids.contains(&"APP.LOAD_PKG.BUILD_HIST") && ids.contains(&"APP.hist.FINDHIST") && ids.contains(&"APP.batch.MAIN"), "{ids:?}");
    let hist = g.tables.iter().find(|t| t.table == "APP.ORDER_HIST").expect("ORDER_HIST");
    // DB 프로시저가 쓰고(C), XML 쿼리와 Pro*C 가 읽는다(R) — 같은 테이블 노드로 이어진다
    assert_eq!(hist.by.get("APP.LOAD_PKG.BUILD_HIST").map(String::as_str), Some("C"), "{:?}", hist.by);
    assert_eq!(hist.by.get("APP.hist.FINDHIST").map(String::as_str), Some("R"), "{:?}", hist.by);
    assert_eq!(hist.by.get("APP.batch.MAIN").map(String::as_str), Some("R"), "{:?}", hist.by);
    let daily = g.tables.iter().find(|t| t.table == "APP.DAILY_SUM").expect("DAILY_SUM");
    assert_eq!(daily.by.get("APP.batch.MAIN").map(String::as_str), Some("C"));
    let _ = std::fs::remove_dir_all(&dir);
}
