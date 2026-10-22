// npm test — 포맷 결과가 원하는 모양인지, 같은 SQL 인지
import { detectPrefs, formatSql, sameSql, startsWithDml, DEFAULT_PREFS, type FormatPrefs } from "./format";

// 자동 포맷이 손댈 문장인지 — PL/SQL 조각·DDL 은 아니다
for (const [s, want] of [["select 1 from dual", true], ["  -- 주석\n  WITH a AS (select 1 from dual) select * from a", true], ["(select a from t) union select b from u", true],
  ["update t set a = 1", true], ["x := 1;", false], ["create table t (a number)", false], ["begin null; end;", false], ["selected := 1", false]] as [string, boolean][]) {
  if (startsWithDml(s) !== want) throw new Error(`startsWithDml(${JSON.stringify(s)}) 가 ${want} 가 아님`);
}

const river: FormatPrefs = { ...DEFAULT_PREFS };
let n = 0;
function eq(sql: string, want: string, prefs: FormatPrefs = river) {
  const r = formatSql(sql, prefs);
  if (!r.ok) throw new Error(`포맷 실패: ${r.reason}\n${sql}`);
  if (r.text !== want) throw new Error(`다름\n--- 원하는 것\n${want}\n--- 나온 것\n${r.text}`);
  if (!sameSql(sql, r.text)) throw new Error("다른 SQL");
  // 다시 포맷해도 같아야 한다
  const again = formatSql(r.text, prefs);
  if (!again.ok || again.text !== want) throw new Error(`두 번째 포맷이 다름\n${again.ok ? again.text : again.reason}`);
  n++;
}

// 사용자가 보여 준 모양
eq("select JOB, MGR, HIREDATE, SAL, COMM from emp where 1=1",
`SELECT JOB
     , MGR
     , HIREDATE
     , SAL
     , COMM
  FROM emp
 WHERE 1 = 1`);

// 조인·CASE·서브쿼리·GROUP BY·ORDER BY
eq("select o.order_id, c.cust_nm, sum(i.qty * i.price) total, case when o.status = 'C' then '취소' else '정상' end st from orders o join customers c on c.cust_id = o.cust_id left join order_items i on i.order_id = o.order_id and i.use_yn = 'Y' where o.order_dt >= trunc(sysdate) - 7 and c.grade in ('A', 'B') and exists (select 1 from payments p where p.order_id = o.order_id) group by o.order_id, c.cust_nm, o.status having sum(i.qty) > 0 order by total desc",
`SELECT o.order_id
     , c.cust_nm
     , SUM(i.qty * i.price) total
     , CASE WHEN o.status = 'C' THEN '취소'
            ELSE '정상'
       END st
  FROM orders o
  JOIN customers c
    ON c.cust_id = o.cust_id
  LEFT JOIN order_items i
    ON i.order_id = o.order_id
   AND i.use_yn = 'Y'
 WHERE o.order_dt >= TRUNC(SYSDATE) - 7
   AND c.grade IN ('A', 'B')
   AND EXISTS (SELECT 1
                 FROM payments p
                WHERE p.order_id = o.order_id)
 GROUP BY o.order_id
        , c.cust_nm
        , o.status
HAVING SUM(i.qty) > 0
 ORDER BY total DESC`);

// PIVOT — 집계는 목록처럼, FOR 는 첫 집계 칸, IN 의 값도 목록처럼 (한 줄로 늘어놓지 않는다)
eq("select * from (select job, deptno, sal from emp) pivot (sum(sal) as s, count(*) as n for deptno in (10 as d10, 20 as d20, 30 as d30)) p where p.job = 'CLERK' order by 1",
`SELECT *
  FROM (SELECT job
             , deptno
             , sal
          FROM emp)
 PIVOT (SUM(sal) AS s
      , COUNT(*) AS n
        FOR deptno IN (10 AS d10
                     , 20 AS d20
                     , 30 AS d30)) p
 WHERE p.job = 'CLERK'
 ORDER BY 1`);
eq("select * from (select deptno, sal, comm from emp) unpivot include nulls (amt for kind in (sal as 'S', comm as 'C'))",
`SELECT *
  FROM (SELECT deptno
             , sal
             , comm
          FROM emp)
 UNPIVOT INCLUDE NULLS (amt
                        FOR kind IN (sal AS 'S'
                                   , comm AS 'C'))`);
eq("select * from emp pivot xml (sum(sal) for deptno in (any))",
`SELECT *
  FROM emp
 PIVOT XML (SUM(sal)
            FOR deptno IN (ANY))`);
// MERGE — USING·ON·WHEN 을 절처럼 맞추고, 갈래 안의 UPDATE·INSERT 는 한 칸 들인 문장처럼
eq("merge into emp t using (select empno, sal from emp_new where deptno = 10) s on (t.empno = s.empno) when matched then update set t.sal = s.sal, t.upd_dt = sysdate where t.sal <> s.sal delete where t.sal = 0 when not matched then insert (empno, sal, ins_dt) values (s.empno, s.sal, sysdate) where s.sal > 0",
`MERGE INTO emp t
 USING (SELECT empno
             , sal
          FROM emp_new
         WHERE deptno = 10) s
    ON (t.empno = s.empno)
  WHEN MATCHED THEN
       UPDATE
          SET t.sal = s.sal
            , t.upd_dt = SYSDATE
        WHERE t.sal <> s.sal
       DELETE
        WHERE t.sal = 0
  WHEN NOT MATCHED THEN
       INSERT (empno, sal, ins_dt)
       VALUES (s.empno, s.sal, SYSDATE)
        WHERE s.sal > 0`);
// 괄호 없는 ON 은 조건처럼 AND 를 맞춘다
eq("merge into emp t using emp_new s on t.empno = s.empno and t.deptno = 10 when not matched then insert values (s.empno, s.sal)",
`MERGE INTO emp t
 USING emp_new s
    ON t.empno = s.empno
   AND t.deptno = 10
  WHEN NOT MATCHED THEN
       INSERT
       VALUES (s.empno, s.sal)`);

// 키워드 사이의 힌트 — 그대로 두고 절은 알아본다
eq("merge /*+ append */ into emp t using emp_new n on (t.empno = n.empno) when matched then update set t.sal = n.sal",
`MERGE /*+ append */ INTO emp t
 USING emp_new n
    ON (t.empno = n.empno)
  WHEN MATCHED THEN
       UPDATE
          SET t.sal = n.sal`);
eq("insert /*+ append */ into t (a) select a from s", "INSERT /*+ append */ INTO t (a)\nSELECT a\n  FROM s");
// INSERT ALL · FIRST — INTO 마다 한 줄, 조건 아래는 한 칸 들여
eq("insert first when x > 10 then into t1 values (x) else into t3 values (x) select x from src",
`INSERT FIRST
  WHEN x > 10 THEN
       INTO t1 VALUES (x)
  ELSE
       INTO t3 VALUES (x)
SELECT x
  FROM src`);
eq("insert all into t1 (a, b) values (x, y) into t2 (a) values (z) select x, y, z from src",
`INSERT ALL
  INTO t1 (a, b) VALUES (x, y)
  INTO t2 (a) VALUES (z)
SELECT x
     , y
     , z
  FROM src`);
// LOG ERRORS — 끝까지 한 줄
eq("insert into t (a) values (1) log errors into err$_t ('x') reject limit 10",
  "INSERT INTO t (a)\nVALUES (1)\n LOG ERRORS INTO err$_t ('x') REJECT LIMIT 10");
// CREATE TABLE·VIEW … AS — 머리 한 줄 + 쿼리
eq("create or replace view v as select a, b from t where c = 1",
`CREATE OR REPLACE VIEW v AS
SELECT a
     , b
  FROM t
 WHERE c = 1`);
eq("create table t2 (a, b) nologging as select x, y from t1;", "CREATE TABLE t2 (a, b) NOLOGGING AS\nSELECT x\n     , y\n  FROM t1;");
// 재귀 WITH 의 SEARCH … SET — SET 은 UPDATE 의 SET 이 아니다
eq("with r (n) as (select 1 from dual union all select n + 1 from r where n < 3) search depth first by n set ord select * from r",
`WITH r (n) AS (SELECT 1
                 FROM dual
               UNION ALL
               SELECT n + 1
                 FROM r
                WHERE n < 3)
     SEARCH DEPTH FIRST BY n SET ord
SELECT *
  FROM r`);
eq("select deptno, sum(sal) from emp group by grouping sets ((deptno), ())",
  "SELECT deptno\n     , SUM(sal)\n  FROM emp\n GROUP BY GROUPING SETS ((deptno), ())");

// PIVOT 이라는 이름의 열은 PIVOT 절로 보지 않는다
eq("select pivot from t", "SELECT PIVOT\n  FROM t");

// UPDATE · IN 서브쿼리
eq("update orders o set o.status = 'S', o.upd_dt = sysdate where o.order_id in (select x.order_id from tmp x where x.flag = 'Y')",
`UPDATE orders o
   SET o.status = 'S'
     , o.upd_dt = SYSDATE
 WHERE o.order_id IN (SELECT x.order_id
                        FROM tmp x
                       WHERE x.flag = 'Y')`);

// DELETE · BETWEEN 의 AND 는 나누지 않는다 · 음수
eq("delete from orders where order_dt between date '2026-01-01' and date '2026-01-31' and amt < -1",
`DELETE FROM orders
 WHERE order_dt BETWEEN DATE '2026-01-01' AND DATE '2026-01-31'
   AND amt < -1`);

// INSERT … SELECT · 함수 괄호 · 바인드
eq("insert into t (a, b) select :a, nvl(x, 0) from s where s.id = :id",
`INSERT INTO t (a, b)
SELECT :a
     , NVL(x, 0)
  FROM s
 WHERE s.id = :id`);

// WITH · UNION ALL · 인라인 뷰 · 분석 함수
eq("with s as (select cust_id, sum(amt) amt from orders group by cust_id) select * from (select s.*, row_number() over (partition by s.cust_id order by s.amt desc) rn from s) t where t.rn = 1 union all select null, 0, 0 from dual",
`WITH s AS (SELECT cust_id
                , SUM(amt) amt
             FROM orders
            GROUP BY cust_id)
SELECT *
  FROM (SELECT s.*
             , ROW_NUMBER() OVER (PARTITION BY s.cust_id ORDER BY s.amt DESC) rn
          FROM s) t
 WHERE t.rn = 1
UNION ALL
SELECT NULL
     , 0
     , 0
  FROM dual`);

// 주석 · 힌트 · 따옴표 이름 · 문장 둘
eq("select /*+ index(e ix_emp) */ e.\"Name\", -- 이름\n e.sal from emp e;\nselect 1 from dual;",
`SELECT /*+ index(e ix_emp) */ e."Name" -- 이름
     , e.sal
  FROM emp e;

SELECT 1
  FROM dual;`);

// 계층 쿼리 · OR · 쉼표 뒤 · AND 끝
eq("select level, name from dept start with parent_id is null connect by prior id = parent_id and use_yn = 'Y' or level < 3 order siblings by sort_no",
`SELECT LEVEL,
       name
  FROM dept
 START WITH parent_id IS NULL
 CONNECT BY PRIOR id = parent_id AND
       use_yn = 'Y' OR
       LEVEL < 3
 ORDER SIBLINGS BY sort_no`, { ...river, commaBefore: false, logicalBefore: false });

// 간결: 목록을 한 줄에
eq("select a, b, c, d from t where x = 1",
`SELECT a, b, c, d
  FROM t
 WHERE x = 1`, { ...river, style: "compact", commaBefore: false, expressionWidth: 100 });

// 소문자 키워드 · 이름 대문자
eq("SELECT a FROM t WHERE x = 1",
`select A
  from T
 where X = 1`, { ...river, keywordCase: "lower", identCase: "upper" });

// MERGE 는 오른쪽 맞춤이 다루지 않는다 → 왼쪽 정렬로 (실패하지 않는다)
const m = formatSql("merge into t using s on (t.id = s.id) when matched then update set t.a = s.a", river);
if (!m.ok) throw new Error("MERGE 포맷 실패");

// 예시로 맞추기
const d = detectPrefs("SELECT JOB\n     , MGR\n  FROM emp\n WHERE 1 = 1\n   AND x = 2");
const want = { style: "river", commaBefore: true, logicalBefore: true, keywordCase: "upper", identCase: "preserve" };
for (const [k, v] of Object.entries(want)) if ((d.prefs as Record<string, unknown>)[k] !== v) throw new Error(`예시 읽기: ${k} = ${(d.prefs as Record<string, unknown>)[k]}`);
const d2 = detectPrefs("select\n    job,\n    mgr\nfrom\n    emp\nwhere\n    a = 1 and\n    b = 2");
if (d2.prefs.style !== "left" || d2.prefs.indent !== 4 || d2.prefs.commaBefore !== false || d2.prefs.logicalBefore !== false || d2.prefs.keywordCase !== "lower") {
  throw new Error(`예시 읽기(왼쪽): ${JSON.stringify(d2.prefs)}`);
}

console.log(`  포맷 ${n}건`);
