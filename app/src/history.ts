// SQL 실행 이력 창 (F8) — 실행한 문장을 찾아 다시 쓴다.
//
// 거르기 칸: 낱말 여러 개(모두 들어간 것) · ↑↓: 고르기 · Enter: 편집기에 넣기 · 더블클릭: 넣기
// 같은 문장(공백 차이 무시)은 가장 최근 하나로 묶고 횟수를 보인다. 기록은 이 PC 에만 있다.

import { invoke } from "@tauri-apps/api/core";
import { errOf } from "./api";
import { h, modal, toast } from "./ui";

interface Row { t: number; profile: string; user: string; sql: string; kind: string; ms: number; rows?: number | null; error?: string | null; count: number }

export interface HistoryHost {
  /** 지금 탭의 접속 프로필 (없으면 undefined) */
  profile?: string;
  insert: (sql: string) => void;
  newTab: (sql: string) => void;
}

const two = (n: number) => String(n).padStart(2, "0");
function when(t: number) {
  const d = new Date(t * 1000), now = new Date();
  const hm = `${two(d.getHours())}:${two(d.getMinutes())}`;
  return d.toDateString() === now.toDateString() ? hm : `${two(d.getMonth() + 1)}-${two(d.getDate())} ${hm}`;
}

/** 왼쪽 패널의 실행 이력 — 좁은 폭에 맞춰 한 건을 두 줄로 (때·접속·성공 / 문장).
 *  클릭: 고르기(전체 문장은 마우스를 올리면) · 더블클릭·Enter: 편집기에 넣기 · 우클릭: 넣기·새 탭·복사.
 *  넓게 보려면 F8 의 이력 창 */
export interface HistoryPanelHost {
  /** 지금 탭의 접속 프로필 — 탭을 바꾸면 달라진다 */
  profile: () => string | undefined;
  insert: (sql: string) => void;
  newTab: (sql: string) => void;
  menu: (x: number, y: number, sql: string) => void;
}

export function historyPanel(host: HistoryPanelHost) {
  const q = h("input", { placeholder: "이력 찾기 (낱말 여러 개)" }) as HTMLInputElement;
  const only = h("input", { type: "checkbox" }) as HTMLInputElement;
  const onlyLabel = h("span", {}, "이 접속만");
  const info = h("span", { class: "hist-info" });
  const list = h("div", { class: "side-hist-list", tabindex: "0" });
  let rows: Row[] = [];
  let sel = -1;
  let seq = 0;
  const sqlOf = (r: Row) => r.sql.trim().replace(/;?\s*$/, ";");
  const show = () => {
    list.replaceChildren(...rows.map((r, i) => {
      const el = h("div", { class: `side-hist${i === sel ? " sel" : ""}${r.error ? " err" : ""}`,
        title: `${r.sql}\n\n${r.user}@${r.profile} · ${new Date(r.t * 1000).toLocaleString()} · ${r.ms.toLocaleString()} ms${r.rows != null ? ` · ${r.rows.toLocaleString()}행` : ""}${r.error ? `\n${r.error}` : ""}` },
        h("div", { class: "side-hist-meta" }, h("span", { class: "hist-s" }, r.error ? "✗" : "✓"), when(r.t), " · ", r.profile,
          r.count > 1 ? h("span", { class: "hist-n" }, ` ×${r.count}`) : ""),
        h("div", { class: "side-hist-sql" }, r.sql.replace(/\s+/g, " ").slice(0, 160)));
      el.onclick = () => { sel = i; show(); };
      el.ondblclick = () => host.insert(sqlOf(r));
      el.oncontextmenu = (e) => { e.preventDefault(); sel = i; show(); host.menu(e.clientX, e.clientY, sqlOf(r)); };
      return el;
    }));
    if (!rows.length) list.append(h("div", { class: "empty" }, q.value.trim() ? "맞는 이력이 없습니다" : "실행한 SQL 이 여기에 쌓입니다"));
    list.querySelector(".sel")?.scrollIntoView({ block: "nearest" });
  };
  const load = async () => {
    const my = ++seq;
    const prof = host.profile();
    only.disabled = !prof;
    onlyLabel.textContent = prof ? `${prof} 만` : "이 접속만";
    try {
      const r = await invoke<Row[]>("history_search", { query: q.value, profile: only.checked && prof ? prof : null, limit: 200 });
      if (my !== seq) return;
      rows = r;
      sel = Math.min(sel, rows.length - 1);
      info.textContent = rows.length ? `${rows.length}${rows.length >= 200 ? "+" : ""}` : "";
      show();
    } catch (e) {
      list.replaceChildren(h("div", { class: "err" }, errOf(e).message));
    }
  };
  let timer = 0;
  q.addEventListener("input", () => { clearTimeout(timer); timer = window.setTimeout(load, 150); });
  only.onchange = load;
  // 목록·찾기 칸에서 ↑↓ 고르기, Enter 넣기
  const keys = (e: KeyboardEvent) => {
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault();
      sel = Math.max(0, Math.min(rows.length - 1, sel + (e.key === "ArrowDown" ? 1 : -1)));
      show();
    } else if (e.key === "Enter" && rows[sel]) {
      e.preventDefault();
      host.insert(sqlOf(rows[sel]));
    }
  };
  q.addEventListener("keydown", keys);
  list.addEventListener("keydown", keys);
  const el = h("div", { class: "side-pane side-hist-pane" },
    h("div", { class: "side-hist-bar" }, q, h("button", { class: "small", title: "다시 읽기", onclick: () => void load() }, "↻")),
    h("div", { class: "side-hist-opts" }, h("label", { class: "check", title: "지금 탭의 접속에서 실행한 것만" }, only, onlyLabel), info),
    list,
    h("small", { class: "hint" }, "더블클릭·Enter: 넣기 · 우클릭: 메뉴 · F8: 넓게 보기"));
  return { el, load };
}

export async function openHistory(host: HistoryHost) {
  const q = h("input", { placeholder: "찾기 — 낱말 여러 개 (예: orders cancel)", class: "hist-q" });
  const only = h("input", { type: "checkbox", checked: !!host.profile });
  const info = h("span", { class: "hist-info" });
  const list = h("div", { class: "hist-list", tabindex: "-1" });
  const preview = h("pre", { class: "hist-preview" });
  let rows: Row[] = [];
  let sel = 0;
  let seq = 0;

  const show = () => {
    list.replaceChildren(...rows.map((r, i) => {
      const el = h("div", { class: `hist-row${i === sel ? " sel" : ""}${r.error ? " err" : ""}`, title: r.error ?? "" },
        h("span", { class: "hist-t" }, when(r.t)),
        h("span", { class: "hist-p" }, r.profile),
        h("span", { class: "hist-s" }, r.error ? "✗" : "✓"),
        h("span", { class: "hist-sql" }, r.sql.replace(/\s+/g, " ").slice(0, 200)),
        r.count > 1 ? h("span", { class: "hist-n", title: `${r.count}번 실행` }, `×${r.count}`) : "");
      el.onclick = () => { sel = i; show(); };
      el.ondblclick = () => { sel = i; pick(); };
      return el;
    }));
    const r = rows[sel];
    preview.textContent = r ? r.sql : "";
    preview.title = r ? `${r.user}@${r.profile} · ${new Date(r.t * 1000).toLocaleString()} · ${r.ms.toLocaleString()} ms${r.rows != null ? ` · ${r.rows.toLocaleString()}행` : ""}${r.error ? `\n${r.error}` : ""}` : "";
    list.querySelector(".sel")?.scrollIntoView({ block: "nearest" });
  };

  const load = async () => {
    const my = ++seq;
    try {
      const r = await invoke<Row[]>("history_search", { query: q.value, profile: only.checked ? host.profile ?? null : null, limit: 300 });
      if (my !== seq) return;
      rows = r;
      sel = 0;
      info.textContent = rows.length ? `${rows.length}개${rows.length >= 300 ? "+" : ""}` : "없음";
      show();
    } catch (e) {
      toast(errOf(e).message, "error");
    }
  };

  let dlg: HTMLDialogElement | null = null;
  const pick = () => dlg?.querySelector<HTMLButtonElement>(".modal-buttons button.primary")?.click();
  let timer = 0;
  q.addEventListener("input", () => { clearTimeout(timer); timer = window.setTimeout(load, 150); });
  q.addEventListener("keydown", (e) => {
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault();
      sel = Math.max(0, Math.min(rows.length - 1, sel + (e.key === "ArrowDown" ? 1 : -1)));
      show();
    }
  });
  only.onchange = load;

  const body = h("div", { class: "hist" },
    h("div", { class: "hist-bar" }, q, h("label", { class: "check", title: "지금 탭의 접속에서 실행한 것만" }, only, host.profile ? `${host.profile} 만` : "이 접속만"), info),
    h("div", { class: "hist-main" }, list, preview),
    h("small", { class: "hint" }, "↑↓ 고르기 · Enter·더블클릭: 편집기에 넣기 · 기록은 이 PC 에만 남습니다 (설정 > 편집기에서 끄기·지우기)"));
  if (!host.profile) only.disabled = true;
  const done = modal("SQL 실행 이력 (F8)", body, [
    { label: "닫기", value: "close" },
    { label: "새 탭에서 열기", value: "tab" },
    { label: "넣기", value: "insert", kind: "primary" },
  ], { wide: true });
  dlg = body.closest("dialog");
  await load();
  q.focus();
  const act = await done;
  const r = rows[sel];
  if (!r) return;
  if (act === "insert") host.insert(r.sql.trim().replace(/;?\s*$/, ";"));
  if (act === "tab") host.newTab(r.sql.trim());
}
