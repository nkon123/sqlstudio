// 가짜 DB 의 PL/SQL 소스 — F4·소스 보기로 코드 탐색(구조 보기·위치 줄·접기)을 시험한다.
// 주석 안의 "/*", 문자열 안의 END; · q'[…]', 이름표 붙은 LOOP, CASE 문·식, 안쪽 익명 블록을 일부러 섞었다.

const PKG_ORDER_SPEC = `CREATE OR REPLACE PACKAGE "SCOTT"."PKG_ORDER" AUTHID DEFINER AS
  /*----------------------------------------------------------------
   * 주문 처리 패키지
   * 변경이력 /* 2024-03 마감 로직 분리 (Oracle 주석은 겹치지 않는다 — 여기 /* 가 있어도 아래 */ 에서 끝)
   *----------------------------------------------------------------*/
  c_status_ok    CONSTANT VARCHAR2(2) := 'OK';
  c_status_cncl  CONSTANT VARCHAR2(2) := 'CN';

  TYPE r_order IS RECORD (
    order_id  ORDERS.ORDER_ID%TYPE,
    cust_id   ORDERS.CUST_ID%TYPE,
    amount    NUMBER(12, 2)
  );
  TYPE t_orders IS TABLE OF r_order INDEX BY PLS_INTEGER;
  TYPE rc_orders IS REF CURSOR RETURN r_order;

  FUNCTION fn_order_amt(p_order_id IN NUMBER) RETURN NUMBER;
  PROCEDURE proc_register(p_cust_id IN NUMBER, p_items IN VARCHAR2, o_order_id OUT NUMBER);
  PROCEDURE proc_cancel(p_order_id IN NUMBER, p_reason IN VARCHAR2 DEFAULT NULL);
  PROCEDURE proc_settle(p_base_dt IN DATE DEFAULT TRUNC(SYSDATE) - 1);
  PROCEDURE proc_purge(p_months IN PLS_INTEGER DEFAULT 6);
END pkg_order;`;

const PKG_ORDER_BODY = `CREATE OR REPLACE PACKAGE BODY "SCOTT"."PKG_ORDER" AS
  /*----------------------------------------------------------------
   * 주문 처리 패키지 본문
   * 변경이력 /* 2024-03 마감 로직 분리
   *          2024-09 취소 사유 추가
   *----------------------------------------------------------------*/

  g_batch_id   NUMBER;
  g_msg        VARCHAR2(200) := 'END; 는 문자열 안에서 블록 끝이 아니다';
  g_sql        VARCHAR2(400) := q'[SELECT 'it''s' FROM dual /* not a comment */]';

  CURSOR c_open_orders(p_base_dt DATE) IS
    SELECT o.order_id, o.cust_id, SUM(i.qty * i.price) amount
      FROM orders o, order_items i
     WHERE i.order_id = o.order_id
       AND o.order_dt < p_base_dt
       AND o.status = 'NEW'
     GROUP BY o.order_id, o.cust_id
       FOR UPDATE OF o.status SKIP LOCKED;

  TYPE t_ids IS TABLE OF NUMBER;

  -- 내부: 로그 한 줄
  PROCEDURE log_msg(p_step IN VARCHAR2, p_msg IN VARCHAR2) IS
    PRAGMA AUTONOMOUS_TRANSACTION;
  BEGIN
    INSERT INTO batch_log (batch_id, step, msg, log_dt)
    VALUES (g_batch_id, p_step, SUBSTR(p_msg, 1, 4000), SYSTIMESTAMP);
    COMMIT;
  EXCEPTION
    WHEN OTHERS THEN
      ROLLBACK;
  END log_msg;

  -- 내부: 주문 상태 바꾸기
  PROCEDURE set_status(p_order_id IN NUMBER, p_status IN VARCHAR2) IS
  BEGIN
    UPDATE orders
       SET status = p_status,
           upd_dt = SYSDATE
     WHERE order_id = p_order_id;
    IF SQL%ROWCOUNT = 0 THEN
      RAISE_APPLICATION_ERROR(-20001, '주문이 없습니다: ' || p_order_id);
    END IF;
  END set_status;

  FUNCTION fn_order_amt(p_order_id IN NUMBER) RETURN NUMBER IS
    v_amt NUMBER := 0;
  BEGIN
    SELECT NVL(SUM(qty * price), 0)
      INTO v_amt
      FROM order_items
     WHERE order_id = p_order_id;
    RETURN CASE WHEN v_amt < 0 THEN 0 ELSE ROUND(v_amt, 2) END;
  EXCEPTION
    WHEN NO_DATA_FOUND THEN
      RETURN 0;
  END fn_order_amt;

  PROCEDURE proc_register(p_cust_id IN NUMBER, p_items IN VARCHAR2, o_order_id OUT NUMBER) IS
    v_item   VARCHAR2(100);
    v_pos    PLS_INTEGER := 1;
    v_line   PLS_INTEGER := 0;
  BEGIN
    SELECT seq_order.NEXTVAL INTO o_order_id FROM dual;

    INSERT INTO orders (order_id, cust_id, order_dt, status)
    VALUES (o_order_id, p_cust_id, SYSDATE, 'NEW');

    -- "상품:수량,상품:수량" 을 하나씩
    WHILE v_pos <= LENGTH(p_items) LOOP
      v_item := REGEXP_SUBSTR(p_items, '[^,]+', 1, v_line + 1);
      EXIT WHEN v_item IS NULL;
      v_line := v_line + 1;

      DECLARE
        v_prod  VARCHAR2(30) := REGEXP_SUBSTR(v_item, '[^:]+', 1, 1);
        v_qty   NUMBER       := TO_NUMBER(REGEXP_SUBSTR(v_item, '[^:]+', 1, 2));
        v_price NUMBER;
      BEGIN
        SELECT price INTO v_price FROM products WHERE prod_cd = v_prod;
        INSERT INTO order_items (order_id, line_no, prod_cd, qty, price)
        VALUES (o_order_id, v_line, v_prod, v_qty, v_price);
      EXCEPTION
        WHEN NO_DATA_FOUND THEN
          log_msg('REGISTER', '없는 상품 ' || v_prod);
      END;

      v_pos := v_pos + LENGTH(v_item) + 1;
    END LOOP;

    IF v_line = 0 THEN
      RAISE_APPLICATION_ERROR(-20002, '상품이 없습니다');
    END IF;
  END proc_register;

  PROCEDURE proc_cancel(p_order_id IN NUMBER, p_reason IN VARCHAR2 DEFAULT NULL) IS
    v_status orders.status%TYPE;
  BEGIN
    SELECT status INTO v_status FROM orders WHERE order_id = p_order_id FOR UPDATE;

    CASE v_status
      WHEN 'NEW' THEN
        set_status(p_order_id, c_status_cncl);
      WHEN c_status_ok THEN
        -- 정산된 주문은 반품으로
        INSERT INTO order_returns (order_id, reason, ret_dt)
        VALUES (p_order_id, NVL(p_reason, '고객 요청'), SYSDATE);
        set_status(p_order_id, 'RT');
      ELSE
        RAISE_APPLICATION_ERROR(-20003, '취소할 수 없는 상태: ' || v_status);
    END CASE;

    log_msg('CANCEL', p_order_id || ' ' || p_reason);
  END proc_cancel;

  PROCEDURE proc_settle(p_base_dt IN DATE DEFAULT TRUNC(SYSDATE) - 1) IS
    v_done  PLS_INTEGER := 0;
    v_fail  PLS_INTEGER := 0;
    v_ids   t_ids := t_ids();
  BEGIN
    g_batch_id := TO_NUMBER(TO_CHAR(SYSDATE, 'YYYYMMDDHH24MISS'));
    log_msg('SETTLE', '시작 ' || TO_CHAR(p_base_dt, 'YYYY-MM-DD'));

    <<orders_loop>>
    FOR r IN c_open_orders(p_base_dt) LOOP
      BEGIN
        IF r.amount <= 0 THEN
          set_status(r.order_id, c_status_cncl);
          v_fail := v_fail + 1;
          CONTINUE orders_loop;
        ELSIF r.amount > 10000000 THEN
          log_msg('SETTLE', '큰 금액 확인 필요 ' || r.order_id);
        END IF;

        MERGE INTO order_sum t
        USING (SELECT r.order_id order_id, r.cust_id cust_id, r.amount amount FROM dual) s
           ON (t.order_id = s.order_id)
         WHEN MATCHED THEN
              UPDATE SET t.amount = s.amount, t.upd_dt = SYSDATE
         WHEN NOT MATCHED THEN
              INSERT (order_id, cust_id, amount, upd_dt)
              VALUES (s.order_id, s.cust_id, s.amount, SYSDATE);

        set_status(r.order_id, c_status_ok);
        v_ids.EXTEND;
        v_ids(v_ids.COUNT) := r.order_id;
        v_done := v_done + 1;
      EXCEPTION
        WHEN OTHERS THEN
          v_fail := v_fail + 1;
          log_msg('SETTLE', r.order_id || ' ' || SQLERRM);
      END;
    END LOOP orders_loop;

    -- 정산된 주문의 고객 등급 다시 매기기
    FORALL i IN 1 .. v_ids.COUNT
      UPDATE customers c
         SET c.grade = CASE
                         WHEN (SELECT SUM(amount) FROM order_sum s WHERE s.cust_id = c.cust_id) > 1000000 THEN 'VIP'
                         ELSE 'NORMAL'
                       END
       WHERE c.cust_id = (SELECT cust_id FROM orders WHERE order_id = v_ids(i));

    log_msg('SETTLE', '끝 성공 ' || v_done || ' 실패 ' || v_fail);
    COMMIT;
  END proc_settle;

  PROCEDURE proc_purge(p_months IN PLS_INTEGER DEFAULT 6) IS
    v_cut DATE := ADD_MONTHS(TRUNC(SYSDATE), -p_months);
    v_cnt PLS_INTEGER;
  BEGIN
    LOOP
      DELETE FROM order_sum WHERE upd_dt < v_cut AND ROWNUM <= 10000;
      v_cnt := SQL%ROWCOUNT;
      COMMIT;
      EXIT WHEN v_cnt < 10000;
    END LOOP;
    log_msg('PURGE', v_cut || ' 이전 정리');
  END proc_purge;

BEGIN
  -- 패키지 초기화
  g_batch_id := 0;
END pkg_order;`;

const P_CLOSE_DAY = `CREATE OR REPLACE PROCEDURE "SCOTT"."P_CLOSE_DAY" (p_dt IN DATE DEFAULT TRUNC(SYSDATE)) IS
  /* 일 마감 — PKG_ORDER 를 차례로 부른다 */
  v_start TIMESTAMP := SYSTIMESTAMP;
BEGIN
  pkg_order.proc_settle(p_dt - 1);

  IF TO_CHAR(p_dt, 'DD') = '01' THEN
    -- 월초에만 오래된 요약 정리
    pkg_order.proc_purge(6);
  END IF;

  FOR d IN (SELECT dept_no FROM dept ORDER BY dept_no) LOOP
    UPDATE dept_daily
       SET close_dt = p_dt
     WHERE dept_no = d.dept_no;
  END LOOP;

  DBMS_OUTPUT.PUT_LINE('마감 ' || EXTRACT(SECOND FROM (SYSTIMESTAMP - v_start)) || '초');
EXCEPTION
  WHEN OTHERS THEN
    ROLLBACK;
    RAISE;
END p_close_day;`;

const FN_EMP_GRADE = `CREATE OR REPLACE FUNCTION "SCOTT"."FN_EMP_GRADE" (p_empno IN NUMBER) RETURN VARCHAR2 IS
  v_sal  emp.sal%TYPE;
  v_comm emp.comm%TYPE;
BEGIN
  SELECT sal, NVL(comm, 0) INTO v_sal, v_comm FROM emp WHERE empno = p_empno;

  IF v_sal + v_comm >= 5000 THEN
    RETURN 'A';
  ELSIF v_sal + v_comm >= 3000 THEN
    RETURN 'B';
  ELSE
    RETURN CASE WHEN v_comm > 0 THEN 'C+' ELSE 'C' END;
  END IF;
EXCEPTION
  WHEN NO_DATA_FOUND THEN
    RETURN NULL;
END fn_emp_grade;`;

const TRG_ORDERS_AUDIT = `CREATE OR REPLACE TRIGGER "SCOTT"."TRG_ORDERS_AUDIT"
  AFTER INSERT OR UPDATE OR DELETE ON scott.orders
  FOR EACH ROW
DECLARE
  v_op VARCHAR2(1);
BEGIN
  v_op := CASE WHEN INSERTING THEN 'I' WHEN UPDATING THEN 'U' ELSE 'D' END;

  IF UPDATING('STATUS') AND :old.status = :new.status THEN
    RETURN;
  END IF;

  INSERT INTO orders_audit (order_id, op, old_status, new_status, aud_dt, aud_user)
  VALUES (NVL(:new.order_id, :old.order_id), v_op, :old.status, :new.status, SYSDATE, USER);
END trg_orders_audit;`;

/** 이름·종류 → 소스 (DBMS_METADATA 처럼 앞에 빈 줄) */
export function mockPlsql(objectType: string, name: string): string | null {
  const n = name.replace(/"/g, "").split(".").pop()?.toUpperCase() ?? "";
  const t = objectType.toUpperCase();
  const src =
    n === "PKG_ORDER" ? (t === "PACKAGE BODY" ? PKG_ORDER_BODY : `${PKG_ORDER_SPEC}\n/\n\n${PKG_ORDER_BODY}`)
    : n === "P_CLOSE_DAY" ? P_CLOSE_DAY
    : n === "FN_EMP_GRADE" ? FN_EMP_GRADE
    : n === "TRG_ORDERS_AUDIT" ? TRG_ORDERS_AUDIT
    : null;
  return src && `\n  ${src}\n/\n`;
}

/** 시험해 볼 문서 — 이름 위에서 F4 */
export const PLSQL_SAMPLE = `-- 코드 탐색 시험 (가짜 DB)
-- 아래 이름(pkg_order · p_close_day · fn_emp_grade · trg_orders_audit) 위에 커서를 두고 F4 → 소스가 새 탭으로 열린다
--   그 탭에서: Ctrl+Shift+O 구조 보기 · 위쪽 위치 줄 · Alt+← → 뒤로·앞으로 · 줄 번호 옆 ▾ 접기 (Ctrl+Alt+- 모두 접기)
BEGIN
  pkg_order.proc_settle(TRUNC(SYSDATE) - 1);
  p_close_day(SYSDATE);
  DBMS_OUTPUT.PUT_LINE(fn_emp_grade(7839));
END;
/
-- 트리거: trg_orders_audit
`;
