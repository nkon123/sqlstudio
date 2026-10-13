// 우클릭 메뉴 — 마우스 자리에 띄우고, 바깥을 누르거나 Esc 면 닫는다. ▸ 하위 메뉴, ↑↓ Enter ← → 로도 고른다.

import { h } from "./ui";

export type MenuItem =
  | { sep: true }
  | { title: string }
  | { label: string; hint?: string; disabled?: boolean; run?: () => void; sub?: MenuItem[] };

let current: { close: () => void } | null = null;

export function closeMenu() {
  current?.close();
}

export function showMenu(x: number, y: number, items: MenuItem[]) {
  closeMenu();
  const layers: HTMLElement[] = [];
  const close = () => {
    for (const l of layers) l.remove();
    layers.length = 0;
    window.removeEventListener("mousedown", outside, true);
    window.removeEventListener("keydown", keys, true);
    window.removeEventListener("blur", close);
    window.removeEventListener("resize", close);
    if (current?.close === close) current = null;
  };
  const outside = (e: MouseEvent) => { if (!layers.some((l) => l.contains(e.target as Node))) close(); };

  const build = (list: MenuItem[], px: number, py: number, depth: number) => {
    while (layers.length > depth) layers.pop()!.remove();
    const el = h("div", { class: "ctx-menu", role: "menu" });
    for (const it of list) {
      if ("sep" in it) { el.append(h("div", { class: "ctx-sep" })); continue; }
      if ("title" in it) { el.append(h("div", { class: "ctx-title" }, it.title)); continue; }
      const row = h("div", { class: `ctx-item${it.disabled ? " disabled" : ""}${it.sub ? " has-sub" : ""}`, role: "menuitem", tabindex: "-1" },
        h("span", { class: "ctx-label" }, it.label), h("span", { class: "ctx-hint" }, it.sub ? "▸" : it.hint ?? ""));
      if (!it.disabled) {
        row.addEventListener("mouseenter", () => {
          for (const r of el.querySelectorAll(".ctx-item.on")) r.classList.remove("on");
          row.classList.add("on");
          if (it.sub) { const b = row.getBoundingClientRect(); build(it.sub, b.right - 2, b.top - 4, depth + 1); }
          else while (layers.length > depth + 1) layers.pop()!.remove();
        });
        row.addEventListener("click", (e) => {
          e.stopPropagation();
          if (it.sub) return;
          close();
          it.run?.();
        });
      }
      el.append(row);
    }
    document.body.append(el);
    layers.push(el);
    // 화면 밖으로 나가지 않게
    const r = el.getBoundingClientRect();
    const left = px + r.width > window.innerWidth - 4 ? Math.max(4, (depth ? px - r.width - layers[depth - 1].offsetWidth + 4 : window.innerWidth - r.width - 4)) : px;
    const top = py + r.height > window.innerHeight - 4 ? Math.max(4, window.innerHeight - r.height - 4) : py;
    el.style.left = `${left}px`;
    el.style.top = `${top}px`;
  };

  const keys = (e: KeyboardEvent) => {
    const layer = layers[layers.length - 1];
    if (!layer) return;
    const rows = [...layer.querySelectorAll<HTMLElement>(".ctx-item:not(.disabled)")];
    const at = rows.findIndex((r) => r.classList.contains("on"));
    const focus = (i: number) => { rows.forEach((r) => r.classList.remove("on")); rows[i]?.classList.add("on"); };
    if (e.key === "Escape") { e.preventDefault(); e.stopPropagation(); if (layers.length > 1) layers.pop()!.remove(); else close(); }
    else if (e.key === "ArrowDown") { e.preventDefault(); focus((at + 1) % rows.length); }
    else if (e.key === "ArrowUp") { e.preventDefault(); focus((at - 1 + rows.length) % rows.length); }
    else if (e.key === "ArrowRight" && at >= 0 && rows[at].classList.contains("has-sub")) { e.preventDefault(); rows[at].dispatchEvent(new MouseEvent("mouseenter")); const sub = layers[layers.length - 1]; sub.querySelector(".ctx-item:not(.disabled)")?.classList.add("on"); }
    else if (e.key === "ArrowLeft" && layers.length > 1) { e.preventDefault(); layers.pop()!.remove(); }
    else if (e.key === "Enter" && at >= 0) { e.preventDefault(); rows[at].click(); }
    else return;
  };

  build(items, x, y, 0);
  window.addEventListener("mousedown", outside, true);
  window.addEventListener("keydown", keys, true);
  window.addEventListener("blur", close);
  window.addEventListener("resize", close);
  current = { close };
}
