// npm test — SELECT 목록을 N 줄로 (Ctrl+Alt+L)
import { findList, findLists, wrapList } from "./listwrap";

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

// 커서만 있으면 그 자리의 SELECT 하나 — UNION ALL 이면 그 갈래, 서브쿼리 안이면 그 서브쿼리
const u = "SELECT a, b, c FROM t\nUNION ALL\nSELECT d, e, f FROM u WHERE x IN (SELECT y, z FROM w)\nUNION ALL\nSELECT 1, 2, 3 FROM dual";
const lists = (r: ReturnType<typeof findLists>) => typeof r === "string" ? r : r.lists.map((l) => l.items.join("|"));
eq(lists(findLists(u, 0)), ["a|b|c"], "커서가 첫 SELECT");
eq(lists(findLists(u, u.indexOf("e,"))), ["d|e|f"], "커서가 둘째 SELECT");
eq(lists(findLists(u, u.indexOf("WHERE"))), ["d|e|f"], "커서가 둘째 SELECT 의 WHERE");
eq(lists(findLists(u, u.length)), ["1|2|3"], "커서가 끝");
eq(lists(findLists(u, u.indexOf("y, z") + 1)), ["y|z"], "서브쿼리");
// FROM 없는 갈래 — 목록이 UNION 을 넘지 않는다
eq(lists(findLists("select 1, 2 union all select 3, 4", 0)), ["1|2"], "FROM 없음");
// 고른 영역(null) — 걸린 SELECT 목록 모두. WHERE 의 서브쿼리도, 항목 안의 스칼라 서브쿼리는 빼고
eq(lists(findLists(u, null)), ["a|b|c", "d|e|f", "y|z", "1|2|3"], "고른 영역 — 모두");
eq(lists(findLists("SELECT a, (SELECT MAX(s) FROM t) m FROM x UNION ALL SELECT b, c FROM y", null)), ["a|(SELECT MAX(s) FROM t) m", "b|c"], "스칼라 서브쿼리는 빼고");
eq(lists(findLists("select 1, 2 union all select 3, 4", null)), ["1|2", "3|4"], "FROM 없는 갈래 모두");
// 줄 주석이 든 갈래는 그 목록만 오류
const lc = findLists("SELECT a, b FROM t UNION SELECT c -- 메모\n, d FROM u", null);
eq(typeof lc === "string" ? lc : lc.lists.map((l) => !!l.error), [false, true], "줄 주석 갈래");
// SELECT 가 없으면 글 전체
eq(lists(findLists("a, b", null)), ["a|b"], "SELECT 없음");
console.log(`  목록 나누기 ${n}건`);
