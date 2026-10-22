// 괄호 — 커서를 감싼 ( … ) 의 짝을 강하게 보이고, 짝으로 바로 간다.
//
//   Ctrl+]        짝 괄호로 (괄호 옆이면 그 짝, 아니면 감싼 괄호의 끝 ")" 뒤로). 다시 누르면 "(" 앞으로
//   Ctrl+Shift+]  감싼 ( … ) 를 통째로 고른다 (서브쿼리 복사 등)
//
// 괄호는 SQL 문법 트리(Parens)로 찾는다 — 문자열·주석 안의 괄호는 세지 않는다.

import { syntaxTree } from "@codemirror/language";
import { EditorSelection, type EditorState, type Extension, RangeSetBuilder } from "@codemirror/state";
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

/** Ctrl+Shift+] — 감싼 ( … ) 를 고른다. 이미 골라져 있으면 한 겹 바깥으로 */
export const selectParen: Command = (view) => {
  const { state } = view;
  const sel = state.selection.main;
  let p = enclosingPair(state, sel.head);
  // 지금 고른 것이 이 괄호 그대로면 바깥 괄호로
  while (p && sel.from === p.open && sel.to === p.close + 1) p = p.open > 0 ? enclosingPair(state, p.open) : null;
  if (!p) return false;
  view.dispatch({ selection: EditorSelection.range(p.open, p.close + 1), scrollIntoView: true, userEvent: "select" });
  return true;
};

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
