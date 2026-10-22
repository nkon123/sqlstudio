// 괄호 원판 (Ctrl+Shift+]) — 커서를 감싼 ( … ) 로 할 일을 8방향으로 고른다.
//
//    ↖ 안쪽 괄호로     ↑ 여는 괄호로     ↗ 바깥 괄호로
//    ← 안쪽 선택       ( 대상 괄호 )      → 안쪽 복사
//    ↙ 안쪽 포맷       ↓ 끝 괄호로       ↘ 안쪽 실행
//
// 방향키 하나는 그 방향, 두 개를 함께 누르면 대각선 — 키를 다 뗄 때 그동안 눌린 방향으로 한다
// (두 키를 꼭 같은 순간에 누르지 않아도 된다). 숫자 키패드 7·8·9·4·6·1·2·3 도 같은 자리.
// 대상 괄호 바꾸기(↖ ↗ · Ctrl+Shift+] 다시)는 원판을 닫지 않는다. 다루는 구간은 에디터에 옅게 칠한다.

import { type Extension, Prec, StateEffect, StateField, EditorSelection } from "@codemirror/state";
import { Decoration, type DecorationSet, EditorView } from "@codemirror/view";
import { dirOf, enclosingPair, innerRange, outerPair, type Dir, type Pair } from "./brackets";
import { toast } from "./ui";

export interface ParenMenuActions {
  /** 고른 구간(서브쿼리) 실행 */
  run: () => void;
  /** 고른 구간 포맷 */
  format: () => void;
}

/** 화면 칸 순서 (가운데는 대상 괄호) */
const GRID: (Dir | null)[] = ["NW", "N", "NE", "W", null, "E", "SW", "S", "SE"];
const ARROW: Record<Dir, string> = { NW: "↖", N: "↑", NE: "↗", W: "←", E: "→", SW: "↙", S: "↓", SE: "↘" };
const NUMPAD: Record<string, Dir> = { Numpad7: "NW", Numpad8: "N", Numpad9: "NE", Numpad4: "W", Numpad6: "E", Numpad1: "SW", Numpad2: "S", Numpad3: "SE" };

interface Item { label: string; run?: () => void; /** 원판을 닫지 않는다 (대상 괄호 바꾸기) */ stay?: boolean }

/** 원판이 다루는 괄호 구간을 옅게 */
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

interface Menu {
  el: HTMLElement;
  /** 안쪽부터 괄호들 — level 이 지금 대상 */
  levels: Pair[];
  level: number;
  items: Partial<Record<Dir, Item>>;
  /** 지금 눌려 있는 방향키 · 이번에 눌린 방향키 전부 (다 떼면 이것으로 정한다) */
  held: Set<string>;
  seen: Set<string>;
}
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

function copy(text: string) {
  navigator.clipboard.writeText(text).then(() => toast("괄호 안쪽을 복사했습니다", "info", 1500)).catch(() => toast("복사하지 못했습니다", "error"));
}

/** 그 괄호로 할 수 있는 일 — 할 수 없는 칸은 run 이 없다 */
function itemsFor(view: EditorView, m: Menu, act: ParenMenuActions): Partial<Record<Dir, Item>> {
  const st = view.state;
  const p = m.levels[m.level];
  const inner = innerRange(st, p);
  const innerText = st.sliceDoc(inner.from, inner.to);
  const select = () => view.dispatch({ selection: EditorSelection.range(inner.from, inner.to), scrollIntoView: true, userEvent: "select" });
  const cursor = (pos: number) => () => view.dispatch({ selection: EditorSelection.cursor(pos), scrollIntoView: true, userEvent: "select" });
  // 서브쿼리면 그것만 실행·포맷
  const query = /^(\/\*[\s\S]*?\*\/\s*|--[^\n]*\n\s*)*(SELECT|WITH)\b/i.test(innerText);
  const outer = m.level + 1 < m.levels.length ? m.levels[m.level + 1] : outerPair(st, p);
  return {
    NW: { label: "안쪽 괄호로", stay: true, run: m.level > 0 ? () => { m.level--; render(view, m, act); } : undefined },
    N: { label: "여는 괄호로", run: cursor(p.open) },
    NE: {
      label: "바깥 괄호로", stay: true,
      run: outer ? () => { if (m.level + 1 >= m.levels.length) m.levels.push(outer); m.level++; render(view, m, act); } : undefined,
    },
    W: { label: "안쪽 선택", run: select },
    E: { label: "안쪽 복사", run: () => copy(innerText) },
    SW: { label: "안쪽 포맷", run: query ? () => { select(); act.format(); } : undefined },
    S: { label: "끝 괄호로", run: cursor(p.close + 1) },
    SE: { label: "안쪽 실행", run: query ? () => { select(); act.run(); } : undefined },
  };
}

/** 원판을 지금 대상 괄호로 다시 그린다 */
function render(view: EditorView, m: Menu, act: ParenMenuActions) {
  const p = m.levels[m.level];
  m.items = itemsFor(view, m, act);
  const cells = GRID.map((d) => {
    const cell = document.createElement("div");
    if (!d) {
      // 가운데 — 대상 괄호
      const text = view.state.sliceDoc(p.open, p.close + 1).replace(/\s+/g, " ");
      cell.className = "radial-center";
      cell.textContent = text.length > 34 ? `${text.slice(0, 22)} … ${text.slice(-9)}` : text;
      cell.title = `안쪽부터 ${m.level + 1}번째 괄호 · Esc 닫기`;
      return cell;
    }
    const it = m.items[d]!;
    cell.className = `radial-cell radial-${d}`;
    cell.dataset.dir = d;
    cell.toggleAttribute("aria-disabled", !it.run);
    const arrow = document.createElement("span");
    arrow.className = "radial-arrow";
    arrow.textContent = ARROW[d];
    cell.append(arrow, it.label);
    // 누르는 동안 에디터가 포커스를 잃지 않게
    cell.addEventListener("mousedown", (e) => { e.preventDefault(); fire(view, d); });
    return cell;
  });
  m.el.replaceChildren(...cells);
  view.dispatch({ effects: setPreview.of({ from: p.open, to: p.close + 1 }) });
}

/** 눌린 방향을 칸에 표시 */
function mark(m: Menu, d: Dir | null) {
  m.el.querySelectorAll<HTMLElement>(".radial-cell").forEach((c) => c.toggleAttribute("aria-selected", c.dataset.dir === d));
}

function fire(view: EditorView, d: Dir) {
  const m = open.get(view);
  const it = m?.items[d];
  if (!m || !it?.run) return;
  if (it.stay) { it.run(); return; }
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
  el.className = "cm-tooltip radial-menu";
  document.body.append(el);
  const m: Menu = { el, levels: [first], level: 0, items: {}, held: new Set(), seen: new Set() };
  open.set(view, m);
  render(view, m, act);
  // 커서 아래, 가운데 칸이 커서 쪽에 오게 (화면을 넘치면 안으로)
  const c = view.coordsAtPos(head) ?? { left: 0, bottom: 0, top: 0 };
  el.style.left = `${Math.max(4, Math.min(c.left - el.offsetWidth / 2, window.innerWidth - el.offsetWidth - 4))}px`;
  el.style.top = `${c.bottom + el.offsetHeight + 4 <= window.innerHeight ? c.bottom + 4 : Math.max(4, c.top - el.offsetHeight - 4)}px`;
  return true;
}

const ARROWS = new Set(["ArrowUp", "ArrowDown", "ArrowLeft", "ArrowRight"]);

function onKeyDown(e: KeyboardEvent, view: EditorView, m: Menu): boolean {
  const key = keyName(e);
  if (ARROWS.has(key)) {
    if (!e.repeat) {
      m.held.add(key);
      m.seen.add(key);
      mark(m, dirOf(m.seen));
    }
  } else if (NUMPAD[e.code]) {
    fire(view, NUMPAD[e.code]);
  } else if ((e.ctrlKey || e.metaKey) && e.shiftKey && e.code === "BracketRight") {
    // 다시 누르면 바깥 괄호로
    m.items.NE?.run?.();
  } else if (key === "Escape") {
    close(view);
  } else if (["Control", "Shift", "Alt", "Meta"].includes(key)) {
    // 수식 키만 누른 것은 무시 (Ctrl+Shift+] 를 누른 채 방향키를 눌러도 된다)
  } else {
    // 그 밖의 키는 원판을 닫고 에디터가 받는다
    close(view);
    return false;
  }
  e.preventDefault();
  return true;
}

function onKeyUp(e: KeyboardEvent, view: EditorView, m: Menu): boolean {
  const key = keyName(e);
  if (!ARROWS.has(key)) return false;
  m.held.delete(key);
  if (m.held.size) return true;
  // 다 뗐다 — 그동안 눌린 방향으로
  const d = dirOf(m.seen);
  m.seen.clear();
  mark(m, null);
  if (d) fire(view, d);
  return true;
}

/** 에디터 확장 — Ctrl+Shift+] 로 연다 */
export function parenMenu(act: ParenMenuActions): Extension {
  return [
    previewField,
    Prec.highest(
      EditorView.domEventHandlers({
        keydown: (e, view) => {
          const m = open.get(view);
          if (m) return onKeyDown(e, view, m);
          // Ctrl+Shift+] — 한글 입력 상태에서도 물리 키로
          if ((e.ctrlKey || e.metaKey) && e.shiftKey && !e.altKey && e.code === "BracketRight") {
            e.preventDefault();
            return show(view, act);
          }
          return false;
        },
        keyup: (e, view) => {
          const m = open.get(view);
          return m ? onKeyUp(e, view, m) : false;
        },
        blur: (_e, view) => { close(view); },
        mousedown: (_e, view) => { close(view); },
        scroll: (_e, view) => { close(view); },
      }),
    ),
  ];
}
