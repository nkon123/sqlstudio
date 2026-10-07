//! PL/SQL 디버거 통합 테스트 (실제 Oracle). DEBUG CONNECT SESSION 권한이 필요하다.
//!   SQLS_TEST_DSN=system/oracle@host:1521/XE cargo test -p sqls-core --test debug_live -- --ignored --test-threads=1

use sqls_core::debug::{Debugger, Step};
use sqls_core::{ConnectSpec, ExecOptions, Session};

fn spec() -> ConnectSpec {
    let dsn = std::env::var("SQLS_TEST_DSN").expect("SQLS_TEST_DSN");
    let (cred, cs) = dsn.split_once('@').unwrap();
    let (u, p) = cred.split_once('/').unwrap();
    ConnectSpec::new(u, p, cs)
}

const PKG_SPEC: &str = "CREATE OR REPLACE PACKAGE sqls_dbg IS
  PROCEDURE add_one(p_in IN NUMBER, p_out OUT NUMBER);
  PROCEDURE outer(p_x IN NUMBER, p_r OUT NUMBER);
  PROCEDURE boom;
END;";

// 줄 번호를 테스트에서 쓴다 — 고치면 아래 숫자도 고칠 것
const PKG_BODY: &str = "CREATE OR REPLACE PACKAGE BODY sqls_dbg IS
  PROCEDURE add_one(p_in IN NUMBER, p_out OUT NUMBER) IS
    v_tmp NUMBER := p_in;
  BEGIN
    v_tmp := v_tmp + 1;
    p_out := v_tmp;
  END;
  PROCEDURE outer(p_x IN NUMBER, p_r OUT NUMBER) IS
    v_half NUMBER;
  BEGIN
    v_half := p_x / 2;
    add_one(p_x, p_r);
    dbms_output.put_line('done ' || p_r);
  END;
  PROCEDURE boom IS
    v NUMBER;
  BEGIN
    v := 1 / 0;
  END;
END;";

async fn setup() -> Session {
    let s = Session::connect(spec()).await.unwrap();
    s.execute(PKG_SPEC, ExecOptions::default()).await.unwrap();
    s.execute(PKG_BODY, ExecOptions::default()).await.unwrap();
    s.execute("ALTER PACKAGE sqls_dbg COMPILE DEBUG", ExecOptions::default()).await.unwrap();
    s
}

#[tokio::test]
#[ignore]
async fn breakpoint_variables_steps_and_finish() {
    let s = setup().await;
    let user = s.info().user.clone();
    let block = "DECLARE\n  r NUMBER;\nBEGIN\n  sqls_dbg.outer(41, r);\n  dbms_output.put_line('r=' || r);\nEND;";
    let (mut d, first) = Debugger::start(spec(), block, vec![]).await.unwrap();
    println!("첫 멈춤: {first:?}");
    assert!(!first.terminated);
    assert_eq!((first.unit_type.as_str(), first.line), ("ANONYMOUS BLOCK", 1), "시작하면 블록 첫 줄");

    // add_one 의 'v_tmp := v_tmp + 1' (5행)
    let bp = d.set_breakpoint(&user, "SQLS_DBG", "PACKAGE BODY", 5).await.unwrap();
    assert!(bp > 0);

    let st = d.step(Step::Run, false).await.unwrap();
    println!("중단점: {st:?}");
    assert_eq!((st.name.as_str(), st.line, st.reason.as_str()), ("SQLS_DBG", 5, "breakpoint"));
    assert_eq!(st.unit_type, "PACKAGE BODY");

    assert_eq!(d.get_value("v_tmp", 0).await.unwrap().value.as_deref(), Some("41"));
    assert_eq!(d.get_value("p_in", 0).await.unwrap().value.as_deref(), Some("41"));
    // 한 단계 바깥(outer)의 변수
    // 부른 쪽(outer)의 변수 — frame 은 스택 깊이
    assert_eq!(d.get_value("v_half", st.depth - 1).await.unwrap().value.as_deref(), Some("20.5"));
    let missing = d.get_value("no_such_var", 0).await.unwrap();
    assert!(missing.value.is_none() && missing.error.is_some(), "{missing:?}");

    let stack = d.backtrace().await.unwrap();
    println!("스택: {stack:?}");
    assert!(stack.len() >= 3);
    assert_eq!(stack[0].line, 5);

    // 한 줄 넘기기 → 6행, 값이 바뀌었다
    let st = d.step(Step::Over, false).await.unwrap();
    assert_eq!(st.line, 6, "{st:?}");
    assert_eq!(d.get_value("v_tmp", 0).await.unwrap().value.as_deref(), Some("42"));

    // 값 바꾸기
    d.set_value(0, "v_tmp := 100;").await.unwrap();
    assert_eq!(d.get_value("v_tmp", 0).await.unwrap().value.as_deref(), Some("100"));

    // 나오기 → outer 로
    let st = d.step(Step::Out, false).await.unwrap();
    println!("나옴: {st:?}");
    assert_eq!(st.name, "SQLS_DBG");
    assert!(st.line >= 12, "{st:?}");

    d.delete_breakpoint(bp).await.unwrap();
    let st = d.step(Step::Run, false).await.unwrap();
    assert!(st.terminated, "{st:?}");
    let r = d.finish(false).await.expect("결과").expect("성공");
    println!("출력: {:?}", r.output);
    assert!(r.output.iter().any(|l| l == "done 100"), "{:?}", r.output);
    assert!(r.output.iter().any(|l| l == "r=100"));
}

#[tokio::test]
#[ignore]
async fn step_into_from_anonymous_block() {
    let _s = setup().await;
    let block = "DECLARE\n  r NUMBER;\nBEGIN\n  sqls_dbg.add_one(1, r);\nEND;";
    let (mut d, _) = Debugger::start(spec(), block, vec![]).await.unwrap();
    let mut st = d.step(Step::Into, false).await.unwrap();
    for _ in 0..8 {
        if st.name == "SQLS_DBG" || st.terminated {
            break;
        }
        st = d.step(Step::Into, false).await.unwrap();
    }
    println!("들어감: {st:?}");
    assert_eq!(st.name, "SQLS_DBG");
    assert!(st.line >= 2 && st.line <= 6);
    let _ = d.finish(false).await;
}

#[tokio::test]
#[ignore]
async fn abort_while_stopped() {
    let _s = setup().await;
    let (mut d, first) = Debugger::start(spec(), "BEGIN\n  sqls_dbg.boom;\nEND;", vec![]).await.unwrap();
    assert!(!first.terminated);
    let st = d.step(Step::Abort, false).await.unwrap();
    assert!(st.terminated, "{st:?}");
    let r = d.finish(false).await;
    println!("중지 결과: {r:?}");
}

#[tokio::test]
#[ignore]
async fn stops_on_exception() {
    let _s = setup().await;
    let (mut d, _) = Debugger::start(spec(), "BEGIN\n  sqls_dbg.boom;\nEND;", vec![]).await.unwrap();
    let st = d.step(Step::Run, true).await.unwrap();
    println!("예외: {st:?}");
    assert_eq!(st.reason, "exception");
    assert_eq!(st.ora_code, Some(1476), "ORA-01476 divisor is equal to zero");
    assert_eq!(st.line, 18);
    // 계속하면 오류로 끝난다
    let st = d.step(Step::Run, false).await.unwrap();
    assert!(st.terminated);
    let r = d.finish(false).await.unwrap();
    assert!(r.is_err(), "{r:?}");
}

#[tokio::test]
#[ignore]
async fn binds_reach_the_target() {
    let _s = setup().await;
    let (mut d, _) = Debugger::start(
        spec(),
        "DECLARE r NUMBER; BEGIN sqls_dbg.add_one(:x, r); dbms_output.put_line('r=' || r); END;",
        vec![("x".into(), Some("9".into()))],
    )
    .await
    .unwrap();
    let st = d.step(Step::Run, false).await.unwrap();
    assert!(st.terminated);
    let r = d.finish(false).await.unwrap().unwrap();
    assert_eq!(r.output, ["r=10"]);
}

#[tokio::test]
#[ignore]
async fn generated_call_block_runs_under_debugger() {
    let s = setup().await;
    let user = s.info().user.clone();
    assert_eq!(sqls_core::debug::has_debug_info(&s, &user, "SQLS_DBG", "PACKAGE BODY").await, Some(true));
    let es = sqls_core::debug::entries(&s, &user, "SQLS_DBG", "PACKAGE").await.unwrap();
    let labels: Vec<_> = es.iter().map(|e| e.label.as_str()).collect();
    assert_eq!(labels, ["SQLS_DBG.ADD_ONE", "SQLS_DBG.BOOM", "SQLS_DBG.OUTER"]);
    let add = &es[0].template;
    println!("{add}");
    let (mut d, _) = Debugger::start(spec(), add, vec![("p_in".into(), Some("5".into()))]).await.unwrap();
    assert!(d.step(Step::Run, false).await.unwrap().terminated);
    let r = d.finish(false).await.unwrap().unwrap();
    assert_eq!(r.output, ["p_out = 6"]);
    let src = sqls_core::debug::source(&s, &user, "SQLS_DBG", "PACKAGE BODY").await.unwrap();
    assert_eq!(src[4].trim(), "v_tmp := v_tmp + 1;");
    assert_eq!(sqls_core::debug::locals_at(&src, 5), ["P_IN", "P_OUT", "V_TMP"]);
}
