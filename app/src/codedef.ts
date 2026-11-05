// 이름과 정의 — 구조(plsqlnav)를 문서마다 한 번 읽어 두고, 커서의 이름이 이 문서 어디에 정의됐는지 찾는다.
// 화면(DOM)에 기대지 않는다 — 테스트(codedef.test.ts)가 node 에서 돈다.

import { syntaxTree } from "@codemirror/language";
import type { EditorState, Text } from "@codemirror/state";
import { type NavNode, navPath, parseNav, UNIT_KINDS } from "./plsqlnav";
import { isProC, parseProC } from "./procnav";

export interface Nav {
  tree: NavNode[];
  /** 줄 처음 자리 → 그 줄에서 시작하는 가장 바깥 여러 줄 마디 */
  byLine: Map<number, NavNode>;
  units: boolean;
  /** Pro*C 로 읽었다 */
  proc: boolean;
}

const cache = new WeakMap<Text, Nav>();

/** 이 문서의 구조 — 문서마다 한 번만 읽는다 */
export function navOf(doc: Text): Nav {
  let n = cache.get(doc);
  if (n) return n;
  const text = doc.toString();
  const proc = isProC(text);
  const tree = proc ? parseProC(text) : parseNav(text);
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
  n = { tree, byLine, units: units || tree.length > 0, proc };
  cache.set(doc, n);
  return n;
}

/** pos 의 이름 — a.b.c 를 통째로 (문자열·주석·숫자·키워드면 없음) */
export function nameAt(state: EditorState, pos: number): { from: number; to: number; parts: string[]; host: boolean } | null {
  const line = state.doc.lineAt(pos);
  const s = line.text;
  const ch = /[\w$#."]/;
  const at = pos - line.from;
  if (!(at < s.length && /[\w$#"]/.test(s[at])) && !(at > 0 && /[\w$#"]/.test(s[at - 1]))) return null;
  const node = syntaxTree(state).resolveInner(pos, at < s.length && /[\w$#"]/.test(s[at]) ? 1 : -1);
  // SQL 문법 나무(String …)와 Pro*C 낱말 나무(string …) 둘 다
  if (/string|comment|number|keyword|bool|null|meta/i.test(node.name)) return null;
  let a = at, b = at;
  while (a > 0 && ch.test(s[a - 1])) a--;
  while (b < s.length && ch.test(s[b])) b++;
  // 끝의 점은 뺀다
  while (b > a && s[b - 1] === ".") b--;
  while (a < b && s[a] === ".") a++;
  const parts = s.slice(a, b).replace(/"/g, "").split(".").filter(Boolean);
  if (!parts.length || /^\d/.test(parts[parts.length - 1])) return null;
  return { from: line.from + a, to: line.from + b, parts, host: s[a - 1] === ":" };
}

const DEF_KINDS = new Set(["procedure", "function", "cursor", "type", "c function"]);
const last = (name: string) => name.split(".").pop()!.toUpperCase();

/** 이 문서 안의 정의 자리 — 프로시저·함수·커서·타입. 본문 머리에서 누르면 명세의 선언으로, 선언에서 누르면 본문으로 */
export function localDefinition(state: EditorState, pos: number): number | null {
  const n = nameAt(state, pos);
  if (!n) return null;
  const name = n.parts[n.parts.length - 1].toUpperCase();
  const { tree, proc } = navOf(state.doc);
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
  if (pick) return pick.from;
  return proc && n.parts.length === 1 ? cVariable(state, tree, n) : null;
}

/** Pro*C 의 변수 (호스트 변수 :v 포함) — 처음 나온 곳을 선언으로 본다: 감싼 함수 안에서 먼저, 없으면 파일 앞(전역 · DECLARE SECTION).
 *  EXEC SQL 안의 :없는 이름은 테이블·컬럼이라 DB 에서 찾게 둔다 */
function cVariable(state: EditorState, tree: NavNode[], n: { from: number; to: number; parts: string[]; host: boolean }): number | null {
  const path = navPath(tree, n.from);
  if (!n.host && path.some((x) => x.kind === "sql" || x.kind === "cursor")) return null;
  const text = state.doc.toString();
  const name = n.parts[0];
  const re = new RegExp(`(?<![\\w$#:.>])${name.replace(/[$]/g, "\\$&")}(?![\\w$#])`, "g");
  const first = (from: number, to: number) => {
    re.lastIndex = from;
    for (let m = re.exec(text); m && m.index < to; m = re.exec(text)) {
      if (m.index === n.from) return null;
      // 주석 안은 건너뛴다
      const line = state.doc.lineAt(m.index);
      const before = line.text.slice(0, m.index - line.from);
      if (/\/\/|^\s*\*|\/\*(?!.*\*\/)/.test(before)) continue;
      return m.index;
    }
    return null;
  };
  const fn = path.find((x) => x.kind === "c function");
  return (fn ? first(fn.from, n.from) : null) ?? first(0, fn ? fn.from : n.from);
}
