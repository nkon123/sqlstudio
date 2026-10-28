// 은은한 진행 표시 — 얇은 막대 위로 빛나는 물결이 흐르고, 앞머리 점이 숨 쉰다.
// 얼마나 남았는지 모르는 기다림(서버 질의·모델 응답)에 쓴다. 지난 시간만 정직하게 보인다.

import { h } from "./ui";

export interface Glow {
  el: HTMLElement;
  /** 아래 글 바꾸기 */
  label(text: string): void;
  /** 멈추고 지운다 */
  done(): void;
}

export function glowProgress(text = "", opts: { compact?: boolean } = {}): Glow {
  const label = h("span", { class: "gp-label" }, text);
  const secs = h("span", { class: "gp-secs" });
  const el = h("div", { class: `glow-prog${opts.compact ? " compact" : ""}`, role: "progressbar", "aria-busy": "true" },
    h("div", { class: "gp-track" },
      h("div", { class: "gp-fill" }, h("div", { class: "gp-wave" }), h("span", { class: "gp-dot" }))),
    h("div", { class: "gp-foot" }, label, secs));
  const started = performance.now();
  const tick = window.setInterval(() => {
    const s = Math.floor((performance.now() - started) / 1000);
    secs.textContent = s >= 1 ? `${s}초` : "";
  }, 500);
  return {
    el,
    label: (t) => { label.textContent = t; },
    done: () => { clearInterval(tick); el.remove(); },
  };
}
