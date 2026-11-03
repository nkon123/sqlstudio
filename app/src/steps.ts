// 하네스 단계 칩 — 답 위에 "무엇을 모아 보냈고, 무엇을 건너뛰었고, 답을 어떻게 확인했는지" 를 한 줄로.
// 칩을 누르면 그 단계의 자세한 줄이 아래에 펼쳐진다. 일반 AI 와 ACP 에이전트가 같이 쓴다.

import { h } from "./ui";

/** run: 하는 중 · ok: 끝남 · skip: 건너뜀 · warn: 됐지만 알릴 것 · fail: 실패 */
export type StepState = "run" | "ok" | "skip" | "warn" | "fail";
export interface Step { id: string; state: StepState; label: string; summary: string; detail: string[] }

const MARK: Record<StepState, string> = { run: "", ok: "✓", skip: "–", warn: "!", fail: "✕" };

export class StepStrip {
  readonly el = h("div", { class: "ai-steps" });
  private chips = h("div", { class: "ai-step-chips", role: "list", "aria-label": "하네스 단계" });
  private more = h("div", { class: "ai-step-detail", hidden: true });
  private steps = new Map<string, { s: Step; chip: HTMLButtonElement }>();
  private open: string | null = null;

  /** order: 칩을 늘어놓을 순서 (단계가 오는 순서와 달라도 — 없는 id 는 뒤에) */
  constructor(private order: string[] = []) {
    this.el.append(this.chips, this.more);
  }

  private rank(id: string) {
    const i = this.order.indexOf(id);
    return i < 0 ? this.order.length : i;
  }

  /** 같은 id 면 덮어쓴다 (run → ok) */
  set(s: Step) {
    let e = this.steps.get(s.id);
    if (!e) {
      const chip = h("button", { class: "ai-step", type: "button", role: "listitem" }) as HTMLButtonElement;
      chip.addEventListener("click", () => this.toggle(s.id));
      chip.dataset.id = s.id;
      const after = [...this.chips.children].find((c) => this.rank((c as HTMLElement).dataset.id ?? "") > this.rank(s.id));
      this.chips.insertBefore(chip, after ?? null);
      e = { s, chip };
      this.steps.set(s.id, e);
    }
    e.s = s;
    e.chip.dataset.state = s.state;
    e.chip.replaceChildren(h("span", { class: "ai-step-mark", "aria-hidden": "true" }, MARK[s.state]), h("b", {}, s.label));
    if (s.summary) e.chip.append(h("span", {}, s.summary));
    e.chip.title = s.detail.join("\n");
    e.chip.setAttribute("aria-label", `${s.label} ${s.summary} (${STATE_NAME[s.state]})`);
    if (this.open === s.id) this.showDetail(s);
  }

  /** 하는 중인 단계를 끝난 것으로 (취소·실패로 끝날 때 돌아가는 표시가 남지 않게) */
  settle(state: StepState = "skip", summary = "멈춤") {
    for (const { s } of this.steps.values()) if (s.state === "run") this.set({ ...s, state, summary });
  }

  get(id: string): Step | undefined {
    return this.steps.get(id)?.s;
  }

  private toggle(id: string) {
    for (const [k, e] of this.steps) e.chip.setAttribute("aria-expanded", String(k === id && this.open !== id));
    if (this.open === id) {
      this.open = null;
      this.more.hidden = true;
      return;
    }
    this.open = id;
    const e = this.steps.get(id);
    if (e) this.showDetail(e.s);
  }

  private showDetail(s: Step) {
    this.more.hidden = false;
    this.more.replaceChildren(
      h("b", {}, s.label),
      ...(s.detail.length ? s.detail : ["(자세한 내용 없음)"]).map((l) => h("div", {}, l)),
    );
  }
}

const STATE_NAME: Record<StepState, string> = { run: "하는 중", ok: "끝남", skip: "건너뜀", warn: "알림", fail: "실패" };
