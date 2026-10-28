// 값 추적 — 결과 그리드의 한 셀에서 "이 행의 이 값은 어디서 왔나".
// 그 열에 값을 넣는 문장(분석 결과)마다 원본 쿼리를 이 행의 키로 다시 돌려, 지금 값과 같으면 "재현됨".
// 문장 → 커서·SELECT INTO·서브쿼리 → 원본 행으로 층마다 식·키·값을 보이고, 원본 행에서 한 단계씩 더 거슬러 오른다.
// 돌리는 것은 SELECT 뿐이고, 운영 DB 에서는 SQL 만 보인다 (백엔드가 정한다).
//
// 화면: 위에 대상(열·키·값)과 출처 흐름(원본 → 대상), 아래에 넣는 문장마다 카드 — 카드 안은 원본에서 대상으로 흐르는 정거장.
// AI 출처 설명은 창 안에서 바로 흘러나온다.

import { invoke } from "@tauri-apps/api/core";
import { errOf, type Column } from "./api";
import { glowProgress } from "./glow";
import { h, modal, toast } from "./ui";

interface Target { owner: string | null; table: string; column: string; keys: [string, string][]; value: string | null; known: boolean; vars: [string, string][] }
interface InputRow { keys: [string, string][]; value: string | null }
interface InputRows { owner: string | null; table: string; column: string; alias: string; key_cols: string[]; rows: InputRow[]; missing: number }
interface JoinRun { alias: string; table: string; on: string[]; rows?: number | null; sql: string }
interface Lacking { node: string; unit: string; line: number; kind: string; produces?: boolean | null; sql?: string | null; binds: [string, string][]; error?: string | null }
interface Via { kind: string; name: string; line?: number | null; expr: string }
/** 탐색 기록 한 줄 — sql 이 없으면 쿼리 없이 길을 고른 것 */
interface LogEntry { seq: number; at: string; purpose: string; sql?: string | null; binds: [string, string][]; sent: boolean; result?: string | null; ms: number; error?: string | null }
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
  node: string; unit: string; line: number; kind: string; label: string; reproduced?: boolean | null;
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
interface RowTrace { target: Target; analyzed: boolean; not_run?: string | null; exists?: boolean | null; exists_sql: string; writers: Writer[]; deleters: { node: string; unit: string; line: number; kind: string; text: string }[]; log: LogEntry[] }
interface RowPrep { owner: string; table: string; key_cols: string[]; values: (string | null)[] }

export interface TraceHooks {
  openSource: (owner: string, name: string, kind: string, line?: number) => void;
  openAnalysis: () => void;
  /** 추적 결과를 AI 에게 풀어 달라고 — 답을 out 에 흘려 넣는다 (로컬 모델만) */
  explain: (text: string, out: HTMLElement) => Promise<void>;
  cancelExplain: () => void;
}

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

export async function showValueTrace(id: number, sql: string, pos: number | null, col: Column, row: [string, string | null][], hooks: TraceHooks) {
  const body = h("div", { class: "vtrace" });
  const g = glowProgress(`${col.name} 값을 만든 문장을 찾아 이 행의 키로 다시 계산하는 중…`);
  body.append(g.el);
  const shown = modal(`값 추적 — ${col.name}`, body, [{ label: "닫기", value: true }], { wide: true, cls: "vt-modal" });
  void shown.then(() => hooks.cancelExplain());
  let step: Step;
  try {
    step = await invoke<Step>("value_trace_start", { id, sql, pos, column: col.name, row });
  } catch (e) {
    g.done();
    body.append(h("div", { class: "vt-error" }, h("b", {}, "추적하지 못했습니다"), h("div", {}, errOf(e).message)));
    return shown;
  }
  g.done();
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
  const tree = stepEl(id, step, hooks, lines, 0, false, [], onPath);
  body.append(hero(step, aiBtn, copyBtn), flowBox, aiCard, tree);
  return shown;
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
function stepEl(id: number, s: Step, hooks: TraceHooks, lines: string[], depth: number, child: boolean, prefix: Crumb[], onPath: OnPath): HTMLElement {
  const pad = "  ".repeat(depth);
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
  for (const w of s.writers) el.append(writerEl(id, s, w, hooks, lines, depth, prefix, onPath));
  if (s.lacking?.length) el.append(lackingEl(s, hooks, lines, pad));
  if (s.log?.length) el.append(logEl(s.log, child ? `${s.target.table}.${s.target.column} 단계` : "이 단계"));
  return el;
}

/** 탐색 기록 — 어디를 거치며 어떤 쿼리를 몇 번, 몇 건·몇 ms. 거친 곳(at)이 바뀔 때마다 머리를 단다 */
function logEl(log: LogEntry[], title: string): HTMLElement {
  const sent = log.filter((e) => e.sql && e.sent);
  const unsent = log.filter((e) => e.sql && !e.sent).length;
  const errs = log.filter((e) => e.error).length;
  const ms = sent.reduce((a, e) => a + e.ms, 0);
  const secs = ms >= 1000 ? `${(ms / 1000).toFixed(1)}s` : `${ms}ms`;
  const text = log.map((e) => {
    const head = `${e.seq}. [${e.at}] ${e.purpose}${e.sql ? ` — ${e.sent ? `${e.error ? `오류: ${e.error}` : e.result ?? ""} (${e.ms}ms)` : "보내지 않음"}` : ""}`;
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
      list.append(h("li", { class: "vt-log-at" }, at));
    }
    if (!e.sql) {
      list.append(h("li", { class: "vt-log-note" }, h("span", { class: "vt-log-seq" }, "›"), e.purpose));
      continue;
    }
    const zero = /^0(건|행)$/.test(e.result ?? "");
    const badge = !e.sent ? h("span", { class: "vt-log-res dim" }, "보내지 않음")
      : e.error ? h("span", { class: "vt-log-res err", title: e.error }, "오류")
      : h("span", { class: `vt-log-res${zero ? " zero" : ""}` }, e.result ?? "");
    const sqlBox = h("div", { class: "vt-log-sql", hidden: true }, sqlCode(e.sql, "vt-code"),
      e.binds.length ? h("div", { class: "dim" }, `바인드 ${e.binds.map(([k, v]) => `:${k} = '${v}'`).join(", ")}`) : "",
      e.error ? h("div", { class: "vt-error" }, e.error) : "");
    const row = h("li", { class: "vt-log-q", title: "눌러서 SQL 보기" },
      h("div", { class: "vt-log-line" },
        h("span", { class: "vt-log-seq q" }, String(sent.indexOf(e) + 1 || "–")),
        h("span", { class: "vt-log-purpose" }, e.purpose),
        h("span", { class: "spacer" }),
        badge,
        e.sent ? h("span", { class: "vt-log-ms" }, `${e.ms}ms`) : ""),
      sqlBox);
    row.addEventListener("click", (ev) => { if (!(ev.target as HTMLElement).closest(".vt-log-sql")) sqlBox.hidden = !sqlBox.hidden; });
    list.append(row);
  }
  return h("details", { class: "vt-log" },
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
function lackingEl(s: Step, hooks: TraceHooks, lines: string[], pad: string): HTMLElement {
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
    ...s.lacking!.map((l) => {
      const obj = dbObject(l.unit);
      return h("div", { class: "vt-input-row" },
        h("span", { class: "vt-proc" }, h("b", {}, procOf(l.node)), h("span", { class: "dim" }, ` ${l.line}줄 · ${l.kind}`)),
        h("span", { class: "spacer" }),
        h("span", { class: l.produces ? "warn-text" : "dim" }, l.produces === true ? "이 키의 행을 만듦" : l.produces === false ? "이 키는 안 만듦" : l.error ? "확인 못 함" : "돌리지 않음"),
        obj ? h("button", { class: "vt-link", onclick: () => hooks.openSource(obj.owner, obj.name, obj.kind, l.line) }, "소스 ↗") : "");
    }));
}

// ── 행 추적: 이 키의 행은 왜 없나 ─────────────────────

export async function showRowTrace(id: number, sql: string | null, table: string | null, hooks: TraceHooks) {
  const body = h("div", { class: "vtrace" });
  const shown = modal("행 추적 — 이 키의 데이터는 왜 없나", body, [{ label: "닫기", value: true }], { wide: true, cls: "vt-modal" });
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
  let keyIns: { col: string; el: HTMLInputElement }[] = [];
  let owner = "";
  const prep = async (q: string | null) => {
    try {
      const p = await invoke<RowPrep>("row_trace_prepare", { id, sql: q, table: tableIn.value.trim() || null });
      owner = p.owner;
      tableIn.value = p.table;
      keyIns = p.key_cols.map((c, i) => ({ col: c, el: h("input", { class: "vt-input", placeholder: c, value: p.values[i] ?? "" }) as HTMLInputElement }));
      keysBox.replaceChildren(...keyIns.flatMap((k) => [h("span", { class: "vt-key" }, h("span", { class: "k" }, k.col)), k.el]));
      keyIns.find((k) => !k.el.value)?.el.focus();
    } catch (e) { keysBox.replaceChildren(h("div", { class: "vt-error" }, errOf(e).message)); }
  };
  const run = async () => {
    if (!keyIns.length) await prep(null);
    const keys = keyIns.map((k) => [k.col, k.el.value.trim()] as [string, string]);
    if (!keys.some(([, v]) => v)) return toast("키 값을 넣으세요", "error");
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
  form.addEventListener("keydown", (e) => { if (e.key === "Enter") { e.preventDefault(); void run(); } });
  await prep(sql);
  // SQL 의 WHERE 에서 키 값을 다 찾았으면 바로 돈다
  if (keyIns.length && keyIns.every((k) => k.el.value)) void run();
  return shown;
}

function rowResult(id: number, r: RowTrace, hooks: TraceHooks): Node[] {
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
    : ran ? "행을 넣는 모든 문장의 원본에 이 키가 없습니다 — 아래 카드에서 어느 조인·조건에서 끊겼는지 보세요"
    : "다시 계산을 돌리지 못했습니다";
  const out: Node[] = [head, h("div", { class: "vt-banner info" }, verdict)];
  if (r.not_run) out.push(h("div", { class: "vt-banner" }, r.not_run));
  const step: Step = { target: t, analyzed: r.analyzed, not_run: r.not_run, writers: r.writers, notes: [], mode: "row" };
  for (const w of r.writers) out.push(writerEl(id, step, w, hooks, lines, 0, [], () => {}));
  if (r.deleters.length) {
    out.push(h("div", { class: "vt-card" },
      h("div", { class: "vt-card-head" }, h("span", { class: "vt-chip" }, "참고"), h("b", {}, `${t.table} 에서 행을 지우는 문장 ${r.deleters.length}개`)),
      h("div", { class: "vt-headline" }, "행이 들어왔다가 이 문장들로 지워졌을 수 있습니다"),
      ...r.deleters.map((d) => {
        const obj = dbObject(d.unit);
        return h("div", { class: "vt-block" },
          h("div", { class: "vt-input-row" }, h("span", { class: "vt-proc" }, h("b", {}, procOf(d.node)), h("span", { class: "dim" }, ` ${d.line}줄 · ${d.kind}`)),
            h("span", { class: "spacer" }), obj ? h("button", { class: "vt-link", onclick: () => hooks.openSource(obj.owner, obj.name, obj.kind, d.line) }, "소스 ↗") : ""),
          sqlCode(d.text, "vt-code dim"));
      })));
  }
  if (r.log?.length) out.push(logEl(r.log, `${t.table} 행 추적`));
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

function writerEl(id: number, s: Step, w: Writer, hooks: TraceHooks, lines: string[], depth: number, prefix: Crumb[], onPath: OnPath): HTMLElement {
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
      obj ? h("button", { class: "vt-link", title: `${w.node} — 소스를 그 줄로 엽니다`, onclick: () => hooks.openSource(obj.owner, obj.name, obj.kind, w.line) }, "소스 열기 ↗") : ""),
    h("div", { class: "vt-headline" }, headline));

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
        if (fresh) card.replaceWith(writerEl(id, again, fresh, hooks, [], depth, prefix, onPath));
      } catch (e) { toast(errOf(e).message, "error"); } finally { g.done(); rerun.disabled = false; }
    });
    card.append(box);
    lines.push(`${pad}   (변수 ${unknown.join(", ")} 의 조건을 빼고 봄)`);
  }
  return card;
}
