// 영역 이동 (Ctrl+Shift+2) — 가운데에 4방향 표시를 띄우고 방향키·넘패드로 고른다.
//
//            ↑ 편집기
//   ← 테이블 목록      AI →
//            ↓ 결과 그리드
//
// Ctrl+Shift 를 누르고 있는 동안 띄우지 않는다 — 편집기의 Ctrl+Shift+←/→ (낱말 단위로 넓혀 고르기)와 부딪힌다.

import { h } from "./ui";

export type Area = "editor" | "grid" | "left" | "right";

const LABEL: Record<Area, string> = { editor: "편집기", grid: "결과 그리드", left: "테이블 목록", right: "AI" };
const ARROW: Record<Area, string> = { editor: "↑", grid: "↓", left: "←", right: "→" };
/** 물리 키(code) — 넘패드는 NumLock 이 꺼져 있어도 code 가 같다 */
const KEYS: Record<string, Area> = {
  ArrowUp: "editor", Numpad8: "editor", ArrowDown: "grid", Numpad2: "grid",
  ArrowLeft: "left", Numpad4: "left", ArrowRight: "right", Numpad6: "right",
};

let close: (() => void) | null = null;

export const areaNavOpen = () => !!close;

/** cur: 지금 있는 영역 (흐리게) · go: 고른 영역으로 · back: 한 번 더 Ctrl+Shift+2 — 직전 영역으로 */
export function showAreaNav(cur: Area | null, go: (a: Area) => void, back: () => void) {
  close?.();
  const cell = (a: Area) => h("button", {
    class: `areanav-cell areanav-${a}${a === cur ? " here" : ""}`,
    title: a === cur ? "지금 여기" : `${LABEL[a]}로`,
    onclick: () => { done(); go(a); },
  }, h("span", { class: "areanav-arrow" }, ARROW[a]), LABEL[a]);
  const box = h("div", { class: "areanav" },
    cell("editor"), cell("left"),
    h("div", { class: "areanav-mid" }, "방향키 · 넘패드", h("small", {}, "한 번 더: 직전 영역"), h("small", {}, "Esc: 닫기")),
    cell("right"), cell("grid"));
  const veil = h("div", { class: "areanav-veil", onmousedown: (e: MouseEvent) => { if (e.target === veil) done(); } }, box);
  const onKey = (e: KeyboardEvent) => {
    // 단축키를 누른 손을 떼는 중 — Ctrl·Shift 만 눌린 것은 넘긴다
    if (["Control", "Shift", "Alt", "Meta", "Process"].includes(e.key) && !KEYS[e.code]) return;
    e.preventDefault();
    e.stopPropagation();
    done();
    if ((e.ctrlKey || e.metaKey) && e.shiftKey && e.code === "Digit2") return back();
    const a = KEYS[e.code];
    if (a) go(a);
  };
  const done = () => {
    window.removeEventListener("keydown", onKey, true);
    veil.remove();
    close = null;
  };
  window.addEventListener("keydown", onKey, true);
  document.body.append(veil);
  close = done;
}
