// 오른쪽 보조 영역 — [AI] 와 테이블 상세(F4)·객체 상세 탭. 넓은 화면이면 위(상세)·아래(AI) 로 나눠 함께 본다.
//
// AI 창은 늘 아래 칸(aiHost)에 그대로 있다 — 옮기면 대화 스크롤·입력 중인 글이 흔들린다. 탭만 보기를 바꾼다:
//   나누지 않음: AI 탭이면 아래 칸만, 상세 탭이면 위 칸만 보인다
//   나눔: 상세가 있으면 위에 고른 상세, 아래에 AI (경계선을 끌어 비율). 상세가 없으면 AI 만
// 상세를 어디에 열지(F4·상세 보기): 자동(창이 넓으면 여기, 아니면 떠 있는 창) · 늘 여기 · 늘 떠 있는 창

import { showMenu } from "./ctxmenu";
import { h } from "./ui";

export interface DockItem {
  key: string;
  /** 탭 글자 (짧게) */
  title: string;
  /** 풍선 (전체 이름) */
  tip?: string;
  el: HTMLElement;
  /** 탭의 × — 그 창을 닫는다 (remove 는 부르지 않는다) */
  onClose?: () => void;
}

export type DockMode = "auto" | "dock" | "float";
const MODE_KEY = "dock.mode";
const SPLIT_KEY = "dock.split";
/** 자동: 창 폭이 이만큼이면 오른쪽에 연다 (와이드 모니터) */
const WIDE = 1600;

const MODE_LABEL: Record<DockMode, string> = { auto: "자동 — 창이 넓으면 오른쪽, 아니면 떠 있는 창", dock: "늘 오른쪽 패널", float: "늘 떠 있는 창" };

function load<T>(k: string, d: T): T {
  try {
    const v = localStorage.getItem(k);
    return v == null ? d : { ...d, ...JSON.parse(v) };
  } catch {
    return d;
  }
}

export class SideDock {
  readonly el: HTMLElement;
  private bar = h("div", { class: "dock-tabs" });
  private detailHost = h("div", { class: "dock-detail" });
  private handle = h("div", { class: "dock-hsplit", title: "끌어서 위아래 비율 · 더블클릭: 반반" });
  private aiHost = h("div", { class: "dock-ai" });
  private items: DockItem[] = [];
  /** 지금 고른 탭 ("ai" 또는 상세의 key) */
  private current = "ai";
  /** 나눴을 때 위에 보일 상세 (마지막으로 고른 것) */
  private detail = "";
  private split = load(SPLIT_KEY, { on: true, ratio: 55 });

  /** reveal: 오른쪽 영역이 접혀 있으면 펼친다 (detail: 상세를 보일 때 — 좁으면 넓힌다) */
  constructor(ai: HTMLElement, private reveal: (detail: boolean) => void) {
    this.aiHost.append(ai);
    this.el = h("aside", { class: "dock" }, this.bar, this.detailHost, this.handle, this.aiHost);
    this.handle.addEventListener("mousedown", (e) => this.dragHandle(e));
    this.handle.addEventListener("dblclick", () => { this.split.ratio = 50; this.save(); this.render(); });
    this.render();
  }

  /** 상세를 어디에 열지 */
  static mode(): DockMode {
    try {
      const m = localStorage.getItem(MODE_KEY);
      return m === "dock" || m === "float" ? m : "auto";
    } catch {
      return "auto";
    }
  }

  /** 이번에 여는 상세를 여기에 둘까 */
  static wanted(): boolean {
    const m = SideDock.mode();
    return m === "dock" || (m === "auto" && window.innerWidth >= WIDE);
  }

  has(key: string) {
    return this.items.some((x) => x.key === key);
  }

  /** 상세 탭을 더한다 (같은 key 면 바꾼다) 그리고 보인다 */
  add(item: DockItem) {
    const i = this.items.findIndex((x) => x.key === item.key);
    if (i >= 0) {
      this.items[i].el.remove();
      this.items[i] = item;
    } else {
      this.items.push(item);
    }
    this.detailHost.append(item.el);
    this.activate(item.key);
  }

  /** 그 탭을 보인다. 없으면 false */
  activate(key: string): boolean {
    if (key !== "ai" && !this.has(key)) return false;
    this.current = key;
    if (key !== "ai") this.detail = key;
    this.reveal(key !== "ai");
    this.render();
    return true;
  }

  /** 탭을 뺀다 (onClose 는 부르지 않는다 — 창이 스스로 닫거나 떼어 낼 때) */
  remove(key: string) {
    const i = this.items.findIndex((x) => x.key === key);
    if (i < 0) return;
    const [it] = this.items.splice(i, 1);
    if (it.el.parentElement === this.detailHost) it.el.remove();
    if (this.detail === key) this.detail = this.items[Math.min(i, this.items.length - 1)]?.key ?? "";
    if (this.current === key) this.current = this.detail || "ai";
    this.render();
  }

  /** AI 탭으로 (Ctrl+K 등) */
  showAi() {
    if (this.current !== "ai" && !this.isSplit()) {
      this.current = "ai";
      this.render();
    }
  }

  /** 위아래로 나눠 보이는 중인가 (나누기를 켰고 상세가 있다) */
  private isSplit() {
    return this.split.on && this.items.length > 0;
  }

  private save() {
    try { localStorage.setItem(SPLIT_KEY, JSON.stringify(this.split)); } catch { /* 기억 못 해도 된다 */ }
  }

  private render() {
    const split = this.isSplit();
    const shown = split ? this.detail || this.items[this.items.length - 1]?.key : this.current === "ai" ? "" : this.current;
    for (const it of this.items) it.el.classList.toggle("dock-off", it.key !== shown);
    const showDetail = !!shown;
    const showAi = split || this.current === "ai" || !this.items.length;
    this.el.classList.toggle("split", split);
    this.detailHost.classList.toggle("dock-off", !showDetail);
    this.aiHost.classList.toggle("dock-off", !showAi);
    this.handle.hidden = !(showDetail && showAi);
    this.detailHost.style.flex = showDetail && showAi ? `0 0 ${this.split.ratio}%` : "";

    const tab = (key: string, label: string, tip: string, on: boolean, close?: () => void) => {
      const b = h("button", { class: `dock-tab${on ? " on" : ""}`, title: tip, onclick: () => this.activate(key) }, h("span", {}, label));
      b.addEventListener("auxclick", (e) => { if (e.button === 1 && close) { e.preventDefault(); close(); } });
      if (close) b.append(h("span", { class: "dock-x", title: "닫기 (가운데 클릭도 됨)", onclick: (e: Event) => { e.stopPropagation(); close(); } }, "×"));
      return b;
    };
    const tabs: HTMLElement[] = [];
    // 나눠 보일 때는 AI 가 늘 아래에 있으니 탭이 필요 없다
    if (!split) tabs.push(tab("ai", "AI", "AI 대화 (Ctrl+K)", this.current === "ai"));
    for (const it of this.items) {
      tabs.push(tab(it.key, it.title, it.tip ?? it.title, it.key === shown, () => (it.onClose ? it.onClose() : this.remove(it.key))));
    }
    const menu = h("button", { class: "dock-menu", title: "오른쪽 패널 설정 — 위아래 나누기 · 상세를 어디에 열지" }, "⋯");
    menu.onclick = () => {
      const r = menu.getBoundingClientRect();
      const m = SideDock.mode();
      showMenu(r.left - 220, r.bottom + 2, [
        { label: `${this.split.on ? "✓ " : ""}위(상세)·아래(AI) 나눠 보기`, run: () => { this.split.on = !this.split.on; this.save(); this.render(); } },
        { sep: true },
        { title: "F4·상세 보기를 여는 곳" },
        ...(["auto", "dock", "float"] as DockMode[]).map((k) => ({
          label: `${m === k ? "✓ " : ""}${MODE_LABEL[k]}`,
          run: () => { try { localStorage.setItem(MODE_KEY, k); } catch { /* 기억 못 해도 된다 */ } },
        })),
        ...(this.items.length ? [{ sep: true as const }, { label: "상세 탭 모두 닫기", run: () => { for (const it of [...this.items]) (it.onClose ? it.onClose() : this.remove(it.key)); } }] : []),
      ]);
    };
    this.bar.replaceChildren(...tabs, h("span", { class: "spacer" }), menu);
  }

  private dragHandle(e: MouseEvent) {
    e.preventDefault();
    const box = this.el.getBoundingClientRect();
    const top = this.detailHost.getBoundingClientRect().top;
    const total = box.bottom - top;
    document.body.classList.add("row-resizing");
    const move = (ev: MouseEvent) => {
      // 위·아래 모두 120px 은 남긴다
      const px = Math.max(120, Math.min(total - 120, ev.clientY - top));
      this.split.ratio = Math.round((px / total) * 100);
      this.detailHost.style.flex = `0 0 ${this.split.ratio}%`;
    };
    const up = () => {
      document.body.classList.remove("row-resizing");
      window.removeEventListener("mousemove", move);
      window.removeEventListener("mouseup", up);
      this.save();
    };
    window.addEventListener("mousemove", move);
    window.addEventListener("mouseup", up);
  }
}
