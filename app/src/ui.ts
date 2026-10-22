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
export function modal<T>(title: string, body: Node, buttons: Button<T>[], opts: { wide?: boolean; cls?: string } = {}): Promise<T | null> {
  return new Promise((resolve) => {
    const dlg = h("dialog", { class: ["modal", opts.wide ? "wide" : "", opts.cls ?? ""].filter(Boolean).join(" ") });
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

/**
 * 바인드 변수 입력. 이름마다 최근 값 10개를 이 PC 에 남겨 두고(다시 켜도 남음) 마지막 값을 채운다.
 * 칸을 누르면 최근 값 목록에서 고른다. 빈 칸 + NULL 체크 = NULL.
 */
const BIND_KEY = "binds.history";
const BIND_KEEP = 10;
type BindHist = Record<string, (string | null)[]>;
function loadBinds(): BindHist {
  try { const v = JSON.parse(localStorage.getItem(BIND_KEY) ?? "{}"); return v && typeof v === "object" ? v : {}; } catch { return {}; }
}
function saveBinds(hist: BindHist) {
  // 이름이 너무 많아지면 오래된 것부터 (최근에 쓴 이름이 뒤에 온다)
  const names = Object.keys(hist);
  for (const n of names.slice(0, Math.max(0, names.length - 300))) delete hist[n];
  try { localStorage.setItem(BIND_KEY, JSON.stringify(hist)); } catch { /* 저장 못 해도 실행은 된다 */ }
}
let bindSeq = 0;
export async function bindBox(names: string[]): Promise<[string, string | null][] | null> {
  const hist = loadBinds();
  const rows = names.map((n) => {
    const past = hist[n] ?? [];
    const last = past[0];
    const listId = `bind-hist-${++bindSeq}`;
    const v = h("input", { value: last ?? "", placeholder: past.length ? "값 (칸을 눌러 최근 값 고르기)" : "값" });
    v.setAttribute("list", listId);
    const list = h("datalist", { id: listId }, ...past.filter((x): x is string => x != null && x !== "").map((x) => h("option", { value: x })));
    const isNull = h("input", { type: "checkbox", checked: past.length > 0 && last === null });
    v.addEventListener("input", () => { if (v.value) isNull.checked = false; });
    return { n, v, isNull, el: h("div", { class: "bind-row" }, h("code", {}, `:${n}`), v, list, h("label", {}, isNull, "NULL")) };
  });
  const r = await modal("바인드 변수", h("div", { class: "bind-list" }, ...rows.map((x) => x.el)), [
    { label: "취소", value: false },
    { label: "실행", value: true, kind: "primary" },
  ]);
  if (!r) return null;
  const out = rows.map(({ n, v, isNull }): [string, string | null] => [n, isNull.checked ? null : v.value]);
  for (const [n, val] of out) {
    const prev = (hist[n] ?? []).filter((x) => x !== val);
    delete hist[n];
    hist[n] = [val, ...prev].slice(0, BIND_KEEP);
  }
  saveBinds(hist);
  return out;
}

export type ThemePref = "system" | "light" | "dark";

/** 화면 테마 — 이 PC 에 기억한다. 시스템 따름이면 Windows 설정이 바뀔 때 따라간다 */
export function themePref(): ThemePref {
  try {
    const v = localStorage.getItem("ui.theme");
    if (v === "light" || v === "dark" || v === "system") return v;
  } catch { /* 기본 */ }
  return "system";
}

export type EditorColorKey = "sel" | "match";
const EDCOLOR_VAR: Record<EditorColorKey, string> = { sel: "--editor-sel", match: "--editor-sel-match" };

const currentTheme = () => (document.documentElement.dataset.theme === "dark" ? "dark" : "light");

/** 편집기 선택 색 — 밝게·어둡게마다 따로 이 PC 에 기억한다. 없으면 null (기본 색) */
export function editorColor(key: EditorColorKey): string | null {
  try { return localStorage.getItem(`ui.edcolor.${key}.${currentTheme()}`); } catch { return null; }
}

export function setEditorColor(key: EditorColorKey, value: string | null) {
  try {
    const k = `ui.edcolor.${key}.${currentTheme()}`;
    if (value) localStorage.setItem(k, value);
    else localStorage.removeItem(k);
  } catch { /* 기억 못 해도 된다 */ }
  applyEditorColors();
}

/** 기억한 색을 화면에 — 없으면 스타일 시트의 기본 색 */
export function applyEditorColors() {
  const root = document.documentElement;
  for (const key of ["sel", "match"] as const) {
    const v = editorColor(key);
    if (v && /^#[0-9a-f]{6}$/i.test(v)) root.style.setProperty(EDCOLOR_VAR[key], v);
    else root.style.removeProperty(EDCOLOR_VAR[key]);
  }
}

/** 지금 적용 중인 색 (#rrggbb) — 색 고르개의 처음 값 */
export function editorColorNow(key: EditorColorKey): string {
  const v = getComputedStyle(document.documentElement).getPropertyValue(EDCOLOR_VAR[key]).trim();
  return /^#[0-9a-f]{6}$/i.test(v) ? v.toLowerCase() : "#3465a4";
}

let themeWatch: MediaQueryList | null = null;

export function applyTheme(pref: ThemePref = themePref()) {
  try { localStorage.setItem("ui.theme", pref); } catch { /* 기억 못 해도 된다 */ }
  const mq = window.matchMedia("(prefers-color-scheme: dark)");
  const resolve = () => {
    const dark = pref === "dark" || (pref === "system" && mq.matches);
    document.documentElement.dataset.theme = dark ? "dark" : "light";
    applyEditorColors();
    // 창 머리(제목 표시줄)도 맞춘다 — 못 해도 화면은 바뀐다
    import("@tauri-apps/api/window").then(({ getCurrentWindow }) => getCurrentWindow().setTheme(pref === "system" ? null : pref)).catch(() => {});
  };
  if (themeWatch) themeWatch.onchange = null;
  themeWatch = mq;
  mq.onchange = () => { if (themePref() === "system") resolve(); };
  resolve();
  // 다른 창(흐름도)에도 알린다 — 같은 저장소를 본다
  window.dispatchEvent(new CustomEvent("sqls-theme"));
}

// 다른 창에서 테마를 바꾸면 따라간다
window.addEventListener("storage", (e) => {
  if (e.key === "ui.theme") applyTheme(themePref());
  else if (e.key?.startsWith("ui.edcolor.")) applyEditorColors();
});

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
