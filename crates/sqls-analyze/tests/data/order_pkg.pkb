PACKAGE BODY order_pkg IS
  g_rate CONSTANT NUMBER := 0.1;
  TYPE t_ids IS TABLE OF NUMBER INDEX BY PLS_INTEGER;
  g_cache t_ids;
  CURSOR c_open IS SELECT id FROM orders WHERE status = 'OPEN';

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
    RETURN v_total;
  END calc_total;

  PROCEDURE close_order(p_id NUMBER) IS
    v_amt NUMBER;
  BEGIN
    v_amt := calc_total(p_id);
    IF v_amt > 1000 THEN
      audit_pkg.log('big', p_id);
    END IF;
    UPDATE orders SET status = 'CLOSED', amount = v_amt WHERE id = p_id;
    INSERT INTO order_hist (id, amount, ts) VALUES (p_id, v_amt, SYSDATE);
    MERGE INTO daily_sum d USING (SELECT TRUNC(SYSDATE) dt FROM dual) s ON (d.dt = s.dt)
      WHEN MATCHED THEN UPDATE SET d.amt = d.amt + v_amt
      WHEN NOT MATCHED THEN INSERT (dt, amt) VALUES (s.dt, v_amt);
    g_cache(p_id) := v_amt;
    COMMIT;
    dbms_output.put_line('closed ' || p_id);
    --
    --
    --
    --
    --
  EXCEPTION
    WHEN NO_DATA_FOUND THEN NULL;
    WHEN OTHERS THEN
      ROLLBACK;
      RAISE;
  END close_order;

  PROCEDURE validate(p_id NUMBER) IS
  BEGIN
    IF p_id IS NULL THEN
      RAISE_APPLICATION_ERROR(-20010, '주문 번호 없음');
    END IF;
  END;

  PROCEDURE retry(p_n NUMBER) IS
  BEGIN
    IF p_n > 0 THEN
      retry(p_n - 1);
    END IF;
  END retry;

  PROCEDURE nightly IS
  BEGIN
    FOR r IN c_open LOOP
      close_order(r.id);
    END LOOP;
    retry(3);
    UPDATE stats_0 SET runs = runs + 1 WHERE k = 0;
    UPDATE stats_1 SET runs = runs + 1 WHERE k = 1;
    UPDATE stats_2 SET runs = runs + 1 WHERE k = 2;
    UPDATE stats_0 SET runs = runs + 1 WHERE k = 3;
    UPDATE stats_1 SET runs = runs + 1 WHERE k = 4;
    UPDATE stats_2 SET runs = runs + 1 WHERE k = 5;
    UPDATE stats_0 SET runs = runs + 1 WHERE k = 6;
    UPDATE stats_1 SET runs = runs + 1 WHERE k = 7;
    UPDATE stats_2 SET runs = runs + 1 WHERE k = 8;
    UPDATE stats_0 SET runs = runs + 1 WHERE k = 9;
    UPDATE stats_1 SET runs = runs + 1 WHERE k = 10;
    UPDATE stats_2 SET runs = runs + 1 WHERE k = 11;
    UPDATE stats_0 SET runs = runs + 1 WHERE k = 12;
    UPDATE stats_1 SET runs = runs + 1 WHERE k = 13;
    UPDATE stats_2 SET runs = runs + 1 WHERE k = 14;
    UPDATE stats_0 SET runs = runs + 1 WHERE k = 15;
    UPDATE stats_1 SET runs = runs + 1 WHERE k = 16;
    UPDATE stats_2 SET runs = runs + 1 WHERE k = 17;
    UPDATE stats_0 SET runs = runs + 1 WHERE k = 18;
    UPDATE stats_1 SET runs = runs + 1 WHERE k = 19;
    UPDATE stats_2 SET runs = runs + 1 WHERE k = 20;
    UPDATE stats_0 SET runs = runs + 1 WHERE k = 21;
    UPDATE stats_1 SET runs = runs + 1 WHERE k = 22;
    UPDATE stats_2 SET runs = runs + 1 WHERE k = 23;
    UPDATE stats_0 SET runs = runs + 1 WHERE k = 24;
    UPDATE stats_1 SET runs = runs + 1 WHERE k = 25;
    UPDATE stats_2 SET runs = runs + 1 WHERE k = 26;
    UPDATE stats_0 SET runs = runs + 1 WHERE k = 27;
    UPDATE stats_1 SET runs = runs + 1 WHERE k = 28;
    UPDATE stats_2 SET runs = runs + 1 WHERE k = 29;
    UPDATE stats_0 SET runs = runs + 1 WHERE k = 30;
    UPDATE stats_1 SET runs = runs + 1 WHERE k = 31;
    UPDATE stats_2 SET runs = runs + 1 WHERE k = 32;
    UPDATE stats_0 SET runs = runs + 1 WHERE k = 33;
    UPDATE stats_1 SET runs = runs + 1 WHERE k = 34;
    UPDATE stats_2 SET runs = runs + 1 WHERE k = 35;
    UPDATE stats_0 SET runs = runs + 1 WHERE k = 36;
    UPDATE stats_1 SET runs = runs + 1 WHERE k = 37;
    UPDATE stats_2 SET runs = runs + 1 WHERE k = 38;
    UPDATE stats_0 SET runs = runs + 1 WHERE k = 39;
    UPDATE stats_1 SET runs = runs + 1 WHERE k = 40;
    UPDATE stats_2 SET runs = runs + 1 WHERE k = 41;
    UPDATE stats_0 SET runs = runs + 1 WHERE k = 42;
    UPDATE stats_1 SET runs = runs + 1 WHERE k = 43;
    UPDATE stats_2 SET runs = runs + 1 WHERE k = 44;
    UPDATE stats_0 SET runs = runs + 1 WHERE k = 45;
    UPDATE stats_1 SET runs = runs + 1 WHERE k = 46;
    UPDATE stats_2 SET runs = runs + 1 WHERE k = 47;
    UPDATE stats_0 SET runs = runs + 1 WHERE k = 48;
    UPDATE stats_1 SET runs = runs + 1 WHERE k = 49;
    UPDATE stats_2 SET runs = runs + 1 WHERE k = 50;
    UPDATE stats_0 SET runs = runs + 1 WHERE k = 51;
    UPDATE stats_1 SET runs = runs + 1 WHERE k = 52;
    UPDATE stats_2 SET runs = runs + 1 WHERE k = 53;
    UPDATE stats_0 SET runs = runs + 1 WHERE k = 54;
    UPDATE stats_1 SET runs = runs + 1 WHERE k = 55;
    UPDATE stats_2 SET runs = runs + 1 WHERE k = 56;
    UPDATE stats_0 SET runs = runs + 1 WHERE k = 57;
    UPDATE stats_1 SET runs = runs + 1 WHERE k = 58;
    UPDATE stats_2 SET runs = runs + 1 WHERE k = 59;
    dbms_output.put_line('done');
  END nightly;
END order_pkg;
