//! 결과 편집 (실제 Oracle): ROWID 를 붙여 다시 조회 → 고친 값을 UPDATE/DELETE 로 → 형식이 그대로 돌아가는지.
//!   SQLS_TEST_DSN=system/oracle@host:1521/XE cargo test -p sqls-core --test edit_live -- --ignored

use sqls_core::edit::{changes, editable, CellEdit, RowEdit};
use sqls_core::{ConnectSpec, ExecOptions, ExecOutcome, Session};

fn spec() -> ConnectSpec {
    let dsn = std::env::var("SQLS_TEST_DSN").expect("SQLS_TEST_DSN");
    let (cred, cs) = dsn.split_once('@').unwrap();
    let (u, p) = cred.split_once('/').unwrap();
    ConnectSpec::new(u, p, cs)
}

fn opts(binds: Vec<(String, Option<String>)>) -> ExecOptions {
    ExecOptions { binds, ..Default::default() }
}

#[tokio::test]
#[ignore]
async fn edit_roundtrip() {
    let s = Session::connect(spec()).await.unwrap();
    let _ = s.execute("DROP TABLE sqls_edit_t", opts(vec![])).await;
    s.execute("CREATE TABLE sqls_edit_t (id NUMBER PRIMARY KEY, amt NUMBER(10,2), d DATE, ts TIMESTAMP(6), name VARCHAR2(30))", opts(vec![])).await.unwrap();
    s.execute("INSERT INTO sqls_edit_t VALUES (1, 10.5, DATE '2026-01-02', TIMESTAMP '2026-01-02 03:04:05.123456', 'a')", opts(vec![])).await.unwrap();
    s.execute("INSERT INTO sqls_edit_t VALUES (2, 20, NULL, NULL, 'b')", opts(vec![])).await.unwrap();
    s.commit().await.unwrap();

    let e = editable("select * from sqls_edit_t order by id").unwrap();
    let r = s.execute(&e.sql, opts(vec![])).await.unwrap();
    let ExecOutcome::Rows(page) = r.outcome else { panic!() };
    let names: Vec<&str> = page.columns.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, vec!["SQLS_ROWID", "ID", "AMT", "D", "TS", "NAME"]);
    let row1 = &page.rows[0];
    // 그리드에 나온 글자 형식 그대로 다시 넣을 수 있어야 한다
    assert_eq!(row1[3].as_deref(), Some("2026-01-02 00:00:00"));
    let ty = |i: usize| page.columns[i].type_name.clone();
    let edits = vec![RowEdit {
        rowid: row1[0].clone().unwrap(),
        cells: vec![
            CellEdit { column: "AMT".into(), type_name: ty(2), value: Some("1234.56".into()) },
            CellEdit { column: "D".into(), type_name: ty(3), value: Some("2026-10-07 13:14:15".into()) },
            CellEdit { column: "TS".into(), type_name: ty(4), value: row1[4].clone() },
            CellEdit { column: "NAME".into(), type_name: ty(5), value: Some("O'NEIL 한글".into()) },
        ],
    }];
    let rid2 = page.rows[1][0].clone().unwrap();
    let ch = changes("SQLS_EDIT_T", &edits, &[rid2]).unwrap();
    for c in &ch {
        let r = s.execute(&c.sql, opts(c.binds.clone())).await.unwrap();
        assert!(matches!(r.outcome, ExecOutcome::Affected { rows: 1 }), "{}", c.preview);
    }
    let q = s
        .query(
            "SELECT TO_CHAR(amt), TO_CHAR(d, 'YYYY-MM-DD HH24:MI:SS'), TO_CHAR(ts, 'YYYY-MM-DD HH24:MI:SS.FF6'), name, (SELECT COUNT(*) FROM sqls_edit_t) FROM sqls_edit_t WHERE id = 1",
            vec![],
            1,
        )
        .await
        .unwrap();
    let r = &q.rows[0];
    assert_eq!(r[0].as_deref(), Some("1234.56"));
    assert_eq!(r[1].as_deref(), Some("2026-10-07 13:14:15"));
    assert_eq!(r[2].as_deref(), Some("2026-01-02 03:04:05.123456"), "타임스탬프를 그대로 돌려 넣어도 바뀌지 않는다");
    assert_eq!(r[3].as_deref(), Some("O'NEIL 한글"));
    assert_eq!(r[4].as_deref(), Some("1"), "2번 행은 지웠다");
    s.rollback().await.unwrap();
    s.execute("DROP TABLE sqls_edit_t", opts(vec![])).await.unwrap();
    s.close();
}
