// 왼쪽 '개요' — 지금 문서의 패키지·프로시저·함수·커서·타입 목록 (Ctrl+Shift+O 를 늘 펼쳐 둔 것).
// 커서가 있는 단위를 칠하고, 누르면 그 자리로 간다. 구조는 codenav(plsqlnav) 가 읽는다.

import type { EditorView } from "@codemirror/view";
import { navJump, navOf, watchNav } from "./codenav";
import { KIND_LABEL, navOutline } from "./plsqlnav";
import { h } from "./ui";

export interface OutlinePanel {
  el: HTMLElement;
  /** 보여 줄 편집기 (탭을 바꾸면) */
  attach(view: EditorView | null): void;
  /** 패널이 보이는지 — 숨었을 때는 다시 그리지 않는다 */
  setVisible(v: boolean): void;
}

/** 짧은 종류 표시 */
const BADGE: Record<string, string> = {
  package: "PKG", "package body": "BODY", "type body": "TYPE", procedure: "PROC", function: "FN", trigger: "TRG", type: "TYPE", cursor: "CUR",
};

/** onState: 이 문서에 단위가 있는지 바뀌면 (메뉴 항목을 흐리게) */
export function outlinePanel(onState?: (has: boolean) => void): OutlinePanel {
  const q = h("input", { placeholder: "이름 거르기" }) as HTMLInputElement;
  const info = h("span", { class: "outline-info" });
  const list = h("div", { class: "outline-list" });
  const el = h("div", { class: "side-pane outline-pane" },
    h("div", { class: "outline-bar" }, q, info),
    list,
    h("small", { class: "hint" }, "누르면 그 자리로 · Ctrl+Shift+O 찾아 가기 · Alt+← 돌아오기"));
  let view: EditorView | null = null;
  let visible = false;
  let timer = 0;
  let sig = "";
  let has: boolean | null = null;

  const draw = () => {
    timer = 0;
    const doc = view?.state.doc;
    const items = doc ? navOutline(navOf(doc).tree) : [];
    const now = items.length > 0;
    if (now !== has) { has = now; onState?.(now); }
    if (!visible) return;
    const here = view?.state.selection.main.head ?? -1;
    // 커서를 감싼 가장 안쪽 단위
    let cur = -1;
    items.forEach((x, i) => { if (x.node.from <= here && here <= x.node.to) cur = i; });
    const words = q.value.trim().toLowerCase().split(/\s+/).filter(Boolean);
    const shown = items.map((x, i) => ({ ...x, i })).filter((x) => words.every((w) => `${x.node.name} ${KIND_LABEL[x.node.kind]}`.toLowerCase().includes(w)));
    const s = `${doc?.length}|${items.map((x) => `${x.node.kind}${x.node.name}${x.node.from}`).join(",")}|${cur}|${q.value}`;
    if (s === sig) return;
    sig = s;
    info.textContent = items.length ? `${items.length}개` : "";
    if (!view) { list.replaceChildren(h("div", { class: "empty" }, "열린 문서가 없습니다")); return; }
    if (!items.length) {
      list.replaceChildren(h("div", { class: "empty" }, "이 문서에는 PACKAGE · PROCEDURE · FUNCTION 이 없습니다", h("br"), "편집기에서 프로시저·패키지 이름 위에 F4 를 누르면 소스가 열리고 여기에 목록이 나옵니다"));
      return;
    }
    const v = view;
    list.replaceChildren(...(shown.length ? shown.map(({ node, depth, i }) => h("div", {
      class: `outline-item${i === cur ? " on" : ""}${node.decl && (node.kind === "procedure" || node.kind === "function") ? " decl" : ""}`,
      style: `padding-left:${8 + depth * 14}px`,
      title: `${KIND_LABEL[node.kind]} ${node.name} — ${v.state.doc.lineAt(node.from).number}행${node.decl ? " (선언)" : ""}`,
      onmousedown: (e: Event) => { e.preventDefault(); navJump(v, node.from); },
    }, h("span", { class: `outline-kind k-${node.kind.replace(" ", "-")}` }, BADGE[node.kind] ?? ""), h("span", { class: "outline-name" }, node.name),
      h("span", { class: "outline-line" }, String(v.state.doc.lineAt(node.from).number)))) : [h("div", { class: "empty" }, "맞는 이름이 없습니다")]));
    list.querySelector(".on")?.scrollIntoView({ block: "nearest" });
  };
  const later = (ms: number) => { if (timer) clearTimeout(timer); timer = window.setTimeout(draw, ms); };

  q.addEventListener("input", () => draw());
  q.addEventListener("keydown", (e) => {
    if (e.key === "Escape") { q.value = ""; draw(); view?.focus(); }
    else if (e.key === "Enter") {
      // 거른 것 중 첫째로
      (list.querySelector(".outline-item") as HTMLElement | null)?.dispatchEvent(new MouseEvent("mousedown"));
    }
  });
  watchNav((v, docChanged) => { if (v === view) later(docChanged ? 300 : 80); });

  return {
    el,
    attach(v) { view = v; sig = ""; later(0); },
    setVisible(x) { visible = x; if (x) { sig = ""; draw(); } },
  };
}
