// npm test — 자동완성 퀵 액션 (구절·타입 → 권할 꼴)
import { clauseBefore, clauseOf, quickActions, typeClass } from "./quickact";

const eq = (got: unknown, want: unknown, what: string) => {
  if (JSON.stringify(got) !== JSON.stringify(want)) throw new Error(`${what}: ${JSON.stringify(got)}, 기대 ${JSON.stringify(want)}`);
};
let n = 0;
const t = (got: unknown, want: unknown, what: string) => { eq(got, want, what); n++; };

// 구절
t(clauseOf("expr(where)", ""), "cond", "백엔드 WHERE");
t(clauseOf("join_on", ""), "cond", "백엔드 ON");
t(clauseOf("expr(set)", ""), "set", "백엔드 SET");
t(clauseOf("expr(group by)", "select * from emp where "), null, "GROUP BY 는 없음");
t(clauseOf("qualified(e)", "select * from emp e where e.sal > 1 and e."), "cond", "e. 뒤는 앞 글로");
t(clauseOf("", "select e."), "select", "가짜 모드(구절 없음)");
t(clauseBefore("select * from emp order by "), "order", "ORDER BY");
t(clauseBefore("select * from emp where ename = 'a and b' or "), "cond", "문자열 안 키워드 무시");
t(clauseBefore("select * from emp e join dept d on d."), "cond", "JOIN ON");
t(clauseBefore("select * from "), null, "FROM 은 없음");
t(clauseBefore("update emp set "), "set", "UPDATE SET");

// 타입
t(typeClass("NUMBER(4)"), "num", "NUMBER");
t(typeClass("EMP · VARCHAR2(10)"), "text", "가짜 모드 설명");
t(typeClass("DATE · E"), "date", "타입 · 별칭");
t(typeClass("TIMESTAMP(6)"), "date", "TIMESTAMP");
t(typeClass("E"), "other", "타입 없음");
t(typeClass(undefined), "other", "설명 없음");

// 꼴
const labels = (c: Parameters<typeof quickActions>[0], ty: Parameters<typeof quickActions>[1]) => quickActions(c, ty, "EMPNO").map((a) => a.label);
t(labels("cond", "num").slice(0, 2), ["EMPNO = ", "EMPNO IN ()"], "숫자 조건");
t(quickActions("cond", "text", "ENAME")[0].template, "ENAME = '${1}'", "문자 조건 템플릿");
t(labels("cond", "text").slice(-2), ["EMPNO IS NULL", "EMPNO IS NOT NULL"], "공통 IS NULL");
t(quickActions("cond", "date", "HIREDATE")[0].template, "HIREDATE = TO_DATE('${1}', 'YYYY-MM-DD')", "날짜 조건");
t(labels("set", "num"), ["EMPNO = ", "EMPNO = NULL"], "SET");
t(labels("order", "num")[0], "EMPNO DESC", "ORDER BY");
t(quickActions(null, "num", "EMPNO"), [], "구절 모르면 없음");
console.log(`  퀵 액션 ${n}건`);
