// 화면 배치 — 왼쪽(테이블·프로젝트) · 가운데 · 오른쪽(AI) 3단과 경계선 끌기 (main.ts 에서 나눔)

import { h } from "./ui";

export class SideLayout {
  private w = { left: 240, right: 360 };
  private open = { left: true, right: true };

  constructor(private root: HTMLElement, left: HTMLElement, center: HTMLElement, right: HTMLElement,
    private onChange: (side: "left" | "right", open: boolean) => void) {
    try {
      const s = JSON.parse(localStorage.getItem("layout.sides") ?? "{}");
      if (s.w) this.w = { ...this.w, ...s.w };
      if (s.open) this.open = { ...this.open, ...s.open };
    } catch { /* 기본값 */ }
    root.append(left, this.splitter("left"), center, this.splitter("right"), right);
    this.apply();
  }

  toggle(side: "left" | "right", open = !this.open[side]) {
    this.open[side] = open;
    this.apply();
  }

  private splitter(side: "left" | "right") {
    const btn = h("button", { class: "vsplit-btn", title: "접기/펼치기 (경계선 더블클릭도 됨)", onclick: (e: Event) => { e.stopPropagation(); this.toggle(side); } });
    const el = h("div", { class: `vsplit ${side}`, title: "끌어서 너비 조절 · 더블클릭: 접기/펼치기" }, btn);
    el.addEventListener("dblclick", () => this.toggle(side));
    el.addEventListener("mousedown", (e) => {
      if (e.target === btn) return;
      e.preventDefault();
      const x0 = e.clientX;
      const w0 = this.open[side] ? this.w[side] : 0;
      document.body.classList.add("col-resizing");
      const move = (ev: MouseEvent) => {
        const dx = side === "left" ? ev.clientX - x0 : x0 - ev.clientX;
        const max = Math.max(200, Math.min(800, window.innerWidth * 0.5));
        const nw = w0 + dx;
        if (nw < 80) {
          this.open[side] = false;
        } else {
          this.open[side] = true;
          this.w[side] = Math.round(Math.min(max, Math.max(160, nw)));
        }
        this.apply(false);
      };
      const up = () => {
        document.body.classList.remove("col-resizing");
        window.removeEventListener("mousemove", move);
        window.removeEventListener("mouseup", up);
        this.apply();
      };
      window.addEventListener("mousemove", move);
      window.addEventListener("mouseup", up);
    });
    return el;
  }

  private apply(save = true) {
    const l = this.open.left ? `${this.w.left}px` : "0px";
    const r = this.open.right ? `${this.w.right}px` : "0px";
    this.root.style.gridTemplateColumns = `${l} 6px minmax(0, 1fr) 6px ${r}`;
    this.root.classList.toggle("left-closed", !this.open.left);
    this.root.classList.toggle("right-closed", !this.open.right);
    for (const s of ["left", "right"] as const) {
      const b = this.root.querySelector(`.vsplit.${s} .vsplit-btn`);
      // 펼쳐져 있으면 접는 방향, 접혀 있으면 펼치는 방향
      if (b) b.textContent = (s === "left") === this.open[s] ? "‹" : "›";
      this.onChange(s, this.open[s]);
    }
    if (save) localStorage.setItem("layout.sides", JSON.stringify({ w: this.w, open: this.open }));
  }
}

export function dragSplit(handle: HTMLElement, top: HTMLElement) {
  handle.addEventListener("mousedown", (e) => {
    e.preventDefault();
    const y0 = e.clientY;
    const h0 = top.getBoundingClientRect().height;
    const total = (top.parentElement?.getBoundingClientRect().height ?? 600) - handle.getBoundingClientRect().height;
    document.body.classList.add("row-resizing");
    // 위 80px·아래 90px 은 남긴다 — 어느 쪽으로든 끌어서 다시 되돌릴 수 있게 (경계선이 화면 밖으로 나가지 않게)
    const move = (ev: MouseEvent) => {
      const px = Math.max(80, Math.min(total - 90, h0 + ev.clientY - y0));
      top.style.flexBasis = `${(px / total) * 100}%`;
    };
    const up = () => {
      window.removeEventListener("mousemove", move);
      window.removeEventListener("mouseup", up);
      document.body.classList.remove("row-resizing");
      try { localStorage.setItem("pane.editor", String(Math.round((top.getBoundingClientRect().height / total) * 100))); } catch { /* 기억 못 해도 된다 */ }
    };
    window.addEventListener("mousemove", move);
    window.addEventListener("mouseup", up);
  });
}
