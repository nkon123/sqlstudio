// 그리드에 여러 칸을 한 번에 넣는 계산 — 붙여넣기(Ctrl+V)·아래로 채우기(Ctrl+D).
// DOM 없이 칸 좌표만 다룬다 (grid.ts 가 화면 행·보이는 열 순번으로 부른다).

/** 클립보드 글(엑셀·그리드 복사의 TSV)을 칸으로. 따옴표로 싼 칸은 탭·줄바꿈·"" 를 품을 수 있다. 끝의 줄바꿈 하나는 버린다 */
export function parseTsv(text: string): string[][] {
  const s = text.replace(/\r\n?/g, "\n");
  const rows: string[][] = [];
  let row: string[] = [];
  let i = 0;
  const n = s.endsWith("\n") ? s.length - 1 : s.length;
  if (n <= 0) return [[""]];
  for (;;) {
    let cell = "";
    if (s[i] === '"') {
      // 따옴표 칸 — 닫는 따옴표 뒤가 탭·줄바꿈·끝이 아니면 엑셀이 만든 칸이 아니므로 글자 그대로
      let j = i + 1;
      let v = "";
      let closed = false;
      while (j < n) {
        if (s[j] === '"') {
          if (s[j + 1] === '"') { v += '"'; j += 2; continue; }
          closed = true;
          j++;
          break;
        }
        v += s[j++];
      }
      if (closed && (j >= n || s[j] === "\t" || s[j] === "\n")) {
        cell = v;
        i = j;
      } else {
        const end = nextSep(s, i, n);
        cell = s.slice(i, end);
        i = end;
      }
    } else {
      const end = nextSep(s, i, n);
      cell = s.slice(i, end);
      i = end;
    }
    row.push(cell);
    if (i >= n) break;
    if (s[i] === "\n") { rows.push(row); row = []; }
    i++;
  }
  rows.push(row);
  return rows;
}

function nextSep(s: string, i: number, n: number) {
  while (i < n && s[i] !== "\t" && s[i] !== "\n") i++;
  return i;
}

/** 넣을 칸 하나 — r: 화면 행, k: 보이는 열 순번 */
export interface Put { r: number; k: number; v: string }

export interface PastePlan {
  puts: Put[];
  /** 아래·오른쪽 끝을 넘어 못 넣은 클립보드 행·열 수 */
  cutRows: number;
  cutCols: number;
}

/** 고른 사각형(화면 행 r0..r1, 보이는 열 k0..k1)에 붙일 칸.
 *  - 값이 하나면 고른 칸 전부에
 *  - 고른 사각형이 클립보드 크기의 배수면 되풀이해 채운다 (엑셀처럼)
 *  - 아니면 왼쪽 위부터 클립보드 모양대로 — 끝을 넘는 것은 잘라 알린다 */
export function planPaste(clip: string[][], box: { r0: number; r1: number; k0: number; k1: number }, nRows: number, nCols: number): PastePlan {
  const h = clip.length;
  const w = Math.max(1, ...clip.map((r) => r.length));
  const at = (i: number, j: number) => clip[i]?.[j] ?? "";
  const r0 = Math.min(box.r0, box.r1), r1 = Math.max(box.r0, box.r1);
  const k0 = Math.min(box.k0, box.k1), k1 = Math.max(box.k0, box.k1);
  const bh = r1 - r0 + 1, bw = k1 - k0 + 1;
  const puts: Put[] = [];
  if ((h === 1 && w === 1) || ((bh > h || bw > w) && bh % h === 0 && bw % w === 0)) {
    for (let r = r0; r <= r1 && r < nRows; r++) for (let k = k0; k <= k1 && k < nCols; k++) puts.push({ r, k, v: at((r - r0) % h, (k - k0) % w) });
    return { puts, cutRows: 0, cutCols: 0 };
  }
  for (let i = 0; i < h && r0 + i < nRows; i++) for (let j = 0; j < w && k0 + j < nCols; j++) puts.push({ r: r0 + i, k: k0 + j, v: at(i, j) });
  return { puts, cutRows: Math.max(0, r0 + h - nRows), cutCols: Math.max(0, k0 + w - nCols) };
}

/** 아래로 채우기 — 열마다 맨 위 칸 값을 그 아래 고른 칸에. 한 행만 골랐으면 바로 위 행 값을 (엑셀 Ctrl+D).
 *  cells: 고른 칸 (화면 행·열 번호), get: 그 칸의 지금 값 */
export function planFillDown<V>(cells: { r: number; c: number }[], get: (r: number, c: number) => V): { r: number; c: number; v: V }[] {
  const byCol = new Map<number, number[]>();
  for (const x of cells) {
    const rs = byCol.get(x.c) ?? [];
    rs.push(x.r);
    byCol.set(x.c, rs);
  }
  const out: { r: number; c: number; v: V }[] = [];
  const oneRow = new Set(cells.map((x) => x.r)).size === 1;
  for (const [c, rs] of byCol) {
    rs.sort((a, b) => a - b);
    if (oneRow) {
      if (rs[0] > 0) out.push({ r: rs[0], c, v: get(rs[0] - 1, c) });
      continue;
    }
    const v = get(rs[0], c);
    for (const r of rs.slice(1)) out.push({ r, c, v });
  }
  return out;
}
