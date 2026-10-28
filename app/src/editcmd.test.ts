// npm test — Toad 의 EDIT 명령을 SELECT 로
import { editToSelect } from "./editcmd";

let n = 0;
for (const [src, want] of [
  ["EDIT emp", "SELECT * FROM emp"],
  ["edit scott.emp where deptno = 10;", "SELECT * FROM scott.emp where deptno = 10"],
  ["  EDIT \"My Tab\"\n where a = :x\n order by 1", "SELECT * FROM \"My Tab\" where a = :x\n order by 1"],
  ["EDIT scott . emp", "SELECT * FROM scott.emp"],
  ["EDITION_NAME", null],
  ["edited := 1", null],
  ["EDIT", null],
  ["select * from emp", null],
] as [string, string | null][]) {
  const got = editToSelect(src);
  if (got !== want) throw new Error(`editToSelect(${JSON.stringify(src)})\n원하는 것: ${want}\n나온 것: ${got}`);
  n++;
}
console.log(`  EDIT 명령 ${n}건`);
