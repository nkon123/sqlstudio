// npm test — 그리드 이동 키 이름 (한글 입력 상태에서 key 가 "Process" 로 오는 경우)
import { navKeyName } from "./grid";

const cases: [string, string, string][] = [
  ["ArrowDown", "ArrowDown", "ArrowDown"],
  ["Process", "ArrowDown", "ArrowDown"],
  ["Process", "ArrowLeft", "ArrowLeft"],
  ["Unidentified", "PageDown", "PageDown"],
  ["Process", "Space", " "],
  [" ", "Space", " "],
  ["Process", "KeyA", "Process"],
  ["c", "KeyC", "c"],
];
for (const [key, code, want] of cases) {
  const got = navKeyName({ key, code });
  if (got !== want) throw new Error(`navKeyName(${key}, ${code}) = ${got}, 기대 ${want}`);
}
console.log(`  이동 키 ${cases.length}건`);
