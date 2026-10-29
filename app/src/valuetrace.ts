// 값 추적 — 결과 그리드의 한 셀에서 "이 행의 이 값은 어디서 왔나".
// 그 열에 값을 넣는 문장(분석 결과)마다 원본 쿼리를 이 행의 키로 다시 돌려, 지금 값과 같으면 "재현됨".
// 문장 → 커서·SELECT INTO·서브쿼리 → 원본 행으로 층마다 식·키·값을 보이고, 원본 행에서 한 단계씩 더 거슬러 오른다.
// 돌리는 것은 SELECT 뿐이고, 운영 DB 에서는 SQL 만 보인다 (백엔드가 정한다).
//
// 화면: 위에 대상(열·키·값)과 출처 흐름(원본 → 대상), 아래에 넣는 문장마다 카드 — 카드 안은 원본에서 대상으로 흐르는 정거장.
// AI 출처 설명은 창 안에서 바로 흘러나온다.
// 왼쪽은 추적한 소스 전체 (tracesrc) — 쿼리마다 그 자리에 번호·결과 배지, 오른쪽 기록·카드와 서로 오간다.

import { invoke } from "@tauri-apps/api/core";
import { errOf, type Column } from "./api";
import { glowProgress } from "./glow";
import { SourcePane, tone, type LogEntry, type SrcLoc } from "./tracesrc";
import { h, modal, toast } from "./ui";

interface Target { owner: string | null; table: string; column: string; keys: [string, string][]; value: string | null; known: boolean; vars: [string, string][] }
interface InputRow { keys: [string, string][]; value: string | null }
interface InputRows { owner: string | null; table: string; column: string; alias: string; key_cols: string[]; rows: InputRow[]; missing: number }
interface JoinRun { alias: string; table: string; on: string[]; rows?: number | null; sql: string }
interface Lacking { node: string; unit: string; line: number; end_line?: number; kind: string; produces?: boolean | null; sql?: string | null; binds: [string, string][]; error?: string | null }
interface Via { kind: string; name: string; line?: number | null; expr: string }
/** 다시 계산 한 번 (문장 또는 서브쿼리 층) */
interface Calc {
  expr: string; source?: string | null; notes: string[];
  sql?: string | null; binds: [string, string][]; error?: string | null; ran: boolean; values: (string | null)[];
  dropped: string[]; unknown_vars: string[]; flag_conds: string[]; flag_rows: (string | null)[][]; flags_sql?: string | null; rows_sql?: string | null;
  /** (키 열, 원본 쪽 식, 값) */
  keys: [string, string, string][];
  /** 이 키로 값이 안 나왔을 때 — 조인을 하나씩 더하며 남는 행 수 */
  join_steps: JoinRun[];
}
interface Writer extends Calc {
  node: string; unit: string; line: number; end_line?: number; kind: string; label: string; reproduced?: boolean | null;
  inputs: InputRows[]; via: Via[]; layers: (Calc & { label: string; via: Via[] })[];
}
interface Step {
  target: Target; analyzed: boolean; not_run?: string | null; writers: Writer[]; notes: string[];
  /** 이 열을 넣지 않는 INSERT (값 추적만) */
  lacking?: Lacking[]; default?: string | null;
  /** 행 추적이면 row — 대상은 '이 키의 행' */
  mode?: "row";
  log?: LogEntry[];
}
interface RowTrace { target: Target; analyzed: boolean; not_run?: string | null; exists?: boolean | null; exists_sql: string; writers: Writer[]; deleters: { node: string; unit: string; line: number; end_line?: number; kind: string; text: string }[]; log: LogEntry[] }
interface RowPrep { owner: string; table: string; key_cols: string[]; values: (string | null)[]; pk: string[]; columns: string[] }

export interface TraceHooks {
  openSource: (owner: string, name: string, kind: string, line?: number) => void;
  openAnalysis: () => void;
  /** 추적 결과를 AI 에게 풀어 달라고 — 답을 out 에 흘려 넣는다 (로컬 모델만) */
  explain: (text: string, out: HTMLElement) => Promise<void>;
  cancelExplain: () => void;
}
/** 창 안에서 — 왼쪽 소스 창까지 */
type Hooks = TraceHooks & { pane: SourcePane };

/** 오른쪽 기록의 그 쿼리를 펼쳐 보인다 (왼쪽 배지를 눌렀을 때) */
function revealLog(root: HTMLElement, gid: number) {
  const row = root.querySelector<HTMLElement>(`.vt-log-q[data-gid="${gid}"]`);
  if (!row) return;
  const det = row.closest("details");
  if (det) det.open = true;
  row.querySelector<HTMLElement>(".vt-log-sql")?.removeAttribute("hidden");
  root.querySelectorAll(".vt-log-q.focus").forEach((x) => x.classList.remove("focus"));
  row.classList.add("focus");
  row.scrollIntoView({ block: "center", behavior: "smooth" });
}

/** 왼쪽 소스 + 오른쪽 결과 */
function split(pane: SourcePane, right: HTMLElement): HTMLElement {
  pane.onPick = (gid) => revealLog(right, gid);
  return h("div", { class: "vt-split" }, pane.el, right);
}

/** 이 문장 카드가 돌린 쿼리들 */
const entriesOf = (log: LogEntry[] | undefined, w: { unit: string; line: number; label: string }) =>
  (log ?? []).filter((e) => e.sql && e.unit === w.unit && e.line === w.line && e.at.includes(w.label));
const locOf = (e: LogEntry): SrcLoc | null => e.unit ? { unit: e.unit, line: e.line ?? 1, end_line: e.end_line, marks: e.marks, hint: e.hint } : null;

type State = "ok" | "diff" | "miss" | "idle" | "err" | "none";
const STATE: Record<State, { chip: string; icon: string }> = {
  ok: { chip: "재현됨", icon: "✓" },
  diff: { chip: "값이 다름", icon: "≠" },
  miss: { chip: "값이 안 나옴", icon: "∅" },
  idle: { chip: "돌리지 않음", icon: "…" },
  err: { chip: "계산 못 함", icon: "!" },
  none: { chip: "원본 쿼리 없음", icon: "–" },
};

/** 출처 흐름의 한 마디 — 대상 쪽에서 원본 쪽 순서로 쌓고, 그릴 때 뒤집는다 */
type Crumb =
  | { kind: "row"; table: string; column: string; keys: [string, string][]; value: string | null; known: boolean }
  | { kind: "writer"; proc: string; line: number; via: string; expr: string; st: State }
  | { kind: "end"; text: string };
type OnPath = (path: Crumb[]) => void;

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
/** 프로시저 이름 — 노드 키의 뒤 두 마디 (PKG.PROC) */
const procOf = (node: string) => node.split(".").slice(-2).join(".");
const viaName = (v: Via) => v.kind === "문장" ? "" : `${v.kind}${v.name ? ` ${v.name}` : ""}${v.line ? ` (${v.line}줄)` : ""}`;

function stateOf(w: Writer): State {
  if (w.error) return "err";
  if (!w.sql) return "none";
  if (!w.ran) return "idle";
  if (w.reproduced === true) return "ok";
  if (w.values.length) return "diff";
  return "miss";
}

function sameish(a: string | null, b: string | null) {
  if (a == null || b == null) return (a ?? "") === (b ?? "");
  const x = a.trim(), y = b.trim();
  return x === y || (x !== "" && y !== "" && !isNaN(Number(x)) && !isNaN(Number(y)) && Number(x) === Number(y));
}

/** 이 행을 만든 길로 볼 문장 — 재현된 것, 없으면 값이 나온 것, 없으면 첫 문장 */
function bestWriter(s: Step): Writer | undefined {
  return s.writers.find((w) => w.reproduced === true) ?? s.writers.find((w) => w.values.length) ?? s.writers[0];
}

const rowCrumb = (t: Target): Crumb => ({ kind: "row", table: t.table, column: t.column, keys: t.keys, value: t.value, known: t.known });
const writerCrumb = (w: Writer): Crumb => ({
  kind: "writer", proc: procOf(w.node), line: w.line, st: stateOf(w),
  via: [w.label.split(" ←")[0], ...w.via.map(viaName).filter(Boolean)].join(" › "),
  expr: w.layers.length ? w.layers[w.layers.length - 1].expr : w.expr,
});

/** 이 단계까지의 출처 흐름 (prefix = 앞 단계들, 대상 쪽부터) */
function pathFor(s: Step, prefix: Crumb[]): Crumb[] {
  const out: Crumb[] = [...prefix, rowCrumb(s.target)];
  const best = bestWriter(s);
  if (!s.analyzed) out.push({ kind: "end", text: "분석 결과가 없어 여기서 멈춤" });
  else if (!best) out.push({ kind: "end", text: "넣는 문장을 못 찾음 — 분석 밖·동적 SQL·트리거" });
  else {
    out.push(writerCrumb(best));
    const inp = best.inputs[0];
    if (inp?.rows[0]) out.push({ kind: "row", table: inp.table, column: inp.column, keys: inp.rows[0].keys, value: inp.rows[0].value, known: true }, { kind: "end", text: "↑ 아래 카드에서 더 거슬러 오를 수 있습니다" });
    else if (!best.sql) out.push({ kind: "end", text: "변수·상수·매개변수에서 옴" });
  }
  return out;
}

function crumbText(c: Crumb): string {
  switch (c.kind) {
    case "row": return `${c.table}(${keyText(c.keys)}).${c.column}${c.known ? ` = ${show(c.value)}` : ""}`;
    case "writer": return `[${c.proc} ${c.line}줄 · ${c.via}${c.expr ? ` · ${c.expr}` : ""}${c.st === "ok" ? " · 재현됨" : ""}]`;
    case "end": return `(${c.text})`;
  }
}

// ── 작은 부품 ───────────────────────────────────────

const valuePill = (v: string | null, cls = "") => h("span", { class: `vt-pill${v == null ? " null" : ""} ${cls}` }, show(v));
const keyChips = (keys: [string, string][]) => h("span", { class: "vt-keys" },
  ...keys.map(([k, v]) => h("span", { class: "vt-key" }, h("span", { class: "k" }, k), h("span", { class: "v" }, v))));

const SQL_KW = new Set(("SELECT FROM WHERE AND OR NOT NULL IS IN AS ON JOIN LEFT RIGHT INNER OUTER FULL CROSS GROUP BY ORDER HAVING CASE WHEN THEN ELSE END " +
  "UNION ALL DISTINCT EXISTS BETWEEN LIKE WITH INSERT INTO VALUES UPDATE SET MERGE USING MATCHED DELETE ROWNUM SYSDATE LEVEL START CONNECT PRIOR").split(" "));

/** SQL 을 색 입힌 조각으로 (키워드·함수·문자열·숫자·바인드) */
function sqlCode(text: string, cls = "vt-code"): HTMLElement {
  const el = h("code", { class: cls });
  const re = /('(?:[^']|'')*')|(:[A-Za-z_][\w$#]*)|(\b\d+(?:\.\d+)?\b)|([A-Za-z_][\w$#]*)(\s*\()?|([^'A-Za-z_\d:]+|:)/g;
  let m: RegExpExecArray | null;
  while ((m = re.exec(text))) {
    if (m[1]) el.append(h("span", { class: "s" }, m[1]));
    else if (m[2]) el.append(h("span", { class: "b" }, m[2]));
    else if (m[3]) el.append(h("span", { class: "n" }, m[3]));
    else if (m[4]) {
      const w = m[4].toUpperCase();
      el.append(h("span", { class: SQL_KW.has(w) ? "k" : m[5] ? "f" : "" }, m[4]));
      if (m[5]) el.append(m[5]);
    } else el.append(m[0]);
  }
  return el;
}

/** 원본 → 대상으로 흐르는 정거장 하나 */
function station(kind: string, title: Node | string, ...body: (Node | string | null)[]): HTMLElement {
  return h("div", { class: `vt-st st-${kind}` },
    h("div", { class: "vt-st-rail" }, h("span", { class: "vt-st-dot" })),
    h("div", { class: "vt-st-body" }, h("div", { class: "vt-st-title" }, title), ...body.filter((x): x is Node | string => x != null)));
}

// ── 창 ──────────────────────────────────────────────

export async function showValueTrace(id: number, sql: string, pos: number | null, col: Column, row: [string, string | null][], hooks0: TraceHooks) {
  const body = h("div", { class: "vtrace" });
  const host = h("div", {});
  const shown = modal(`값 추적 — ${col.name}`, host, [{ label: "닫기", value: true }], { wide: true, cls: "vt-modal" });
  void shown.then(() => hooks0.cancelExplain());
  // 키 열을 골라 (없으면 PK) 처음부터 — 창 안을 새로 그린다
  const start = async (keyCols: string[] | null) => {
    const pane = new SourcePane(id);
    const hooks: Hooks = { ...hooks0, pane };
    body.replaceChildren();
    host.replaceChildren(split(pane, body));
    const g = glowProgress(`${col.name} 값을 만든 문장을 찾아 이 행의 ${keyCols ? keyCols.join(", ") : "키"}로 다시 계산하는 중…`);
    body.append(g.el);
    let step: Step;
    try {
      step = await invoke<Step>("value_trace_start", { id, sql, pos, column: col.name, row, keyCols });
    } catch (e) {
      g.done();
      const err = errOf(e);
      body.append(h("div", { class: "vt-error" }, h("b", {}, "추적하지 못했습니다"), h("div", {}, err.message)));
      if (err.kind === "nokey" || /키 열/.test(err.message)) body.append(keyPicker(row, keyCols ?? [], start));
      return;
    }
    g.done();
    traceBody(id, step, hooks, body, () => body.prepend(keyPicker(row, step.target.keys.map(([k]) => k), start)));
  };
  await start(null);
  return shown;
}

/** 이 행을 집을 열 고르기 — 결과 행의 열 중에서 (여러 개면 AND) */
function keyPicker(row: [string, string | null][], picked: string[], go: (cols: string[]) => void): HTMLElement {
  const want = new Set(picked.map((x) => x.toUpperCase()));
  const boxes = row.filter(([n]) => n).map(([n, v]) => {
    const cb = h("input", { type: "checkbox" }) as HTMLInputElement;
    cb.checked = want.has(n.toUpperCase());
    return { n, cb, el: h("label", { class: `vt-keypick${v == null ? " null" : ""}`, title: v == null ? "NULL 은 키로 쓸 수 없습니다" : "" }, cb, h("b", {}, n), h("span", { class: "dim" }, v == null ? "NULL" : v.length > 24 ? `${v.slice(0, 24)}…` : v)) };
  });
  const box = h("div", { class: "vt-section vt-keys-box" },
    h("div", { class: "vt-cap" }, "이 행을 집을 열 — PK 가 없거나 다른 열로 보고 싶을 때 고르세요 (고른 열이 모두 같은 행을 원본에서 찾습니다)"),
    h("div", { class: "vt-row wrap" }, ...boxes.map((b) => b.el)),
    h("div", { class: "vt-row" }, h("button", { class: "vt-ai-btn", onclick: (e: Event) => {
      e.preventDefault();
      const cols = boxes.filter((b) => b.cb.checked).map((b) => b.n);
      if (!cols.length) return toast("열을 하나 이상 고르세요", "error");
      go(cols);
    } }, "이 열들로 추적")));
  return box;
}

/** 값 추적 결과 — 맨 위 요약·출처 흐름·문장 카드·기록 */
function traceBody(id: number, step: Step, hooks: Hooks, body: HTMLElement, changeKeys: () => void) {
  const lines: string[] = [];
  let current: Crumb[] = [];
  const flowText = () => `출처 흐름 (원본 → 대상): ${[...current].reverse().filter((c) => c.kind !== "end" || !c.text.startsWith("↑")).map(crumbText).join(" → ")}`;
  const aiOut = h("div", { class: "vt-ai-body" });
  const aiCard = h("div", { class: "vt-ai", hidden: true },
    h("div", { class: "vt-ai-head" }, h("span", { class: "spark" }, "✦"), h("b", {}, "AI 출처 설명"), h("span", { class: "dim" }, "로컬 모델 · 고치지 않고 어디서 왔는지만, 추적에 나온 것만 근거로")),
    aiOut);
  const aiBtn = h("button", { class: "vt-ai-btn", title: "추적 결과를 로컬 AI 에게 풀어 달라고 합니다 (조회한 값이 들어 있어 외부 AI 로는 보내지 않습니다)" }, h("span", { class: "spark" }, "✦"), "AI 로 출처 설명") as HTMLButtonElement;
  aiBtn.addEventListener("click", async () => {
    aiBtn.disabled = true;
    aiCard.hidden = false;
    try {
      await hooks.explain(`## 값 추적\n${flowText()}\n\n${lines.join("\n")}`, aiOut);
    } catch (e) {
      aiOut.replaceChildren(h("div", { class: "vt-error" }, errOf(e).message));
    } finally {
      aiBtn.disabled = false;
    }
  });
  const copyBtn = h("button", { class: "vt-ghost", onclick: () => navigator.clipboard.writeText(`${flowText()}\n\n${lines.join("\n")}`).then(() => toast("추적을 복사했습니다", "info", 1500)) }, "추적 복사");
  const flowBox = h("div", { class: "vt-flowbox" });
  const onPath: OnPath = (p) => {
    current = p;
    flowBox.replaceChildren(h("div", { class: "vt-cap" }, "출처 흐름 — 원본에서 이 값까지 (재현된 문장을 따라가고, 거슬러 오를수록 위로 길어집니다)"), flowEl(p));
  };
  const keyBtn = h("button", { class: "vt-ghost", title: "이 행을 집을 열을 바꿔 다시 추적합니다", onclick: () => { keyBtn.remove(); changeKeys(); } }, "키 바꾸기");
  const tree = stepEl(id, step, hooks, lines, 0, false, [], onPath);
  body.append(hero(step, aiBtn, h("span", { class: "vt-row" }, keyBtn, copyBtn)), flowBox, aiCard, tree);
}

/** 출처 흐름 — 원본(위) → 대상(아래). 행 마디는 테이블·키·값, 문장 마디는 프로시저·거쳐 온 층·식 */
function flowEl(path: Crumb[]): HTMLElement {
  const el = h("div", { class: "vt-flow" });
  const items = [...path].reverse();
  items.forEach((c, i) => {
    if (c.kind === "row") {
      const last = i === items.length - 1;
      el.append(h("div", { class: `vt-fl-row${last ? " target" : ""}` },
        h("span", { class: "vt-fl-dot" }),
        h("b", { class: "mono" }, `${c.table}.${c.column}`), keyChips(c.keys), c.known ? valuePill(c.value) : h("span", { class: "vt-pill dim" }, "?"),
        last ? h("span", { class: "vt-fl-tag" }, "지금 보는 값") : ""));
    } else if (c.kind === "writer") {
      el.append(h("div", { class: `vt-fl-edge s-${c.st}` },
        h("span", { class: "vt-fl-arrow" }, "↓"),
        h("span", { class: "vt-fl-proc" }, h("span", { class: "vt-fl-icon" }, STATE[c.st].icon), h("b", {}, c.proc), h("span", { class: "dim" }, ` ${c.line}줄`)),
        h("span", { class: "vt-fl-via" }, c.via),
        c.expr ? sqlCode(c.expr, "vt-code inline") : ""));
    } else {
      el.append(h("div", { class: "vt-fl-end" }, c.text));
    }
  });
  return el;
}

/** 맨 위 — 대상 열, 키, 지금 값, 요약 */
function hero(s: Step, aiBtn: HTMLElement, copyBtn: HTMLElement): HTMLElement {
  const t = s.target;
  const counts = { ok: 0, diff: 0, miss: 0, other: 0 };
  for (const w of s.writers) {
    const st = stateOf(w);
    if (st === "ok" || st === "diff" || st === "miss") counts[st]++;
    else counts.other++;
  }
  const chip = (cls: string, text: string) => h("span", { class: `vt-sum ${cls}` }, text);
  return h("div", { class: "vt-hero" },
    h("div", { class: "vt-hero-text" },
      h("div", { class: "vt-eyebrow" }, `${t.owner ? `${t.owner}.` : ""}${t.table}`),
      h("div", { class: "vt-title" }, t.column),
      h("div", { class: "vt-hero-keys" }, keyChips(t.keys)),
      h("div", { class: "vt-sums" },
        chip("", `넣는 문장 ${s.writers.length}`),
        s.log ? chip("dim", `쿼리 ${s.log.filter((e) => e.sql && e.sent).length}번`) : "",
        counts.ok ? chip("ok", `✓ 재현 ${counts.ok}`) : "",
        counts.diff ? chip("warn", `≠ 값 다름 ${counts.diff}`) : "",
        counts.miss ? chip("warn", `∅ 값 안 나옴 ${counts.miss}`) : "",
        counts.other ? chip("dim", `확인 못 함 ${counts.other}`) : "")),
    h("div", { class: "vt-hero-side" },
      h("div", { class: "vt-cap" }, t.known ? "지금 값" : "지금 값 모름"),
      t.known ? valuePill(t.value, "big") : h("span", { class: "vt-pill big dim" }, "?"),
      h("div", { class: "vt-hero-actions" }, copyBtn, aiBtn)));
}

/** 한 단계 — 넣는 문장마다 카드 (거슬러 오른 단계면 위에 작은 대상 머리) */
function stepEl(id: number, s: Step, hooks: Hooks, lines: string[], depth: number, child: boolean, prefix: Crumb[], onPath: OnPath): HTMLElement {
  const pad = "  ".repeat(depth);
  hooks.pane.add(s.log ?? [], [...s.writers, ...(s.lacking ?? [])].filter((w) => w.unit).map((w) => ({ unit: w.unit, line: w.line, end_line: w.end_line })));
  onPath(pathFor(s, prefix));
  lines.push(`${pad}${targetText(s.target)}`);
  const el = h("div", { class: "vt-step" });
  if (child) {
    el.append(h("div", { class: "vt-subtarget" },
      h("b", {}, `${s.target.table}.${s.target.column}`), keyChips(s.target.keys), s.target.known ? valuePill(s.target.value) : "",
      h("span", { class: "dim" }, "— 이 값은 어디서 왔나")));
  }
  for (const n of s.notes) { el.append(h("div", { class: "vt-note" }, n)); lines.push(`${pad}  (${n})`); }
  if (!s.analyzed) {
    lines.push(`${pad}  분석 결과 없음`);
    el.append(h("div", { class: "vt-empty" },
      h("b", {}, "분석 결과가 없습니다"),
      h("div", {}, "이 접속의 PL/SQL 을 한 번 분석해 두면(모델 없이 정적 분석만 해도 됩니다) 값을 넣는 문장을 찾을 수 있습니다."),
      h("button", { class: "vt-ghost", onclick: () => hooks.openAnalysis() }, "분석 화면 열기")));
    return el;
  }
  if (s.not_run) el.append(h("div", { class: "vt-banner" }, s.not_run));
  if (!s.writers.length) {
    lines.push(`${pad}  넣는 문장 없음`);
    el.append(h("div", { class: "vt-empty" }, h("b", {}, "값을 넣는 문장을 찾지 못했습니다"),
      h("div", {}, "다른 시스템이 넣거나, 동적 SQL·트리거의 :NEW 대입일 수 있습니다.")));
  }
  if (s.writers.length) el.append(...writerTables(id, s, hooks, lines, depth, prefix, onPath));
  if (s.lacking?.length) el.append(lackingEl(s, hooks, lines, pad));
  if (s.log?.length) el.append(logEl(s.log, child ? `${s.target.table}.${s.target.column} 단계` : "이 단계", hooks.pane, !child));
  return el;
}

/** 탐색 기록 — 어디를 거치며 어떤 쿼리를 몇 번, 몇 건·몇 ms. 거친 곳(at)이 바뀔 때마다 머리를 단다 */
function logEl(log: LogEntry[], title: string, pane: SourcePane, open = true): HTMLElement {
  const sent = log.filter((e) => e.sql && e.sent);
  const unsent = log.filter((e) => e.sql && !e.sent).length;
  const errs = log.filter((e) => e.error).length;
  const ms = sent.reduce((a, e) => a + e.ms, 0);
  const secs = ms >= 1000 ? `${(ms / 1000).toFixed(1)}s` : `${ms}ms`;
  const text = log.map((e) => {
    const head = `${e.gid ? `#${e.gid}` : "  ›"} [${e.at}${e.line ? ` · ${e.line}줄` : ""}] ${e.purpose}${e.sql ? ` — ${e.sent ? `${e.error ? `오류: ${e.error}` : e.result ?? ""} (${e.ms}ms)` : "보내지 않음"}` : ""}`;
    if (!e.sql) return head;
    const b = e.binds.length ? `\n   -- 바인드: ${e.binds.map(([k, v]) => `:${k} = '${v}'`).join(", ")}` : "";
    return `${head}\n   ${e.sql.replace(/\n/g, "\n   ")};${b}`;
  }).join("\n");
  const copy = h("button", { class: "vt-link", onclick: (ev: Event) => { ev.preventDefault(); navigator.clipboard.writeText(text).then(() => toast("탐색 기록을 복사했습니다", "info", 1500)); } }, "복사");
  const list = h("ol", { class: "vt-log-list" });
  let at = "";
  for (const e of log) {
    if (e.at !== at) {
      at = e.at;
      const loc = locOf(e);
      list.append(h("li", { class: `vt-log-at${loc ? " link" : ""}`, title: loc ? "왼쪽 소스에서 이 문장 보기" : "", onclick: () => { if (loc) void pane.show(loc); } },
        at, e.line ? h("span", { class: "vt-log-lines" }, ` ${e.line}${e.end_line && e.end_line !== e.line ? `–${e.end_line}` : ""}줄`) : ""));
    }
    if (!e.sql) {
      list.append(h("li", { class: "vt-log-note" }, h("span", { class: "vt-log-seq" }, "›"), e.purpose));
      continue;
    }
    const t = tone(e);
    const badge = h("span", { class: `vt-log-res ${t}`, title: e.error ?? "" }, t === "idle" ? "보내지 않음" : t === "err" ? "오류" : e.result ?? "");
    const sqlBox = h("div", { class: "vt-log-sql", hidden: true }, sqlCode(e.sql, "vt-code"),
      e.binds.length ? h("div", { class: "dim" }, `바인드 ${e.binds.map(([k, v]) => `:${k} = '${v}'`).join(", ")}`) : "",
      e.error ? h("div", { class: "vt-error" }, e.error) : "");
    const row = h("li", { class: `vt-log-q t-${t}`, "data-gid": String(e.gid ?? ""), title: "눌러서 SQL 보기 · 왼쪽 소스의 그 자리로" },
      h("div", { class: "vt-log-line" },
        h("span", { class: "vt-log-seq q" }, `#${e.gid ?? "–"}`),
        h("span", { class: "vt-log-purpose" }, e.purpose),
        h("span", { class: "spacer" }),
        badge,
        e.sent ? h("span", { class: "vt-log-ms" }, `${e.ms}ms`) : ""),
      sqlBox);
    row.addEventListener("click", (ev) => {
      if ((ev.target as HTMLElement).closest(".vt-log-sql")) return;
      sqlBox.hidden = !sqlBox.hidden;
      const loc = locOf(e);
      if (loc) void pane.show(loc, e.gid);
    });
    list.append(row);
  }
  return h("details", { class: "vt-log", open },
    h("summary", {},
      h("b", {}, `탐색 기록 — ${title}`),
      h("span", { class: "vt-sum" }, `쿼리 ${sent.length}번`),
      sent.length ? h("span", { class: "vt-sum dim" }, `DB 시간 ${secs}`) : "",
      unsent ? h("span", { class: "vt-sum dim" }, `안 보냄 ${unsent}`) : "",
      errs ? h("span", { class: "vt-sum warn" }, `오류 ${errs}`) : "",
      h("span", { class: "spacer" }), copy),
    list);
}

/** 이 열을 넣지 않는 INSERT — 이 키의 행을 만들면 이 열은 기본값·NULL */
function lackingEl(s: Step, hooks: Hooks, lines: string[], pad: string): HTMLElement {
  const def = s.default?.trim();
  const made = s.lacking!.filter((l) => l.produces === true);
  lines.push(`${pad}  이 열을 넣지 않는 문장: ${s.lacking!.map((l) => `${procOf(l.node)} ${l.line}줄${l.produces ? " (이 키의 행을 만듦)" : ""}`).join(", ")}`);
  return h("div", { class: `vt-card vt-lacking${made.length ? " s-miss" : ""}` },
    h("div", { class: "vt-card-head" },
      h("span", { class: `vt-chip ${made.length ? "s-miss" : ""}` }, made.length ? "∅ NULL 의 출처일 수 있음" : "참고"),
      h("b", {}, `${s.target.column} 을(를) 넣지 않는 문장 ${s.lacking!.length}개`)),
    h("div", { class: "vt-headline" }, made.length
      ? `이 키의 행을 이 문장${made.length > 1 ? "들" : ""}이 만듭니다 — 그렇게 들어온 행이면 ${s.target.column} 은(는) ${def ? `기본값 ${def}` : "NULL"} 입니다`
      : `이 문장들로 들어온 행이면 ${s.target.column} 은(는) ${def ? `기본값 ${def}` : "NULL"} 입니다`),
    dataGrid("", s.lacking!, [
      { name: "프로시저", cell: (l) => h("b", {}, procOf(l.node)), text: (l) => procOf(l.node) },
      { name: "줄", cell: (l) => String(l.line), cls: "num" },
      { name: "종류", cell: (l) => l.kind },
      { name: "이 키의 행", cell: (l) => h("span", { class: l.produces ? "warn-text" : "dim" }, produced(l)), text: produced },
      { name: "", cell: (l) => srcLinks(hooks, l), text: () => "" },
    ], { onRow: (l) => void hooks.pane.show({ unit: l.unit, line: l.line, end_line: l.end_line }) }));
}

const produced = (l: Lacking) => l.produces === true ? "이 키의 행을 만듦" : l.produces === false ? "이 키는 안 만듦" : l.error ? "확인 못 함" : "돌리지 않음";

/** 소스 보기 단추 — 왼쪽 소스, 에디터 새 탭 */
function srcLinks(hooks: Hooks, w: { unit: string; line: number; end_line?: number; node: string }): HTMLElement {
  const obj = dbObject(w.unit);
  return h("span", { class: "vt-row nowrap" },
    h("button", { class: "vt-link", title: "왼쪽 소스에서 이 문장을 봅니다", onclick: (e: Event) => { e.stopPropagation(); void hooks.pane.show({ unit: w.unit, line: w.line, end_line: w.end_line }); } }, "← 소스"),
    obj ? h("button", { class: "vt-link", title: `${w.node} — 소스를 에디터 새 탭으로 엽니다`, onclick: (e: Event) => { e.stopPropagation(); hooks.openSource(obj.owner, obj.name, obj.kind, w.line); } }, "새 탭 ↗") : "");
}

// ── 표 ─────────────────────────────────────────────

interface GridCol<T> { name: string; cell: (x: T) => Node | string; text?: (x: T) => string; cls?: string }

/** 결과 표 — 행을 누르면 onRow, detail 이 있으면 그 행 아래에 자세히를 펼친다. '복사' 는 탭으로 나눈 글자 (엑셀에 붙여 넣기) */
function dataGrid<T>(title: string, items: T[], cols: GridCol<T>[], opts: { onRow?: (x: T) => void; detail?: (x: T) => HTMLElement; open?: (x: T) => boolean; rowCls?: (x: T) => string; foot?: Node | string } = {}): HTMLElement {
  const textOf = (c: GridCol<T>, x: T) => c.text ? c.text(x) : (() => { const v = c.cell(x); return typeof v === "string" ? v : (v.textContent ?? ""); })();
  const tsv = () => [cols.filter((c) => c.name).map((c) => c.name), ...items.map((x) => cols.filter((c) => c.name).map((c) => textOf(c, x).replace(/[\t\n]+/g, " ")))].map((r) => r.join("\t")).join("\n");
  const body = h("tbody", {});
  for (const x of items) {
    const tr = h("tr", { class: `${opts.rowCls?.(x) ?? ""}${opts.onRow || opts.detail ? " link" : ""}` }, ...cols.map((c) => h("td", { class: c.cls ?? "" }, c.cell(x))));
    body.append(tr);
    if (opts.detail) {
      const d = opts.detail(x);
      const open = !!opts.open?.(x);
      const dr = h("tr", { class: "vt-gd", hidden: !open }, h("td", { colspan: String(cols.length) }, d));
      if (open) tr.classList.add("open");
      body.append(dr);
      tr.addEventListener("click", (e) => {
        if ((e.target as HTMLElement).closest("button, input, a")) return;
        dr.hidden = !dr.hidden;
        tr.classList.toggle("open", !dr.hidden);
        opts.onRow?.(x);
      });
    } else if (opts.onRow) {
      tr.addEventListener("click", (e) => { if (!(e.target as HTMLElement).closest("button, input, a")) opts.onRow!(x); });
    }
  }
  const copy = h("button", { class: "vt-link", title: "표를 복사합니다 (엑셀에 붙여 넣을 수 있게 탭으로 나눔)", onclick: (e: Event) => { e.preventDefault(); navigator.clipboard.writeText(tsv()).then(() => toast("표를 복사했습니다", "info", 1500)); } }, "표 복사");
  return h("div", { class: "vt-grid-box" },
    h("div", { class: "vt-grid-head" }, title ? h("b", {}, title) : "", h("span", { class: "vt-sum dim" }, `${items.length}행`), h("span", { class: "spacer" }), copy),
    h("div", { class: "vt-grid-wrap" },
      h("table", { class: "vt-grid" }, h("thead", {}, h("tr", {}, ...cols.map((c) => h("th", { class: c.cls ?? "" }, c.name)))), body)),
    opts.foot ?? "");
}

const valuesText = (vs: (string | null)[]) => vs.length ? vs.slice(0, 3).map(show).join(", ") + (vs.length > 3 ? ` 외 ${vs.length - 3}` : "") : "";
const viaText = (w: Writer) => w.via.map(viaName).filter(Boolean).join(" › ");

/** 이 단계의 넣는 문장 표 + 찾은 원본 행 표 (+ 원본 행에서 거슬러 오른 단계) */
function writerTables(id: number, s: Step, hooks: Hooks, lines: string[], depth: number, prefix: Crumb[], onPath: OnPath): HTMLElement[] {
  const t = s.target;
  const row = s.mode === "row";
  const detail = (w: Writer) => writerEl(id, s, w, hooks, lines, depth, prefix, onPath);
  const cols: GridCol<Writer>[] = [
    row
      ? { name: "결과", cell: (w) => h("span", { class: `vt-chip ${w.values.length ? "s-ok" : w.ran ? "s-miss" : "s-idle"}` }, w.values.length ? "✓ 원본에 있음" : w.ran ? "∅ 원본에 없음" : STATE[stateOf(w)].chip) }
      : { name: "상태", cell: (w) => { const st = stateOf(w); return h("span", { class: `vt-chip s-${st}` }, `${STATE[st].icon} ${STATE[st].chip}`); } },
    { name: "프로시저", cell: (w) => h("b", {}, procOf(w.node)), text: (w) => procOf(w.node) },
    { name: "줄", cell: (w) => String(w.line), cls: "num" },
    { name: "문장", cell: (w) => w.label.split(" ←")[0] },
    { name: "거친 곳", cell: (w) => h("span", { class: "dim" }, viaText(w)), text: viaText },
    row
      ? { name: "원본 행", cell: (w) => w.ran ? `${w.values.length}${w.values.length >= 20 ? "+" : ""}` : "", cls: "num" }
      : { name: "다시 계산 값", cell: (w) => w.ran ? (w.values.length ? h("span", { class: "mono" }, valuesText(w.values)) : h("span", { class: "warn-text" }, "행 없음")) : "", text: (w) => w.ran ? (w.values.length ? valuesText(w.values) : "행 없음") : "" },
    ...(row || !t.known ? [] : [{ name: "지금 값", cell: () => h("span", { class: "mono" }, show(t.value)) } as GridCol<Writer>]),
    { name: "끊긴 곳", cell: (w) => { const y = whyText(w); return y ? h("span", { class: "vt-why-cell" }, y) : (w.error ? h("span", { class: "vt-error-text" }, w.error) : ""); }, text: (w) => whyText(w) ?? w.error ?? "", cls: "wide" },
    { name: "쿼리", cell: (w) => queryChips(entriesOf(s.log, w), hooks.pane, null), text: (w) => entriesOf(s.log, w).map((e) => `#${e.gid} ${e.result ?? ""}`).join(" ") , cls: "q" },
    { name: "", cell: (w) => srcLinks(hooks, w), text: () => "" },
  ];
  const box = h("div", { class: "vt-gridstep" });
  const out: HTMLElement[] = [dataGrid(row ? `${t.table} 에 행을 넣는 문장` : `${t.column} 을(를) 넣는 문장`, s.writers, cols, {
    detail, open: () => s.writers.length === 1,
    rowCls: (w) => `s-${stateOf(w)}`,
    onRow: (w) => void hooks.pane.show({ unit: w.unit, line: w.line, end_line: w.end_line }),
    foot: h("div", { class: "vt-grid-foot dim" }, "행을 누르면 그 문장의 자세한 계산(식·키·조건별 통과·조인 단계)이 펼쳐지고 왼쪽 소스가 그 자리로 갑니다"),
  })];
  // 찾은 원본 행 — 원본 테이블·열마다 한 표 (키 열이 칸)
  type Src = { w: Writer; inp: InputRows; r: InputRow };
  const groups = new Map<string, Src[]>();
  const missing: string[] = [];
  for (const w of s.writers) for (const inp of w.inputs) {
    if (inp.missing) missing.push(`${inp.table} 에 이 키와 짝인 행이 없습니다 (${inp.missing}건) — 아우터 조인이라 행은 남고 ${inp.column} 은(는) NULL (${procOf(w.node)} ${w.line}줄)`);
    const k = `${inp.owner ?? ""}.${inp.table}.${inp.column}`;
    for (const r of inp.rows) groups.set(k, [...(groups.get(k) ?? []), { w, inp, r }]);
  }
  const kids = h("div", { class: "vt-kids" });
  for (const items of groups.values()) {
    const { inp } = items[0];
    const keyNames: string[] = [];
    for (const it of items) for (const [k] of it.r.keys) if (!keyNames.includes(k)) keyNames.push(k);
    const upCol: GridCol<Src> = { name: "", text: () => "", cell: (it) => {
      const btn = h("button", { class: "vt-up", title: "이 원본 행의 값은 또 어디서 왔는지 한 단계 더" }, "↑ 어디서?") as HTMLButtonElement;
      btn.addEventListener("click", async (e) => {
        e.stopPropagation();
        btn.disabled = true;
        const slot = h("div", { class: "vt-child" });
        kids.append(slot);
        const g = glowProgress(`${it.inp.table}.${it.inp.column} 을(를) 거슬러 오르는 중…`, { compact: true });
        slot.replaceChildren(g.el);
        try {
          const child = await invoke<Step>("value_trace_step", { id, target: { owner: it.inp.owner, table: it.inp.table, column: it.inp.column, keys: it.r.keys, value: it.r.value, known: true, vars: [] } });
          slot.replaceChildren(stepEl(id, child, hooks, lines, depth + 2, true, [...prefix, rowCrumb(t), writerCrumb(it.w)], onPath));
          slot.scrollIntoView({ block: "nearest", behavior: "smooth" });
          btn.textContent = "↑ 아래 펼침";
        } catch (err) {
          slot.replaceChildren(h("div", { class: "vt-error" }, errOf(err).message));
          btn.disabled = false;
        } finally { g.done(); }
      });
      return btn;
    } };
    out.push(dataGrid(`찾은 원본 행 — ${inp.table}.${inp.column}`, items, [
      ...keyNames.map((k): GridCol<Src> => ({ name: k, cell: (it) => h("span", { class: "mono" }, it.r.keys.find(([n]) => n === k)?.[1] ?? ""), text: (it) => it.r.keys.find(([n]) => n === k)?.[1] ?? "" })),
      { name: inp.column, cell: (it) => h("span", { class: `mono${it.r.value == null ? " dim" : ""}` }, show(it.r.value)), text: (it) => show(it.r.value), cls: "val" },
      { name: "넣는 문장", cell: (it) => h("span", { class: "dim" }, `${procOf(it.w.node)} ${it.w.line}줄`), text: (it) => `${procOf(it.w.node)} ${it.w.line}줄` },
      ...(row ? [] : [upCol]),
    ], { onRow: (it) => void hooks.pane.show({ unit: it.w.unit, line: it.w.line, end_line: it.w.end_line }) }));
  }
  if (missing.length) out.push(h("div", { class: "vt-null-why" }, ...missing.map((m) => h("div", {}, m))));
  box.append(...out, kids);
  return [box];
}

// ── 행 추적: 이 키의 행은 왜 없나 ─────────────────────

export async function showRowTrace(id: number, sql: string | null, table: string | null, hooks0: TraceHooks) {
  const body = h("div", { class: "vtrace" });
  let pane = new SourcePane(id);
  let hooks: Hooks = { ...hooks0, pane };
  const wrap = h("div", {}, split(pane, body));
  const shown = modal("행 추적 — 이 키의 데이터는 왜 없나", wrap, [{ label: "닫기", value: true }], { wide: true, cls: "vt-modal" });
  const tableIn = h("input", { class: "vt-input", placeholder: "테이블 (예: ORD_SUM)", value: table ?? "" }) as HTMLInputElement;
  const keysBox = h("div", { class: "vt-row" });
  const goBtn = h("button", { class: "vt-ai-btn" }, "추적") as HTMLButtonElement;
  const result = h("div", {});
  const form = h("div", { class: "vt-hero vt-form" },
    h("div", { class: "vt-hero-text" },
      h("div", { class: "vt-eyebrow" }, "테이블과 키 값을 넣으면, 이 테이블에 행을 넣는 문장마다 원본을 이 키로 돌려 어디서 끊겼는지 찾습니다"),
      h("div", { class: "vt-row" }, tableIn, h("button", { class: "vt-ghost", onclick: () => void prep(null) }, "키 열 읽기")),
      keysBox),
    h("div", { class: "vt-hero-side" }, goBtn));
  body.append(form, result);
  // 키 열·값 — PK 가 없어도 아무 열이나 골라 (여러 개면 AND)
  let keyIns: { sel: HTMLSelectElement; el: HTMLInputElement }[] = [];
  let columns: string[] = [];
  let owner = "";
  const keyRow = (col: string, val: string) => {
    const sel = h("select", { class: "vt-input sel" }, ...columns.map((c) => h("option", { value: c }, c))) as HTMLSelectElement;
    sel.value = col;
    const el = h("input", { class: "vt-input", placeholder: "값", value: val }) as HTMLInputElement;
    const k = { sel, el };
    keyIns.push(k);
    return k;
  };
  const drawKeys = (pkNote: string) => {
    const add = h("button", { class: "vt-ghost", onclick: (e: Event) => {
      e.preventDefault();
      const used = new Set(keyIns.map((k) => k.sel.value));
      keyRow(columns.find((c) => !used.has(c)) ?? columns[0], "");
      drawKeys(pkNote);
      keyIns[keyIns.length - 1].sel.focus();
    } }, "+ 열 추가");
    keysBox.replaceChildren(
      ...keyIns.map((k) => h("span", { class: "vt-keyedit" }, k.sel, h("span", { class: "eq" }, "="), k.el,
        h("button", { class: "vt-link", title: "이 열 빼기", onclick: (e: Event) => { e.preventDefault(); keyIns = keyIns.filter((x) => x !== k); drawKeys(pkNote); } }, "✕"))),
      add, pkNote ? h("span", { class: "dim" }, pkNote) : "");
  };
  const prep = async (q: string | null) => {
    try {
      const p = await invoke<RowPrep>("row_trace_prepare", { id, sql: q, table: tableIn.value.trim() || null });
      owner = p.owner;
      tableIn.value = p.table;
      columns = p.columns;
      keyIns = [];
      const cols = p.key_cols.length ? p.key_cols : columns.slice(0, 1);
      cols.forEach((c, i) => keyRow(c, p.values[i] ?? ""));
      drawKeys(p.pk.length ? `PK: ${p.pk.join(", ")}` : "PK·고유 인덱스가 없습니다 — 행을 집을 열을 고르세요");
      keyIns.find((k) => !k.el.value)?.el.focus();
    } catch (e) { keysBox.replaceChildren(h("div", { class: "vt-error" }, errOf(e).message)); }
  };
  const run = async () => {
    if (!keyIns.length) await prep(null);
    const keys = keyIns.map((k) => [k.sel.value, k.el.value.trim()] as [string, string]).filter(([, v]) => v);
    if (!keys.length) return toast("키 값을 넣으세요", "error");
    // 새로 돌 때마다 왼쪽 소스·번호도 새로
    pane = new SourcePane(id);
    hooks = { ...hooks0, pane };
    wrap.replaceChildren(split(pane, body));
    goBtn.disabled = true;
    const g = glowProgress(`${tableIn.value} 에 행을 넣는 문장마다 원본을 이 키로 돌려 보는 중…`);
    result.replaceChildren(g.el);
    try {
      const r = await invoke<RowTrace>("row_trace", { id, owner: owner || null, table: tableIn.value.trim(), keys });
      result.replaceChildren(...rowResult(id, r, hooks));
    } catch (e) {
      result.replaceChildren(h("div", { class: "vt-error" }, errOf(e).message));
    } finally { g.done(); goBtn.disabled = false; }
  };
  goBtn.addEventListener("click", () => void run());
  form.addEventListener("keydown", (e) => { if (e.key === "Enter" && !e.isComposing) { e.preventDefault(); e.stopPropagation(); void run(); } });
  await prep(sql);
  // SQL 의 WHERE 에서 키 값을 다 찾았으면 바로 돈다
  if (keyIns.length && keyIns.every((k) => k.el.value)) void run();
  void shown.then(() => hooks0.cancelExplain());
  return shown;
}

function rowResult(id: number, r: RowTrace, hooks: Hooks): Node[] {
  const t = r.target;
  const lines: string[] = [];
  const have = r.writers.filter((w) => w.values.length).length;
  const ran = r.writers.filter((w) => w.ran).length;
  const head = h("div", { class: "vt-hero" },
    h("div", { class: "vt-hero-text" },
      h("div", { class: "vt-eyebrow" }, t.owner ?? ""),
      h("div", { class: "vt-title" }, t.table),
      h("div", { class: "vt-hero-keys" }, keyChips(t.keys)),
      h("div", { class: "vt-sums" },
        h("span", { class: "vt-sum" }, `넣는 문장 ${r.writers.length}`),
        have ? h("span", { class: "vt-sum ok" }, `✓ 원본에 있음 ${have}`) : "",
        ran - have ? h("span", { class: "vt-sum warn" }, `∅ 원본에 없음 ${ran - have}`) : "",
        r.deleters.length ? h("span", { class: "vt-sum dim" }, `지우는 문장 ${r.deleters.length}`) : "",
        h("span", { class: "vt-sum dim" }, `쿼리 ${r.log.filter((e) => e.sql && e.sent).length}번`))),
    h("div", { class: "vt-hero-side" },
      h("div", { class: "vt-cap" }, "지금 이 키의 행"),
      h("span", { class: `vt-pill big ${r.exists === false ? "warn-pill" : r.exists ? "ok-pill" : "dim"}` }, r.exists == null ? "?" : r.exists ? "있음" : "없음")));
  const verdict = !r.analyzed ? "분석 결과가 없어 행을 넣는 문장을 모릅니다 — 분석 화면에서 PL/SQL 을 한 번 분석해 두세요"
    : !r.writers.length ? "분석한 소스에는 이 테이블에 행을 넣는 문장이 없습니다 — 다른 시스템·동적 SQL·트리거일 수 있습니다"
    : have ? `원본에 이 키가 있는 문장이 ${have}개 있습니다 — 그 문장이 아직 돌지 않았거나, 돈 뒤에 지워졌을 수 있습니다${r.deleters.length ? " (아래 '지우는 문장' 확인)" : ""}`
    : ran ? "행을 넣는 모든 문장의 원본에 이 키가 없습니다 — 아래 표의 '끊긴 곳' 에서 어느 조인·조건에서 끊겼는지 보세요"
    : "다시 계산을 돌리지 못했습니다";
  const out: Node[] = [head, h("div", { class: "vt-banner info" }, verdict)];
  if (r.not_run) out.push(h("div", { class: "vt-banner" }, r.not_run));
  const step: Step = { target: t, analyzed: r.analyzed, not_run: r.not_run, writers: r.writers, notes: [], mode: "row", log: r.log };
  hooks.pane.add(r.log ?? [], [...r.writers, ...r.deleters].filter((w) => w.unit).map((w) => ({ unit: w.unit, line: w.line, end_line: w.end_line })));
  if (r.writers.length) out.push(...writerTables(id, step, hooks, lines, 0, [], () => {}));
  if (r.deleters.length) {
    type Del = RowTrace["deleters"][number];
    out.push(h("div", { class: "vt-banner info" }, `${t.table} 에서 행을 지우는 문장 ${r.deleters.length}개 — 행이 들어왔다가 이 문장들로 지워졌을 수 있습니다`),
      dataGrid(`${t.table} 에서 행을 지우는 문장`, r.deleters, [
        { name: "프로시저", cell: (d) => h("b", {}, procOf(d.node)), text: (d) => procOf(d.node) },
        { name: "줄", cell: (d) => String(d.line), cls: "num" },
        { name: "종류", cell: (d) => d.kind },
        { name: "문장", cell: (d) => h("span", { class: "mono vt-clip", title: d.text }, d.text.replace(/\s+/g, " ")), text: (d) => d.text.replace(/\s+/g, " "), cls: "wide" },
        { name: "", cell: (d: Del) => srcLinks(hooks, d), text: () => "" },
      ], { onRow: (d) => void hooks.pane.show({ unit: d.unit, line: d.line, end_line: d.end_line }) }));
  }
  if (r.log?.length) out.push(logEl(r.log, `${t.table} 행 추적`, hooks.pane));
  return out;
}

/** 다시 계산 결과 — 식, 키 대응, 값 (또는 0건이면 조건별 통과 막대) */
function calcBody(c: Calc, lines: string[], pad: string): (Node | null)[] {
  const out: (Node | null)[] = [];
  out.push(h("div", { class: "vt-st-line" }, h("span", { class: "vt-st-k" }, "식"), sqlCode(c.expr, "vt-code inline wide")));
  if (c.keys.length) {
    out.push(h("div", { class: "vt-st-line" }, h("span", { class: "vt-st-k" }, "키"),
      ...c.keys.map(([k, e, v]) => h("span", { class: "vt-keymap", title: `대상 키 ${k} 가 이 층에서는 ${e}` }, sqlCode(e, "vt-code inline"), h("span", { class: "eq" }, "="), h("b", {}, v)))));
  }
  if (c.ran && c.values.length) {
    out.push(h("div", { class: "vt-st-line" }, h("span", { class: "vt-st-k" }, "값"), ...c.values.slice(0, 3).map((v) => valuePill(v)),
      c.values.length > 3 ? h("span", { class: "vt-meta" }, `외 ${c.values.length - 3}행`) : ""));
  } else if (c.ran) {
    out.push(h("div", { class: "vt-st-line warn" }, h("span", { class: "vt-st-k" }, "값"), h("b", {}, "이 키로는 행이 없음")));
  }
  if (c.flag_rows.length) {
    const total = c.flag_rows.length;
    out.push(h("div", { class: "vt-meters" },
      h("div", { class: "vt-cap" }, `이 키로 조인까지 맞는 원본 행 ${total}${total >= 20 ? "+" : ""}개 — 거르는 조건별 통과 (여기서 걸러짐)`),
      ...c.flag_conds.map((cond, i) => {
        const ok = c.flag_rows.filter((r) => r[i] === "O").length;
        lines.push(`${pad}   조건 ${cond}: ${ok}/${total} 행 통과`);
        return h("div", { class: `vt-meter${ok ? "" : " fail"}` },
          h("div", { class: "vt-meter-top" }, sqlCode(cond, "vt-code inline"), h("span", { class: "vt-meter-n" }, `${ok} / ${total}`)),
          h("div", { class: "vt-bar" }, h("span", { style: `width:${ok ? Math.max(ok / total * 100, 4) : 0}%` })));
      })));
  } else if (c.ran && !c.values.length && c.flag_conds.length && !c.join_steps?.length) {
    out.push(h("div", { class: "vt-note" }, "조인 조건만으로도 이 키의 원본 행이 없습니다 — 원본 데이터가 없거나 조인 키가 다릅니다."));
  }
  if (c.join_steps?.length) {
    const first = c.join_steps[0].rows ?? 0;
    out.push(h("div", { class: "vt-joins" },
      h("div", { class: "vt-cap" }, "이 키로 조인을 하나씩 이으며 남는 행 (어디서 끊기나)"),
      ...c.join_steps.map((j, i) => {
        const n = j.rows;
        const cut = n === 0;
        lines.push(`${pad}   조인 ${i ? `+ ${j.table} ${j.alias} (${j.on.join(" AND ")})` : `${j.table} ${j.alias} (이 키)`}: ${n ?? "?"}행`);
        return h("div", { class: `vt-join${cut ? " cut" : ""}` },
          h("span", { class: "vt-join-n" }, n == null ? "?" : `${n}행`),
          h("span", { class: "vt-bar" }, h("span", { style: `width:${n ? Math.max(Math.min(n / Math.max(first, 1), 1) * 100, 6) : 0}%` })),
          h("span", { class: "vt-join-what" }, i ? "+ " : "", h("b", { class: "mono" }, `${j.table} ${j.alias}`), i ? "" : h("span", { class: "dim" }, " 에서 이 키"),
            ...j.on.map((o) => sqlCode(o, "vt-code inline"))),
          cut ? h("span", { class: "vt-join-cut" }, i ? `← 여기서 끊김: ${j.table} 에 짝이 없음` : `← ${j.table} 에 이 키가 없음`) : "");
      })));
  }
  if (c.dropped.length) {
    out.push(h("div", { class: "vt-st-line dim" }, h("span", { class: "vt-st-k" }, "뺀 조건"), ...c.dropped.map((d) => sqlCode(d, "vt-code inline strike"))));
  }
  for (const n of c.notes) out.push(h("div", { class: "vt-note" }, n));
  if (c.error) out.push(h("div", { class: "vt-error" }, c.error));
  return out;
}

function sqlDetails(c: Calc, title: string): HTMLElement | null {
  const sqls = [["다시 계산", c.sql], ["원본 행", c.rows_sql], ["조건 진단", c.flags_sql]].filter(([, q]) => q) as [string, string][];
  if (!sqls.length) return null;
  const binds = c.binds.length ? `\n-- 바인드: ${c.binds.map(([k, v]) => `:${k} = '${v}'`).join(", ")}` : "";
  const text = sqls.map(([n, q]) => `-- ${n}\n${q};`).join("\n\n") + binds;
  return h("details", { class: "vt-sql" },
    h("summary", {}, `${title} SQL`, h("button", { class: "vt-link", onclick: (e: Event) => { e.preventDefault(); navigator.clipboard.writeText(text).then(() => toast("SQL 을 복사했습니다", "info", 1500)); } }, "복사")),
    h("pre", {}, text));
}

/** 왜 안 나왔나 — 조인 단계에서 0 이 된 곳, 아니면 아무 행도 통과하지 못한 조건 (바깥 문장과 서브쿼리 층) */
function whyOf(c: Calc, where: string): string | null {
  if (!c.ran || c.values.length) return null;
  const cut = c.join_steps.findIndex((j) => j.rows === 0);
  if (cut === 0) return `${where}${c.join_steps[0].table} 에 이 키의 행이 없습니다 (0건)`;
  if (cut > 0) {
    const a = c.join_steps[cut - 1], b = c.join_steps[cut];
    return `${where}${b.table} ${b.alias} 조인에서 끊겼습니다 — ${a.table} 까지 ${a.rows}건 → + ${b.table} 0건${b.on.length ? ` (${b.on.join(" AND ")})` : ""}`;
  }
  const n = c.flag_rows.length;
  if (n) {
    const zero = c.flag_conds.filter((_, i) => c.flag_rows.every((r) => r[i] !== "O"));
    if (zero.length) return `${where}조건 ${zero.join(", ")} 을(를) 이 키의 행 ${n}건이 하나도 통과하지 못했습니다`;
    return `${where}조건마다는 통과하는 행이 있지만, 모두 함께 맞는 행이 없습니다 (${n}건 중)`;
  }
  return null;
}

function whyText(w: Writer): string | null {
  const ws = [...w.layers].reverse().map((l) => whyOf(l, `${l.via[0]?.kind ?? "서브쿼리"} ${l.via[0]?.name ?? ""} 안: `)).filter(Boolean);
  return ws[0] ?? whyOf(w, "");
}

function whyEl(w: Writer): HTMLElement | string {
  const why = whyText(w);
  return why ? h("div", { class: "vt-why" }, h("b", {}, "끊긴 곳 "), why) : "";
}

/** 이 카드가 돌린 쿼리 — 번호·목적·결과. 누르면 왼쪽 소스의 그 자리와 아래 기록의 그 쿼리로 */
function queryChips(es: LogEntry[], pane: SourcePane, card: HTMLElement | null): HTMLElement | string {
  if (!es.length) return "";
  const short = !card;
  return h("div", { class: `vt-qchips${short ? " short" : ""}` }, short ? "" : h("span", { class: "vt-cap-inline" }, "돌린 쿼리"),
    ...es.map((e) => h("button", { class: `vt-qchip t-${tone(e)}`, title: `${e.purpose}${e.error ? ` — ${e.error}` : ""}`, onclick: (ev: Event) => {
      ev.stopPropagation();
      const loc = locOf(e);
      if (loc) void pane.show(loc, e.gid);
      const root = (card ?? ev.currentTarget as HTMLElement).closest<HTMLElement>(".vtrace");
      if (root && e.gid) revealLog(root, e.gid);
    } }, h("b", {}, `#${e.gid}`), short ? " " : ` ${e.purpose.replace(/^이 키로 /, "").replace(/ — .*/, "")} `, h("span", { class: "r" }, tone(e) === "idle" ? "안 보냄" : e.error ? "오류" : e.result ?? ""))));
}

function writerEl(id: number, s: Step, w: Writer, hooks: Hooks, lines: string[], depth: number, prefix: Crumb[], onPath: OnPath): HTMLElement {
  const pad = "  ".repeat(depth + 1);
  const st = stateOf(w);
  const obj = dbObject(w.unit);
  const t = s.target;
  const row = s.mode === "row";
  const headline = row
    ? (w.ran && w.values.length ? "원본에 이 키가 있습니다 — 이 문장이 돌면 이 행이 생깁니다 (돌지 않았거나, 뒤에 지워졌을 수 있습니다)"
      : w.ran ? "이 문장의 원본에는 이 키가 없습니다 — 여기서는 이 행이 생기지 않습니다 (아래에서 어디서 끊겼는지)"
      : st === "err" ? "다시 계산하지 못했습니다" : st === "none" ? "변수·상수 값이라 원본 쿼리가 없습니다" : "돌리지 않았습니다 — 아래 SQL 로 확인하세요")
    : st === "ok" ? "이 값은 여기서 왔습니다 — 이 문장을 이 키로 다시 계산하면 지금 값과 같습니다"
    : st === "diff" ? `다시 계산하면 ${w.values.map(show).join(", ")}${t.known ? ` — 지금 값은 ${show(t.value)}` : ""}`
    : st === "miss" ? "이 키로는 이 문장에서 값이 나오지 않습니다 — 지금 데이터로는 이 행의 출처가 아닙니다"
    : st === "idle" ? "돌리지 않았습니다 — 아래 SQL 로 확인하세요"
    : st === "err" ? "다시 계산하지 못했습니다"
    : "변수·상수 값이라 다시 계산할 원본 쿼리가 없습니다";
  lines.push(`${pad}← ${procOf(w.node)} ${w.line}줄 · ${w.label}${w.via.length ? ` · ${w.via.map(viaName).filter(Boolean).join(" › ")}` : ""} · ${STATE[st].chip}${w.values.length ? ` (다시 계산 ${w.values.map(show).join(", ")})` : ""}`);
  lines.push(`${pad}   식: ${w.expr}`);
  for (const l of w.layers) lines.push(`${pad}   ${l.label}: ${l.expr}${l.ran ? ` → ${l.values.length ? l.values.map(show).join(", ") : "행 없음"}` : ""}`);

  const card = h("div", { class: `vt-card s-${st}` });
  card.append(
    h("div", { class: "vt-card-head" },
      row ? h("span", { class: `vt-chip ${w.values.length ? "s-ok" : w.ran ? "s-miss" : "s-idle"}` }, w.values.length ? "✓ 원본에 있음" : w.ran ? "∅ 원본에 없음" : STATE[st].chip)
        : h("span", { class: `vt-chip s-${st}` }, `${STATE[st].icon} ${STATE[st].chip}`),
      h("span", { class: "vt-proc" }, h("span", { class: "vt-cap-inline" }, "프로시저"), h("b", {}, procOf(w.node)), h("span", { class: "dim" }, ` ${w.line}줄`)),
      h("span", { class: "vt-kind" }, w.label.split(" ←")[0]),
      h("span", { class: "spacer" }),
      h("button", { class: "vt-link", title: "왼쪽 소스에서 이 문장을 봅니다", onclick: () => void hooks.pane.show({ unit: w.unit, line: w.line, end_line: w.end_line }) }, "← 소스에서 보기"),
      obj ? h("button", { class: "vt-link", title: `${w.node} — 소스를 에디터 새 탭으로 엽니다`, onclick: () => hooks.openSource(obj.owner, obj.name, obj.kind, w.line) }, "새 탭 ↗") : ""),
    h("div", { class: "vt-headline" }, headline),
    whyEl(w),
    queryChips(entriesOf(s.log, w), hooks.pane, card));

  // 정거장: 원본 행 → (서브쿼리 안쪽 → 바깥) → 커서·SELECT INTO → 문장 → 대상
  const flow = h("div", { class: "vt-stations" });
  // ① 원본 행
  for (const inp of w.inputs) {
    const rows = h("div", { class: "vt-st-rows" });
    if (inp.missing) {
      rows.append(h("div", { class: "vt-null-why" }, h("b", {}, `${inp.table} 에 이 키와 짝인 행이 없습니다 (${inp.missing}건)`),
        h("span", {}, " — 아우터 조인이라 행은 남고 이 값은 NULL 이 됩니다")));
      lines.push(`${pad}   원본 ${inp.table}: 아우터 조인 짝 없음 ${inp.missing}건 → NULL`);
    }
    if (!inp.rows.length && !inp.missing) rows.append(h("div", { class: "vt-note" }, "원본 행의 키를 읽지 못했습니다"));
    for (const r of inp.rows) {
      lines.push(`${pad}   원본 ${inp.table}(${keyText(r.keys)}).${inp.column} = ${show(r.value)}`);
      const slot = h("div", { class: "vt-child" });
      const btn = h("button", { class: "vt-up", title: "이 원본 행의 값은 또 어디서 왔는지 한 단계 더" }, "↑ 이 값은 어디서?") as HTMLButtonElement;
      btn.addEventListener("click", async () => {
        btn.disabled = true;
        const g = glowProgress(`${inp.table}.${inp.column} 을(를) 거슬러 오르는 중…`, { compact: true });
        slot.replaceChildren(g.el);
        try {
          const child = await invoke<Step>("value_trace_step", { id, target: { owner: inp.owner, table: inp.table, column: inp.column, keys: r.keys, value: r.value, known: true, vars: [] } });
          slot.replaceChildren(stepEl(id, child, hooks, lines, depth + 2, true, [...prefix, rowCrumb(t), writerCrumb(w)], onPath));
          btn.textContent = "↑ 펼침";
        } catch (e) {
          slot.replaceChildren(h("div", { class: "vt-error" }, errOf(e).message));
          btn.disabled = false;
        } finally { g.done(); }
      });
      rows.append(h("div", { class: "vt-input-row" }, keyChips(r.keys), h("span", { class: "vt-arrow" }, "→"), valuePill(r.value), h("span", { class: "spacer" }), btn), slot);
    }
    flow.append(station("src", h("span", {}, h("span", { class: "vt-st-kind" }, "원본 행"), h("b", { class: "mono" }, `${inp.table}.${inp.column}`)), rows));
  }
  // ② 서브쿼리 층 (안쪽부터)
  for (const l of [...w.layers].reverse()) {
    const v = l.via[0];
    flow.append(station("sub", h("span", {}, h("span", { class: "vt-st-kind" }, v?.kind ?? "서브쿼리"), h("b", { class: "mono" }, v?.name ?? l.label)),
      ...calcBody(l, lines, pad), sqlDetails(l, "이 층")));
  }
  // ③ 커서·SELECT INTO (바깥 계산이 이 층의 쿼리로 돌았다)
  const mid = w.via.filter((v) => v.kind === "커서" || v.kind === "SELECT INTO");
  const outerRanHere = mid.length > 0;
  for (const v of mid) {
    flow.append(station("cur", h("span", {}, h("span", { class: "vt-st-kind" }, v.kind), h("b", { class: "mono" }, v.name), v.line ? h("span", { class: "dim" }, ` ${v.line}줄`) : ""),
      ...(outerRanHere ? calcBody(w, lines, pad) : []), sqlDetails(w, "이 층")));
  }
  // ④ 문장
  const stmtVia = w.via.find((v) => v.kind === "문장");
  const stmtExpr = stmtVia?.expr ?? w.expr;
  flow.append(station("stmt", h("span", {}, h("span", { class: "vt-st-kind" }, "문장"), h("b", {}, w.label.split(" ←")[0]), h("span", { class: "dim" }, ` ${w.line}줄`)),
    ...(outerRanHere
      ? [h("div", { class: "vt-st-line" }, h("span", { class: "vt-st-k" }, "식"), sqlCode(stmtExpr, "vt-code inline wide"))]
      : [...calcBody(w, lines, pad), w.source ? (() => {
        const src = h("div", { class: "vt-block vt-source clamp", title: "눌러서 펼치기·접기" }, h("div", { class: "vt-cap" }, "원본 쿼리"), sqlCode(w.source!, "vt-code dim"));
        src.addEventListener("click", () => src.classList.toggle("clamp"));
        return src;
      })() : null, sqlDetails(w, "이 문장")])));
  // ⑤ 대상
  const v0 = w.values.find((x) => t.known && sameish(x, t.value)) ?? w.values[0];
  if (row) {
    flow.append(station("target", h("span", {}, h("span", { class: "vt-st-kind" }, "대상 행"), h("b", { class: "mono" }, t.table), keyChips(t.keys)),
      h("div", { class: "vt-st-line" }, h("span", { class: "vt-st-k" }, "결과"), w.ran
        ? h("b", { class: w.values.length ? "ok-text" : "warn-text" }, w.values.length ? "이 문장으로 이 키의 행이 생길 수 있음" : "이 문장으로는 이 키의 행이 생기지 않음")
        : h("span", { class: "dim" }, "확인하지 않음"))));
    card.append(flow);
    return card;
  }
  flow.append(station("target", h("span", {}, h("span", { class: "vt-st-kind" }, "대상"), h("b", { class: "mono" }, `${t.table}.${t.column}`), keyChips(t.keys)),
    h("div", { class: "vt-st-line" }, h("span", { class: "vt-st-k" }, "지금"), t.known ? valuePill(t.value) : h("span", { class: "vt-pill dim" }, "?"),
      w.ran && w.values.length ? h("span", { class: `vt-verdict ${st === "ok" ? "ok" : "warn"}` }, st === "ok" ? `= 다시 계산 ${show(v0)} ✓` : `≠ 다시 계산 ${show(v0)}`) : "")));
  card.append(flow);

  // 값을 모르는 변수 — 값을 넣고 다시
  const unknown = [...new Set([...w.unknown_vars, ...w.layers.flatMap((l) => l.unknown_vars)])];
  if (unknown.length) {
    const inputs = unknown.map((v) => ({ v, el: h("input", { class: "vt-input", placeholder: v }) as HTMLInputElement }));
    const rerun = h("button", { class: "vt-ghost" }, "값 넣고 다시 계산") as HTMLButtonElement;
    const box = h("div", { class: "vt-section vt-vars" },
      h("div", { class: "vt-cap" }, `값을 모르는 변수 ${unknown.join(", ")} — 이 변수를 쓰는 조건을 빼고 넓게 봤습니다. 값을 알면 넣어 다시 계산하세요`),
      h("div", { class: "vt-row" }, ...inputs.map((i) => i.el), rerun));
    rerun.addEventListener("click", async () => {
      const vars: [string, string][] = [...t.vars, ...inputs.filter((i) => i.el.value.trim()).map((i) => [i.v, i.el.value.trim()] as [string, string])];
      const g = glowProgress("변수 값을 넣어 다시 계산하는 중…", { compact: true });
      box.append(g.el);
      rerun.disabled = true;
      try {
        const again = await invoke<Step>("value_trace_step", { id, target: { ...t, vars } });
        const fresh = again.writers.find((x) => x.node === w.node && x.line === w.line && x.label === w.label);
        hooks.pane.add(again.log ?? [], []);
        if (fresh) card.replaceWith(writerEl(id, again, fresh, hooks, [], depth, prefix, onPath));
      } catch (e) { toast(errOf(e).message, "error"); } finally { g.done(); rerun.disabled = false; }
    });
    card.append(box);
    lines.push(`${pad}   (변수 ${unknown.join(", ")} 의 조건을 빼고 봄)`);
  }
  return card;
}
