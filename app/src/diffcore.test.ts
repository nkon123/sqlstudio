// npm test — 결과 비교: 키 열 자동 선택(기본 키 → 겹치지 않는 열), 자동 비교를 할 결과인지
import { diffResults, initialKey, quickDiff, type Snapshot } from "./diffcore";

let n = 0;
const eq = (got: unknown, want: unknown, what: string) => {
  if (JSON.stringify(got) !== JSON.stringify(want)) throw new Error(`${what}\n--- 나온 것\n${JSON.stringify(got)}\n--- 원하는 것\n${JSON.stringify(want)}`);
  n++;
};
const snap = (cols: string[], rows: (string | null)[][], pk?: string[]): Snapshot =>
  ({ label: "x", columns: cols.map((name) => ({ name, type_name: "VARCHAR2" })), rows, partial: false, pk });

// 첫 열(DEPT)이 겹쳐도 기본 키(EMP_NO)로 맞춘다 — 값이 바뀐 행은 '변경'
const a = snap(["DEPT", "EMP_NO", "SAL"], [["10", "1", "100"], ["10", "2", "200"], ["20", "3", "300"]], ["EMP_NO"]);
const b = snap(["DEPT", "EMP_NO", "SAL"], [["10", "1", "100"], ["10", "2", "250"], ["30", "4", "400"]], ["emp_no"]);
eq(initialKey(a, b), ["EMP_NO"], "기본 키");
const d = diffResults(a, b, initialKey(a, b));
eq(d.counts, { chg: 1, add: 1, del: 1, same: 1 }, "변경 · 추가 · 삭제 · 같음");
eq(d.rows.find((r) => r.status === "chg")!.changed, [2], "바뀐 열은 SAL");
// 기본 키를 모르면 겹치지 않는 앞쪽 열 (DEPT 는 겹치므로 DEPT+EMP_NO)
eq(initialKey({ ...a, pk: undefined }, { ...b, pk: undefined }), ["DEPT", "EMP_NO"], "겹치지 않는 열");
// 기본 키가 결과에 없거나 겹치면 쓰지 않는다
eq(initialKey(snap(["SAL"], [["1"], ["2"]], ["EMP_NO"]), snap(["SAL"], [["1"], ["3"]])), ["SAL"], "기본 키가 결과에 없음");

// 자동 비교 — 같은 모양일 때만
eq(quickDiff(a, b)?.counts.chg, 1, "같은 모양");
eq(quickDiff(a, snap(["DNAME", "LOC", "X", "EMP_NO"], [["a", "b", "c", "1"]])), null, "공통 열이 절반 미만이면 하지 않는다");
eq(quickDiff(a, snap(["Q"], [])), null, "공통 열 없음");

console.log(`  결과 비교 ${n}건`);
