PACKAGE BODY report_pkg IS
  -- 월별 고객 매출 보고서
  CURSOR c_report(p_month DATE) IS
    WITH active_cust AS (
         SELECT c.id, c.name, c.grade, c.region_id
           FROM customers c
          WHERE c.status = 'A'
            AND c.created_at < ADD_MONTHS(p_month, 1)
            AND NOT EXISTS (SELECT 1
                              FROM cust_block b
                             WHERE b.cust_id = c.id
                               AND b.until_dt >= p_month)
    ),
    monthly AS (
         SELECT o.cust_id
           , SUM(CASE WHEN o.status = 'S1' THEN o.amount ELSE 0 END) AS amt_s1
           , SUM(CASE WHEN o.status = 'S2' THEN o.amount ELSE 0 END) AS amt_s2
           , SUM(CASE WHEN o.status = 'S3' THEN o.amount ELSE 0 END) AS amt_s3
           , SUM(CASE WHEN o.status = 'S4' THEN o.amount ELSE 0 END) AS amt_s4
           , SUM(CASE WHEN o.status = 'S5' THEN o.amount ELSE 0 END) AS amt_s5
           , SUM(CASE WHEN o.status = 'S6' THEN o.amount ELSE 0 END) AS amt_s6
           , SUM(CASE WHEN o.status = 'S7' THEN o.amount ELSE 0 END) AS amt_s7
           , SUM(CASE WHEN o.status = 'S8' THEN o.amount ELSE 0 END) AS amt_s8
           , SUM(CASE WHEN o.status = 'S9' THEN o.amount ELSE 0 END) AS amt_s9
           , SUM(CASE WHEN o.status = 'S10' THEN o.amount ELSE 0 END) AS amt_s10
           , SUM(CASE WHEN o.status = 'S11' THEN o.amount ELSE 0 END) AS amt_s11
           , SUM(CASE WHEN o.status = 'S12' THEN o.amount ELSE 0 END) AS amt_s12
           , SUM(CASE WHEN o.status = 'S13' THEN o.amount ELSE 0 END) AS amt_s13
           , SUM(CASE WHEN o.status = 'S14' THEN o.amount ELSE 0 END) AS amt_s14
           , SUM(CASE WHEN o.status = 'S15' THEN o.amount ELSE 0 END) AS amt_s15
           , SUM(CASE WHEN o.status = 'S16' THEN o.amount ELSE 0 END) AS amt_s16
           , SUM(CASE WHEN o.status = 'S17' THEN o.amount ELSE 0 END) AS amt_s17
           , SUM(CASE WHEN o.status = 'S18' THEN o.amount ELSE 0 END) AS amt_s18
           , SUM(CASE WHEN o.status = 'S19' THEN o.amount ELSE 0 END) AS amt_s19
           , SUM(CASE WHEN o.status = 'S20' THEN o.amount ELSE 0 END) AS amt_s20
           , SUM(CASE WHEN o.status = 'S21' THEN o.amount ELSE 0 END) AS amt_s21
           , SUM(CASE WHEN o.status = 'S22' THEN o.amount ELSE 0 END) AS amt_s22
           , SUM(CASE WHEN o.status = 'S23' THEN o.amount ELSE 0 END) AS amt_s23
           , SUM(CASE WHEN o.status = 'S24' THEN o.amount ELSE 0 END) AS amt_s24
           , SUM(CASE WHEN o.status = 'S25' THEN o.amount ELSE 0 END) AS amt_s25
              , COUNT(*) AS cnt
           FROM orders o
          WHERE o.order_dt >= p_month
            AND o.order_dt < ADD_MONTHS(p_month, 1)
          GROUP BY o.cust_id
    ),
    ranked AS (
         SELECT m.cust_id,
                RANK() OVER (ORDER BY m.cnt DESC) AS rnk
           FROM monthly m
    )
    SELECT a.id
         , a.name
         , a.grade
         , g.region_name
         , NVL(m.amt_s1, 0) AS amt_s1
         , NVL(m.amt_s2, 0) AS amt_s2
         , NVL(m.amt_s3, 0) AS amt_s3
         , NVL(m.amt_s4, 0) AS amt_s4
         , NVL(m.amt_s5, 0) AS amt_s5
         , NVL(m.amt_s6, 0) AS amt_s6
         , NVL(m.amt_s7, 0) AS amt_s7
         , NVL(m.amt_s8, 0) AS amt_s8
         , NVL(m.amt_s9, 0) AS amt_s9
         , NVL(m.amt_s10, 0) AS amt_s10
         , NVL(m.amt_s11, 0) AS amt_s11
         , NVL(m.amt_s12, 0) AS amt_s12
         , NVL(m.amt_s13, 0) AS amt_s13
         , NVL(m.amt_s14, 0) AS amt_s14
         , NVL(m.amt_s15, 0) AS amt_s15
         , NVL(m.amt_s16, 0) AS amt_s16
         , NVL(m.amt_s17, 0) AS amt_s17
         , NVL(m.amt_s18, 0) AS amt_s18
         , NVL(m.amt_s19, 0) AS amt_s19
         , NVL(m.amt_s20, 0) AS amt_s20
         , NVL(m.amt_s21, 0) AS amt_s21
         , NVL(m.amt_s22, 0) AS amt_s22
         , NVL(m.amt_s23, 0) AS amt_s23
         , NVL(m.amt_s24, 0) AS amt_s24
         , NVL(m.amt_s25, 0) AS amt_s25
         , r.rnk
      FROM active_cust a
      JOIN monthly m ON m.cust_id = a.id
      JOIN ranked r ON r.cust_id = a.id
      LEFT JOIN regions g ON g.id = a.region_id
     WHERE r.rnk <= 1000
       AND (a.grade IN ('VIP', 'GOLD')
            OR EXISTS (SELECT 1
                         FROM promotions p
                        WHERE p.cust_id = a.id
                          AND p.month = p_month))
     ORDER BY r.rnk;

  PROCEDURE build(p_month DATE) IS
    v_cnt NUMBER := 0;
  BEGIN
    DELETE FROM report_monthly WHERE month = p_month;
    FOR r IN c_report(p_month) LOOP
      INSERT INTO report_monthly (month, cust_id, cust_name, grade, region, amt_s1, amt_s2, rnk)
      VALUES (p_month, r.id, r.name, r.grade, r.region_name, r.amt_s1, r.amt_s2, r.rnk);
      v_cnt := v_cnt + 1;
      IF MOD(v_cnt, 1000) = 0 THEN
        COMMIT;
      END IF;
    END LOOP;
    UPDATE report_runs SET rows_done = v_cnt, done_at = SYSDATE WHERE month = p_month;
    COMMIT;
  END build;
END report_pkg;
