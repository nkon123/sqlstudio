// 명령 검색 (Ctrl+Shift+P) — 프로그램의 모든 명령을 이름으로 찾아 실행한다. 단축키도 같이 보인다.

import { h } from "./ui";

export interface Command {
  group: string;
  label: string;
  keys?: string;
  /** 찾기에만 쓰는 다른 이름 (예: "commit") */
  alias?: string;
  disabled?: boolean;
  run: () => void;
}

let open: { close: () => void } | null = null;

export function openPalette(commands: Command[]) {
  open?.close();
  const q = h("input", { class: "pal-q", placeholder: "명령 찾기 (예: 실행계획, 커밋, 세션, 저장)", spellcheck: "false" }) as HTMLInputElement;
  const list = h("div", { class: "pal-list", role: "listbox" });
  const box = h("div", { class: "pal", role: "dialog" }, q, list);
  const back = h("div", { class: "pal-back" }, box);
  let shown: Command[] = [];
  let at = 0;

  const close = () => {
    back.remove();
    if (open?.close === close) open = null;
  };
  const pick = (c: Command | undefined) => {
    if (!c || c.disabled) return;
    close();
    c.run();
  };
  const mark = () => {
    list.querySelectorAll(".pal-item").forEach((el, i) => el.classList.toggle("on", i === at));
    list.querySelector(".pal-item.on")?.scrollIntoView({ block: "nearest" });
  };
  const render = () => {
    const words = q.value.trim().toLowerCase().split(/\s+/).filter(Boolean);
    shown = commands.filter((c) => {
      const hay = `${c.group} ${c.label} ${c.keys ?? ""} ${c.alias ?? ""}`.toLowerCase();
      return words.every((w) => hay.includes(w));
    });
    // 쓸 수 있는 것이 먼저, 이름이 찾는 말로 시작하는 것이 먼저
    const w0 = words[0] ?? "";
    shown.sort((a, b) => Number(!!a.disabled) - Number(!!b.disabled) || Number(!a.label.toLowerCase().startsWith(w0)) - Number(!b.label.toLowerCase().startsWith(w0)));
    at = shown.findIndex((c) => !c.disabled);
    list.replaceChildren(...(shown.length ? shown.map((c, i) => h("div", {
      class: `pal-item${c.disabled ? " disabled" : ""}`, role: "option",
      onmousemove: () => { if (at !== i && !c.disabled) { at = i; mark(); } },
      onmousedown: (e: Event) => { e.preventDefault(); pick(c); },
    }, h("span", { class: "pal-group" }, c.group), h("span", { class: "pal-label" }, c.label), c.keys ? h("kbd", {}, c.keys) : "")) : [h("div", { class: "pal-empty" }, "맞는 명령이 없습니다")]));
    mark();
  };
  const step = (d: number) => {
    if (!shown.length) return;
    for (let i = 0, k = at; i < shown.length; i++) {
      k = (k + d + shown.length) % shown.length;
      if (!shown[k].disabled) { at = k; break; }
    }
    mark();
  };
  q.addEventListener("input", render);
  q.addEventListener("keydown", (e) => {
    if (e.isComposing) return;
    if (e.key === "ArrowDown") { e.preventDefault(); step(1); }
    else if (e.key === "ArrowUp") { e.preventDefault(); step(-1); }
    else if (e.key === "Enter") { e.preventDefault(); pick(shown[at]); }
    else if (e.key === "Escape") { e.preventDefault(); e.stopPropagation(); close(); }
  });
  back.addEventListener("mousedown", (e) => { if (e.target === back) close(); });
  document.body.append(back);
  open = { close };
  render();
  q.focus();
}
