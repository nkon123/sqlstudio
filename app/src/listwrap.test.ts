// npm test — SELECT 목록을 N 줄로 (Ctrl+Alt+L)
import { findList, wrapList } from "./listwrap";

let n = 0;
const eq = (got: unknown, want: unknown, what: string) => {
  if (JSON.stringify(got) !== JSON.stringify(want)) throw new Error(`${what}\n--- 나온 것\n${typeof got === "string" ? got : JSON.stringify(got)}\n--- 원하는 것\n${typeof want === "string" ? want : JSON.stringify(want)}`);
  n++;
};

// 목록 찾기 — SELECT 다음부터 FROM 앞까지, 괄호·문자열 안의 쉼표·FROM 은 아니다
const sql = "SELECT /*+ first_rows */ DISTINCT a.emp_no\n     , NVL(a.nm, 'x, FROM y') nm\n     , (SELECT MAX(s) FROM t) mx\n  FROM emp a";
const span = findList(sql);
if (typeof span === "string") throw new Error(span);
eq(span.items, ["a.emp_no", "NVL(a.nm, 'x, FROM y') nm", "(SELECT MAX(s) FROM t) mx"], "항목");
eq(sql.slice(span.from, span.to), "a.emp_no\n     , NVL(a.nm, 'x, FROM y') nm\n     , (SELECT MAX(s) FROM t) mx", "범위");
// SELECT 가 없으면 고른 글 전체가 목록
const bare = findList("a, b ,\n c");
eq(typeof bare === "string" ? bare : bare.items, ["a", "b", "c"], "목록만 고름");
// 줄 주석이 든 항목은 한 줄에 못 놓는다
eq(typeof findList("SELECT a -- 번호\n, b FROM t"), "string", "줄 주석");

// 나누기 — 줄마다 같은 수, 칸을 맞춘다 (앞 쉼표)
const items = ["a.emp_no", "a.emp_nm", "a.dept_cd", "a.sal", "a.comm", "a.hire_date", "a.x"];
eq(wrapList(items, 3, 7, true), [
  "a.emp_no, a.emp_nm, a.dept_cd",
  "     , a.sal,    a.comm,   a.hire_date",
  "     , a.x",
].join("\n"), "3줄 · 앞 쉼표");
// 뒤 쉼표
eq(wrapList(items.slice(0, 4), 2, 7, false), ["a.emp_no,  a.emp_nm,", "       a.dept_cd, a.sal"].join("\n"), "2줄 · 뒤 쉼표");
// 한글은 두 칸
eq(wrapList(["사번", "x", "a", "b"], 2, 7, true), ["사번, x", "     , a,    b"].join("\n"), "한글 폭");
// 줄 수는 1 ~ 항목 수
eq(wrapList(["a", "b"], 9, 7, true), "a\n     , b", "항목보다 많은 줄");
eq(wrapList(["a", "b"], 0, 7, true), "a, b", "0 줄은 1 줄");
console.log(`  목록 나누기 ${n}건`);
