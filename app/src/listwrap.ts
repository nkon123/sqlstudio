// SELECT 목록을 N 줄로 — 컬럼이 많을 때 표처럼 칸을 맞춰 나눈다 (Ctrl+Alt+L).
// 기본 포맷(한 줄에 하나)은 그대로 두고, 고른 목록만 손으로 다시 배치한다.
//
//   SELECT a.emp_no, a.emp_nm,  a.dept_cd
//        , a.sal,    a.comm,    a.hire_dt

import { vw } from "./sqlfmt";

/** 글 안의 목록 — [from, to) 를 바꾸면 된다. items 는 쉼표로 나눈 항목 (공백 정리) */
export interface ListSpan { from: number; to: number; items: string[] }

/** 문자열·따옴표 이름·주석을 건너 맨 바깥 글자만 보며 돈다. f 가 참을 돌려주면 멈춘다 */
function scan(text: string, f: (i: number, depth: number) => boolean | void): void {
  let depth = 0;
  for (let i = 0; i < text.length; i++) {
    const ch = text[i];
    const two = text.slice(i, i + 2);
    if (two === "--") { const e = text.indexOf("\n", i); i = e < 0 ? text.length : e; continue; }
    if (two === "/*") { const e = text.indexOf("*/", i + 2); i = e < 0 ? text.length : e + 1; continue; }
    if (/^[qQ]'/.test(two)) {
      // q'[ … ]'
      const open = text[i + 2];
      const close = ({ "[": "]", "(": ")", "{": "}", "<": ">" } as Record<string, string>)[open] ?? open;
      const e = text.indexOf(`${close}'`, i + 3);
      i = e < 0 ? text.length : e + 1;
      continue;
    }
    if (ch === "'") {
      let j = i + 1;
      while (j < text.length && !(text[j] === "'" && text[j + 1] !== "'")) j += text[j] === "'" ? 2 : 1;
      i = j;
      continue;
    }
    if (ch === '"') { const e = text.indexOf('"', i + 1); i = e < 0 ? text.length : e; continue; }
    if (ch === "(") depth++;
    else if (ch === ")") depth--;
    else if (f(i, depth)) return;
  }
}

/** 맨 바깥 낱말이 그 자리에서 시작하는지 (앞뒤가 낱말 글자가 아니다) */
function wordAt(text: string, i: number, w: string): boolean {
  const before = text[i - 1];
  const after = text[i + w.length];
  return text.slice(i, i + w.length).toUpperCase() === w && !(before && /[\w$#]/.test(before)) && !(after && /[\w$#]/.test(after));
}

/** 항목 글을 한 줄로 — 문자열 밖의 공백·줄바꿈을 하나로 */
function oneLine(s: string): string {
  let out = "";
  let last = 0;
  // 문자열은 그대로 두고 그 밖만 접는다
  const re = /'(?:[^']|'')*'|"[^"]*"|\s+/g;
  for (let m; (m = re.exec(s)); ) {
    out += s.slice(last, m.index) + (/^\s/.test(m[0]) ? " " : m[0]);
    last = m.index + m[0].length;
  }
  return (out + s.slice(last)).trim();
}

/** 글 안의 맨 바깥 SELECT 목록 (SELECT [힌트] [DISTINCT] 다음부터 FROM·INTO 앞까지). SELECT 가 없으면 글 전체를 목록으로.
 *  줄 주석(--)이 든 항목이 있으면 한 줄에 둘 수 없어 오류 글을 돌려준다 */
export function findList(text: string): ListSpan | string {
  let sel = -1;
  scan(text, (i, d) => {
    if (d === 0 && wordAt(text, i, "SELECT")) { sel = i; return true; }
  });
  let from = 0, to = text.length;
  if (sel >= 0) {
    from = sel + 6;
    // 힌트·공백·DISTINCT 를 건너
    for (;;) {
      const rest = text.slice(from);
      const m = /^\s+|^\/\*[\s\S]*?\*\/|^(DISTINCT|UNIQUE|ALL)\b/i.exec(rest);
      if (!m) break;
      from += m[0].length;
    }
    scan(text.slice(from), (i, d) => {
      if (d === 0 && (wordAt(text, from + i, "FROM") || wordAt(text, from + i, "INTO"))) { to = from + i; return true; }
    });
  }
  // 앞뒤 공백은 목록 밖
  while (from < to && /\s/.test(text[from])) from++;
  while (to > from && /\s/.test(text[to - 1])) to--;
  const body = text.slice(from, to);
  const cuts: number[] = [];
  scan(body, (i, d) => { if (d === 0 && body[i] === ",") cuts.push(i); });
  const parts: string[] = [];
  let a = 0;
  for (const c of [...cuts, body.length]) { parts.push(body.slice(a, c)); a = c + 1; }
  if (parts.some((p) => /--/.test(p.replace(/'(?:[^']|'')*'/g, "")))) return "줄 주석(--)이 든 항목이 있어 한 줄에 놓을 수 없습니다 — 주석을 /* */ 로 바꾸거나 빼세요";
  const items = parts.map(oneLine);
  if (items.some((x) => !x)) return "빈 항목이 있습니다 (쉼표가 겹쳐 있나요?)";
  return { from, to, items };
}

/** 항목들을 rows 줄로 — 줄마다 같은 수씩, 칸을 맞춘다. startCol 은 첫 항목이 놓인 칸, 둘째 줄부터 그 칸에 맞춘다 */
export function wrapList(items: string[], rows: number, startCol: number, commaBefore: boolean): string {
  const n = items.length;
  rows = Math.max(1, Math.min(n, Math.round(rows) || 1));
  const per = Math.ceil(n / rows);
  const grid: string[][] = [];
  for (let i = 0; i < n; i += per) grid.push(items.slice(i, i + per));
  const widths = Array.from({ length: per }, (_, j) => Math.max(...grid.map((r) => (r[j] != null ? vw(r[j]) : 0))));
  return grid
    .map((row, r) => {
      const cells = row.map((it, j) => {
        if (j === row.length - 1) return it;
        const s = `${it},`;
        // 다음 칸이 같은 자리에서 시작하게 (쉼표 뒤 한 칸 + 모자란 만큼)
        return s + " ".repeat(widths[j] + 2 - vw(s));
      });
      const head = r === 0 ? "" : commaBefore ? `${" ".repeat(Math.max(0, startCol - 2))}, ` : " ".repeat(startCol);
      const tail = !commaBefore && r < grid.length - 1 ? "," : "";
      return head + cells.join("") + tail;
    })
    .join("\n");
}
