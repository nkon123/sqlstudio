// 접속 간 데이터 복사 — 원본 탭 접속의 테이블에서 대상 탭 접속의 테이블로 (QA → DEV 등).
// 미리 보기로 열 맞춤·건수·실행할 SQL 을 보고, 확인한 뒤 옮긴다. 커밋하지 않는다 — 대상 탭에서 커밋·롤백.

import { Channel, invoke } from "@tauri-apps/api/core";
import { api, errOf } from "./api";
import { confirmBox, h, modal, toast } from "./ui";

type Mode = "append" | "replace" | "merge";
interface ColPlan { name: string; src_type: string | null; dst_type: string | null; status: "copy" | "src_only" | "dst_only" | "unsupported"; note?: string }
interface CopyPlan {
  src: string; dst: string; columns: ColPlan[]; primary_key: string[]; keys: string[];
  src_rows: number; dst_rows: number | null; select_sql: string; write_sql: string; delete_sql: string | null;
  warnings: string[]; production: boolean;
}
interface Summary { read: number; written: number; failed: number; deleted: number; stopped: boolean; errors: [number, string][]; txn_pending: boolean; elapsed_ms: number }
type Ev = { type: "progress"; read: number; written: number; failed: number; total: number };

/** 고를 수 있는 접속 (접속한 탭마다) */
export interface ConnChoice { id: number; label: string; readOnly: boolean; production: boolean }

export interface CopyHooks {
  conns: ConnChoice[];
  srcId: number;
  srcTable?: string;
  /** 끝난 뒤 — 대상 탭의 '커밋 전' 표시 */
  done: (dstId: number, txnPending: boolean) => void;
}

const STATUS: Record<ColPlan["status"], string> = { copy: "옮김", src_only: "원본에만", dst_only: "대상에만", unsupported: "옮길 수 없음" };

export function openCopyData(hk: CopyHooks) {
  const sel = (pick: number, filter: (c: ConnChoice) => boolean) => {
    const s = h("select", {}) as HTMLSelectElement;
    for (const c of hk.conns.filter(filter)) s.append(h("option", { value: String(c.id) }, `${c.label}${c.readOnly ? " (읽기 전용)" : ""}${c.production ? " · 운영" : ""}`));
    s.value = String(pick);
    return s;
  };
  const others = hk.conns.filter((c) => c.id !== hk.srcId && !c.readOnly);
  const srcSel = sel(hk.srcId, () => true);
  const dstSel = sel(others[0]?.id ?? hk.srcId, (c) => !c.readOnly);
  const srcTable = h("input", { value: hk.srcTable ?? "", placeholder: "OWNER.TABLE 또는 TABLE", style: "width:240px" }) as HTMLInputElement;
  // 대상은 스키마를 빼고 이름만 — 대상 접속 계정의 스키마로 찾는다 (QA_APP.CUST → DEV 의 CUST)
  const dstTable = h("input", { value: (hk.srcTable ?? "").split(".").pop() ?? "", placeholder: "비우면 원본과 같은 이름 (대상 계정 스키마)", style: "width:240px" }) as HTMLInputElement;
  const cond = h("input", { placeholder: "예: deptno = 10 AND hired >= DATE '2026-01-01'  (비우면 전체)", style: "flex:1" }) as HTMLInputElement;
  const modeName = `copy-mode-${Date.now()}`;
  const radios: HTMLInputElement[] = [];
  const radio = (v: Mode, label: string, title: string) => {
    const r = h("input", { type: "radio", name: modeName, value: v }) as HTMLInputElement;
    r.checked = v === "append";
    radios.push(r);
    r.onchange = () => { plan = null; renderPlan(); };
    return h("label", { class: "rdiff-key", title }, r, label);
  };
  const mode = () => (radios.find((r) => r.checked)?.value ?? "append") as Mode;
  const skipErr = h("input", { type: "checkbox" }) as HTMLInputElement;
  skipErr.checked = true;
  const planBox = h("div", { class: "copy-plan" });
  const runBox = h("div", { class: "copy-run" });
  const previewBtn = h("button", { class: "small primary" }, "미리 보기") as HTMLButtonElement;
  const runBtn = h("button", { class: "small danger" }, "복사 시작") as HTMLButtonElement;
  const stopBtn = h("button", { class: "small", hidden: true }, "중지");
  let plan: CopyPlan | null = null;
  let picked = new Set<string>();
  let keys: string[] = [];

  const args = (confirmed = false) => ({
    src_id: Number(srcSel.value), src_table: srcTable.value.trim(),
    dst_id: Number(dstSel.value), dst_table: (dstTable.value.trim() || (srcTable.value.trim().split(".").pop() ?? "")),
    cond: cond.value.trim() || null, mode: mode(), keys: mode() === "merge" ? keys : [],
    columns: plan ? [...picked] : [], skip_errors: skipErr.checked, confirmed,
  });

  const renderPlan = () => {
    runBtn.disabled = !plan;
    if (!plan) { planBox.replaceChildren(h("p", { class: "hint" }, "조건과 방식을 고르고 '미리 보기' 를 누르세요 — 열을 맞춰 보고 건수를 셉니다. 아무것도 바꾸지 않습니다.")); return; }
    const p = plan;
    const m = mode();
    const head = h("div", { class: "row" }, h("b", {}, `${p.src} → ${p.dst}`),
      h("span", {}, ` · 원본 ${p.src_rows.toLocaleString()}행${p.dst_rows != null ? ` · 대상에서 지울 행 ${p.dst_rows.toLocaleString()}` : ""}`),
      p.production ? h("span", { class: "warn-text" }, " · 대상이 운영 DB") : "");
    const rows = p.columns.map((c) => {
      const cb = h("input", { type: "checkbox", disabled: c.status !== "copy" }) as HTMLInputElement;
      cb.checked = c.status === "copy" && picked.has(c.name);
      cb.onchange = () => { if (cb.checked) picked.add(c.name); else picked.delete(c.name); };
      const kb = h("input", { type: "checkbox", disabled: c.status !== "copy", title: "MERGE 키" }) as HTMLInputElement;
      kb.checked = keys.includes(c.name);
      kb.onchange = () => { keys = p.columns.map((x) => x.name).filter((n) => (n === c.name ? kb.checked : keys.includes(n))); };
      return h("tr", { class: `copy-${c.status}` },
        h("td", {}, cb), m === "merge" ? h("td", {}, kb) : "",
        h("td", {}, c.name, p.primary_key.includes(c.name) ? " 🔑" : ""), h("td", {}, c.src_type ?? ""), h("td", {}, c.dst_type ?? ""),
        h("td", {}, STATUS[c.status]), h("td", { class: "dim" }, c.note ?? ""));
    });
    const table = h("table", { class: "copy-cols" },
      h("thead", {}, h("tr", {}, h("th", {}, "옮김"), m === "merge" ? h("th", {}, "키") : "", h("th", {}, "열"), h("th", {}, "원본"), h("th", {}, "대상"), h("th", {}, "상태"), h("th", {}, "알림"))),
      h("tbody", {}, ...rows));
    const sqlText = [p.delete_sql && `-- 1. 대상에서 지우기\n${p.delete_sql};`, `-- ${p.delete_sql ? 2 : 1}. 원본에서 읽기\n${p.select_sql};`, `-- ${p.delete_sql ? 3 : 2}. 대상에 넣기 (1000행씩 묶어)\n${p.write_sql};`].filter(Boolean).join("\n\n");
    planBox.replaceChildren(head,
      ...p.warnings.map((w) => h("div", { class: "warn-text", style: "font-size:12px" }, w)),
      h("div", { class: "copy-cols-box" }, table),
      h("details", {}, h("summary", {}, "실행할 SQL"), h("pre", { class: "ob-src" }, sqlText)),
      h("div", { class: "dim", style: "font-size:12px" }, "열이나 키를 바꾸면 '미리 보기' 를 다시 누르세요. 복사는 커밋하지 않습니다 — 대상 탭에서 결과를 보고 커밋·롤백하세요."));
  };

  previewBtn.onclick = async () => {
    if (!srcTable.value.trim()) return toast("원본 테이블을 적으세요", "error");
    previewBtn.disabled = true;
    planBox.replaceChildren(h("div", { class: "hint" }, "열을 맞추고 건수를 세는 중…"));
    try {
      const first = !plan;
      plan = await invoke<CopyPlan>("copy_plan", { args: args() });
      if (first || !picked.size) picked = new Set(plan.columns.filter((c) => c.status === "copy").map((c) => c.name));
      if (mode() === "merge" && !keys.length) keys = plan.keys.length ? plan.keys : plan.primary_key;
      runBox.replaceChildren();
    } catch (e) {
      plan = null;
      renderPlan();
      planBox.append(h("div", { class: "err" }, errOf(e).message));
      previewBtn.disabled = false;
      return;
    }
    previewBtn.disabled = false;
    renderPlan();
  };
  // 입력을 바꾸면 미리 보기를 다시
  for (const el of [srcSel, dstSel, srcTable, dstTable, cond]) el.addEventListener("input", () => { if (plan) { plan = null; renderPlan(); } });

  runBtn.onclick = async () => {
    const p = plan;
    if (!p) return;
    const dst = hk.conns.find((c) => c.id === Number(dstSel.value));
    const m = mode();
    const what = m === "replace" ? `대상에서 ${p.dst_rows?.toLocaleString() ?? "?"}행을 지우고 ${p.src_rows.toLocaleString()}행을 넣습니다` :
      m === "merge" ? `${p.src_rows.toLocaleString()}행을 키(${keys.join(", ")})로 MERGE 합니다` : `${p.src_rows.toLocaleString()}행을 넣습니다`;
    if (!(await confirmBox("데이터 복사", `${p.src} → ${dst?.label ?? ""} ${p.dst}\n\n${what}.\n커밋하지 않습니다 — 대상 탭에서 커밋하거나 롤백하세요.${p.production ? "\n\n⚠ 대상이 운영 DB 입니다." : ""}`, "복사"))) return;
    runBtn.disabled = previewBtn.disabled = true;
    stopBtn.hidden = false;
    const bar = h("progress", { max: Math.max(1, p.src_rows), value: 0, style: "width:100%" }) as HTMLProgressElement;
    const line = h("div", { class: "dim" }, "시작하는 중…");
    runBox.replaceChildren(bar, line);
    const ch = new Channel<Ev>();
    ch.onmessage = (ev) => {
      bar.max = Math.max(1, ev.total);
      bar.value = ev.read;
      line.textContent = `읽음 ${ev.read.toLocaleString()} / ${ev.total.toLocaleString()} · 넣음 ${ev.written.toLocaleString()}${ev.failed ? ` · 실패 ${ev.failed.toLocaleString()}` : ""}`;
    };
    const dstId = Number(dstSel.value);
    stopBtn.onclick = () => void api.cancel(dstId).catch(() => {});
    try {
      const s = await invoke<Summary>("copy_run", { args: args(p.production), onEvent: ch });
      hk.done(dstId, s.txn_pending);
      const secs = (s.elapsed_ms / 1000).toFixed(1);
      bar.value = s.read;
      runBox.replaceChildren(bar,
        h("div", { class: s.failed || s.stopped ? "warn-text" : "" },
          `${s.stopped ? "멈춤" : "끝"} · ${secs}초 · 읽음 ${s.read.toLocaleString()} · ${m === "merge" ? "넣거나 고침" : "넣음"} ${s.written.toLocaleString()}` +
          `${s.deleted ? ` · 지움 ${s.deleted.toLocaleString()}` : ""}${s.failed ? ` · 실패 ${s.failed.toLocaleString()}` : ""}`),
        s.txn_pending ? h("div", {}, h("b", {}, "아직 커밋하지 않았습니다"), " — 대상 탭에서 결과를 확인하고 커밋하거나 롤백하세요.") : "",
        s.errors.length ? h("details", { open: true }, h("summary", {}, `오류 ${s.errors.length}건${s.errors.length >= 200 ? " (앞 200건)" : ""}`),
          h("pre", { class: "ob-src" }, s.errors.map(([n, m]) => `${n}행: ${m}`).join("\n"))) : "");
    } catch (e) {
      runBox.replaceChildren(h("div", { class: "err" }, errOf(e).message));
    } finally {
      stopBtn.hidden = true;
      previewBtn.disabled = false;
      runBtn.disabled = !plan;
    }
  };

  const lbl = (t: string) => h("span", { class: "copy-lbl" }, t);
  const body = h("div", { class: "usage copy-data" },
    h("div", { class: "row" }, lbl("원본"), srcSel, srcTable),
    h("div", { class: "row" }, lbl("대상"), dstSel, dstTable),
    h("div", { class: "row" }, lbl("조건"), h("span", { class: "dim" }, "WHERE"), cond),
    h("div", { class: "row" }, lbl("방식"),
      radio("append", "추가", "INSERT 만 — 키가 겹치면 그 행은 실패"),
      radio("replace", "지우고 넣기", "대상에서 같은 조건의 행을 지우고 넣는다 (조건이 없으면 전체)"),
      radio("merge", "MERGE", "키가 같으면 고치고 없으면 넣는다 (키: 대상의 기본 키, 미리 보기에서 바꿀 수 있다)"),
      h("label", { class: "rdiff-key", style: "margin-left:16px" }, skipErr, "오류 행은 건너뛰고 계속")),
    h("div", { class: "row", style: "margin:6px 0" }, previewBtn, runBtn, stopBtn,
      h("span", { class: "dim", style: "font-size:12px" }, "원본 탭의 결과는 그대로 둡니다. 대상 탭에서 실행 중인 일이 있으면 끝난 뒤 시작합니다.")),
    planBox, runBox);
  if (!others.length) body.prepend(h("p", { class: "warn-text" }, "대상으로 쓸 다른 접속 탭이 없습니다 — 새 탭에서 대상(예: DEV)에 접속한 뒤 다시 여세요. (같은 접속의 다른 테이블로 옮길 수도 있습니다)"));
  renderPlan();
  void modal("다른 접속으로 데이터 복사", body, [{ label: "닫기", value: true }], { wide: true });
}
