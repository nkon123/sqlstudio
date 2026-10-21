// 쓰는 동안 자동 포맷 — 켜고 끄는 것은 파일 메뉴 > 자동 포맷 (format.ts 의 autoFormat).
//
//   키워드 대소문자: 낱말을 다 치고 띄어쓰기·줄바꿈·괄호·쉼표 등을 치면 그 낱말이 키워드면 설정의 대소문자로
//   ; 를 치면:       줄 끝에서 ; 를 치면 그 문장(위의 빈 줄 또는 앞 ; 뒤부터)을 정리
//   붙여 넣으면:      줄 처음에 SQL 문장을 붙여 넣으면 정리해서 넣는다
// 정리는 Ctrl+Shift+F 와 같은 포맷(formatSql)이라, 같은 SQL 이 아니게 되면 손대지 않는다.

import { syntaxTree } from "@codemirror/language";
import { EditorState, type Extension } from "@codemirror/state";
import { EditorView } from "@codemirror/view";
import { autoFormat, formatSql, loadPrefs, startsWithDml } from "./format";
import { FUNCS, KEYWORDS } from "./sqlfmt";

/** 이름으로 흔히 쓰는 키워드 — 치는 동안에는 바꾸지 않는다 (컬럼 이름일 수 있다) */
const NAME_LIKE = new Set("YEAR MONTH DAY HOUR MINUTE SECOND FIRST LAST NEXT CURRENT ROW ROWS ONLY OF TO DEFAULT KEEP RANGE".split(" "));

/** 이 글자를 치면 앞 낱말이 끝난 것 */
const BOUNDARY = /^[\s,()+\-*/=<>|;]$/;

/** pos 바로 앞 낱말이 키워드면 설정의 대소문자로 바꾼다 (문자열·주석·따옴표 이름·별칭 뒤 컬럼·바인드는 그대로) */
export function caseWordBefore(view: EditorView, pos: number, next: string) {
  if (!autoFormat().caseOnType || view.state.selection.ranges.length > 1) return;
  const mode = loadPrefs().keywordCase;
  if (mode === "preserve") return;
  const line = view.state.doc.lineAt(pos);
  const before = line.text.slice(0, pos - line.from);
  const m = /[A-Za-z_][\w$#]*$/.exec(before);
  if (!m) return;
  const prevCh = before[before.length - m[0].length - 1] ?? "";
  if (prevCh === "." || prevCh === ":" || prevCh === "&" || prevCh === "@") return;
  const u = m[0].toUpperCase();
  if (!((KEYWORDS.has(u) && !NAME_LIKE.has(u)) || (next === "(" && FUNCS.has(u)))) return;
  const from = pos - m[0].length;
  if (/String|Comment|QuotedIdentifier/.test(syntaxTree(view.state).resolveInner(from, 1).name)) return;
  const want = mode === "upper" ? u : m[0].toLowerCase();
  if (want === m[0]) return;
  view.dispatch({ changes: { from, to: pos, insert: want }, userEvent: "input.case" });
}

/** 문자열·주석 밖의 마지막 ; 위치 (없으면 -1) */
function lastSemicolon(s: string): number {
  let last = -1;
  for (let i = 0; i < s.length; i++) {
    const c = s[i];
    if (c === "'" || c === '"') {
      const e = s.indexOf(c, i + 1);
      if (e < 0) return last;
      i = e;
    } else if (c === "-" && s[i + 1] === "-") {
      const e = s.indexOf("\n", i);
      if (e < 0) return last;
      i = e;
    } else if (c === "/" && s[i + 1] === "*") {
      const e = s.indexOf("*/", i + 2);
      if (e < 0) return last;
      i = e + 1;
    } else if (c === ";") last = i;
  }
  return last;
}

/** 줄 끝에서 ; 를 치면 — 그 문장을 ; 까지 정리해서 넣는다. 정리하지 않으면 false (평소대로 ; 만 들어간다) */
function formatOnSemicolon(view: EditorView, from: number, to: number): boolean {
  const st = view.state;
  if (from !== to || st.selection.ranges.length > 1) return false;
  const line = st.doc.lineAt(from);
  if (line.text.slice(from - line.from).trim() !== "") return false;
  // 위로 빈 줄까지, 그 안의 앞 ; 뒤부터
  let n = line.number;
  while (n > 1 && st.doc.line(n - 1).text.trim() !== "") n--;
  let start = st.doc.line(n).from;
  const region = st.sliceDoc(start, from);
  const semi = lastSemicolon(region);
  if (semi >= 0) start += semi + 1;
  while (start < from && /\s/.test(st.sliceDoc(start, start + 1))) start++;
  // 줄 처음에서 시작하는 문장만 (PL/SQL 안의 들여 쓴 문장 등은 그대로)
  if (st.doc.lineAt(start).from !== start) return false;
  const text = st.sliceDoc(start, from) + ";";
  if (!startsWithDml(text)) return false;
  const r = formatSql(text);
  if (!r.ok || r.text === text) return false;
  view.dispatch({ changes: { from: start, to, insert: r.text }, selection: { anchor: start + r.text.length }, userEvent: "input.format", scrollIntoView: true });
  return true;
}

/** 붙여 넣은 SQL 을 정리한다 — 줄 처음(들여쓰기 뒤)에 문장을 통째로 넣을 때만 */
const pasteFilter = EditorState.transactionFilter.of((tr) => {
  if (!tr.isUserEvent("input.paste") || !autoFormat().onPaste) return tr;
  let one: { from: number; to: number; text: string } | null = null;
  let count = 0;
  tr.changes.iterChanges((fromA, toA, _fb, _tb, ins) => { count++; one = { from: fromA, to: toA, text: ins.toString() }; });
  const c = one as { from: number; to: number; text: string } | null;
  if (count !== 1 || !c || c.text.length > 200_000 || !startsWithDml(c.text)) return tr;
  const line = tr.startState.doc.lineAt(c.from);
  const lead = line.text.slice(0, c.from - line.from);
  if (lead.trim() !== "") return tr;
  const r = formatSql(c.text);
  if (!r.ok || r.text === c.text) return tr;
  // 들여 쓴 자리에 넣으면 둘째 줄부터도 같이 들인다
  const out = lead ? r.text.split("\n").map((l, i) => (i && l ? lead + l : l)).join("\n") : r.text;
  return { changes: { from: c.from, to: c.to, insert: out }, selection: { anchor: c.from + out.length }, userEvent: "input.paste", scrollIntoView: true };
});

export function autoFormatExtension(): Extension {
  return [
    EditorView.inputHandler.of((view, from, to, text) => {
      if (text.length !== 1) return false;
      if (BOUNDARY.test(text)) caseWordBefore(view, from, text);
      if (text === ";" && autoFormat().onSemicolon) return formatOnSemicolon(view, from, to);
      return false;
    }),
    pasteFilter,
  ];
}
