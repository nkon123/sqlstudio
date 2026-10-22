// 괄호 메뉴 (Ctrl+Shift+]) — 커서를 감싼 ( … ) 로 할 일을 방향키로 고른다.
//
//   ↑↓ 고르기 · Enter 하기 · ← 안쪽 괄호 · → 바깥 괄호 · Esc 닫기 · 1~9 바로
//
// 메뉴가 떠 있는 동안 대상 괄호 구간을 옅게 칠해 어디를 다루는지 보인다. 괄호 찾기는 brackets.ts.

import { type Extension, Prec, StateEffect, StateField, EditorSelection } from "@codemirror/state";
import { Decoration, type DecorationSet, EditorView } from "@codemirror/view";
import { enclosingPair, innerRange, outerPair, type Pair } from "./brackets";
import { toast } from "./ui";

export interface ParenMenuActions {
  /** 고른 구간(서브쿼리) 실행 */
  run: () => void;
  /** 고른 구간 포맷 */
  format: () => void;
}

interface Item { label: string; hint?: string; run: () => void }

/** 메뉴가 다루는 괄호 구간을 옅게 */
const setPreview = StateEffect.define<{ from: number; to: number } | null>();
const previewMark = Decoration.mark({ class: "cm-parenPreview" });
const previewField = StateField.define<DecorationSet>({
  create: () => Decoration.none,
  update(deco, tr) {
    for (const e of tr.effects) if (e.is(setPreview)) return e.value ? Decoration.set([previewMark.range(e.value.from, e.value.to)]) : Decoration.none;
    return tr.docChanged ? Decoration.none : deco;
  },
  provide: (f) => EditorView.decorations.from(f),
});

interface Menu { el: HTMLElement; levels: Pair[]; level: number; index: number; items: Item[] }
const open = new WeakMap<EditorView, Menu>();

/** 한글 입력 상태에서는 key 가 "Process" 로 올 수 있다 — 물리 키(code)로 */
function keyName(e: KeyboardEvent): string {
  return e.key === "Process" || e.key === "Unidentified" ? e.code : e.key;
}

function close(view: EditorView) {
  const m = open.get(view);
  if (!m) return;
  m.el.remove();
  open.delete(view);
  view.dispatch({ effects: setPreview.of(null) });
}

function copy(text: string, what: string) {
  navigator.clipboard.writeText(text).then(() => toast(`${what}을 복사했습니다`, "info", 1500)).catch(() => toast("복사하지 못했습니다", "error"));
}

/** 그 괄호로 할 수 있는 일 */
function itemsFor(view: EditorView, p: Pair, act: ParenMenuActions): Item[] {
  const st = view.state;
  const inner = innerRange(st, p);
  const innerText = st.sliceDoc(inner.from, inner.to);
  const select = (from: number, to: number) => view.dispatch({ selection: EditorSelection.range(from, to), scrollIntoView: true, userEvent: "select" });
  const items: Item[] = [
    { label: "안쪽 선택", run: () => select(inner.from, inner.to) },
    { label: "안쪽 복사", run: () => copy(innerText, "괄호 안쪽") },
    { label: "괄호째 선택", run: () => select(p.open, p.close + 1) },
    { label: "괄호째 복사", run: () => copy(st.sliceDoc(p.open, p.close + 1), "괄호") },
    { label: "끝 괄호로 이동", hint: "Ctrl+]", run: () => view.dispatch({ selection: EditorSelection.cursor(p.close + 1), scrollIntoView: true }) },
    { label: "여는 괄호로 이동", run: () => view.dispatch({ selection: EditorSelection.cursor(p.open), scrollIntoView: true }) },
  ];
  // 서브쿼리면 그것만 실행·포맷
  if (/^(\/\*[\s\S]*?\*\/\s*|--[^\n]*\n\s*)*(SELECT|WITH)\b/i.test(innerText)) {
    items.push(
      { label: "안쪽 실행 (서브쿼리)", hint: "Ctrl+Enter", run: () => { select(inner.from, inner.to); act.run(); } },
      { label: "안쪽 포맷", hint: "Ctrl+Shift+F", run: () => { select(inner.from, inner.to); act.format(); } },
    );
  }
  return items;
}

/** 메뉴를 그 수준(levels[level])의 괄호로 다시 그린다 */
function render(view: EditorView, m: Menu, act: ParenMenuActions) {
  const p = m.levels[m.level];
  m.items = itemsFor(view, p, act);
  m.index = Math.min(m.index, m.items.length - 1);
  const preview = view.state.sliceDoc(p.open, p.close + 1).replace(/\s+/g, " ");
  const head = document.createElement("div");
  head.className = "quick-menu-head";
  head.textContent = preview.length > 60 ? `${preview.slice(0, 40)} … ${preview.slice(-15)}` : preview;
  head.title = `${m.level + 1}번째 괄호 (안쪽부터)`;
  const ul = document.createElement("ul");
  m.items.forEach((it, i) => {
    const li = document.createElement("li");
    li.append(`${i + 1}. ${it.label}`);
    if (it.hint) {
      const k = document.createElement("span");
      k.className = "quick-menu-key";
      k.textContent = it.hint;
      li.append(k);
    }
    li.toggleAttribute("aria-selected", i === m.index);
    // 누르는 동안 에디터가 포커스를 잃지 않게
    li.addEventListener("mousedown", (e) => { e.preventDefault(); pick(view, i); });
    ul.append(li);
  });
  const hint = document.createElement("div");
  hint.className = "quick-menu-hint";
  const outer = outerPair(view.state, p);
  hint.textContent = `Enter 하기 · ${m.level > 0 ? "← 안쪽 괄호 · " : ""}${outer ? "→ 바깥 괄호 · " : ""}Esc 닫기`;
  m.el.replaceChildren(head, ul, hint);
  view.dispatch({ effects: setPreview.of({ from: p.open, to: p.close + 1 }) });
}

function pick(view: EditorView, i: number) {
  const m = open.get(view);
  const it = m?.items[i];
  if (!it) return;
  close(view);
  view.focus();
  it.run();
}

function show(view: EditorView, act: ParenMenuActions): boolean {
  const head = view.state.selection.main.head;
  const first = enclosingPair(view.state, head);
  if (!first) {
    toast("커서가 괄호 안에 있지 않습니다", "info", 1500);
    return true;
  }
  const el = document.createElement("div");
  el.className = "cm-tooltip quick-menu paren-menu";
  document.body.append(el);
  const m: Menu = { el, levels: [first], level: 0, index: 0, items: [] };
  open.set(view, m);
  render(view, m, act);
  // 커서 아래에 (화면을 넘치면 위로)
  const c = view.coordsAtPos(head) ?? { left: 0, bottom: 0, top: 0 };
  el.style.left = `${Math.max(0, Math.min(c.left, window.innerWidth - el.offsetWidth - 4))}px`;
  el.style.top = `${c.bottom + el.offsetHeight + 4 <= window.innerHeight ? c.bottom + 2 : Math.max(0, c.top - el.offsetHeight - 2)}px`;
  return true;
}

function onKey(e: KeyboardEvent, view: EditorView, act: ParenMenuActions): boolean {
  const m = open.get(view);
  if (!m) return false;
  const key = keyName(e);
  const move = (to: number) => {
    m.index = (to + m.items.length) % m.items.length;
    m.el.querySelectorAll("li").forEach((li, i) => li.toggleAttribute("aria-selected", i === m.index));
  };
  if (/^[1-9]$/.test(e.key) || /^Digit[1-9]$/.test(e.code)) {
    pick(view, Number(e.code.slice(-1)) - 1);
  } else {
    switch (key) {
      case "ArrowDown": move(m.index + 1); break;
      case "ArrowUp": move(m.index - 1); break;
      case "Home": case "PageUp": move(0); break;
      case "End": case "PageDown": move(m.items.length - 1); break;
      case "Enter": case "Tab": pick(view, m.index); break;
      case "ArrowRight": {
        // 한 겹 바깥 괄호로
        const outer = m.level + 1 < m.levels.length ? m.levels[m.level + 1] : outerPair(view.state, m.levels[m.level]);
        if (outer) {
          if (m.level + 1 >= m.levels.length) m.levels.push(outer);
          m.level++;
          render(view, m, act);
        }
        break;
      }
      case "ArrowLeft": if (m.level > 0) { m.level--; render(view, m, act); } break;
      case "Escape": close(view); break;
      // 수식 키만 누른 것은 무시 — 그 밖의 키는 메뉴를 닫고 에디터가 받는다
      case "Control": case "Shift": case "Alt": case "Meta": return true;
      default: close(view); return false;
    }
  }
  e.preventDefault();
  return true;
}

/** 에디터 확장 — Ctrl+Shift+] 로 연다 */
export function parenMenu(act: ParenMenuActions): Extension {
  return [
    previewField,
    Prec.highest(
      EditorView.domEventHandlers({
        keydown: (e, view) => {
          if (open.has(view)) return onKey(e, view, act);
          // Ctrl+Shift+] — 한글 입력 상태에서도 물리 키로
          if ((e.ctrlKey || e.metaKey) && e.shiftKey && !e.altKey && e.code === "BracketRight") {
            e.preventDefault();
            return show(view, act);
          }
          return false;
        },
        blur: (_e, view) => { close(view); },
        mousedown: (_e, view) => { close(view); },
        scroll: (_e, view) => { close(view); },
      }),
    ),
  ];
}
