// 코드 탐색 — 긴 패키지 본문에서 길을 잃지 않게.
//   Ctrl+Shift+O        구조 보기 (프로시저·함수·커서·타입 목록, 글자로 거르고 Enter 로 이동)
//   위쪽 줄              지금 커서가 어디 안에 있는지 (패키지 › 프로시저 › LOOP › IF) — 누르면 그 처음으로
//   Alt+← · Alt+→       커서 뒤로 · 앞으로 (멀리 옮기기 전 자리)
//   Ctrl+Shift+- · =    블록 접기 · 펴기 (Ctrl+Alt+- · = 모두), 줄 번호 옆 ▾
// 구조는 plsqlnav 가 낱말로 짐작한다.

import { codeFolding, foldAll, foldCode, foldEffect, foldGutter, foldService, foldedRanges, unfoldAll, unfoldCode } from "@codemirror/language";
import { Annotation, EditorSelection, type Extension, StateEffect, StateField } from "@codemirror/state";
import { type Command, Decoration, type DecorationSet, EditorView, type Panel, showPanel, type ViewUpdate } from "@codemirror/view";
import { openPalette, type Command as PalCommand } from "./palette";
import { localDefinition, nameAt, navOf } from "./codedef";
import { KIND_LABEL, navOutline, navPath, type NavNode, UNIT_KINDS } from "./plsqlnav";
import { h } from "./ui";

/** 접는 곳 — 여는 줄 끝부터, END 줄은 보이게 남긴다 */
const nodeFolds = foldService.of((state, lineStart) => {
  const x = navOf(state.doc).byLine.get(lineStart);
  if (!x) return null;
  const from = state.doc.lineAt(x.from).to;
  const to = x.end >= 0 ? state.doc.lineAt(x.end).from - 1 : x.to;
  return to > from ? { from, to } : null;
});

/** 모두 접기 — 패키지는 펼쳐 두고 그 안의 프로시저·함수를 접는다 (없으면 바깥 블록부터) */
export const foldUnits: Command = (view) => {
  const { state } = view;
  const { tree } = navOf(state.doc);
  const targets: NavNode[] = [];
  const walk = (list: NavNode[]) => {
    for (const x of list) {
      if (x.kind === "package" || x.kind === "package body" || x.kind === "type body") walk(x.children);
      else if (!x.decl && x.to > state.doc.lineAt(x.from).to) targets.push(x);
    }
  };
  walk(tree);
  if (!targets.length) return foldAll(view);
  const folded = foldedRanges(state);
  const effects = targets.flatMap((x) => {
    const from = state.doc.lineAt(x.from).to;
    const to = x.end >= 0 ? state.doc.lineAt(x.end).from - 1 : x.to;
    let dup = false;
    folded.between(from, from, (a) => { if (a === from) dup = true; });
    return to > from && !dup ? [foldEffect.of({ from, to })] : [];
  });
  if (effects.length) view.dispatch({ effects });
  return true;
};

// ── 커서 뒤로·앞으로 ────────────────────────────────────────────────

/** 문서를 통째로 바꿨다 — 기록을 지운다 */
export const navReset = Annotation.define<boolean>();
/** 이 이동은 기록하지 않는다 (뒤로·앞으로 자신) */
const replay = Annotation.define<boolean>();
/** 이 이동은 줄 차이와 상관없이 기록한다 (구조 보기·위치 표시에서 고른 것) */
const leap = Annotation.define<boolean>();
const setJumps = StateEffect.define<Jumps>();
/** 이만큼 줄을 건너뛰면 기록한다 */
const FAR = 10;
const KEEP = 50;

interface Jumps { back: number[]; fwd: number[] }

const jumps = StateField.define<Jumps>({
  create: () => ({ back: [], fwd: [] }),
  update(v, tr) {
    if (tr.annotation(navReset)) return { back: [], fwd: [] };
    for (const e of tr.effects) if (e.is(setJumps)) return e.value;
    let { back, fwd } = v;
    if (tr.docChanged) {
      back = back.map((p) => tr.changes.mapPos(p));
      fwd = fwd.map((p) => tr.changes.mapPos(p));
    }
    const same = back === v.back ? v : { back, fwd };
    // 고치면서 옮겨진 커서는 기록하지 않는다
    if (tr.annotation(replay) || !tr.selection || tr.docChanged) return same;
    const doc = tr.state.doc;
    const was = tr.startState.selection.main.head;
    const now = tr.state.selection.main.head;
    if (!tr.annotation(leap) && Math.abs(doc.lineAt(was).number - doc.lineAt(now).number) < FAR) return same;
    // 같은 줄을 거듭 남기지 않는다
    const last = back[back.length - 1];
    if (last != null && doc.lineAt(last).number === doc.lineAt(was).number) return { back, fwd: [] };
    return { back: [...back, was].slice(-KEEP), fwd: [] };
  },
});

function go(view: EditorView, pos: number, ann: Annotation<boolean>, effects: StateEffect<unknown>[] = []) {
  const p = Math.min(Math.max(0, pos), view.state.doc.length);
  view.dispatch({
    selection: EditorSelection.cursor(p),
    effects: [...effects, EditorView.scrollIntoView(p, { y: "center" })],
    annotations: ann,
    userEvent: "select",
  });
  view.focus();
}

const travel = (dir: -1 | 1): Command => (view) => {
  const j = view.state.field(jumps, false);
  const from = dir < 0 ? j?.back : j?.fwd;
  if (!j || !from?.length) return false;
  const here = view.state.selection.main.head;
  const next: Jumps = dir < 0
    ? { back: j.back.slice(0, -1), fwd: [...j.fwd, here] }
    : { back: [...j.back, here], fwd: j.fwd.slice(0, -1) };
  go(view, from[from.length - 1], replay.of(true), [setJumps.of(next)]);
  return true;
};

export const navBack: Command = travel(-1);
export const navForward: Command = travel(1);

/** 그 자리로 — 뒤로 가기에 남긴다 */
export function navJump(view: EditorView, pos: number) {
  go(view, pos, leap.of(true));
}

// ── 구조 보기 ───────────────────────────────────────────────────────

export const openOutline: Command = (view) => {
  const { tree } = navOf(view.state.doc);
  const items = navOutline(tree);
  const doc = view.state.doc;
  const here = view.state.selection.main.head;
  // 커서를 감싼 가장 안쪽 단위를 처음에 고른다
  let start = -1;
  items.forEach((it, i) => { if (it.node.from <= here && here <= it.node.to) start = i; });
  const cmds: PalCommand[] = items.map(({ node, depth }) => ({
    group: KIND_LABEL[node.kind],
    label: node.decl && (node.kind === "procedure" || node.kind === "function") ? `${node.name} (선언)` : node.name,
    keys: `${doc.lineAt(node.from).number}행`,
    indent: depth,
    run: () => navJump(view, node.from),
  }));
  openPalette(cmds, {
    placeholder: "구조 찾기 — 프로시저·함수·커서·타입 이름",
    keepOrder: true,
    cls: "pal-outline",
    start: start < 0 ? undefined : start,
    empty: items.length ? "맞는 이름이 없습니다" : "PROCEDURE · FUNCTION · PACKAGE 가 없습니다",
    onCancel: () => view.focus(),
  });
  return true;
};

// ── 현재 위치 표시 ──────────────────────────────────────────────────

function crumbsPanel(view: EditorView): Panel {
  const list = h("div", { class: "cm-crumbs-path" });
  const dom = h("div", { class: "cm-crumbs" },
    h("button", { class: "cm-crumbs-open", title: "구조 보기 (Ctrl+Shift+O)", onmousedown: (e: Event) => { e.preventDefault(); openOutline(view); } }, "≡"),
    list);
  let timer = 0;
  let sig = "";
  const draw = () => {
    timer = 0;
    const nav = navOf(view.state.doc);
    const show = nav.units;
    if (dom.hidden === show) { dom.hidden = !show; view.requestMeasure(); }
    if (!show) return;
    const path = navPath(nav.tree, view.state.selection.main.head);
    const s = path.map((x) => `${x.kind}:${x.from}`).join("|");
    if (s === sig) return;
    sig = s;
    list.replaceChildren(...(path.length ? path.flatMap((x, i) => [
      i ? h("span", { class: "cm-crumbs-sep" }, "›") : "",
      h("button", {
        class: `cm-crumb${UNIT_KINDS.has(x.kind) ? " unit" : ""}`,
        title: `${KIND_LABEL[x.kind]} — ${view.state.doc.lineAt(x.from).number}행으로`,
        onmousedown: (e: Event) => { e.preventDefault(); navJump(view, x.from); },
      }, UNIT_KINDS.has(x.kind) ? x.name : x.name.toUpperCase()),
    ]) : [h("span", { class: "cm-crumbs-none" }, "맨 바깥")]));
  };
  dom.hidden = true;
  return {
    dom,
    top: true,
    mount: () => draw(),
    update: (u: ViewUpdate) => {
      if (!u.docChanged && !u.selectionSet) return;
      // 치는 동안은 쉬었다가 — 큰 패키지를 글자마다 다시 읽지 않는다
      if (timer) clearTimeout(timer);
      timer = window.setTimeout(draw, u.docChanged ? 250 : 60);
    },
    destroy: () => { if (timer) clearTimeout(timer); },
  };
}

// ── 정의로 이동 (Ctrl+클릭 · F12) ────────────────────────────────────

/** Ctrl 을 누르고 이름 위에 올리면 밑줄 — 누르면 찾아 간다 */
const setLink = StateEffect.define<{ from: number; to: number } | null>();
const linkMark = Decoration.mark({ class: "cm-def-link" });
const links = StateField.define<DecorationSet>({
  create: () => Decoration.none,
  update(v, tr) {
    for (const e of tr.effects) if (e.is(setLink)) return e.value ? Decoration.set([linkMark.range(e.value.from, e.value.to)]) : Decoration.none;
    return tr.docChanged ? Decoration.none : v;
  },
  provide: (f) => EditorView.decorations.from(f),
});

function linkAt(view: EditorView, x: number, y: number, ctrl: boolean) {
  const pos = ctrl ? view.posAtCoords({ x, y }) : null;
  const n = pos == null ? null : nameAt(view.state, pos);
  const cur = view.state.field(links, false);
  let had: { from: number; to: number } | null = null;
  cur?.between(0, view.state.doc.length, (from, to) => { had = { from, to }; });
  const h0 = had as { from: number; to: number } | null;
  if (n ? h0?.from === n.from && h0?.to === n.to : !h0) return;
  view.dispatch({ effects: setLink.of(n ? { from: n.from, to: n.to } : null) });
}

let mouse = { x: -1, y: -1 };
const ctrlLinks: Extension = [
  links,
  EditorView.domEventHandlers({
    mousemove: (e, view) => { mouse = { x: e.clientX, y: e.clientY }; linkAt(view, e.clientX, e.clientY, e.ctrlKey && !e.altKey && !e.shiftKey); },
    mouseleave: (_e, view) => linkAt(view, 0, 0, false),
    keydown: (e, view) => { if (e.key === "Control") linkAt(view, mouse.x, mouse.y, true); },
    keyup: (e, view) => { if (e.key === "Control") linkAt(view, 0, 0, false); },
    blur: (_e, view) => linkAt(view, 0, 0, false),
  }),
];

/** 문서·커서가 바뀌면 — 왼쪽 개요 패널이 듣는다 */
const watchers = new Set<(view: EditorView, docChanged: boolean) => void>();
export function watchNav(f: (view: EditorView, docChanged: boolean) => void) {
  watchers.add(f);
  return () => watchers.delete(f);
}

/** 편집기에 붙일 것 */
export const codeNav: Extension = [
  jumps,
  ctrlLinks,
  EditorView.updateListener.of((u) => {
    if (watchers.size && (u.docChanged || u.selectionSet)) for (const f of watchers) f(u.view, u.docChanged);
  }),
  codeFolding({ placeholderText: "…" }),
  foldGutter({ openText: "▾", closedText: "▸" }),
  nodeFolds,
  showPanel.of(crumbsPanel),
];

export { foldCode, localDefinition, nameAt, navOf, unfoldAll, unfoldCode };
