// 자동완성 퀵 액션 메뉴 — 컬럼 후보에서 → 로 열고 ← 로 목록에 돌아간다.
// 고르면 컬럼과 꼴(`EMPNO = ''` …)을 코드 조각으로 넣어 커서를 값 자리에 둔다 (Tab 으로 다음 자리).
// 무엇을 권할지는 quickact.ts, 후보에 붙이는 것은 completion.ts.

import { closeCompletion, completionStatus, selectedCompletion, snippet } from "@codemirror/autocomplete";
import { Prec, type Extension } from "@codemirror/state";
import { EditorView } from "@codemirror/view";
import type { QuickCompletion } from "./completion";
import type { QuickAction } from "./quickact";

/** 한글 입력 상태에서는 key 가 "Process" 로 올 수 있다 — 물리 키(code)로 알아본다 */
function keyName(e: KeyboardEvent): string {
  return e.key === "Process" || e.key === "Unidentified" ? e.code : e.key;
}

/** 열린 메뉴 — 에디터마다 하나 */
interface Menu {
  el: HTMLElement;
  items: QuickAction[];
  index: number;
  from: number;
  completion: QuickCompletion;
}
const open = new WeakMap<EditorView, Menu>();

function close(view: EditorView) {
  const m = open.get(view);
  if (!m) return;
  m.el.remove();
  open.delete(view);
}

function paint(m: Menu) {
  m.el.querySelectorAll("li").forEach((li, i) => li.toggleAttribute("aria-selected", i === m.index));
  m.el.querySelector("li[aria-selected]")?.scrollIntoView({ block: "nearest" });
}

function apply(view: EditorView, m: Menu, i: number) {
  const head = view.state.selection.main.head;
  // 후보를 부른 뒤로 같은 줄에서 이어 친 낱말만 바꾼다 (아니면 커서 앞 낱말)
  const between = m.from <= head ? view.state.sliceDoc(m.from, head) : "\n";
  const from = /^[\p{L}\p{N}_$#.]*$/u.test(between) ? m.from : head - /[\p{L}\p{N}_$#]*$/u.exec(view.state.sliceDoc(Math.max(0, head - 200), head))![0].length;
  close(view);
  closeCompletion(view);
  snippet(m.items[i].template)(view, m.completion, from, head);
  view.focus();
}

function show(view: EditorView, c: QuickCompletion): boolean {
  const q = c.quick;
  if (!q) return false;
  // 고른 후보 줄 옆에 붙인다
  const row = view.dom.querySelector(".cm-tooltip-autocomplete li[aria-selected]") as HTMLElement | null;
  if (!row) return false;
  const r = row.getBoundingClientRect();
  const el = document.createElement("div");
  el.className = "cm-tooltip quick-menu";
  const ul = document.createElement("ul");
  q.actions.forEach((a, i) => {
    const li = document.createElement("li");
    li.textContent = a.label;
    // 누르는 동안 에디터가 포커스를 잃지 않게
    li.addEventListener("mousedown", (e) => {
      e.preventDefault();
      const m = open.get(view);
      if (m) apply(view, m, i);
    });
    ul.append(li);
  });
  const hint = document.createElement("div");
  hint.className = "quick-menu-hint";
  hint.textContent = "Enter 넣기 · ← 돌아가기";
  el.append(ul, hint);
  document.body.append(el);
  // 오른쪽에 자리가 없으면 목록 왼쪽에
  const list = row.closest(".cm-tooltip-autocomplete")!.getBoundingClientRect();
  const w = el.offsetWidth;
  el.style.left = `${list.right + w + 4 <= window.innerWidth ? list.right + 2 : Math.max(0, list.left - w - 2)}px`;
  el.style.top = `${Math.max(0, Math.min(r.top - 3, window.innerHeight - el.offsetHeight - 4))}px`;
  const m: Menu = { el, items: q.actions, index: 0, from: q.from, completion: c };
  open.set(view, m);
  paint(m);
  return true;
}

function onKey(e: KeyboardEvent, view: EditorView): boolean {
  if (view.composing) return false;
  const key = keyName(e);
  const m = open.get(view);
  if (m) {
    const move = (to: number) => {
      m.index = (to + m.items.length) % m.items.length;
      paint(m);
    };
    switch (key) {
      case "ArrowDown": move(m.index + 1); break;
      case "ArrowUp": move(m.index - 1); break;
      case "PageDown": case "End": move(m.items.length - 1); break;
      case "PageUp": case "Home": move(0); break;
      case "Enter": case "Tab": apply(view, m, m.index); break;
      // 목록으로 돌아간다 (목록은 그대로 열려 있다)
      case "ArrowLeft": case "Escape": close(view); break;
      default:
        // 다른 키는 에디터가 받는다 — 메뉴만 닫는다
        close(view);
        return false;
    }
    e.preventDefault();
    return true;
  }
  if (key !== "ArrowRight" || e.shiftKey || e.ctrlKey || e.altKey || e.metaKey) return false;
  if (completionStatus(view.state) !== "active") return false;
  const c = selectedCompletion(view.state) as QuickCompletion | null;
  if (!c?.quick || !show(view, c)) return false;
  e.preventDefault();
  return true;
}

/** 에디터 확장 — 자동완성 키보다 먼저 키를 본다 */
export function quickMenu(): Extension {
  return Prec.highest([
    EditorView.domEventHandlers({
      keydown: onKey,
      blur: (_e, view) => { close(view); },
      mousedown: (_e, view) => { close(view); },
      scroll: (_e, view) => { close(view); },
    }),
    EditorView.updateListener.of((u) => {
      // 글이 바뀌거나 자동완성 목록이 닫히면 메뉴도 닫는다
      if (open.has(u.view) && (u.docChanged || completionStatus(u.state) !== "active")) close(u.view);
    }),
  ]);
}
