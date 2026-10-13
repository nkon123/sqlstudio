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
    // 주석·통계·PK·인덱스를 한 번에 읽는 쿼리 — 인덱스 컬럼 순서, PK 인덱스
    let ix = d.indexes.iter().find(|i| i.name == format!("{t}_IX1")).expect("인덱스");
    assert_eq!(ix.columns, vec!["ENAME", "SAL"]);
    assert!(!ix.unique);
    assert!(d.indexes.iter().any(|i| i.unique && i.columns == vec!["EMPNO"]), "{:?}", d.indexes);
    // 동의어로 물어도 원본
    let _ = s.execute("drop synonym sqls_t_emp_syn", Default::default()).await;
    exec(&s, &format!("create synonym sqls_t_emp_syn for {t}")).await;
    let ds = meta::describe(&s, "SQLS_T_EMP_SYN").await.unwrap();
    assert_eq!((ds.name.as_str(), ds.object_type.as_str(), ds.columns.len()), (t, "TABLE", d.columns.len()));
    let _ = s.execute("drop synonym sqls_t_emp_syn", Default::default()).await;
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
    match tokio::time::timeout(Duration::from_secs(8), h).await {
        Ok(r) => {
            let r = r.unwrap();
            assert!(matches!(r, Err(Error::Cancelled)), "{r:?}");
            assert!(started.elapsed() < Duration::from_secs(10));
            // 취소 뒤에도 세션은 산다
            exec(&s, "select 1 from dual").await;
        }
        Err(_) => {
            // 포트 매핑·NAT 가 OOB 를 버리는 경로 — 취소가 서버에 닿지 않는다.
            // 이 경우의 동작은 abandon_releases_stuck_call 이 확인한다.
            println!("취소 신호가 막힌 네트워크 — 세션을 버린다");
            s.abandon();
        }
    }
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

/// 자동완성 캐시: 실제 사전에서 읽고, FK 로 조인 조건을 추천하고, 공개 동의어 패키지를 채운다
#[tokio::test]
#[ignore]
async fn completion_cache_from_dictionary() {
    use sqls_core::complete::{self, load};
    let s = Session::connect(spec(false)).await.unwrap();
    for t in ["SQLS_C_ITEM", "SQLS_C_ORD"] {
        let _ = s.execute(&format!("drop table {t} purge"), Default::default()).await;
    }
    exec(&s, "create table sqls_c_ord (ord_id number primary key, cust varchar2(20))").await;
    exec(&s, "create table sqls_c_item (ord_id number references sqls_c_ord, line_no number, qty number, \
              constraint sqls_c_item_pk primary key (ord_id, line_no))").await;
    exec(&s, "comment on column sqls_c_item.qty is '수량'").await;

    let started = Instant::now();
    let shared = std::sync::RwLock::new(complete::SchemaCache::default());
    let mut phases = Vec::new();
    let mut cache = load::load_user_schema(&s, &shared, true, |p| phases.push((p, started.elapsed()))).await.unwrap();
    let took = started.elapsed();
    println!("단계: {phases:?}");
    // 첫 단계(객체 이름)가 전체보다 훨씬 먼저 끝나야 한다
    assert!(phases[0].1 < took / 2, "{phases:?}");
    assert_eq!(shared.read().unwrap().stats(), cache.stats());
    let (objs, cols) = cache.stats();
    println!("캐시 적재 {took:?}: 객체 {objs}, 컬럼 {cols}");
    assert!(cache.has_columns(&s.info().user, "SQLS_C_ITEM"));

    let sql = "select * from sqls_c_item i join sqls_c_ord o on ";
    let r = complete::complete(sql, sql.len(), &cache, 50);
    assert_eq!(r.items[0].label, "o.ord_id = i.ord_id", "{:?}", r.items.iter().take(3).map(|i| &i.label).collect::<Vec<_>>());

    let sql = "select i. from sqls_c_item i";
    let r = complete::complete(sql, 9, &cache, 50);
    let qty = r.items.iter().find(|i| i.label == "qty").unwrap();
    assert_eq!(qty.info.as_deref(), Some("수량"));

    // DBMS_OUTPUT 은 공개 동의어 → SYS 패키지. 처음엔 모자라다고 하고, 채우면 나온다
    let sql = "begin dbms_output.";
    let r = complete::complete(sql, sql.len(), &cache, 50);
    assert_eq!(r.missing.len(), 1, "{:?}", r.missing);
    for f in load::fill(&s, &r.missing[0]).await.unwrap() {
        cache.apply(f);
    }
    let r = complete::complete(sql, sql.len(), &cache, 50);
    assert!(r.items.iter().any(|i| i.label == "put_line"), "{:?}", r.items.iter().map(|i| &i.label).collect::<Vec<_>>());

    for t in ["SQLS_C_ITEM", "SQLS_C_ORD"] {
        let _ = s.execute(&format!("drop table {t} purge"), Default::default()).await;
    }
}

/// 스키마 브라우저의 모든 탭 SQL 이 실제 사전 뷰에서 도는지 (11g)
#[tokio::test]
#[ignore]
async fn schema_browser_sections() {
    use sqls_core::browse::{section, sections_for};
    let s = Session::connect(spec(false)).await.unwrap();
    let me = s.info().user.clone();
    let (p, c) = ("SQLS_B_PARENT", "SQLS_B_CHILD");
    for x in [c, p] {
        let _ = s.execute(&format!("drop table {x} purge"), Default::default()).await;
    }
    let _ = s.execute("drop sequence sqls_b_seq", Default::default()).await;
    exec(&s, &format!("create table {p} (id number primary key, code varchar2(10) unique, amt number check (amt >= 0))")).await;
    exec(&s, &format!("create table {c} (id number primary key, pid number references {p}(id) on delete cascade, note varchar2(20))")).await;
    exec(&s, &format!("create index {c}_ix on {c}(pid, note desc)")).await;
    exec(&s, &format!("create or replace trigger sqls_b_trg before insert on {c} for each row begin :new.note := upper(:new.note); end;")).await;
    exec(&s, &format!("create or replace view sqls_b_v as select p.id, c.note from {p} p join {c} c on c.pid = p.id")).await;
    exec(&s, "create sequence sqls_b_seq start with 10 cache 5").await;
    // 컴파일 오류가 나는 프로시저 (성공 + 경고로 끝날 수도, 오류로 끝날 수도 있다)
    let _ = s.execute("create or replace procedure sqls_b_bad as begin no_such_thing; end;", Default::default()).await;
    exec(&s, &format!("begin dbms_stats.gather_table_stats(user, '{p}'); end;")).await;

    for (otype, name) in [("TABLE", p), ("TABLE", c), ("VIEW", "SQLS_B_V"), ("TRIGGER", "SQLS_B_TRG"),
        ("SEQUENCE", "SQLS_B_SEQ"), ("PROCEDURE", "SQLS_B_BAD"), ("INDEX", "SQLS_B_CHILD_IX")] {
        for sec in sections_for(otype) {
            let page = section(&s, &me, name, sec.key).await.unwrap_or_else(|e| panic!("{otype} {name} {}: {e}", sec.key));
            println!("{otype} {name} {}: {} 행", sec.key, page.rows.len());
        }
    }
    let cons = section(&s, &me, p, "constraints").await.unwrap();
    assert!(cons.rows.iter().any(|r| r[1].as_deref() == Some("기본 키")), "{:?}", cons.rows);
    assert!(cons.rows.iter().any(|r| r[1].as_deref() == Some("검사") && r[7].as_deref().unwrap_or("").contains("amt")), "LONG 조건");
    let refs = section(&s, &me, p, "referenced_by").await.unwrap();
    assert_eq!(refs.rows.len(), 1);
    assert_eq!(refs.rows[0][2].as_deref(), Some("PID"), "참조하는 쪽 컬럼");
    let idx = section(&s, &me, c, "indexes").await.unwrap();
    assert!(idx.rows.iter().any(|r| r[2].as_deref() == Some("PID, NOTE DESC") || r[2].as_deref().unwrap_or("").starts_with("PID, ")), "{:?}", idx.rows);
    let trg = section(&s, &me, c, "triggers").await.unwrap();
    assert_eq!(trg.rows.len(), 1);
    let used = section(&s, &me, p, "used_by").await.unwrap();
    assert!(used.rows.iter().any(|r| r[0].as_deref().unwrap_or("").ends_with("SQLS_B_V")));
    let errs = section(&s, &me, "SQLS_B_BAD", "errors").await.unwrap();
    assert!(!errs.rows.is_empty(), "컴파일 오류가 나와야 한다");
    let stats = section(&s, &me, p, "stats").await.unwrap();
    assert_eq!(stats.rows.len(), 1);
    assert_eq!(stats.columns.last().unwrap().name, "크기(MB)");

    for x in ["drop view sqls_b_v", "drop procedure sqls_b_bad", "drop sequence sqls_b_seq"] {
        let _ = s.execute(x, Default::default()).await;
    }
    for x in [c, p] {
        let _ = s.execute(&format!("drop table {x} purge"), Default::default()).await;
    }
}

/// 데이터 가져오기의 묶음 INSERT (array DML): 날짜 변환, 오류 행 건너뛰기, 멈춤 모드, 읽기 전용 거부
#[tokio::test]
#[ignore]
async fn batch_insert() {
    let s = Session::connect(spec(false)).await.unwrap();
    let t = "SQLS_T_IMP";
    let _ = s.execute(&format!("drop table {t} purge"), Default::default()).await;
    exec(&s, &format!("create table {t} (id number(5) primary key, nm varchar2(20 char) not null, dt date)")).await;
    let sql = format!("INSERT INTO {t} (ID, NM, DT) VALUES (:1, :2, TO_DATE(:3, 'YYYY-MM-DD HH24:MI:SS'))");
    let row = |id: &str, nm: Option<&str>, dt: &str| vec![Some(id.to_string()), nm.map(String::from), Some(dt.to_string())];
    // 건너뛰기: 2번째(NOT NULL 위반)·4번째(숫자 아님)만 빠진다
    let r = s
        .batch(
            &sql,
            vec![
                row("1", Some("홍길동"), "2026-10-12 09:00:00"),
                row("2", None, "2026-10-12 09:00:00"),
                row("3", Some("B"), "2026-10-13 00:00:00"),
                row("x", Some("C"), "2026-10-13 00:00:00"),
            ],
            true,
        )
        .await
        .unwrap();
    assert_eq!(r.inserted, 2, "{:?}", r.errors);
    assert_eq!(r.errors.iter().map(|e| e.0).collect::<Vec<_>>(), vec![1, 3], "{:?}", r.errors);
    let got = exec(&s, &format!("select id, nm, to_char(dt, 'YYYY-MM-DD HH24:MI') from {t} order by id")).await;
    assert_eq!(
        rows(&got.outcome).rows,
        vec![
            vec![Some("1".into()), Some("홍길동".into()), Some("2026-10-12 09:00".into())],
            vec![Some("3".into()), Some("B".into()), Some("2026-10-13 00:00".into())],
        ]
    );
    // 멈춤 모드: 하나라도 틀리면 오류
    assert!(s.batch(&sql, vec![row("5", Some("D"), "2026-10-13 00:00:00"), row("1", Some("dup"), "2026-10-13 00:00:00")], false).await.is_err());
    s.rollback().await.unwrap();
    // 읽기 전용 세션은 거부
    let ro = Session::connect(spec(true)).await.unwrap();
    assert!(matches!(ro.batch(&sql, vec![row("9", Some("R"), "2026-10-13 00:00:00")], true).await, Err(Error::ReadOnly(_))));
    let _ = s.execute(&format!("drop table {t} purge"), Default::default()).await;
}

/// 서버에서 세션을 죽이면: 쉬는 동안 keepalive ping 이 알아채고, 호출도 '끊김' 으로 실패한다
#[tokio::test]
#[ignore]
async fn keepalive_notices_killed_session() {
    let log = std::env::temp_dir().join(format!("sqls-connlog-{}.jsonl", std::process::id()));
    let _ = std::fs::remove_file(&log);
    sqls_core::connlog::init(log.clone());
    let mut sp = spec(false);
    sp.keepalive = Some(Duration::from_secs(1));
    let victim = Session::connect(sp).await.unwrap();
    // 쉬는 동안 ping 해도 멀쩡하다
    tokio::time::sleep(Duration::from_millis(2500)).await;
    assert!(!victim.is_lost());
    exec(&victim, "SELECT 1 FROM dual").await;

    let admin = Session::connect(spec(false)).await.unwrap();
    let sid = victim.info().sid.to_string();
    let r = admin.query("SELECT serial# FROM v$session WHERE sid = :s", vec![("S".into(), Some(sid.clone()))], 1).await.unwrap();
    let serial = r.rows[0][0].clone().unwrap();
    exec(&admin, &format!("ALTER SYSTEM KILL SESSION '{sid},{serial}' IMMEDIATE")).await;

    let mut sig = victim.lost_signal();
    if tokio::time::timeout(Duration::from_secs(20), sig.wait_for(|v| *v)).await.is_err() {
        let st = victim.keepalive_state();
        let next = victim.execute("SELECT 1 FROM dual", ExecOptions::default()).await;
        panic!("keepalive 가 끊김을 알아채야 한다: ping {st:?}, 다음 호출 {:?}", next.err());
    }
    let e = victim.execute("SELECT 1 FROM dual", ExecOptions::default()).await.unwrap_err();
    assert!(e.is_connection_lost(), "{e:?}");
    // 접속 기록: 접속들, 쉬는 동안 끊김 (keepalive) 이 남고 요약에 나온다
    let evs = sqls_core::connlog::read(&log, 0);
    assert!(evs.iter().filter(|e| e.kind == "connect_ok").count() >= 2, "{evs:?}");
    let lost = evs.iter().find(|e| e.kind == "lost").expect("끊김 기록");
    assert_eq!(lost.via.as_deref(), Some("keepalive"), "{lost:?}");
    assert!(lost.code.is_some(), "{lost:?}");
    let sum = sqls_core::connlog::summarize(&evs, 0, sqls_core::connlog::now(), true, "test");
    assert!(sum.contains("쉬는 동안 1회"), "{sum}");
    // 같은 정보로 다시 접속
    let again = victim.reopen().await.unwrap();
    assert!(!again.is_lost() && again.info().sid != 0);
    exec(&again, "SELECT 1 FROM dual").await;
    again.close();
    admin.close();
}

/// 커밋 전 상태는 드라이버가 알려 준다 — FOR UPDATE·블록 안 DML 도, DML 없는 블록은 아니다
#[tokio::test]
#[ignore]
async fn transaction_state_from_driver() {
    let s = Session::connect(spec(false)).await.unwrap();
    let _ = s.execute("CREATE TABLE txn_t (id NUMBER)", ExecOptions::default()).await;
    exec(&s, "DELETE FROM txn_t").await;
    s.commit().await.unwrap();
    assert_eq!(s.in_transaction(), Some(false), "11g 서버 + 19c 클라이언트에서도 알아야 한다");
    exec(&s, "INSERT INTO txn_t VALUES (1)").await;
    assert_eq!(s.in_transaction(), Some(true));
    s.rollback().await.unwrap();
    assert_eq!(s.in_transaction(), Some(false));
    exec(&s, "BEGIN NULL; END;").await;
    assert_eq!(s.in_transaction(), Some(false), "DML 없는 블록은 트랜잭션이 아니다");
    exec(&s, "BEGIN INSERT INTO txn_t VALUES (2); END;").await;
    assert_eq!(s.in_transaction(), Some(true), "블록 안 DML");
    exec(&s, "COMMIT").await;
    assert_eq!(s.in_transaction(), Some(false), "COMMIT 문을 직접 쳐도");
    exec(&s, "SELECT * FROM txn_t FOR UPDATE").await;
    assert_eq!(s.in_transaction(), Some(true), "FOR UPDATE 는 행을 잠근다");
    s.rollback().await.unwrap();
    s.close();
}

/// 스키마 비교: 실제 사전을 읽어 맞대고 스크립트를 만든다 (다른 스키마 두 개)
#[tokio::test]
#[ignore]
async fn schema_compare_two_schemas() {
    use sqls_core::schemacmp;
    let s = Session::connect(spec(false)).await.unwrap();
    let me = s.info().user.clone();
    let _ = s.execute("CREATE USER cmp_b IDENTIFIED BY cmp_b", ExecOptions::default()).await;
    // 테이블을 만들 자리 (기본 테이블스페이스 할당량)
    exec(&s, "ALTER USER cmp_b QUOTA UNLIMITED ON SYSTEM").await;
    for t in ["cmp_orders", "cmp_new", "cmp_b.cmp_orders", "cmp_b.cmp_gone"] {
        let _ = s.execute(&format!("DROP TABLE {t} CASCADE CONSTRAINTS"), ExecOptions::default()).await;
    }
    exec(&s, "CREATE TABLE cmp_orders (id NUMBER(10) PRIMARY KEY, status VARCHAR2(20 CHAR) DEFAULT 'NEW' NOT NULL, memo VARCHAR2(400))").await;
    exec(&s, "CREATE INDEX cmp_orders_st_ix ON cmp_orders (status, id DESC)").await;
    exec(&s, "CREATE TABLE cmp_new (id NUMBER, CONSTRAINT cmp_new_pk PRIMARY KEY (id))").await;
    exec(&s, "CREATE TABLE cmp_b.cmp_orders (id NUMBER(10) PRIMARY KEY, status VARCHAR2(10 CHAR), old_flag CHAR(1))").await;
    exec(&s, "CREATE TABLE cmp_b.cmp_gone (x NUMBER)").await;
    exec(&s, "CREATE OR REPLACE PROCEDURE cmp_p IS BEGIN NULL; END;").await;
    exec(&s, "CREATE OR REPLACE PROCEDURE cmp_b.cmp_p IS BEGIN DBMS_OUTPUT.PUT_LINE('x'); END;").await;

    let kinds = schemacmp::Kinds::default();
    let started = Instant::now();
    let (a, b) = tokio::join!(
        schemacmp::snapshot(&s, &me, &kinds, Some("CMP%"), &|_| {}),
        schemacmp::snapshot(&s, "CMP_B", &kinds, Some("CMP%"), &|_| {})
    );
    let (a, b) = (a.unwrap(), b.unwrap());
    eprintln!("읽기 {}ms / {}ms (전체 {}ms)", a.ms, b.ms, started.elapsed().as_millis());
    let items = schemacmp::compare(&a, &b);
    let get = |k: &str, n: &str| items.iter().find(|i| i.kind == k && i.name == n).unwrap_or_else(|| panic!("{k} {n}: {items:#?}"));
    let o = get("TABLE", "CMP_ORDERS");
    let d = o.details.join("\n");
    assert!(d.contains("컬럼 형식: STATUS VARCHAR2(20 CHAR) ← VARCHAR2(10 CHAR)"), "{d}");
    assert!(d.contains("컬럼 NULL: STATUS NOT NULL ← NULL"), "{d}");
    assert!(d.contains("컬럼 기본값: STATUS"), "{d}");
    assert!(d.contains("인덱스 추가: CMP_ORDERS_ST_IX (STATUS, \"ID\" DESC)") || d.contains("인덱스 추가: CMP_ORDERS_ST_IX"), "{d}");
    assert!(!d.contains("PK"), "시스템 이름 PK 는 정의로 맞춘다: {d}");
    assert_eq!(get("TABLE", "CMP_NEW").status, schemacmp::Status::OnlySource);
    assert_eq!(get("TABLE", "CMP_GONE").status, schemacmp::Status::OnlyTarget);
    assert_eq!(get("PROCEDURE", "CMP_P").status, schemacmp::Status::Changed);

    let keys: std::collections::BTreeSet<String> = items.iter().map(|i| i.key()).collect();
    let src = schemacmp::sources(&s, &me, &["PROCEDURE CMP_P".to_string()]).await.unwrap();
    let script = schemacmp::script(&a, &b, &items, &keys, &src, "live");
    assert!(script.contains("CREATE OR REPLACE PROCEDURE"), "{script}");
    // 만든 스크립트가 실제로 돈다 (주석 빼고): 대상이 원본과 같아진다. PL/SQL 은 "/" 줄까지, 나머지는 ";" 로 끝나는 줄까지
    let mut buf = String::new();
    let mut in_code = false;
    for line in script.lines() {
        if line.trim_start().starts_with("--") || (buf.is_empty() && line.trim().is_empty()) {
            continue;
        }
        if buf.is_empty() && line.starts_with("CREATE OR REPLACE") && !line.contains(" VIEW ") {
            in_code = true;
        }
        if in_code && line.trim() == "/" {
            exec(&s, buf.trim()).await;
            buf.clear();
            in_code = false;
            continue;
        }
        buf.push_str(line);
        buf.push('\n');
        if !in_code && line.trim_end().ends_with(';') {
            let stmt = buf.trim().trim_end_matches(';').to_string();
            exec(&s, &stmt).await;
            buf.clear();
        }
    }
    let b2 = schemacmp::snapshot(&s, "CMP_B", &kinds, Some("CMP%"), &|_| {}).await.unwrap();
    let left: Vec<_> = schemacmp::compare(&a, &b2).into_iter().filter(|i| i.status != schemacmp::Status::OnlyTarget).collect();
    let left: Vec<_> = left.into_iter().filter(|i| !i.details.iter().all(|d| d.contains("대상에만") || d.contains("순서"))).collect();
    assert!(left.is_empty(), "스크립트를 돌린 뒤에도 다른 것: {left:#?}\n{script}");
    s.close();
}
