// npm test — MERGE 점검 쿼리
import { mergeCheck, parseMerge } from "./mergecheck";

let n = 0;
function check(ok: boolean, what: string, got?: unknown) {
  n++;
  if (!ok) throw new Error(`${what}${got === undefined ? "" : `\n--- 받은 것 ---\n${typeof got === "string" ? got : JSON.stringify(got, null, 2)}`}`);
}

// 서브쿼리 소스 · 두 키 · 소스만 거르는 조건 · 갈래 둘
const sql1 = `MERGE /*+ append */ INTO app.ord_sum t
USING (SELECT d.ord_no, d.line_no, SUM(d.amt) amt
         FROM ord_dtl d
        WHERE d.status = 'OK' -- 확정만
        GROUP BY d.ord_no, d.line_no) s
   ON (t.ord_no = s.ord_no AND t.line_no = s.line_no AND s.amt > 0)
 WHEN MATCHED THEN
   UPDATE SET t.tot_amt = s.amt, t.upd_dt = SYSDATE
 WHEN NOT MATCHED THEN
   INSERT (ord_no, line_no, tot_amt) VALUES (s.ord_no, s.line_no, s.amt)`;
const p1 = parseMerge(sql1)!;
check(p1.target === "app.ord_sum" && p1.targetAlias === "t" && p1.sourceAlias === "s", "대상·별칭", p1);
check(p1.source.startsWith("(SELECT") && p1.source.endsWith("d.line_no)"), "서브쿼리 소스", p1.source);
check(p1.on === "t.ord_no = s.ord_no AND t.line_no = s.line_no AND s.amt > 0", "ON", p1.on);
check(p1.hasUpdate && p1.hasInsert && p1.setCols.join() === "TOT_AMT,UPD_DT", "갈래·SET 열", p1);
const c1 = mergeCheck(sql1)!;
check(c1.dupMatch.includes("PARTITION BY t.ROWID") && c1.dupMatch.includes("s.*") && c1.dupMatch.includes("FROM app.ord_sum t,"), "중복 매칭 쿼리", c1.dupMatch);
check(c1.dupMatch.includes("WHERE t.ord_no = s.ord_no AND t.line_no = s.line_no AND s.amt > 0)"), "ON 을 WHERE 로", c1.dupMatch);
check(c1.script.includes("SELECT s.ord_no, s.line_no, COUNT(*) AS CNT__") && c1.script.includes(" WHERE s.amt > 0\n") && c1.script.includes("GROUP BY s.ord_no, s.line_no"), "소스 키 중복", c1.script);
check(!c1.script.includes("ORA-38104"), "ON 열을 고치지 않음", c1.script);

// 테이블 소스, 별칭 없음, 키가 오른쪽·왼쪽 섞임, ON 열을 SET 으로 고침
const sql2 = `merge into emp using emp_stage on (emp_stage.empno = emp.empno)
when matched then update set emp.empno = emp_stage.empno, sal = emp_stage.sal
when not matched then insert values (emp_stage.empno, emp_stage.sal)`;
const c2 = mergeCheck(sql2)!;
check(c2.dupMatch.includes("PARTITION BY emp.ROWID") && c2.dupMatch.includes("emp_stage.*"), "별칭 없으면 이름으로", c2.dupMatch);
check(c2.script.includes("SELECT emp_stage.empno, COUNT(*)"), "소스 쪽 키 (왼쪽에 있어도)", c2.script);
check(c2.script.includes("ORA-38104") && c2.script.includes("(EMPNO)"), "ON 열 고침 경고", c2.script);

// OR 이 있으면 키 쿼리는 만들지 않는다, 갈래가 UPDATE 뿐이면 2) 없음
const c3 = mergeCheck(`MERGE INTO a x USING b y ON (x.k = y.k OR x.k2 = y.k2) WHEN NOT MATCHED THEN INSERT (k) VALUES (y.k)`)!;
check(c3.script.includes("만들지 않았습니다"), "OR 이면 키 모름", c3.script);
const c4 = mergeCheck(`MERGE INTO a x USING b y ON (x.k = y.k) WHEN MATCHED THEN UPDATE SET x.v = y.v`)!;
check(!c4.script.includes("2)"), "UPDATE 갈래뿐", c4.script);

// 문자열 안의 ) 와 AND · BETWEEN 의 AND
const c5 = mergeCheck(`MERGE INTO a t USING (SELECT * FROM b WHERE c = 'x)y') s ON (t.k = s.k AND s.d BETWEEN 1 AND 9) WHEN NOT MATCHED THEN INSERT (k) VALUES (s.k)`)!;
check(c5.script.includes("SELECT s.k, COUNT(*)") && c5.script.includes("WHERE s.d BETWEEN 1 AND 9"), "문자열 괄호·BETWEEN", c5.script);

// MERGE 가 아니면 null
check(mergeCheck("SELECT * FROM dual") === null && mergeCheck("MERGE INTO a USING b") === null, "MERGE 아님·덜 씀");

console.log(`  MERGE 점검 ${n}건`);
