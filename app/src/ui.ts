// 작은 DOM 도우미와 대화상자. 프레임워크 없이 — 기동이 빠르고 의존성이 적다.

type Child = Node | string | null | undefined | false;
type Props = Record<string, unknown> & { class?: string; style?: string };

export function h<K extends keyof HTMLElementTagNameMap>(tag: K, props: Props = {}, ...children: Child[]): HTMLElementTagNameMap[K] {
  const el = document.createElement(tag);
  // 폼 안의 <button> 은 기본이 submit 이다 — Enter 가 엉뚱한 버튼을 누르지 않게 기본을 button 으로
  if (tag === "button" && !("type" in props)) (el as HTMLButtonElement).type = "button";
  for (const [k, v] of Object.entries(props)) {
    if (v == null || v === false) continue;
    if (k === "class") el.className = String(v);
    else if (k === "style") el.setAttribute("style", String(v));
    else if (k.startsWith("on") && typeof v === "function") el.addEventListener(k.slice(2), v as EventListener);
    else if (k in el) (el as unknown as Record<string, unknown>)[k] = v;
    else el.setAttribute(k, String(v));
  }
  for (const c of children) if (c != null && c !== false) el.append(c);
  return el;
}

export interface Button<T> { label: string; value: T; kind?: "primary" | "danger" }

/** 모달. 버튼 값을 돌려준다 (Esc 는 null). */
export function modal<T>(title: string, body: Node, buttons: Button<T>[], opts: { wide?: boolean } = {}): Promise<T | null> {
  return new Promise((resolve) => {
    const dlg = h("dialog", { class: opts.wide ? "modal wide" : "modal" });
    let result: T | null = null;
    const bar = h("div", { class: "modal-buttons" },
      ...buttons.map((b) =>
        // type=button: 폼 안의 버튼은 기본이 submit 이라, Enter 가 첫 버튼(대개 "취소")을 누르게 된다
        h("button", {
          type: "button",
          class: b.kind ?? "",
          onclick: (e: Event) => { e.preventDefault(); result = b.value; dlg.close(); },
        }, b.label),
      ),
    );
    const form = h("form", { method: "dialog", onsubmit: (e: Event) => {
      e.preventDefault();
      const primary = buttons.find((b) => b.kind === "primary") ?? buttons[0];
      result = primary.value;
      dlg.close();
    } }, h("h3", {}, title), body, bar);
    dlg.append(form);
    // 입력칸에서 Enter = 기본 버튼 (입력칸이 여러 개여도)
    dlg.addEventListener("keydown", (e) => {
      const t = e.target as HTMLElement;
      if (e.key === "Enter" && !e.isComposing && t.tagName === "INPUT" && (t as HTMLInputElement).type !== "checkbox") {
        e.preventDefault();
        const primary = buttons.find((b) => b.kind === "primary") ?? buttons[0];
        result = primary.value;
        dlg.close();
      }
    });
    dlg.addEventListener("close", () => { dlg.remove(); resolve(result); });
    document.body.append(dlg);
    dlg.showModal();
    const first = dlg.querySelector<HTMLElement>("input, textarea, select");
    (first ?? bar.querySelector("button.primary") as HTMLElement | null)?.focus();
  });
}

export async function alertBox(title: string, text: string) {
  await modal(title, h("pre", { class: "modal-text" }, text), [{ label: "확인", value: true, kind: "primary" }]);
}

export async function confirmBox(title: string, text: string, okLabel = "실행", danger = true): Promise<boolean> {
  const r = await modal(title, h("pre", { class: "modal-text" }, text), [
    { label: "취소", value: false },
    { label: okLabel, value: true, kind: danger ? "danger" : "primary" },
  ]);
  return r === true;
}

export async function passwordBox(profile: string, user: string): Promise<{ password: string; remember: boolean } | null> {
  const input = h("input", { type: "password", autocomplete: "off", placeholder: "비밀번호" });
  const remember = h("input", { type: "checkbox" });
  const body = h("div", {}, h("label", { class: "field" }, `${user} 비밀번호`, input),
    h("label", { class: "check", title: "Windows 자격 증명 관리자(macOS 키체인)에 저장합니다. 설정 파일에는 쓰지 않습니다." },
      remember, "비밀번호 저장 (OS 자격 증명 관리자)"));
  const r = await modal(`${profile} 접속`, body, [
    { label: "취소", value: false },
    { label: "접속", value: true, kind: "primary" },
  ]);
  return r ? { password: input.value, remember: remember.checked } : null;
}

/** 바인드 변수 입력. 지난 값을 기억해 채워 준다. 빈 칸 + NULL 체크 = NULL. */
const lastBinds: Record<string, string | null> = {};
export async function bindBox(names: string[]): Promise<[string, string | null][] | null> {
  const rows = names.map((n) => {
    const v = h("input", { value: lastBinds[n] ?? "", placeholder: "값" });
    const isNull = h("input", { type: "checkbox", checked: lastBinds[n] === null });
    return { n, v, isNull, el: h("div", { class: "bind-row" }, h("code", {}, `:${n}`), v, h("label", {}, isNull, "NULL")) };
  });
  const r = await modal("바인드 변수", h("div", { class: "bind-list" }, ...rows.map((x) => x.el)), [
    { label: "취소", value: false },
    { label: "실행", value: true, kind: "primary" },
  ]);
  if (!r) return null;
  return rows.map(({ n, v, isNull }) => {
    const val = isNull.checked ? null : v.value;
    lastBinds[n] = val;
    return [n, val];
  });
}

export function toast(text: string, kind: "info" | "error" | "ok" = "info", ms = 3500) {
  let host = document.getElementById("toasts");
  if (!host) {
    host = h("div", { id: "toasts" });
    document.body.append(host);
  }
  const t = h("div", { class: `toast ${kind}` }, text);
  host.append(t);
  setTimeout(() => t.remove(), ms);
}

export function field(label: string, input: HTMLElement, hint?: string) {
  return h("label", { class: "field" }, h("span", {}, label), input, hint ? h("small", {}, hint) : null);
}
