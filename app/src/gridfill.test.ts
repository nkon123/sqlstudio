// npm test — 그리드 붙여넣기·아래로 채우기 계산
import { parseTsv, planFillDown, planPaste } from "./gridfill";

const eq = (got: unknown, want: unknown, what: string) => {
  const g = JSON.stringify(got), w = JSON.stringify(want);
  if (g !== w) throw new Error(`${what}\n  결과 ${g}\n  기대 ${w}`);
};

// ── TSV 읽기 (엑셀·그리드 복사)
eq(parseTsv("a"), [["a"]], "값 하나");
eq(parseTsv("a\n"), [["a"]], "끝 줄바꿈 하나는 버린다");
eq(parseTsv(""), [[""]], "빈 글");
eq(parseTsv("a\tb\r\nc\td\r\n"), [["a", "b"], ["c", "d"]], "CRLF 표");
eq(parseTsv("a\t\tc"), [["a", "", "c"]], "빈 칸");
eq(parseTsv('"x\ty"\t"줄\n바꿈"\t"따""옴표"'), [["x\ty", "줄\n바꿈", '따"옴표']], "따옴표 칸");
eq(parseTsv('"반쪽 따옴표 abc'), [['"반쪽 따옴표 abc']], "닫히지 않은 따옴표는 글자 그대로");
eq(parseTsv('"ab"cd\tx'), [['"ab"cd', "x"]], "따옴표 뒤에 글자가 붙으면 글자 그대로");
eq(parseTsv("a\n\nb"), [["a"], [""], ["b"]], "빈 줄");

// ── 붙여넣기 계획 (r: 화면 행, k: 보이는 열 순번)
const keys = (p: { puts: { r: number; k: number; v: string }[] }) => p.puts.map((x) => `${x.r},${x.k}=${x.v}`);
eq(keys(planPaste([["7"]], { r0: 1, r1: 3, k0: 2, k1: 2 }, 10, 5)), ["1,2=7", "2,2=7", "3,2=7"], "값 하나 → 고른 칸 전부");
eq(keys(planPaste([["a", "b"], ["c", "d"]], { r0: 0, r1: 0, k0: 0, k1: 0 }, 10, 5)), ["0,0=a", "0,1=b", "1,0=c", "1,1=d"], "표 → 왼쪽 위부터");
eq(keys(planPaste([["a"], ["b"]], { r0: 0, r1: 3, k0: 1, k1: 1 }, 10, 5)), ["0,1=a", "1,1=b", "2,1=a", "3,1=b"], "배수면 되풀이");
eq(keys(planPaste([["a"], ["b"]], { r0: 0, r1: 2, k0: 1, k1: 1 }, 10, 5)), ["0,1=a", "1,1=b"], "배수가 아니면 한 번만");
eq(keys(planPaste([["a"], ["b"]], { r0: 3, r1: 0, k0: 1, k1: 1 }, 10, 5)), ["0,1=a", "1,1=b", "2,1=a", "3,1=b"], "거꾸로 고른 사각형");
const cut = planPaste([["a", "b", "c"], ["d", "e", "f"], ["g", "h", "i"]], { r0: 8, r1: 8, k0: 3, k1: 3 }, 10, 5);
eq(keys(cut), ["8,3=a", "8,4=b", "9,3=d", "9,4=e"], "끝을 넘는 칸은 자른다");
eq([cut.cutRows, cut.cutCols], [1, 1], "자른 행·열 수");
eq(keys(planPaste([["x"]], { r0: 9, r1: 12, k0: 0, k1: 0 }, 10, 5)), ["9,0=x"], "값 하나도 끝 너머는 넣지 않는다");

// ── 아래로 채우기
const grid: Record<string, string> = { "0,0": "A0", "1,0": "A1", "2,0": "A2", "0,1": "B0", "1,1": "B1", "3,1": "B3" };
const get = (r: number, c: number) => grid[`${r},${c}`] ?? null;
const fk = (xs: { r: number; c: number; v: string | null }[]) => xs.map((x) => `${x.r},${x.c}=${x.v}`).sort();
eq(fk(planFillDown([{ r: 0, c: 0 }, { r: 1, c: 0 }, { r: 2, c: 0 }], get)), ["1,0=A0", "2,0=A0"], "한 열 아래로");
eq(fk(planFillDown([{ r: 1, c: 0 }, { r: 2, c: 0 }, { r: 1, c: 1 }, { r: 3, c: 1 }], get)), ["2,0=A1", "3,1=B1"], "열마다 맨 위 값");
eq(fk(planFillDown([{ r: 2, c: 0 }, { r: 2, c: 1 }], get)), ["2,0=A1", "2,1=B1"], "한 행만 골랐으면 바로 위 행 값");
eq(fk(planFillDown([{ r: 0, c: 0 }], get)), [], "맨 위 한 칸은 채울 것이 없다");

console.log("  붙여넣기·채우기 계산 통과");
