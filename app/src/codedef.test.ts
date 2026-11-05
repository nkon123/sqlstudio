// npm test — 정의로 이동 (문서 안)
import { EditorState } from "@codemirror/state";
import { localDefinition, nameAt } from "./codedef";
import { PROC_SAMPLE } from "./mock/plsql";
import { oracleSql } from "./oracle-dialect";
import { procLanguage } from "./proc-lang";

let n = 0;
function eq(got: unknown, want: unknown, what: string) {
  if (JSON.stringify(got) !== JSON.stringify(want)) throw new Error(`${what}\n  got  ${JSON.stringify(got)}\n  want ${JSON.stringify(want)}`);
  n++;
}
const SRC = `CREATE OR REPLACE PACKAGE pkg_a AS
  PROCEDURE p_run(x NUMBER);
END pkg_a;
/
CREATE OR REPLACE PACKAGE BODY pkg_a AS
  CURSOR c_emp IS SELECT empno FROM emp;
  FUNCTION fn_calc(a NUMBER) RETURN NUMBER IS BEGIN RETURN a * 2; END fn_calc;
  PROCEDURE p_run(x NUMBER) IS
  BEGIN
    FOR r IN c_emp LOOP
      DBMS_OUTPUT.PUT_LINE(fn_calc(r.empno) || 'fn_calc'); -- fn_calc
    END LOOP;
    pkg_a.p_run(1);
    pkg_b.p_run(1);
  END p_run;
END pkg_a;
/`;
const st = EditorState.create({ doc: SRC, extensions: [oracleSql()] });
const at = (s: string, k = 0, off = 1) => { let p = -1; for (let i = 0; i <= k; i++) p = SRC.indexOf(s, p + 1); return p + off; };
const lineOf = (p: number | null) => (p == null ? null : st.doc.lineAt(p).text.trim().slice(0, 24));

eq(nameAt(st, at("pkg_a.p_run(1)", 0, 8))?.parts, ["pkg_a", "p_run"], "a.b 통째로");
eq(nameAt(st, at("'fn_calc'")), null, "문자열 안은 이름이 아니다");
eq(nameAt(st, at("-- fn_calc", 0, 4)), null, "주석 안도");
eq(nameAt(st, at("SELECT")), null, "키워드도");
eq(lineOf(localDefinition(st, at("fn_calc(r.empno)"))), "FUNCTION fn_calc(a NUMBE", "부른 곳 → 함수 본문");
eq(lineOf(localDefinition(st, at("IN c_emp", 0, 4))), "CURSOR c_emp IS SELECT e", "커서");
eq(lineOf(localDefinition(st, at("pkg_a.p_run(1)", 0, 8))), "PROCEDURE p_run(x NUMBER", "pkg.proc — 이 문서의 패키지면 본문으로");
eq(localDefinition(st, at("pkg_b.p_run(1)", 0, 8)), null, "다른 패키지는 DB 에서");
eq(lineOf(localDefinition(st, at("PROCEDURE p_run(x NUMBER) IS", 0, 12))), "PROCEDURE p_run(x NUMBER", "본문 머리 → 명세의 선언");
eq(st.doc.lineAt(localDefinition(st, at("PROCEDURE p_run(x NUMBER) IS", 0, 12))!).number, 2, "명세는 2행");
eq(st.doc.lineAt(localDefinition(st, at("PROCEDURE p_run(x NUMBER);", 0, 12))!).number, 8, "선언 → 본문");
eq(localDefinition(st, at("fn_calc(a NUMBER)", 0, 2)), null, "정의가 하나뿐이면 제자리");

// Pro*C — C 함수 · 커서 · 호스트 변수 · C 변수
{
  const P = PROC_SAMPLE;
  const ps = EditorState.create({ doc: P, extensions: [procLanguage()] });
  const at2 = (s: string, k = 0, off = 1) => { let p = -1; for (let i = 0; i <= k; i++) p = P.indexOf(s, p + 1); return p + off; };
  const ln = (p: number | null) => (p == null ? null : ps.doc.lineAt(p).text.trim().slice(0, 30).trim());
  eq(ln(localDefinition(ps, at2("settle_one(v_order_id"))), "static void settle_one(int ord", "C 함수 부른 곳 → 정의");
  eq(ln(localDefinition(ps, at2("OPEN c_ord", 0, 6))), "EXEC SQL DECLARE c_ord CURSOR", "OPEN 커서 → DECLARE CURSOR");
  eq(ln(localDefinition(ps, at2(":v_amount;", 0, 2))), "double   v_amount;", "호스트 변수 → DECLARE SECTION");
  eq(ln(localDefinition(ps, at2("n++"))), "int n = 0;", "C 지역 변수 → 함수 안의 선언");
  eq(localDefinition(ps, at2("FROM orders o", 0, 6)), null, "EXEC SQL 안의 테이블은 DB 에서");
  eq(nameAt(ps, at2("\"connect failed", 0, 3)), null, "C 문자열 안은 이름이 아니다");
}

console.log(`  정의로 이동 ${n}건`);
