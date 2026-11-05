// 이름과 정의 — 구조(plsqlnav)를 문서마다 한 번 읽어 두고, 커서의 이름이 이 문서 어디에 정의됐는지 찾는다.
// 화면(DOM)에 기대지 않는다 — 테스트(codedef.test.ts)가 node 에서 돈다.

import { syntaxTree } from "@codemirror/language";
import type { EditorState, Text } from "@codemirror/state";
import { type NavNode, parseNav, UNIT_KINDS } from "./plsqlnav";

export interface Nav { tree: NavNode[]; /** 줄 처음 자리 → 그 줄에서 시작하는 가장 바깥 여러 줄 마디 */ byLine: Map<number, NavNode>; units: boolean }

const cache = new WeakMap<Text, Nav>();

/** 이 문서의 구조 — 문서마다 한 번만 읽는다 */
export function navOf(doc: Text): Nav {
  let n = cache.get(doc);
  if (n) return n;
  const tree = parseNav(doc.toString());
  const byLine = new Map<number, NavNode>();
  let units = false;
  const walk = (list: NavNode[]) => {
    for (const x of list) {
      if (UNIT_KINDS.has(x.kind) && !x.decl) units = true;
      const line = doc.lineAt(x.from);
      if (x.to > line.to && !byLine.has(line.from)) byLine.set(line.from, x);
      walk(x.children);
    }
  };
  walk(tree);
  n = { tree, byLine, units: units || tree.length > 0 };
  cache.set(doc, n);
  return n;
}

/** pos 의 이름 — a.b.c 를 통째로 (문자열·주석·숫자·키워드면 없음) */
export function nameAt(state: EditorState, pos: number): { from: number; to: number; parts: string[] } | null {
  const line = state.doc.lineAt(pos);
  const s = line.text;
  const ch = /[\w$#."]/;
  const at = pos - line.from;
  if (!(at < s.length && /[\w$#"]/.test(s[at])) && !(at > 0 && /[\w$#"]/.test(s[at - 1]))) return null;
  const node = syntaxTree(state).resolveInner(pos, at < s.length && /[\w$#"]/.test(s[at]) ? 1 : -1);
  if (/String|Comment|Number|Keyword|Bool|Null/.test(node.name)) return null;
  let a = at, b = at;
  while (a > 0 && ch.test(s[a - 1])) a--;
  while (b < s.length && ch.test(s[b])) b++;
  // 끝의 점은 뺀다
  while (b > a && s[b - 1] === ".") b--;
  while (a < b && s[a] === ".") a++;
  const parts = s.slice(a, b).replace(/"/g, "").split(".").filter(Boolean);
  if (!parts.length || /^\d/.test(parts[parts.length - 1])) return null;
  return { from: line.from + a, to: line.from + b, parts };
}

const DEF_KINDS = new Set(["procedure", "function", "cursor", "type"]);
const last = (name: string) => name.split(".").pop()!.toUpperCase();

/** 이 문서 안의 정의 자리 — 프로시저·함수·커서·타입. 본문 머리에서 누르면 명세의 선언으로, 선언에서 누르면 본문으로 */
export function localDefinition(state: EditorState, pos: number): number | null {
  const n = nameAt(state, pos);
  if (!n) return null;
  const name = n.parts[n.parts.length - 1].toUpperCase();
  const { tree } = navOf(state.doc);
  const all: NavNode[] = [];
  const pkgs = new Set<string>();
  const walk = (list: NavNode[]) => {
    for (const x of list) {
      if (x.kind === "package" || x.kind === "package body") pkgs.add(last(x.name));
      if (DEF_KINDS.has(x.kind) && last(x.name) === name) all.push(x);
      walk(x.children);
    }
  };
  walk(tree);
  // pkg.proc 는 그 패키지가 이 문서에 있을 때만
  if (n.parts.length >= 2 && !pkgs.has(n.parts[n.parts.length - 2].toUpperCase())) return null;
  const line = state.doc.lineAt(pos).number;
  const header = (x: NavNode) => state.doc.lineAt(x.from).number === line && pos > x.from;
  if (all.some(header) && all.length === 1) return null;
  const pick = [...all.filter((x) => !x.decl), ...all.filter((x) => x.decl)].find((x) => !header(x));
  return pick ? pick.from : null;
}
