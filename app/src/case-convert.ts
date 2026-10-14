// 대문자·소문자 바꾸기 — 구문(키워드·이름)만 바꾸고 값은 그대로 둔다.
//
// 바꾸지 않는 것 (값·이름표):
//   '문자열'  q'[ … ]'  nq'{ … }'      — 문자열 값
//   "따옴표 이름"                       — 대소문자를 구분하는 이름
//   -- 주석   /* 주석·힌트 */
//   :바인드  :1  &치환변수  &&치환변수   — 바인드 이름은 화면의 바인드 칸과 맞아야 한다
//   123  1.5e10  0x1F                   — 숫자
// 바꾸는 것: SELECT · FROM · 테이블·컬럼 이름 · 함수 이름 같은 따옴표 없는 낱말.
// 따옴표 없는 이름은 Oracle 이 어차피 대문자로 읽으므로 바꿔도 뜻이 같다.
//
// 선택이 문자열 한가운데에서 시작해도 맞도록 문서 전체를 훑어 낱말 자리를 찾고, 선택과 겹치는 낱말만 바꾼다.

const isWordStart = (c: string) => /[\p{L}_]/u.test(c);
const isWordChar = (c: string) => /[\p{L}\p{N}_$#]/u.test(c);
const isDigit = (c: string) => c >= "0" && c <= "9";

/** 'abc' 뒤의 위치 — '' 는 따옴표 하나 */
function skipString(s: string, i: number): number {
  let j = i + 1;
  while (j < s.length) {
    if (s[j] === "'") {
      if (s[j + 1] === "'") { j += 2; continue; }
      return j + 1;
    }
    j++;
  }
  return s.length;
}

/** q'[ … ]' 뒤의 위치. i 는 여는 ' 의 자리 */
function skipQQuote(s: string, i: number): number {
  const open = s[i + 1];
  if (open === undefined) return s.length;
  const close = ({ "[": "]", "(": ")", "{": "}", "<": ">" } as Record<string, string>)[open] ?? open;
  const end = s.indexOf(close + "'", i + 2);
  return end < 0 ? s.length : end + 2;
}

/** 바꿀 수 있는 낱말의 [시작, 끝) 목록 */
export function wordSpans(s: string): [number, number][] {
  const out: [number, number][] = [];
  const n = s.length;
  let i = 0;
  while (i < n) {
    const c = s[i];
    if (c === "-" && s[i + 1] === "-") {
      const j = s.indexOf("\n", i);
      i = j < 0 ? n : j;
    } else if (c === "/" && s[i + 1] === "*") {
      const j = s.indexOf("*/", i + 2);
      i = j < 0 ? n : j + 2;
    } else if (c === "'") {
      i = skipString(s, i);
    } else if (c === '"') {
      const j = s.indexOf('"', i + 1);
      i = j < 0 ? n : j + 1;
    } else if (c === ":" && s[i + 1] !== undefined && (isWordChar(s[i + 1]))) {
      // :name  :1 — := 는 바인드가 아니다 (다음이 = 라 여기에 오지 않는다)
      i += 2;
      while (i < n && isWordChar(s[i])) i++;
    } else if (c === "&") {
      i++;
      if (s[i] === "&") i++;
      while (i < n && isWordChar(s[i])) i++;
    } else if (isDigit(c)) {
      // 숫자 (1.5e10, 0x1F) — 이름이 숫자로 시작할 수는 없다
      i++;
      while (i < n && (isWordChar(s[i]) || s[i] === ".")) i++;
    } else if (isWordStart(c)) {
      const start = i;
      i++;
      while (i < n && isWordChar(s[i])) i++;
      const w = s.slice(start, i).toLowerCase();
      if ((w === "q" || w === "nq") && s[i] === "'") {
        i = skipQQuote(s, i); // q'[ … ]' 는 값
        continue;
      }
      out.push([start, i]);
    } else {
      i++;
    }
  }
  return out;
}

export type CaseMode = "upper" | "lower";

/** doc 의 [from, to) 를 바꾼 글 (구문만) */
export function convertRange(doc: string, from: number, to: number, mode: CaseMode): string {
  const conv = (t: string) => (mode === "upper" ? t.toUpperCase() : t.toLowerCase());
  let out = "";
  let pos = from;
  for (const [s, e] of wordSpans(doc)) {
    if (e <= from) continue;
    if (s >= to) break;
    const a = Math.max(s, from);
    const b = Math.min(e, to);
    out += doc.slice(pos, a) + conv(doc.slice(a, b));
    pos = b;
  }
  return out + doc.slice(pos, to);
}
