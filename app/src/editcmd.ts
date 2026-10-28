// Toad 의 EDIT 명령 — `EDIT 테이블 [WHERE …] [ORDER BY …]` 를 실행하면 그 테이블을 편집 그리드로 연다.
// 서버로 보내지 않고 SELECT * FROM … 으로 바꿔 결과 편집(편집 버튼)과 같은 길로 간다.

const NAME = String.raw`(?:"[^"]+"|[A-Za-z_][\w$#]*)`;
const EDIT = new RegExp(String.raw`^\s*EDIT\s+(${NAME})(?:\s*\.\s*(${NAME}))?(?=\s|;|$)([\s\S]*?)\s*;?\s*$`, "i");

/** EDIT 명령이면 같은 행을 읽는 SELECT, 아니면 null */
export function editToSelect(text: string): string | null {
  const m = EDIT.exec(text);
  if (!m) return null;
  const table = m[2] ? `${m[1]}.${m[2]}` : m[1];
  const rest = m[3].trim();
  return `SELECT * FROM ${table}${rest ? ` ${rest}` : ""}`;
}
