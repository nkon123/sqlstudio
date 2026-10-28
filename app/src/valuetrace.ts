// 값 추적 — 결과 그리드의 한 셀에서 "이 행의 이 값은 어떻게 나왔나".
// 그 열에 값을 넣는 문장(분석 결과)마다 원본 쿼리를 이 행의 키로 다시 돌려, 지금 값과 같으면 "재현됨".
// 값이 안 나오면 거르는 조건마다 O/X, 값이 나오면 그 값을 만든 원본 행 — 원본 행에서 한 단계씩 더 거슬러 오른다.
// 돌리는 것은 SELECT 뿐이고, 운영 DB 에서는 SQL 만 보인다 (백엔드가 정한다).

import { invoke } from "@tauri-apps/api/core";
import { errOf, type Column } from "./api";
import { h, modal, toast } from "./ui";

interface Target { owner: string | null; table: string; column: string; keys: [string, string][]; value: string | null; known: boolean; vars: [string, string][] }
interface InputRow { keys: [string, string][]; value: string | null }
interface InputRows { owner: string | null; table: string; column: string; alias: string; key_cols: string[]; rows: InputRow[] }
interface Writer {
  node: string; unit: string; line: number; kind: string; label: string; expr: string; source?: string | null; notes: string[];
  sql?: string | null; binds: [string, string][]; error?: string | null; ran: boolean; values: (string | null)[]; reproduced?: boolean | null;
  dropped: string[]; unknown_vars: string[]; flag_conds: string[]; flag_rows: (string | null)[][]; flags_sql?: string | null;
  inputs: InputRows[]; rows_sql?: string | null;
}
interface Step { target: Target; analyzed: boolean; not_run?: string | null; writers: Writer[]; notes: string[] }

export interface TraceHooks {
  openSource: (owner: string, name: string, kind: string, line?: number) => void;
  openAnalysis: () => void;
  /** 추적 결과를 AI 에게 풀어 달라고 (로컬 모델만 — 값이 들어 있다) */
  explain: (text: string) => void;
}

/** 분석 단위 키(OWNER.NAME.PACKAGE_BODY) → DB 객체 */
function dbObject(unit: string): { owner: string; name: string; kind: string } | null {
  const p = unit.split(".");
  if (p.length < 3) return null;
  const kind = p.slice(2).join(".").replace(/_/g, " ");
  return ["PACKAGE BODY", "PACKAGE", "PROCEDURE", "FUNCTION", "TRIGGER", "TYPE BODY"].includes(kind) ? { owner: p[0], name: p[1], kind } : null;
}

const show = (v: string | null) => v == null ? "NULL" : v === "" ? "''" : v;
const keyText = (keys: [string, string][]) => keys.map(([k, v]) => `${k}=${v}`).join(", ");
const targetText = (t: Target) => `${t.owner ? `${t.owner}.` : ""}${t.table}(${keyText(t.keys)}).${t.column}${t.known ? ` = ${show(t.value)}` : ""}`;

export async function showValueTrace(id: number, sql: string, pos: number | null, col: Column, row: [string, string | null][], hooks: TraceHooks) {
  const body = h("div", { class: "vtrace" }, h("div", { class: "hint" }, `${col.name} 값을 거슬러 오르는 중…`));
  const lines: string[] = [];
  const explainBtn = h("button", { class: "small", title: "추적 결과를 로컬 AI 에게 풀어 달라고 합니다 (값이 들어 있어 외부 AI 로는 보내지 않습니다)", disabled: true,
    onclick: () => hooks.explain(lines.join("\n")) }, "AI 로 설명");
  const copyBtn = h("button", { class: "small", disabled: true, onclick: () => navigator.clipboard.writeText(lines.join("\n")).then(() => toast("추적을 복사했습니다", "info", 1500)) }, "추적 복사");
  const shown = modal(`값 추적 — ${col.name}`, h("div", {}, h("div", { class: "row" }, h("span", { class: "spacer" }), copyBtn, explainBtn), body), [{ label: "닫기", value: true }], { wide: true });
  let step: Step;
  try {
    step = await invoke<Step>("value_trace_start", { id, sql, pos, column: col.name, row });
  } catch (e) {
    body.replaceChildren(h("div", { class: "err" }, errOf(e).message));
    return shown;
  }
  body.replaceChildren(stepEl(id, step, hooks, lines, 0));
  explainBtn.disabled = copyBtn.disabled = false;
  return shown;
}

/** 한 단계 — 대상 머리 + 넣는 문장마다 카드 */
function stepEl(id: number, s: Step, hooks: TraceHooks, lines: string[], depth: number): HTMLElement {
  const pad = "  ".repeat(depth);
  lines.push(`${pad}${targetText(s.target)}`);
  const el = h("div", { class: "vt-step" },
    h("div", { class: "vt-target" }, h("b", {}, `${s.target.table}.${s.target.column}`), h("span", { class: "dim" }, ` (${keyText(s.target.keys)})`),
      s.target.known ? h("span", { class: "vt-val" }, show(s.target.value)) : h("span", { class: "dim" }, " · 지금 값은 모름")));
  for (const n of s.notes) { el.append(h("div", { class: "hint" }, n)); lines.push(`${pad}  (${n})`); }
  if (!s.analyzed) {
    el.append(h("p", {}, "이 접속의 분석 결과가 없어 값을 넣는 문장을 모릅니다. "), h("button", { class: "small", onclick: () => hooks.openAnalysis() }, "분석 화면 열기"));
    lines.push(`${pad}  분석 결과 없음`);
    return el;
  }
  if (s.not_run) el.append(h("div", { class: "vt-warn" }, s.not_run));
  if (!s.writers.length) {
    el.append(h("p", { class: "hint" }, "분석한 소스에서는 이 열에 값을 넣는 문장을 찾지 못했습니다 (다른 시스템·동적 SQL·트리거의 :NEW 대입일 수 있습니다)."));
    lines.push(`${pad}  넣는 문장 없음`);
  }
  for (const w of s.writers) el.append(writerEl(id, s, w, hooks, lines, depth));
  return el;
}

function writerEl(id: number, s: Step, w: Writer, hooks: TraceHooks, lines: string[], depth: number): HTMLElement {
  const pad = "  ".repeat(depth + 1);
  const obj = dbObject(w.unit);
  const status = statusOf(s.target, w);
  lines.push(`${pad}← ${w.label} · ${w.node} ${w.line}줄 · ${status.text}`);
  if (w.expr) lines.push(`${pad}   식: ${w.expr}`);
  if (w.source) lines.push(`${pad}   원본: ${w.source}`);
  const card = h("div", { class: `vt-writer ${status.cls}` },
    h("div", { class: "vt-head" },
      h("span", { class: "vt-badge" }, status.badge),
      h("b", {}, w.label),
      h("span", { class: "dim" }, ` · ${w.node} ${w.line}줄`),
      obj ? h("button", { class: "small", onclick: () => hooks.openSource(obj.owner, obj.name, obj.kind, w.line) }, "소스") : ""),
    h("div", { class: "vt-status" }, status.text));
  if (w.expr) card.append(h("div", {}, h("span", { class: "dim" }, "식 "), h("code", {}, w.expr)));
  if (w.source) card.append(h("div", { class: "vt-src" }, h("span", { class: "dim" }, "원본 "), h("code", {}, w.source)));
  for (const n of w.notes) card.append(h("div", { class: "hint" }, n));
  if (w.error) card.append(h("div", { class: "err" }, w.error));
  // 값이 안 나왔을 때 — 조건 진단
  if (w.flag_rows.length) {
    const ok = w.flag_conds.map((_, i) => w.flag_rows.filter((r) => r[i] === "O").length);
    card.append(h("div", { class: "vt-flags" },
      h("div", { class: "dim" }, `이 키로 조인까지 맞는 원본 행 ${w.flag_rows.length}${w.flag_rows.length >= 20 ? "+" : ""}개 — 거르는 조건별로 통과한 행:`),
      ...w.flag_conds.map((c, i) => h("div", { class: ok[i] ? "" : "vt-fail" }, `${ok[i] ? "○" : "✕"} ${ok[i]}/${w.flag_rows.length}  `, h("code", {}, c)))));
    w.flag_conds.forEach((c, i) => lines.push(`${pad}   조건 ${c}: ${ok[i]}/${w.flag_rows.length} 행 통과`));
  } else if (w.ran && w.values.length === 0 && w.flag_conds.length) {
    card.append(h("div", { class: "hint" }, "조인 조건만으로도 이 키의 원본 행이 없습니다 — 원본 데이터가 없거나 조인 키가 다릅니다."));
  }
  // 값을 모르는 변수
  if (w.dropped.length || w.unknown_vars.length) {
    const inputs = w.unknown_vars.map((v) => ({ v, el: h("input", { placeholder: `${v} 값`, style: "width:140px" }) as HTMLInputElement }));
    card.append(h("div", { class: "vt-vars" },
      h("div", { class: "dim" }, `값을 모르는 변수(${w.unknown_vars.join(", ")})를 쓰는 조건을 빼고 넓게 봤습니다: `, ...w.dropped.map((d) => h("code", {}, d))),
      h("div", { class: "row" }, ...inputs.map((i) => i.el),
        h("button", { class: "small", onclick: async () => {
          const vars: [string, string][] = [...s.target.vars, ...inputs.filter((i) => i.el.value.trim()).map((i) => [i.v, i.el.value.trim()] as [string, string])];
          try {
            const again = await invoke<Step>("value_trace_step", { id, target: { ...s.target, vars } });
            const fresh = again.writers.find((x) => x.node === w.node && x.line === w.line && x.label === w.label);
            if (fresh) card.replaceWith(writerEl(id, again, fresh, hooks, [], depth));
          } catch (e) { toast(errOf(e).message, "error"); }
        } }, "값 넣고 다시"))));
    lines.push(`${pad}   (변수 ${w.unknown_vars.join(", ")} 의 조건을 빼고 봄)`);
  }
  // 값을 만든 원본 행 → 거슬러 오르기
  for (const inp of w.inputs) {
    const list = h("div", { class: "vt-inputs" }, h("div", { class: "dim" }, `값을 만든 원본: ${inp.table}.${inp.column}${inp.rows.length ? "" : " — 키를 읽지 못했습니다"}`));
    for (const r of inp.rows) {
      lines.push(`${pad}   원본 ${inp.table}(${keyText(r.keys)}).${inp.column} = ${show(r.value)}`);
      const slot = h("div", { class: "vt-child" });
      const btn = h("button", { class: "small", title: "이 원본 행의 값이 어떻게 들어왔는지 한 단계 더", onclick: async () => {
        btn.disabled = true;
        slot.replaceChildren(h("div", { class: "hint" }, "거슬러 오르는 중…"));
        try {
          const child = await invoke<Step>("value_trace_step", { id, target: { owner: inp.owner, table: inp.table, column: inp.column, keys: r.keys, value: r.value, known: true, vars: [] } });
          slot.replaceChildren(stepEl(id, child, hooks, lines, depth + 2));
        } catch (e) { slot.replaceChildren(h("div", { class: "err" }, errOf(e).message)); btn.disabled = false; }
      } }, "거슬러 오르기");
      list.append(h("div", { class: "vt-row" }, h("span", {}, `(${keyText(r.keys)}) = `), h("b", {}, show(r.value)), btn), slot);
    }
    card.append(list);
  }
  // SQL
  const sqls = [["다시 계산", w.sql], ["원본 행", w.rows_sql], ["조건 진단", w.flags_sql]].filter(([, q]) => q) as [string, string][];
  if (sqls.length) {
    const binds = w.binds.length ? `\n-- 바인드: ${w.binds.map(([k, v]) => `:${k} = '${v}'`).join(", ")}` : "";
    const text = sqls.map(([t, q]) => `-- ${t}\n${q};`).join("\n\n") + binds;
    card.append(h("details", { class: "vt-sql" }, h("summary", {}, "만든 SQL"),
      h("pre", {}, text),
      h("button", { class: "small", onclick: () => navigator.clipboard.writeText(text).then(() => toast("SQL 을 복사했습니다", "info", 1500)) }, "복사")));
  }
  return card;
}

function statusOf(t: Target, w: Writer): { cls: string; badge: string; text: string } {
  if (w.error) return { cls: "vt-err", badge: "!", text: "다시 계산하지 못함" };
  if (!w.sql) return { cls: "vt-none", badge: "–", text: "다시 계산할 원본 쿼리가 없음" };
  if (!w.ran) return { cls: "vt-none", badge: "?", text: "돌리지 않음 — 아래 SQL 로 확인" };
  if (w.reproduced === true) return { cls: "vt-ok", badge: "✓", text: `재현됨 — 다시 계산한 값 ${show(w.values.find((v) => t.known && sameish(v, t.value)) ?? w.values[0])}` };
  if (w.values.length) return { cls: "vt-diff", badge: "≠", text: `다시 계산한 값 ${w.values.map(show).join(", ")}${t.known ? ` (지금 값 ${show(t.value)})` : ""}` };
  return { cls: "vt-miss", badge: "0", text: "이 키로는 원본에서 값이 나오지 않음" };
}

function sameish(a: string | null, b: string | null) {
  if (a == null || b == null) return (a ?? "") === (b ?? "");
  const x = a.trim(), y = b.trim();
  return x === y || (!isNaN(Number(x)) && !isNaN(Number(y)) && x !== "" && Number(x) === Number(y));
}
