PACKAGE BODY flow_pkg IS
  CURSOR c_open IS SELECT id, amount FROM orders WHERE status = 'OPEN';
  PROCEDURE run IS
    TYPE t_ids IS TABLE OF NUMBER;
    v_ids t_ids;
    v_amt NUMBER;
    CURSOR c_lines(p NUMBER) IS SELECT l.qty FROM order_lines l JOIN products p ON p.id = l.product_id WHERE l.order_id = p;
    CURSOR c_lock IS SELECT id FROM accounts FOR UPDATE;
    v_lock c_lock%ROWTYPE;
  BEGIN
    FOR r IN c_open LOOP
      INSERT INTO order_log (id, amt) VALUES (r.id, r.amount);
      DELETE FROM order_tmp WHERE ts < SYSDATE;
    END LOOP;
    OPEN c_lines(10);
    FETCH c_lines BULK COLLECT INTO v_ids LIMIT 100;
    CLOSE c_lines;
    FORALL i IN 1 .. v_ids.COUNT
      UPDATE stock SET qty = qty - 1 WHERE id = v_ids(i);
    FOR x IN (SELECT id, name FROM customers WHERE vip = 'Y') LOOP
      MERGE INTO vip_sum s USING (SELECT x.id id FROM dual) d ON (s.id = d.id)
        WHEN MATCHED THEN UPDATE SET s.cnt = s.cnt + 1
        WHEN NOT MATCHED THEN INSERT (id, cnt) VALUES (d.id, 1);
    END LOOP;
    SELECT SUM(amount) INTO v_amt FROM orders WHERE status = 'CLOSED';
    INSERT INTO daily (dt, amt) VALUES (TRUNC(SYSDATE), v_amt);
    OPEN c_lock;
    LOOP
      FETCH c_lock INTO v_lock;
      EXIT WHEN c_lock%NOTFOUND;
      UPDATE accounts SET checked = 'Y' WHERE CURRENT OF c_lock;
    END LOOP;
    CLOSE c_lock;
    EXECUTE IMMEDIATE 'INSERT INTO audit_x (a) VALUES (1)';
    FOR i IN 1 .. 10 LOOP
      NULL;
    END LOOP;
  END run;
END flow_pkg;
