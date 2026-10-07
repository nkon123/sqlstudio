use crate::chunk::{plan, Limits, UnitSource};
use crate::plsql::{lex, structure, Kind, SubKind};

pub const PKG: &str = r#"PACKAGE BODY order_pkg IS
  g_rate CONSTANT NUMBER := 0.1;
  TYPE t_ids IS TABLE OF NUMBER INDEX BY PLS_INTEGER;
  g_cache t_ids;
  CURSOR c_open IS SELECT id FROM orders WHERE status = 'OPEN';

  -- 합계 계산
  FUNCTION calc_total(p_id IN NUMBER) RETURN NUMBER IS
    v_total NUMBER := 0;
    FUNCTION line_amt(p_qty NUMBER, p_price NUMBER) RETURN NUMBER IS
    BEGIN
      RETURN p_qty * p_price * (1 - g_rate);
    END line_amt;
  BEGIN
    FOR r IN (SELECT qty, price FROM order_lines l JOIN products p ON p.id = l.product_id WHERE l.order_id = p_id) LOOP
      v_total := v_total + line_amt(r.qty, r.price);
    END LOOP;
    v_total := CASE WHEN v_total < 0 THEN 0 ELSE v_total END;
    RETURN v_total;
  END calc_total;

  PROCEDURE close_order(p_id NUMBER) IS
    v_amt NUMBER;
  BEGIN
    v_amt := calc_total(p_id);
    IF v_amt > 1000 THEN
      audit_pkg.log('big', p_id);
    ELSIF v_amt = 0 THEN
      RAISE_APPLICATION_ERROR(-20001, '빈 주문');
    END IF;
    UPDATE orders SET status = 'CLOSED', amount = v_amt WHERE id = p_id;
    INSERT INTO order_hist (id, amount, ts) VALUES (p_id, v_amt, SYSDATE);
    DELETE FROM order_tmp WHERE id = p_id;
    g_cache(p_id) := v_amt;
    MERGE INTO daily_sum d USING (SELECT TRUNC(SYSDATE) dt FROM dual) s ON (d.dt = s.dt)
      WHEN MATCHED THEN UPDATE SET d.amt = d.amt + v_amt
      WHEN NOT MATCHED THEN INSERT (dt, amt) VALUES (s.dt, v_amt);
    EXECUTE IMMEDIATE 'TRUNCATE TABLE tmp_x';
    COMMIT;
  EXCEPTION
    WHEN NO_DATA_FOUND THEN
      NULL;
    WHEN OTHERS THEN
      ROLLBACK;
      RAISE;
  END close_order;

  PROCEDURE nightly IS
  BEGIN
    FOR r IN c_open LOOP
      close_order(r.id);
    END LOOP;
    refresh_stats;
  END;
BEGIN
  g_cache.DELETE;
END order_pkg;
"#;

#[test]
fn lexer_skips_comments_and_strings() {
    let t = lex("select 'a;--b' x -- c\n/* BEGIN */ from q'[it's]' \"Mixed\" 1..10");
    let words: Vec<&str> = t.iter().filter_map(|t| t.word()).collect();
    assert_eq!(words, vec!["SELECT", "X", "FROM"]);
    assert!(matches!(&t[1].kind, Kind::Str(s) if s == "a;--b"));
    assert!(matches!(&t[4].kind, Kind::Str(s) if s == "it's"));
    assert!(matches!(&t[5].kind, Kind::Quoted(s) if s == "Mixed"));
    assert!(t[7].sym(".."));
    assert_eq!(t.last().unwrap().line, 2);
}

#[test]
fn package_structure() {
    let (st, _) = structure(PKG);
    assert_eq!(st.unit_type, "PACKAGE BODY");
    assert_eq!(st.name, "ORDER_PKG");
    assert!(st.warning.is_none(), "{:?}", st.warning);
    let names: Vec<(&str, u32, u32)> = st.subprograms.iter().map(|s| (s.path.as_str(), s.start_line, s.end_line)).collect();
    assert_eq!(
        names,
        vec![
            ("CALC_TOTAL", 8, 20),
            ("CALC_TOTAL.LINE_AMT", 10, 13),
            ("CLOSE_ORDER", 22, 46),
            ("NIGHTLY", 48, 54),
            ("(초기화)", 55, 57),
        ]
    );
    let calc = &st.subprograms[0];
    assert_eq!(calc.doc_line, 7);
    assert_eq!(calc.kind, SubKind::Function);
    assert_eq!(calc.children, vec![1]);
    assert_eq!(calc.signature, "FUNCTION CALC_TOTAL(P_ID IN NUMBER) RETURN NUMBER");
    assert_eq!(st.global_lines, vec![(1, 6), (21, 21), (47, 47)]);
}

#[test]
fn facts_of_close_order() {
    let p = plan(&UnitSource { owner: "APP".into(), name: "ORDER_PKG".into(), unit_type: "PACKAGE BODY".into(), text: PKG.into() }, None, Limits::default());
    let c = p.chunks.iter().find(|c| c.subprogram.as_deref() == Some("CLOSE_ORDER")).unwrap();
    let t: Vec<(&str, &str)> = c.facts.tables.iter().map(|t| (t.name.as_str(), t.ops.as_str())).collect();
    assert_eq!(t, vec![("DAILY_SUM", "CU"), ("ORDERS", "U"), ("ORDER_HIST", "C"), ("ORDER_TMP", "D")]);
    let calls: Vec<&str> = c.facts.calls.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(calls, vec!["AUDIT_PKG.LOG", "CALC_TOTAL"]);
    assert_eq!(c.facts.dynamic_sql.len(), 1);
    let tx: Vec<&str> = c.facts.transactions.iter().map(|m| m.what.as_str()).collect();
    assert_eq!(tx, vec!["COMMIT", "ROLLBACK"]);
    assert_eq!(c.facts.handles, vec!["NO_DATA_FOUND", "OTHERS"]);
    assert_eq!(c.facts.swallowed, vec![41]);
    assert!(c.facts.raises.iter().any(|r| r.what.starts_with("RAISE_APPLICATION_ERROR(-20001) '빈 주문'")), "{:?}", c.facts.raises);

    let calc = p.chunks.iter().find(|c| c.subprogram.as_deref() == Some("CALC_TOTAL")).unwrap();
    let t: Vec<&str> = calc.facts.tables.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(t, vec!["ORDER_LINES", "PRODUCTS"]);
    let calls: Vec<&str> = calc.facts.calls.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(calls, vec!["LINE_AMT"]);
    // 중첩 함수 본문은 한 줄 표시로
    assert!(calc.code.contains("▶ 중첩 FUNCTION LINE_AMT (10~13행"), "{}", calc.code);
    assert!(!calc.code.contains("p_qty * p_price"));

    let nightly = p.chunks.iter().find(|c| c.subprogram.as_deref() == Some("NIGHTLY")).unwrap();
    let calls: Vec<&str> = nightly.facts.calls.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(calls, vec!["CLOSE_ORDER", "REFRESH_STATS"]);

    let g = p.chunks.iter().find(|c| c.subprogram.is_none()).unwrap();
    assert_eq!(g.facts.tables[0].name, "ORDERS");
    // 전역 컬렉션 g_cache(p_id) 는 호출이 아니다
    let init = p.chunks.iter().find(|c| c.subprogram.as_deref() == Some("(초기화)")).unwrap();
    assert!(init.facts.calls.is_empty(), "{:?}", init.facts.calls);
    assert!(c.facts.calls.iter().all(|c| c.name != "G_CACHE"));
}

#[test]
fn big_subprogram_is_split_at_statements() {
    let mut body = String::from("PROCEDURE big(p NUMBER) IS\n  v NUMBER;\nBEGIN\n");
    for i in 0..30 {
        body.push_str(&format!("  IF p = {i} THEN\n    UPDATE t{i} SET a = 1;\n    v := {i};\n  END IF;\n"));
    }
    body.push_str("EXCEPTION\n  WHEN OTHERS THEN\n    RAISE;\nEND big;\n");
    let p = plan(
        &UnitSource { owner: "A".into(), name: "BIG".into(), unit_type: "PROCEDURE".into(), text: body.clone() },
        None,
        Limits { max_lines: 30, max_chars: 100_000 },
    );
    assert!(p.chunks.len() >= 4, "{}", p.chunks.len());
    let total = body.lines().count() as u32;
    // 빈틈·겹침 없이 이어진다
    let mut next = 1;
    for c in &p.chunks {
        assert_eq!(c.start_line, next, "{:?}", p.chunks.iter().map(|c| (c.start_line, c.end_line)).collect::<Vec<_>>());
        assert!(c.end_line - c.start_line < 30);
        next = c.end_line + 1;
        // IF 블록 중간에서 자르지 않는다: 조각 끝은 END IF; 이거나 선언/EXCEPTION 앞
        let last = body.lines().nth(c.end_line as usize - 1).unwrap().trim();
        assert!(last.ends_with(';') || last == "BEGIN" , "{last}");
        assert_eq!(c.parts, p.chunks.len() as u32);
    }
    assert_eq!(next, total + 1);
    let tables: usize = p.chunks.iter().map(|c| c.facts.tables.len()).sum();
    assert_eq!(tables, 30);
    assert!(p.chunks[1].context.contains("선언부"));
}

#[test]
fn standalone_and_trigger() {
    let (st, _) = structure("CREATE OR REPLACE PROCEDURE app.p_one AS BEGIN NULL; END;");
    assert_eq!(st.owner.as_deref(), Some("APP"));
    assert_eq!(st.subprograms.len(), 1);
    assert_eq!(st.subprograms[0].name, "P_ONE");
    let (st, _) = structure(
        "TRIGGER trg_emp BEFORE INSERT ON emp FOR EACH ROW\nDECLARE\n  v NUMBER;\nBEGIN\n  IF :new.id IS NULL THEN\n    SELECT emp_seq.NEXTVAL INTO :new.id FROM dual;\n  END IF;\nEND;",
    );
    assert_eq!(st.subprograms.len(), 1);
    assert_eq!(st.subprograms[0].kind, SubKind::Trigger);
    assert_eq!(st.subprograms[0].end_line, 8);
    let p = plan(
        &UnitSource { owner: "A".into(), name: "TRG_EMP".into(), unit_type: "TRIGGER".into(), text: "TRIGGER trg_emp BEFORE INSERT ON emp FOR EACH ROW\nBEGIN\n  SELECT emp_seq.NEXTVAL INTO :new.id FROM dual;\nEND;".into() },
        None,
        Limits::default(),
    );
    assert_eq!(p.chunks[0].facts.sequences, vec!["EMP_SEQ"]);
    assert!(p.chunks[0].facts.tables.is_empty(), "{:?}", p.chunks[0].facts.tables);
}

#[test]
fn spec_comments_reach_body_chunks() {
    let spec = "PACKAGE order_pkg IS\n  -- 주문을 닫고 이력을 남긴다\n  PROCEDURE close_order(p_id NUMBER);\n  FUNCTION calc_total(p_id IN NUMBER) RETURN NUMBER; -- 할인 포함 합계\nEND;";
    let p = plan(&UnitSource { owner: "APP".into(), name: "ORDER_PKG".into(), unit_type: "PACKAGE BODY".into(), text: PKG.into() }, Some(spec), Limits::default());
    let c = p.chunks.iter().find(|c| c.subprogram.as_deref() == Some("CLOSE_ORDER")).unwrap();
    assert!(c.context.contains("주문을 닫고"), "{}", c.context);
    let c = p.chunks.iter().find(|c| c.subprogram.as_deref() == Some("CALC_TOTAL")).unwrap();
    assert!(c.context.contains("할인 포함"), "{}", c.context);
    let (st, _) = structure(spec);
    assert_eq!(st.decls.len(), 2);
    assert!(st.subprograms.is_empty());
}

#[test]
fn unbalanced_source_does_not_panic() {
    for src in ["PACKAGE BODY x IS PROCEDURE p IS BEGIN IF a THEN", "PROCEDURE", "", "PACKAGE BODY x IS END", "FUNCTION f RETURN NUMBER IS BEGIN RETURN CASE WHEN 1=1 THEN 1 END"] {
        let p = plan(&UnitSource { owner: "A".into(), name: "X".into(), unit_type: "PACKAGE BODY".into(), text: src.into() }, None, Limits::default());
        let _ = serde_json::to_string(&p).unwrap();
    }
}

#[test]
fn wrapped_source_is_skipped() {
    let src = "PACKAGE BODY dbms_x wrapped \na000000\n1\nabcd\n2 :e:\n1PACKAGE:\n";
    let p = plan(&UnitSource { owner: "SYS".into(), name: "DBMS_X".into(), unit_type: "PACKAGE BODY".into(), text: src.into() }, None, Limits::default());
    assert!(p.chunks.is_empty());
    assert!(p.structure.warning.as_deref().unwrap().contains("wrap"));
}
