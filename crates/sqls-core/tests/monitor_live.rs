//! 세션·락 모니터 (실제 Oracle). A 가 행을 잠그고 B 가 같은 행을 기다리게 한 뒤 모니터로 본다.
//!   SQLS_TEST_DSN=system/oracle@host:1521/XE cargo test -p sqls-core --test monitor_live -- --ignored

use std::time::Duration;

use sqls_core::monitor;
use sqls_core::{ConnectSpec, ExecOptions, Session};

fn spec() -> ConnectSpec {
    let dsn = std::env::var("SQLS_TEST_DSN").expect("SQLS_TEST_DSN");
    let (cred, cs) = dsn.split_once('@').unwrap();
    let (u, p) = cred.split_once('/').unwrap();
    ConnectSpec::new(u, p, cs)
}

#[tokio::test]
#[ignore]
async fn blocking_tree_detail_and_kill() {
    let a = Session::connect(spec()).await.unwrap();
    let _ = a.execute("CREATE TABLE sqls_lock_t (id NUMBER PRIMARY KEY, v NUMBER)", ExecOptions::default()).await;
    let _ = a.execute("INSERT INTO sqls_lock_t VALUES (1, 0)", ExecOptions::default()).await;
    a.commit().await.unwrap();
    // A 가 잠근다 (커밋하지 않음)
    a.execute("UPDATE sqls_lock_t SET v = v + 1 WHERE id = 1", ExecOptions::default()).await.unwrap();
    let a_sid: i64 = a.query("SELECT SYS_CONTEXT('USERENV','SID') FROM dual", vec![], 1).await.unwrap().rows[0][0].clone().unwrap().parse().unwrap();

    // B 가 같은 행을 기다린다
    let b = Session::connect(spec()).await.unwrap();
    let b_sid: i64 = b.query("SELECT SYS_CONTEXT('USERENV','SID') FROM dual", vec![], 1).await.unwrap().rows[0][0].clone().unwrap().parse().unwrap();
    let b2 = b.clone();
    let waiter = tokio::spawn(async move { b2.execute("UPDATE sqls_lock_t SET v = v + 10 WHERE id = 1", ExecOptions::default()).await });

    // 모니터 (읽기 전용 세션)
    let mut ms = spec();
    ms.read_only = true;
    let m = Session::connect(ms).await.unwrap();
    let mut found = None;
    for _ in 0..50 {
        tokio::time::sleep(Duration::from_millis(200)).await;
        let w = monitor::waits(&m).await.unwrap();
        if let Some(x) = w.into_iter().find(|x| x.sid == b_sid) {
            found = Some(x);
            break;
        }
    }
    let w = found.expect("B 가 기다리는 것이 보여야 한다");
    assert_eq!(w.blocker, a_sid);
    assert!(w.event.as_deref().unwrap_or("").contains("enq: TX"), "{:?}", w.event);
    assert!(w.object.as_deref().unwrap_or("").ends_with(".SQLS_LOCK_T"), "{:?}", w.object);

    let all = monitor::sessions(&m, true, false).await.unwrap();
    let ra = all.iter().find(|s| s.sid == a_sid).expect("막는 세션은 활성만 보기에도 나온다");
    assert_eq!(ra.blocks, 1);
    assert!(ra.in_transaction);
    let rb = all.iter().find(|s| s.sid == b_sid).unwrap();
    assert_eq!(rb.blocking_session, Some(a_sid));
    assert_eq!(rb.status, "ACTIVE");

    let d = monitor::detail(&m, a_sid).await.unwrap();
    assert!(d.tx_start.is_some());
    assert!(d.locked.iter().any(|l| l.object.ends_with(".SQLS_LOCK_T") && l.mode == "Row-X (SX)"), "{:?}", d.locked);
    let db = monitor::detail(&m, b_sid).await.unwrap();
    assert!(db.sql_text.as_deref().unwrap_or("").contains("v + 10"), "{:?}", db.sql_text);

    // B 를 죽인다 (사람이 확인한 뒤 앱이 하는 것과 같은 문장)
    // 별도 세션으로 실행 — A(작업 세션)는 멀쩡해야 한다
    let msg = monitor::kill(spec(), b_sid, rb.serial, true).await.unwrap();
    assert!(!msg.is_empty());
    let r = tokio::time::timeout(Duration::from_secs(20), waiter).await.expect("B 가 풀려야 한다").unwrap();
    assert!(r.is_err(), "죽은 세션의 호출은 오류로 끝난다");
    a.rollback().await.expect("작업 세션은 살아 있어야 한다");
    // 없는 세션
    assert!(monitor::kill(spec(), b_sid, rb.serial, true).await.is_err());
    a.close();
    m.close();
}
