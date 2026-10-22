// 괄호 — 커서를 감싼 ( … ) 의 짝을 강하게 보이고, 짝으로 바로 간다.
//
//   Ctrl+]        짝 괄호로 (괄호 옆이면 그 짝, 아니면 감싼 괄호의 끝 ")" 뒤로). 다시 누르면 "(" 앞으로
//   Ctrl+Shift+]  감싼 괄호의 안쪽을 고른다 (괄호·바로 안의 공백 빼고). [ 로 넓힌 뒤면 한 겹 안쪽으로
//   Ctrl+Shift+[  한 겹 바깥 괄호의 안쪽으로 넓힌다 — 누를 때마다 더 바깥
//   Ctrl+Alt+] [  같은 높이의 다음·이전 괄호 안쪽으로 (WHERE a IN (…) AND b IN (…) 의 둘째로)
//   안쪽으로 갈 때(Ctrl+Shift+]) 넓혀 온 길이 없으면 그 안의 첫 괄호로 들어간다
//
// 괄호는 SQL 문법 트리(Parens)로 찾는다 — 문자열·주석 안의 괄호는 세지 않는다.

import { syntaxTree } from "@codemirror/language";
import { EditorSelection, type EditorState, type Extension, Prec, RangeSetBuilder } from "@codemirror/state";
import { Decoration, type DecorationSet, EditorView, ViewPlugin, type ViewUpdate, type Command } from "@codemirror/view";

/** 괄호 한 쌍 — open 은 "(" 의 위치, close 는 ")" 의 위치 */
export interface Pair { open: number; close: number }

/** 이 노드가 짝이 맞는 ( … ) 면 그 쌍 */
function pairOf(state: EditorState, node: { name: string; from: number; to: number } | null): Pair | null {
  if (!node || node.name !== "Parens") return null;
  if (state.sliceDoc(node.from, node.from + 1) !== "(" || state.sliceDoc(node.to - 1, node.to) !== ")" || node.to - node.from < 2) return null;
  return { open: node.from, close: node.to - 1 };
}

/** 커서(pos)를 감싼 가장 안쪽 ( … ). 괄호 바로 옆이면 그 괄호의 쌍 */
export function enclosingPair(state: EditorState, pos: number): Pair | null {
  const tree = syntaxTree(state);
  if (tree.length < pos) return null;
  // "(" 앞 · ")" 뒤에 커서가 있어도 그 괄호로 (안쪽부터 보도록 양쪽을 다 본다)
  for (const side of [1, -1] as const) {
    const ch = side === 1 ? state.sliceDoc(pos, pos + 1) : state.sliceDoc(pos - 1, pos);
    if (ch !== "(" && ch !== ")") continue;
    const p = pairOf(state, tree.resolveInner(pos, side).parent);
    if (p) return p;
  }
  for (let n: ReturnType<typeof tree.resolveInner> | null = tree.resolveInner(pos, 0); n; n = n.parent) {
    const p = pairOf(state, n);
    if (p && p.open < pos && pos <= p.close) return p;
  }
  return null;
}

/** Ctrl+] — 짝 괄호로 */
export const gotoMatchingParen: Command = (view) => {
  const { state } = view;
  const pos = state.selection.main.head;
  const p = enclosingPair(state, pos);
  if (!p) return false;
  // ")" 바로 뒤면 "(" 앞으로, 그 밖(괄호 안 · "(" 옆)이면 ")" 뒤로 — 누를 때마다 오간다
  const target = pos === p.close + 1 ? p.open : p.close + 1;
  view.dispatch({ selection: EditorSelection.cursor(target), scrollIntoView: true, userEvent: "select" });
  return true;
};

/** 이 쌍을 감싼 한 겹 바깥 ( … ) — 트리에서 그 괄호의 부모 쪽으로 */
export function outerPair(state: EditorState, p: Pair): Pair | null {
  // "(" 의 부모가 이 쌍의 Parens, 그 위로 올라가며 다음 Parens
  for (let n = syntaxTree(state).resolveInner(p.open, 1).parent?.parent ?? null; n; n = n.parent) {
    const o = pairOf(state, n);
    if (o && (o.open < p.open || o.close > p.close)) return o;
  }
  return null;
}

/** 괄호 안쪽 — 괄호와 그 바로 안의 공백·줄바꿈은 뺀다 (서브쿼리면 SELECT … 만) */
export function innerRange(state: EditorState, p: Pair): { from: number; to: number } {
  const text = state.sliceDoc(p.open + 1, p.close);
  const lead = text.length - text.trimStart().length;
  const trail = text.length - text.trimEnd().length;
  const from = p.open + 1 + lead;
  return { from, to: Math.max(from, p.close - trail) };
}

const mark = Decoration.mark({ class: "cm-parenPair" });

function decorate(view: EditorView): DecorationSet {
  const sel = view.state.selection.main;
  const b = new RangeSetBuilder<Decoration>();
  const p = sel.empty ? enclosingPair(view.state, sel.head) : null;
  if (p) {
    b.add(p.open, p.open + 1, mark);
    b.add(p.close, p.close + 1, mark);
  }
  return b.finish();
}

/** 감싼 괄호 쌍을 굵게 · 색으로 (커서가 괄호 옆이 아니어도) */
export function parenHighlight(): Extension {
  return ViewPlugin.fromClass(
    class {
      decorations: DecorationSet;
      constructor(view: EditorView) { this.decorations = decorate(view); }
      update(u: ViewUpdate) {
        // 문법 트리가 늦게 채워지기도 해서 바뀐 트리에도 다시
        if (u.docChanged || u.selectionSet || syntaxTree(u.state) !== syntaxTree(u.startState)) this.decorations = decorate(u.view);
      }
    },
    { decorations: (v) => v.decorations },
  );
}

/** 넓히기 전의 선택들 — Ctrl+Shift+] 가 한 겹씩 되돌린다 */
const widened = new WeakMap<EditorView, { from: number; to: number }[]>();

function selectRange(view: EditorView, r: { from: number; to: number }) {
  view.dispatch({ selection: EditorSelection.range(r.from, r.to), scrollIntoView: true, userEvent: "select.paren" });
}

/** 지금 선택(또는 커서)을 감싼 괄호 중, 그 안쪽이 지금 선택보다 넓은 가장 안쪽 것 */
export function wider(state: EditorState, from: number, to: number): Pair | null {
  for (let p = enclosingPair(state, from); p; p = outerPair(state, p)) {
    const r = innerRange(state, p);
    if (r.from <= from && r.to >= to && (r.from < from || r.to > to)) return p;
  }
  return null;
}

/** 지금 선택이 어느 괄호의 안쪽 그대로면 그 괄호 */
function selectedPair(state: EditorState, from: number, to: number): Pair | null {
  for (let p = enclosingPair(state, from); p; p = outerPair(state, p)) {
    const r = innerRange(state, p);
    if (r.from === from && r.to === to) return p;
    if (r.from < from || r.to > to) return null;
  }
  return null;
}

/** 괄호의 문법 노드 */
function nodeOf(state: EditorState, p: Pair) {
  return syntaxTree(state).resolveInner(p.open, 1).parent;
}

/** 괄호 바로 안(한 겹 아래)의 괄호들 — 순서대로 */
export function childPairs(state: EditorState, p: Pair): Pair[] {
  const out: Pair[] = [];
  const node = nodeOf(state, p);
  if (!node) return out;
  // 바로 아래 자식만 — 더 깊은 괄호는 그 자식 괄호의 것
  const walk = (n: typeof node) => {
    for (let c = n.firstChild; c; c = c.nextSibling) {
      const q = pairOf(state, c);
      if (q) out.push(q);
      else walk(c);
    }
  };
  walk(node);
  return out;
}

/** 같은 높이(같은 괄호 바로 안)의 괄호들 — 가장 바깥이면 문장 안의 바깥 괄호들 */
export function siblingPairs(state: EditorState, p: Pair): Pair[] {
  const parent = outerPair(state, p);
  if (parent) return childPairs(state, parent);
  // 맨 바깥 — 같은 문장(트리 최상위 노드) 안의 맨 바깥 괄호들
  let top = nodeOf(state, p);
  while (top?.parent?.parent) top = top.parent;
  if (!top) return [p];
  const out: Pair[] = [];
  const walk = (n: typeof top) => {
    for (let c = n!.firstChild; c; c = c.nextSibling) {
      const q = pairOf(state, c);
      if (q) out.push(q);
      else walk(c);
    }
  };
  walk(top);
  return out;
}

/** Ctrl+Shift+] — 안쪽으로: 넓혀 온 길이 있으면 그 길로, 괄호 안쪽을 골라 둔 상태면 그 안의 첫 괄호로, 아니면 감싼 괄호의 안쪽 */
export const selectInner: Command = (view) => {
  const { state } = view;
  const sel = state.selection.main;
  const stack = widened.get(view);
  const last = stack?.[stack.length - 1];
  if (stack && last && !sel.empty) {
    stack.pop();
    selectRange(view, last);
    return true;
  }
  widened.delete(view);
  const cur = sel.empty ? null : selectedPair(state, sel.from, sel.to);
  const p = cur ? childPairs(state, cur)[0] : enclosingPair(state, sel.head);
  if (!p) return false;
  selectRange(view, innerRange(state, p));
  return true;
};

/** Ctrl+Alt+] · Ctrl+Alt+[ — 같은 높이의 다음·이전 괄호 안쪽으로 (끝에서는 처음으로 돌아간다) */
export function selectSibling(step: 1 | -1): Command {
  return (view) => {
    const { state } = view;
    const sel = state.selection.main;
    const cur = (sel.empty ? null : selectedPair(state, sel.from, sel.to)) ?? enclosingPair(state, sel.head);
    if (!cur) return false;
    const sibs = siblingPairs(state, cur);
    const i = sibs.findIndex((q) => q.open === cur.open);
    if (sibs.length < 2 || i < 0) return false;
    // 옆으로 옮기면 넓혀 온 길은 맞지 않는다
    widened.delete(view);
    selectRange(view, innerRange(state, sibs[(i + step + sibs.length) % sibs.length]));
    return true;
  };
}

/** Ctrl+Shift+[ — 한 겹 바깥 괄호의 안쪽으로 넓힌다 (아무것도 안 골랐으면 감싼 괄호의 안쪽부터) */
export const selectOuter: Command = (view) => {
  const sel = view.state.selection.main;
  const p = sel.empty ? enclosingPair(view.state, sel.head) : wider(view.state, sel.from, sel.to);
  if (!p) return false;
  if (!sel.empty) widened.set(view, [...(widened.get(view) ?? []), { from: sel.from, to: sel.to }]);
  selectRange(view, innerRange(view.state, p));
  return true;
};

/** Ctrl+Shift+] · Ctrl+Shift+[ · Ctrl+Alt+] · Ctrl+Alt+[ — 물리 키로 (한글 입력 상태·자판 배열에 상관없이) */
export function parenSelect(): Extension {
  return Prec.highest([
    EditorView.domEventHandlers({
      keydown: (e, view) => {
        if (!(e.ctrlKey || e.metaKey)) return false;
        const bracket = e.code === "BracketRight" ? 1 : e.code === "BracketLeft" ? -1 : 0;
        if (!bracket) return false;
        const run = e.shiftKey && !e.altKey ? (bracket > 0 ? selectInner : selectOuter)
          : e.altKey && !e.shiftKey ? selectSibling(bracket)
          : null;
        if (!run) return false;
        e.preventDefault();
        run(view);
        return true;
      },
    }),
    // 다른 방법(방향키·마우스·입력)으로 선택이 바뀌면 되돌릴 자리는 잊는다
    EditorView.updateListener.of((u) => {
      if (u.selectionSet && widened.has(u.view) && !u.transactions.some((tr) => tr.isUserEvent("select.paren"))) widened.delete(u.view);
    }),
  ]);
}
