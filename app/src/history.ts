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
