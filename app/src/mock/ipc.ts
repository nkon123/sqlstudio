// 가짜 모드: DB 에 붙는 명령을 화면 안에서 흉내 낸다.
//
// 앱 창(npm run tauri:mock): DB 명령만 가로채고 나머지(문장 나누기·설정·파일·이력)는 진짜 Rust 로 보낸다.
// 브라우저(npm run mock): 백엔드가 없으니 전부 여기서 — 문장 나누기도 간단한 JS 로 흉내 낸다.

import type { Channel } from "@tauri-apps/api/core";
import { mockIPC, mockWindows } from "@tauri-apps/api/mocks";
import { byteToUtf16, utf16ToByte, type Column, type CompletionItem, type RowPage, type ScriptEvent, type Statement, type StmtKind } from "../api";
import { ddlOf, findTable, OTHER_OBJECTS, TABLES } from "./data";

type Args = Record<string, unknown>;
const fail = (message: string, kind = "mock") => { throw { kind, message }; };
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
const NOT_HERE = "가짜 모드에서는 이 기능을 쓸 수 없습니다 (DB 가 없음)";

// ── 문장 나누기 (브라우저용 — 앱 창에서는 진짜 Rust 를 쓴다) ──────────────

const PLSQL_HEAD = /^(BEGIN|DECLARE|CREATE\s+(OR\s+REPLACE\s+)?(EDITIONABLE\s+|NONEDITIONABLE\s+)?(PROCEDURE|FUNCTION|PACKAGE|TRIGGER|TYPE))\b/i;

function classify(sql: string): StmtKind {
  const t = sql.replace(/^(\s|--[^\n]*\n|\/\*[\s\S]*?\*\/)+/, "").toUpperCase();
  if (/^(SELECT|WITH|\()/.test(t)) return "query";
  if (/^(INSERT|UPDATE|DELETE|MERGE)\b/.test(t)) return "dml";
  if (PLSQL_HEAD.test(t)) return t.startsWith("CREATE") ? "plsql_unit" : "plsql";
  if (/^(CREATE|ALTER|DROP|TRUNCATE|COMMENT|GRANT|REVOKE|RENAME)\b/.test(t)) return "ddl";
  if (/^(COMMIT|ROLLBACK|SAVEPOINT)\b/.test(t)) return "tcl";
  if (/^EXPLAIN\b/.test(t)) return "explain";
  return "other";
}

/** UTF-16 위치로 문장들 — ';' 또는 '/' 한 줄, blank 면 빈 줄에서도 끊는다 (PL/SQL 블록은 '/' 에서만) */
function split(src: string, blank = false): { from: number; to: number; text: string }[] {
  const out: { from: number; to: number; text: string }[] = [];
  let start = -1;
  let i = 0;
  const n = src.length;
  const end = (to: number) => {
    if (start >= 0) {
      const text = src.slice(start, to).replace(/\s+$/, "");
      if (text) out.push({ from: start, to: start + text.length, text });
    }
    start = -1;
  };
  while (i < n) {
    const ch = src[i];
    // 줄 전체가 "/"
    if (ch === "/" && /(^|\n)[ \t]*$/.test(src.slice(Math.max(0, i - 200), i)) && /^[ \t]*(\n|$)/.test(src.slice(i + 1, i + 200))) { end(i); i++; continue; }
    if (blank && ch === "\n" && start >= 0 && !PLSQL_HEAD.test(src.slice(start, i)) && /^[ \t\r]*\n/.test(src.slice(i + 1))) { end(i); i++; continue; }
    if (/\s/.test(ch)) { i++; continue; }
    if (start < 0) start = i;
    if (ch === "-" && src[i + 1] === "-") { while (i < n && src[i] !== "\n") i++; continue; }
    if (ch === "/" && src[i + 1] === "*") { const j = src.indexOf("*/", i + 2); i = j < 0 ? n : j + 2; continue; }
    if (ch === "'") { i++; while (i < n) { if (src[i] === "'") { if (src[i + 1] === "'") { i += 2; continue; } break; } i++; } i++; continue; }
    if (ch === '"') { const j = src.indexOf('"', i + 1); i = j < 0 ? n : j + 1; continue; }
    if (ch === ";" && !PLSQL_HEAD.test(src.slice(start, i))) { end(i); i++; continue; }
    i++;
  }
  end(n);
  return out;
}

function toStatement(src: string, s: { from: number; to: number; text: string }): Statement {
  return { text: s.text, kind: classify(s.text), start: utf16ToByte(src, s.from), end: utf16ToByte(src, s.to), line: src.slice(0, s.from).split("\n").length };
}

function analyze(a: Args) {
  const sel = (a.selection as string | undefined)?.trim() ? (a.selection as string) : null;
  const src = sel ?? (a.text as string);
  const parts = split(src, !sel);
  let s = parts[0];
  if (!sel) {
    const cur = byteToUtf16(src, (a.cursor as number) ?? 0);
    s = parts.find((p) => cur >= p.from && cur <= p.to) ?? [...parts].reverse().find((p) => p.to <= cur) ?? parts[0];
  }
  const statement = s ? toStatement(src, s) : null;
  const binds = statement ? [...new Set([...statement.text.matchAll(/(?<![:\w]):([A-Za-z_]\w*)/g)].map((m) => m[1].toUpperCase()))] : [];
  const confirm = statement && /^\s*(DELETE|UPDATE)\b/i.test(statement.text) && !/\bWHERE\b/i.test(statement.text) ? "WHERE 없는 UPDATE/DELETE 입니다 (가짜 모드라 실제로는 아무것도 바뀌지 않습니다)" : null;
  return { statement, binds, confirm };
}

// ── 조회 흉내 ─────────────────────────────────────

/** 괄호 밖 쉼표로 나눈다 */
function splitTop(s: string): string[] {
  const out: string[] = [];
  let depth = 0, from = 0, q = false;
  for (let i = 0; i < s.length; i++) {
    const ch = s[i];
    if (ch === "'") q = !q;
    if (q) continue;
    if (ch === "(") depth++;
    else if (ch === ")") depth--;
    else if (ch === "," && depth === 0) { out.push(s.slice(from, i)); from = i + 1; }
  }
  out.push(s.slice(from));
  return out.map((x) => x.trim()).filter(Boolean);
}

interface Cursor { columns: Column[]; make: (i: number) => (string | null)[]; total: number; pos: number }
const cursors = new Map<number, Cursor>();

/** SELECT 를 흉내 낸다: FROM 의 첫 테이블에서 행을 만든다. WHERE 는 보지 않는다 (ROWNUM <= n 만) */
function query(sql: string): Cursor {
  const text = sql.replace(/--[^\n]*/g, "").replace(/\/\*[\s\S]*?\*\//g, "");
  const m = /\bSELECT\s+([\s\S]*?)\s+FROM\s+([\w$#."]+)(?:\s+(?!WHERE|ORDER|GROUP|JOIN|LEFT|RIGHT|INNER|CONNECT|START|UNION|FOR\b)([A-Za-z_]\w*))?/i.exec(text);
  const limit = Number(/\bROWNUM\s*<=?\s*(\d+)/i.exec(text)?.[1] ?? Infinity);
  if (!m) fail("가짜 모드: SELECT … FROM … 모양만 흉내 냅니다");
  const [, list, tname] = m!;
  const t = findTable(tname);
  if (!t) {
    if (tname.toUpperCase() !== "DUAL") fail(`ORA-00942: 테이블 또는 뷰가 존재하지 않습니다 (가짜 모드의 테이블: ${Object.keys(TABLES).join(", ")})`, "db");
    const items = splitTop(list);
    const columns = items.map((x) => ({ name: (/\s(?:AS\s+)?"?([\w$#]+)"?$/i.exec(x)?.[1] ?? x).toUpperCase().slice(0, 30), type_name: "VARCHAR2(4000)", nullable: true }));
    const vals = items.map((x) => { const v = x.replace(/\s+(AS\s+)?"?[\w$#]+"?$/i, ""); return /^'.*'$/.test(v) ? v.slice(1, -1).replace(/''/g, "'") : /^-?\d+(\.\d+)?$/.test(v) ? v : v.toUpperCase() === "SYSDATE" ? new Date().toISOString().slice(0, 19).replace("T", " ") : `(가짜) ${v}`; });
    return { columns, make: () => vals, total: Math.min(1, limit), pos: 0 };
  }
  const tcols = t.desc.columns;
  const picks: { col: Column; get: (r: (string | null)[], i: number) => string | null }[] = [];
  for (const item of splitTop(list)) {
    if (/^([\w$#]+\.)?\*$/.test(item)) {
      tcols.forEach((x, k) => picks.push({ col: { name: x.name, type_name: x.data_type, nullable: x.nullable }, get: (r) => r[k] }));
      continue;
    }
    const am = /^([\s\S]+?)\s+(?:AS\s+)?"?([A-Za-z_][\w$#]*)"?$/i.exec(item);
    const expr = (am && !/^[\w$#.]+$/.test(item) ? am[1] : am && /\s/.test(item) ? am[1] : item).trim();
    const name = (am && item !== expr ? am[2] : expr.split(".").pop() ?? expr).toUpperCase().slice(0, 30);
    const bare = expr.split(".").pop()!.replace(/"/g, "").toUpperCase();
    const k = /^[\w$#."]+$/.test(expr) ? tcols.findIndex((x) => x.name === bare) : -1;
    if (k >= 0) picks.push({ col: { name, type_name: tcols[k].data_type, nullable: tcols[k].nullable }, get: (r) => r[k] });
    else if (bare === "ROWID" || /ROWIDTOCHAR/i.test(expr)) picks.push({ col: { name, type_name: "ROWID", nullable: false }, get: (_r, i) => `AAAR3sAAEAAAACX${String(i).padStart(3, "A")}` });
    else if (bare === "ROWNUM") picks.push({ col: { name, type_name: "NUMBER", nullable: false }, get: (_r, i) => String(i + 1) });
    else if (/^COUNT\s*\(/i.test(expr)) picks.push({ col: { name, type_name: "NUMBER", nullable: true }, get: () => String(t.count) });
    else picks.push({ col: { name, type_name: "VARCHAR2(4000)", nullable: true }, get: () => `(가짜) ${expr}` });
  }
  const agg = picks.some((p) => /^COUNT/i.test(p.col.name)) && picks.length === 1;
  return { columns: picks.map((p) => p.col), make: (i) => { const r = t.row(i); return picks.map((p) => p.get(r, i)); }, total: agg ? 1 : Math.min(t.count, limit), pos: 0 };
}

function page(c: Cursor, n: number): RowPage {
  const rows: (string | null)[][] = [];
  for (; c.pos < c.total && rows.length < n; c.pos++) rows.push(c.make(c.pos));
  return { columns: c.columns, rows, has_more: c.pos < c.total, fetched_total: c.pos };
}

let txn = false;
async function execute(a: { id: number; sql: string; page_size?: number }) {
  await sleep(120);
  const kind = classify(a.sql);
  const t0 = performance.now();
  if (kind === "query") {
    const c = query(a.sql);
    cursors.set(a.id, c);
    const p = page(c, a.page_size ?? 500);
    return { kind, outcome: { type: "rows", ...p }, elapsed_ms: Math.round(performance.now() - t0) + 37, output: [], txn_pending: txn };
  }
  if (kind === "dml") {
    txn = true;
    return { kind, outcome: { type: "affected", rows: 1 }, elapsed_ms: 12, output: ["(가짜 모드 — 실제로 바뀐 것은 없습니다)"], txn_pending: true };
  }
  if (kind === "tcl") txn = false;
  return { kind, outcome: { type: "done" }, elapsed_ms: 5, output: kind === "plsql" ? ["(가짜 DBMS_OUTPUT) 완료"] : [], txn_pending: txn };
}

// ── 자동완성 ─────────────────────────────────────

/** 자주 쓰는 내장 함수 (진짜 목록은 sqls-core::complete 의 FUNCTIONS) */
const FUNCTIONS: [string, string][] = [
  ["NVL", "NVL(expr, 대체값)"], ["NVL2", "NVL2(expr, NULL 아닐 때, NULL 일 때)"], ["DECODE", "DECODE(expr, 값1, 결과1, ..., 기본값)"],
  ["COALESCE", "COALESCE(expr1, expr2, ...)"], ["NULLIF", "NULLIF(a, b) — 같으면 NULL"], ["GREATEST", "GREATEST(a, b, ...)"], ["LEAST", "LEAST(a, b, ...)"],
  ["TO_CHAR", "TO_CHAR(값, 'YYYY-MM-DD HH24:MI:SS')"], ["TO_DATE", "TO_DATE('문자', 'YYYY-MM-DD')"], ["TO_NUMBER", "TO_NUMBER(문자 [, 형식])"],
  ["SUBSTR", "SUBSTR(문자, 시작 [, 길이])"], ["INSTR", "INSTR(문자, 찾을 문자 [, 시작 [, n번째]])"], ["LENGTH", "LENGTH(문자)"],
  ["UPPER", "UPPER(문자)"], ["LOWER", "LOWER(문자)"], ["TRIM", "TRIM(문자)"], ["LPAD", "LPAD(문자, 길이, 채울 문자)"], ["RPAD", "RPAD(문자, 길이, 채울 문자)"],
  ["REPLACE", "REPLACE(문자, 찾을, 바꿀)"], ["ROUND", "ROUND(n [, 자리])"], ["TRUNC", "TRUNC(n|날짜 [, 자리|형식])"], ["MOD", "MOD(a, b)"],
  ["ADD_MONTHS", "ADD_MONTHS(날짜, n)"], ["LAST_DAY", "LAST_DAY(날짜)"], ["MONTHS_BETWEEN", "MONTHS_BETWEEN(a, b)"],
  ["COUNT", "COUNT(expr)"], ["SUM", "SUM(expr)"], ["AVG", "AVG(expr)"], ["MIN", "MIN(expr)"], ["MAX", "MAX(expr)"],
  ["ROW_NUMBER", "ROW_NUMBER() OVER (ORDER BY ...)"], ["LISTAGG", "LISTAGG(expr, ',') WITHIN GROUP (ORDER BY ...)"],
];

function complete(a: { text: string; cursor: number }) {
  const text = a.text;
  const cur = byteToUtf16(text, a.cursor);
  const head = text.slice(0, cur);
  const word = /[\w$#]*$/.exec(head)![0];
  const from = utf16ToByte(text, cur - word.length);
  const dot = /([\w$#]+)\.[\w$#]*$/.exec(head);
  const items: CompletionItem[] = [];
  const up = word.toUpperCase();
  const like = (s: string) => s.startsWith(up);
  if (dot) {
    // 별칭.컬럼
    const alias = dot[1].toUpperCase();
    const re = new RegExp(String.raw`\b(?:FROM|JOIN)\s+(?:[\w$#]+\.)?([\w$#]+)\s+${alias}\b`, "i");
    const t = findTable(re.exec(text)?.[1] ?? alias);
    for (const x of t?.desc.columns ?? []) if (like(x.name)) items.push({ label: x.name, kind: "column", detail: x.data_type, info: x.comment ?? null, boost: 2 });
  } else {
    for (const t of Object.values(TABLES)) if (like(t.desc.name)) items.push({ label: t.desc.name, kind: "table", detail: t.desc.comment ?? null, info: null, boost: 1 });
    for (const t of Object.values(TABLES)) {
      if (!new RegExp(String.raw`\b${t.desc.name}\b`, "i").test(text)) continue;
      for (const x of t.desc.columns) if (like(x.name)) items.push({ label: x.name, kind: "column", detail: `${t.desc.name} · ${x.data_type}`, info: x.comment ?? null, boost: 0 });
    }
    if (word) for (const [f, sig] of FUNCTIONS) if (like(f)) items.push({ label: f, kind: "function", detail: sig, info: null, apply: `${f}(`, boost: -1 });
  }
  return { from, items, context: "" };
}

// ── 명령 표 ─────────────────────────────────────

/** 가짜로 답하는 명령. undefined 를 돌려주면 (앱 창에서) 진짜 백엔드로 보낸다 */
const DB: Record<string, (a: Args) => unknown> = {
  list_profiles: () => [{ name: "가짜 DB", user: "scott", connect_string: "mock", has_saved_password: true, color: "#c0392b" }],
  connect: async () => { await sleep(200); return { id: 1, user: "SCOTT", connect_string: "mock", server_version: "가짜 Oracle 11.2 (DB 없음)", read_only: false, sid: 42 }; },
  disconnect: () => null,
  forget_password: () => null,
  execute: (a) => execute(a.args as { id: number; sql: string; page_size?: number }),
  execute_script: async (a) => {
    const ch = a.onEvent as Channel<ScriptEvent>;
    const parts = split(a.script as string);
    let ok = 0;
    for (const [index, p] of parts.entries()) {
      const st = toStatement(a.script as string, p);
      ch.onmessage({ event: "started", index, total: parts.length, line: st.line, kind: st.kind, preview: p.text.slice(0, 80) });
      try {
        const r = await execute({ id: a.id as number, sql: p.text, page_size: 500 });
        if (r.outcome.type === "rows") ch.onmessage({ event: "rows", index, page: r.outcome as unknown as RowPage });
        ch.onmessage({ event: "finished", index, elapsed_ms: r.elapsed_ms, summary: r.outcome.type === "affected" ? "1행" : "완료", output: r.output });
        ok++;
      } catch (e) {
        ch.onmessage({ event: "failed", index, line: st.line, error: e as { kind: string; message: string } });
        if (a.stopOnError) break;
      }
    }
    return { total: parts.length, ok, failed: parts.length - ok, stopped: ok < parts.length && !!a.stopOnError, txn_pending: txn };
  },
  fetch_more: async (a) => {
    await sleep(150);
    const c = cursors.get(a.id as number);
    if (!c) fail("열린 결과가 없습니다");
    return page(c!, a.rows as number);
  },
  cancel: () => null,
  abandon: () => null,
  commit: () => { txn = false; return null; },
  rollback: () => { txn = false; return null; },
  explain: (a) => ["Plan hash value: 1234567890 (가짜)", "", "| Id | Operation         | Name |", "|  0 | SELECT STATEMENT  |      |",
    `|  1 |  TABLE ACCESS FULL| ${/\bFROM\s+([\w$#.]+)/i.exec(a.sql as string)?.[1]?.toUpperCase() ?? "?"} |`],
  count_rows: (a) => ({ rows: String(findTable(/\bFROM\s+([\w$#."]+)/i.exec(a.sql as string)?.[1] ?? "")?.count ?? 1), elapsed_ms: 3 }),
  list_schemas: () => ["SCOTT", "HR"],
  list_objects: (a) => {
    const like = ((a.nameLike as string | undefined) ?? "").replace(/%/g, "").toUpperCase();
    const all = [...Object.values(TABLES).map((t) => ({ owner: t.desc.owner, name: t.desc.name, object_type: "TABLE" })), ...OTHER_OBJECTS];
    return all.filter((o) => (!a.objectType || o.object_type === a.objectType) && (!like || o.name.includes(like)))
      .map((o) => ({ ...o, status: "VALID", last_ddl_time: "2026-10-01 03:00:00" }));
  },
  describe: async (a) => { await sleep(80); const t = findTable(a.name as string); if (!t) fail(`'${a.name}' 를 찾지 못했습니다 (가짜 모드)`, "not_found"); return t!.desc; },
  describe_fast: (a) => findTable(a.name as string)?.desc ?? null,
  object_deps: (a) => {
    const t = findTable(a.name as string);
    const n = t?.desc.name ?? "?";
    const dep = (owner: string, name: string, kind: string, how: string, lines: [number, string][] = [], status = "VALID") =>
      ({ owner, name, kind, status, how, lines: lines.map(([line, text]) => ({ line, text, in_comment: text.trimStart().startsWith("--") })) });
    const by = [
      dep("SCOTT", "PKG_ORDER", "PACKAGE BODY", "직접", [[12, `    SELECT COUNT(*) INTO v_cnt FROM ${n} WHERE ROWNUM = 1;`], [40, `    UPDATE ${n} SET upd_dt = SYSDATE`], [41, `    -- ${n} 마감 처리`]]),
      dep("SCOTT", "P_CLOSE_DAY", "PROCEDURE", "직접", [[7, `  DELETE FROM ${n} WHERE 1 = 0;`]], "INVALID"),
      dep("SCOTT", "V_EMP_DEPT", "VIEW", "직접", [[3, `  FROM ${n} x`]]),
      dep("APP", "TRG_AUDIT", "TRIGGER", `동의어 APP.${n}`),
    ];
    const uses = n === "ORDER_ITEMS" ? [dep("SCOTT", "ORDERS", "TABLE", "FK FK_ITEMS_ORDER")] : n === "EMP" ? [dep("SCOTT", "DEPT", "TABLE", "FK FK_EMP_DEPT")] : [];
    return { object: `SCOTT.${n}`, used_by: by, uses };
  },
  get_ddl: (a) => {
    // 흐름도의 가짜 패키지 — 줄 번호가 보이게
    if (String(a.name).includes("PKG_SAL")) return `\n  CREATE OR REPLACE PACKAGE BODY "SCOTT"."PKG_SAL" AS\n${Array.from({ length: 39 }, (_, i) => `  -- ${i + 2}행`).join("\n")}`;
    const t = findTable(a.name as string); return t ? ddlOf(t.desc) : `-- (가짜 모드) ${a.objectType} ${a.name} 의 소스는 없습니다`; },
  complete: (a) => complete(a as { text: string; cursor: number }),
  completion_status: () => ({ profile: "가짜 DB", phase: "ready", objects: Object.keys(TABLES).length + OTHER_OBJECTS.length, columns: Object.values(TABLES).reduce((n, t) => n + t.desc.columns.length, 0) }),
  refresh_completion: () => null,
  column_notes: (a) => {
    const t = findTable(/\bFROM\s+([\w$#."]+)/i.exec(a.sql as string)?.[1] ?? "");
    return Object.fromEntries((t?.desc.columns ?? []).filter((x) => x.comment).map((x) => [x.name, x.comment!]));
  },
  grid_editable: () => fail(NOT_HERE),
  grid_apply: () => fail(NOT_HERE),
};
for (const cmd of ["column_source", "column_usage", "object_section", "object_sections", "table_sizes", "tablespace_usage", "monitor_snapshot", "monitor_detail", "monitor_kill",
  "copy_plan", "copy_run", "schema_compare_start", "import_run"]) DB[cmd] ??= () => fail(NOT_HERE);
for (const cmd of Object.keys(DB)) if (cmd.startsWith("debug_")) DB[cmd] = () => fail(NOT_HERE);

/** 브라우저에서만 — 진짜 백엔드가 맡던 것 */
const LOCAL: Record<string, (a: Args) => unknown> = {
  analyze_sql: analyze,
  app_info: () => ({ version: "가짜 모드 (브라우저)", config_path: "(없음)" }),
  tns_list: () => ({ entries: [] }),
  list_providers: () => [],
  get_mcp_settings: () => ({ allowed_connections: [], call_timeout_secs: 30, allow_explain: false }),
  history_search: () => [],
  client_log: () => null,
  conn_event: () => null,
  // 파일은 브라우저 저장소에 (탭 되살리기·저장을 시험할 수 있게)
  read_sql_file: (a) => {
    const v = localStorage.getItem(`mock.file:${a.path}`);
    return v == null ? fail(`${a.path}: 파일이 없습니다`, "io") : { text: v, encoding: "utf-8" };
  },
  write_sql_file: (a) => { localStorage.setItem(`mock.file:${a.path}`, String(a.text)); return null; },
  // 분석 화면을 열 수 있게 — 결과는 없다. 중단된 가져오기는 localStorage "mock.pending" 으로 시험한다
  analysis_sets: () => [],
  // 흐름도를 볼 수 있게 — 패키지 하나(서브프로그램 둘)와 테이블 둘
  analysis_dir: () => "C:/mock/analysis",
  analysis_result: async () => { await sleep(1500); return MOCK_ANALYSIS; },
  analysis_unit: () => ({ unit: MOCK_UNIT, chunks: [] }),
  analysis_pending_imports: () => { try { return JSON.parse(localStorage.getItem("mock.pending") ?? "[]"); } catch { return []; } },
  analysis_discard_import: (a) => {
    try { localStorage.setItem("mock.pending", JSON.stringify(JSON.parse(localStorage.getItem("mock.pending") ?? "[]").filter((p: { id: string }) => p.id !== a.id))); } catch { /* 없음 */ }
    return null;
  },
};

const MOCK_FACTS = (tables: { name: string; ops: string; lines: number[] }[]) =>
  ({ tables, calls: [], sequences: [], dynamic_sql: [], transactions: [], raises: [], handles: [], swallowed: [], complexity: 2, lines: 20 });
const MOCK_NODE = (path: string, start_line: number, end_line: number, commits: boolean, summary: string) => ({
  id: `SCOTT.PKG_SAL.${path}`, unit: "SCOTT.PKG_SAL.PACKAGE_BODY", path, kind: "PROCEDURE", signature: `PROCEDURE ${path}`,
  start_line, end_line, summary, complexity: 2, commits });
const MOCK_ANALYSIS = {
  units: 1,
  nodes: [MOCK_NODE("CALC", 3, 20, false, "부서별 급여 합계를 계산한다"), MOCK_NODE("RUN", 22, 40, true, "급여를 올리고 커밋한다")],
  edges: [{ from: "SCOTT.PKG_SAL.RUN", to: "SCOTT.PKG_SAL.CALC", resolved: true, lines: [30] }],
  tables: [
    { table: "SCOTT.EMP", by: { "SCOTT.PKG_SAL.CALC": "R", "SCOTT.PKG_SAL.RUN": "RU" }, impacted_entries: [] },
    { table: "SCOTT.DEPT", by: { "SCOTT.PKG_SAL.CALC": "R" }, impacted_entries: [] },
  ],
  entries: ["SCOTT.PKG_SAL.RUN"], cycles: [], unused: [], transactions: {}, findings: [],
};
const MOCK_UNIT = {
  key: "SCOTT.PKG_SAL.PACKAGE_BODY", owner: "SCOTT", name: "PKG_SAL", unit_type: "PACKAGE BODY", lines: 40, analyzed_at: 0,
  stats: { chunks: 2, asked: 0, cached: 0, failed: 0 }, model: null,
  facts: MOCK_FACTS([{ name: "EMP", ops: "RU", lines: [8, 12, 25, 33] }, { name: "DEPT", ops: "R", lines: [9] }]),
  subprograms: [
    { path: "CALC", name: "CALC", overload: 0, kind: "PROCEDURE", signature: "PROCEDURE CALC", start_line: 3, end_line: 20, chunk_ids: [],
      facts: MOCK_FACTS([{ name: "EMP", ops: "R", lines: [8, 12] }, { name: "DEPT", ops: "R", lines: [9] }]) },
    { path: "RUN", name: "RUN", overload: 0, kind: "PROCEDURE", signature: "PROCEDURE RUN", start_line: 22, end_line: 40, chunk_ids: [],
      facts: MOCK_FACTS([{ name: "EMP", ops: "RU", lines: [25, 33] }]) },
  ],
};

function banner(where: string) {
  const b = document.createElement("div");
  b.textContent = `가짜 DB 모드 (${where}) — 테이블 EMP · DEPT · ORDERS · ORDER_ITEMS`;
  b.style.cssText = "position:fixed;right:8px;bottom:6px;z-index:99999;padding:2px 8px;border-radius:4px;background:#c0392b;color:#fff;font:600 11px sans-serif;pointer-events:none;opacity:.85";
  const add = () => document.body.append(b);
  if (document.body) add(); else addEventListener("DOMContentLoaded", add);
}

export function installMock() {
  type Internals = { invoke: (cmd: string, args?: Args, opts?: unknown) => Promise<unknown> };
  const internals = (window as unknown as { __TAURI_INTERNALS__?: Internals }).__TAURI_INTERNALS__;
  if (internals?.invoke) {
    // 앱 창: DB 명령만 가로챈다 — internals.invoke 는 읽기 전용이라 mock/core.ts 가 이 표를 본다
    (window as unknown as { __SQLS_MOCK_DB__?: typeof DB }).__SQLS_MOCK_DB__ = DB;
    banner("앱");
    return;
  }
  // 브라우저: 전부 흉내
  mockWindows("main");
  mockIPC(async (cmd, args) => {
    const a = (args ?? {}) as Args;
    const f = DB[cmd] ?? LOCAL[cmd];
    if (f) return f(a);
    if (cmd.startsWith("plugin:")) return null;
    console.debug("[mock] 흉내 내지 않는 명령:", cmd, a);
    return /(_list|_search|s)$/.test(cmd) ? [] : null;
  }, { shouldMockEvents: true });
  banner("브라우저");
}
