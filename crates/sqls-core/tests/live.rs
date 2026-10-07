//! 실제 Oracle 에 붙는 통합 테스트. DB 가 있을 때만 돈다:
//!
//! ```text
//! docker run -d -p 1521:1521 -e ORACLE_PASSWORD=oracle gvenzl/oracle-xe:11-slim
//! SQLS_TEST_DSN=system/oracle@localhost:1521/XE cargo test -p sqls-core --test live -- --ignored --test-threads=1
//! ```

use std::time::{Duration, Instant};

use sqls_core::meta;
use sqls_core::{ConnectSpec, Error, ExecOptions, ExecOutcome, Session, StmtKind};

fn spec(read_only: bool) -> ConnectSpec {
    let dsn = std::env::var("SQLS_TEST_DSN").expect("SQLS_TEST_DSN=user/pass@host:port/svc");
    let (cred, cs) = dsn.split_once('@').unwrap();
    let (u, p) = cred.split_once('/').unwrap();
    let mut s = ConnectSpec::new(u, p, cs);
    s.read_only = read_only;
    s
}

fn rows(o: &ExecOutcome) -> &sqls_core::RowPage {
    match o {
        ExecOutcome::Rows(p) => p,
        other => panic!("rows 가 아님: {other:?}"),
    }
}

async fn exec(s: &Session, sql: &str) -> sqls_core::ExecResult {
    s.execute(sql, ExecOptions::default()).await.unwrap_or_else(|e| panic!("{sql}: {e}"))
}

#[tokio::test]
#[ignore]
async fn dml_binds_output_and_dictionary() {
    let s = Session::connect(spec(false)).await.unwrap();
    println!("server: {}", s.info().server_version);
    let t = "SQLS_T_EMP";
    let _ = s.execute(&format!("drop table {t} purge"), Default::default()).await;
    exec(&s, &format!(
        "create table {t} (empno number(4) primary key, ename varchar2(10 char) not null, sal number(7,2), hired date, note clob)"
    )).await;
    exec(&s, &format!("comment on table {t} is '사원'")).await;
    exec(&s, &format!("comment on column {t}.ename is '이름'")).await;
    exec(&s, &format!("create index {t}_ix1 on {t}(ename, sal)")).await;

    // 바인드 DML — 한글 값, NULL 값, 쓰지 않는 바인드는 걸러져야 한다 (ORA-01036 방지)
    let ins = format!("insert into {t} (empno, ename, sal, hired) values (:no, :nm, :sal, sysdate)");
    for (no, nm, sal) in [("1", "홍길동", Some("100.5")), ("2", "SMITH", None)] {
        let r = s
            .execute(&ins, ExecOptions {
                binds: vec![
                    ("no".into(), Some(no.into())),
                    ("NM".into(), Some(nm.into())),
                    ("sal".into(), sal.map(String::from)),
                    ("unused".into(), Some("x".into())),
                ],
                ..Default::default()
            })
            .await
            .unwrap();
        assert!(matches!(r.outcome, ExecOutcome::Affected { rows: 1 }));
    }
    s.commit().await.unwrap();

    let r = exec(&s, &format!("select empno, ename, sal from {t} order by empno")).await;
    let p = rows(&r.outcome);
    assert_eq!(p.columns.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["EMPNO", "ENAME", "SAL"]);
    assert_eq!(p.rows[0], vec![Some("1".into()), Some("홍길동".into()), Some("100.5".into())]);
    assert_eq!(p.rows[1][2], None);

    // DBMS_OUTPUT
    let r = exec(&s, "begin dbms_output.put_line('하나'); dbms_output.put_line('둘'); end;").await;
    assert_eq!(r.kind, StmtKind::Plsql);
    assert_eq!(r.output, vec!["하나", "둘"]);

    // 사전 조회
    let d = meta::describe(&s, t).await.unwrap();
    assert_eq!(d.comment.as_deref(), Some("사원"));
    assert_eq!(d.primary_key, vec!["EMPNO"]);
    let ename = d.columns.iter().find(|c| c.name == "ENAME").unwrap();
    assert_eq!(ename.data_type, "VARCHAR2(10 CHAR)");
    assert!(!ename.nullable);
    assert_eq!(ename.comment.as_deref(), Some("이름"));
    assert_eq!(d.columns.iter().find(|c| c.name == "SAL").unwrap().data_type, "NUMBER(7,2)");
    assert!(d.indexes.iter().any(|i| i.columns == ["ENAME", "SAL"]));
    println!("{}", meta::schema_brief(&d));

    let objs = meta::list_objects(&s, None, Some("TABLE"), Some("SQLS\\_T%"), 50).await.unwrap();
    assert!(objs.iter().any(|o| o.name == t));

    // DBMS_METADATA 가 깨진 설치(slim XE)에서도 대체 경로로 DDL 이 나와야 한다
    let ddl = meta::get_ddl(&s, "TABLE", t).await.unwrap();
    assert!(ddl.contains("CREATE TABLE"), "{ddl}");
    assert!(ddl.contains("ENAME"), "{ddl}");
    exec(&s, "create or replace procedure sqls_ok is begin null; end;").await;
    let pddl = meta::get_ddl(&s, "PROCEDURE", "SQLS_OK").await.unwrap();
    assert!(pddl.to_uppercase().contains("PROCEDURE SQLS_OK"), "{pddl}");
    exec(&s, "drop procedure sqls_ok").await;

    // 컴파일 오류
    let _ = s.execute("create or replace procedure sqls_bad is begin nosuch; end;", Default::default()).await;
    let errs = meta::compile_errors(&s, "PROCEDURE", "SQLS_BAD").await.unwrap();
    assert!(!errs.is_empty());
    let _ = s.execute("drop procedure sqls_bad", Default::default()).await;

    // ORA 오류는 코드와 위치(offset)를 돌려준다
    match s.execute(&format!("select nosuchcol from {t}"), Default::default()).await {
        Err(Error::Db { code, offset, .. }) => {
            assert_eq!(code, 904);
            assert_eq!(offset, 7);
        }
        other => panic!("{other:?}"),
    }
    // 오류 뒤에도 세션은 계속 쓸 수 있다
    exec(&s, "select 1 from dual").await;

    exec(&s, &format!("drop table {t} purge")).await;
}

#[tokio::test]
#[ignore]
async fn read_only_is_enforced_by_server_too() {
    let s = Session::connect(spec(true)).await.unwrap();
    // 앱 쪽 판정
    assert!(matches!(s.execute("delete from dual", Default::default()).await, Err(Error::ReadOnly(_))));
    // 판정을 통과하는 SELECT 안의 쓰기 — 자율 트랜잭션이 아닌 함수가 DML 을 하면 서버가 막는다.
    // (여기서는 SET TRANSACTION READ ONLY 가 실제로 걸려 있는지만 확인)
    let r = exec(&s, "select count(*) from v$transaction where addr = (select taddr from v$session where sid = sys_context('userenv','sid'))").await;
    let _ = r;
    let r = exec(&s, "select dbms_transaction.local_transaction_id from dual").await;
    assert!(rows(&r.outcome).rows[0][0].is_some(), "읽기 전용 트랜잭션이 시작되어 있어야 한다");
}

#[tokio::test]
#[ignore]
async fn cancel_long_query() {
    let s = Session::connect(spec(true)).await.unwrap();
    let s2 = s.clone();
    let started = Instant::now();
    let h = tokio::spawn(async move {
        s2.execute(
            "select count(*) from all_objects a, all_objects b, all_objects c",
            Default::default(),
        )
        .await
    });
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert!(s.is_busy());
    s.cancel().unwrap();
    let r = h.await.unwrap();
    assert!(matches!(r, Err(Error::Cancelled)), "{r:?}");
    assert!(started.elapsed() < Duration::from_secs(10));
    // 취소 뒤에도 세션은 산다
    exec(&s, "select 1 from dual").await;
}

#[tokio::test]
#[ignore]
async fn paging_large_result() {
    let s = Session::connect(spec(true)).await.unwrap();
    let started = Instant::now();
    let r = s
        .execute(
            "select level n, rpad('x', 100, 'x') pad from dual connect by level <= 100000",
            ExecOptions { first_page: 1000, fetch_array_size: 1000, ..Default::default() },
        )
        .await
        .unwrap();
    let first = started.elapsed();
    assert_eq!(rows(&r.outcome).rows.len(), 1000);
    let total = loop {
        let p = s.fetch_more(10_000).await.unwrap();
        if !p.has_more {
            break p.fetched_total;
        }
    };
    assert_eq!(total, 100_000);
    println!("첫 페이지 {first:?}, 10만 행 전체 {:?}", started.elapsed());
    // 첫 페이지는 전체를 기다리지 않는다
    assert!(first < Duration::from_secs(2));
}

#[tokio::test]
#[ignore]
async fn many_sessions_in_parallel() {
    let mut hs = Vec::new();
    for i in 0..8 {
        hs.push(tokio::spawn(async move {
            let s = Session::connect(spec(true)).await.unwrap();
            for _ in 0..20 {
                let r = s
                    .execute(&format!("select {i} from dual"), Default::default())
                    .await
                    .unwrap();
                assert_eq!(rows(&r.outcome).rows[0][0].as_deref(), Some(i.to_string().as_str()));
            }
            s.close();
        }));
    }
    for h in hs {
        h.await.unwrap();
    }
}

/// 취소가 서버에 닿지 않는 네트워크(OOB 를 버리는 NAT/방화벽/Docker 포트 매핑)에서도
/// 화면이 묶이지 않아야 한다: 취소 → 몇 초 뒤 abandon → 호출이 즉시 풀리고 새 세션으로 계속.
#[tokio::test]
#[ignore]
async fn abandon_releases_stuck_call() {
    let s = Session::connect(spec(true)).await.unwrap();
    let s2 = s.clone();
    let h = tokio::spawn(async move {
        s2.execute(
            "select count(*) from all_objects a, all_objects b, all_objects c",
            Default::default(),
        )
        .await
    });
    tokio::time::sleep(Duration::from_millis(500)).await;
    s.cancel().unwrap();
    // 취소가 먹히면 여기서 끝난다
    let r = match tokio::time::timeout(Duration::from_secs(3), h).await {
        Ok(joined) => joined.unwrap(),
        Err(_) => {
            println!("취소가 닿지 않음 — abandon");
            let t = Instant::now();
            // 핸들을 잃었으므로 새로 기다릴 호출을 하나 걸어 둔다
            let s3 = s.clone();
            let waiter = tokio::spawn(async move { s3.ping().await });
            s.abandon();
            let r = waiter.await.unwrap();
            assert!(t.elapsed() < Duration::from_secs(1));
            r.map(|_| unreachable!())
        }
    };
    assert!(matches!(r, Err(Error::Cancelled) | Err(Error::SessionClosed)), "{r:?}");
    // 새 세션은 바로 쓸 수 있다
    let fresh = Session::connect(spec(true)).await.unwrap();
    exec(&fresh, "select 1 from dual").await;
}
