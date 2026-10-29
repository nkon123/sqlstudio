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
// 접속·그룹 — 화면 확인용으로 메모리에서만 바꾼다 (규칙은 db.rs 의 그룹 명령을 간추린 것)
type MockProfile = { name: string; user: string; connect_string: string; group?: string | null; color?: string | null; production?: boolean; read_only?: boolean; has_saved_password?: boolean };
let profiles: MockProfile[] = [
  { name: "가짜 DB", user: "scott", connect_string: "mock", has_saved_password: true, color: "#c0392b" },
  { name: "ERP 운영", user: "erp", connect_string: "erp-prd:1521/ERP", group: "ERP/운영", production: true, has_saved_password: true },
  { name: "ERP 개발", user: "erp", connect_string: "erp-dev:1521/ERP", group: "ERP/개발", has_saved_password: true },
  { name: "MES", user: "mes", connect_string: "mes:1521/MES", group: "MES", has_saved_password: true },
];
let groups: { path: string; color?: string | null }[] = [{ path: "ERP" }, { path: "ERP/운영", color: "#c0392b" }, { path: "ERP/개발" }, { path: "MES", color: "#27ae60" }];
const U = (x?: string | null) => (x ?? "").toUpperCase();
const under = (g: string | null | undefined, path: string) => !!g && (U(g) === U(path) || U(g).startsWith(U(path) + "/"));
const parentOf = (g: string) => (g.includes("/") ? g.slice(0, g.lastIndexOf("/")) : null);
function swapNear<T>(v: T[], idx: number[], k: number, up: boolean) {
  const j = up ? k - 1 : k + 1;
  if (k >= 0 && j >= 0 && j < idx.length) [v[idx[k]], v[idx[j]]] = [v[idx[j]], v[idx[k]]];
}
function tidyGroups() {
  for (const p of profiles) for (const g of [p.group ? parentOf(p.group) : null, p.group]) if (g && !groups.some((x) => U(x.path) === U(g))) groups.push({ path: g });
}

const DB: Record<string, (a: Args) => unknown> = {
  list_profiles: () => profiles.map((p) => ({ ...p })),
  list_groups: () => groups.map((g) => ({ ...g })),
  save_profile: (a) => {
    const p = a.profile as MockProfile, key = U((a.originalName as string | undefined) ?? p.name);
    const i = profiles.findIndex((x) => U(x.name) === key);
    if (i >= 0) profiles[i] = { ...profiles[i], ...p }; else profiles.push(p);
    tidyGroups();
    return null;
  },
  delete_profile: (a) => { profiles = profiles.filter((x) => U(x.name) !== U(a.name as string)); return null; },
  move_profile: (a) => {
    const me = profiles.findIndex((x) => U(x.name) === U(a.name as string));
    const sib = profiles.map((_, i) => i).filter((i) => U(profiles[i].group) === U(profiles[me]?.group));
    swapNear(profiles, sib, sib.indexOf(me), a.up as boolean);
    return null;
  },
  save_group: (a) => {
    const path = (a.path as string).split("/").map((x) => x.trim()).filter(Boolean).join("/"), orig = a.original as string | undefined;
    if (!orig) {
      if (groups.some((g) => U(g.path) === U(path))) fail(`같은 이름의 그룹이 있습니다: ${path}`, "invalid");
      groups.push({ path, color: a.color as string | null });
    } else {
      const moved = (g: string) => path + g.slice(orig.length);
      for (const g of groups) if (under(g.path, orig)) { if (U(g.path) === U(orig)) g.color = a.color as string | null; g.path = moved(g.path); }
      for (const p of profiles) if (under(p.group, orig)) p.group = moved(p.group!);
    }
    tidyGroups();
    return null;
  },
  delete_group: (a) => {
    const path = a.path as string;
    if (a.withConnections) profiles = profiles.filter((p) => !under(p.group, path));
    else for (const p of profiles) if (under(p.group, path)) p.group = parentOf(path);
    groups = groups.filter((g) => !under(g.path, path));
    return null;
  },
  move_group: (a) => {
    const path = a.path as string;
    const sib = groups.map((_, i) => i).filter((i) => U(parentOf(groups[i].path)) === U(parentOf(path)));
    swapNear(groups, sib, sib.findIndex((i) => U(groups[i].path) === U(path)), a.up as boolean);
    return null;
  },
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

// 탐색 기록 흉내 — [at, purpose, sql?, result?, ms?, error?]
// 위치는 at 에서: "APP.PKG.PROC 88줄" → 단위·줄, "커서 C (12줄)" → 거친 줄, "조인 단계: + T" → 짚을 낱말
const mockLog = (rows: [string, string, string?, string?, number?, string?][]) => rows.map(([at, purpose, sql, result, ms, error], i) => {
  const m = /^(\w+)\.(\w+)\.\w+ (\d+)줄/.exec(at);
  const line = m ? Number(m[3]) : null;
  const marks = [...at.matchAll(/커서 \w+ \((\d+)줄\)/g)].map((x) => Number(x[1]));
  const hint = /조인 단계: (?:\+ )?(\w+)/.exec(purpose)?.[1] ?? /^(\w+) 은\(는\) 값을 모르는/.exec(purpose)?.[1] ?? (at.includes("› 서브쿼리") ? "ORD_DTL" : null);
  return { seq: i + 1, at, purpose, sql: sql ?? null, binds: sql ? [["K1", "A123"]] : [], sent: !!sql, result: result ?? null, ms: ms ?? 0, error: error ?? null,
    unit: m ? `${m[1]}.${m[2]}.PACKAGE_BODY` : null, line, end_line: line ? line + (at.includes("MERGE") ? 14 : 9) : null, marks, hint };
});
/** 가짜 패키지 본문 — 추적 화면 왼쪽 */
function mockSource(unit: string): string[] {
  const L: string[] = [];
  const put = (n: number, ...ls: string[]) => { while (L.length < n - 1) L.push(L.length % 17 === 0 ? "" : `    -- ${L.length + 1}`); L.push(...ls); };
  if (unit.includes("PKG_IF")) {
    put(1, "PACKAGE BODY PKG_IF AS");
    put(12, "  CURSOR C_SRC IS", "    SELECT S.ORD_NO, S.SEQ, S.AMT_TXT AS AMT", "      FROM IF_ORD_DTL S", "     WHERE S.IF_DT = V_BASE_DT;");
    put(30, "  PROCEDURE P_REG_ORD(P_ORD_NO VARCHAR2) IS", "  BEGIN", "    INSERT INTO ORD_SUM (ORD_NO, UPD_DT) VALUES (P_ORD_NO, SYSDATE);", "  END;");
    put(41, "    INSERT INTO ORD_DTL (ORD_NO, SEQ, AMT)", "    VALUES (R.ORD_NO, R.SEQ, R.AMT);");
    put(60, "END PKG_IF;");
    return L;
  }
  put(1, "PACKAGE BODY PKG_ORD AS");
  put(80, "  PROCEDURE P_INIT IS", "  BEGIN", "    -- 어제 들어온 주문으로 요약 행을 만든다");
  put(88, "    INSERT INTO ORD_SUM (ORD_NO, TOT_AMT, UPD_DT)", "    SELECT D.ORD_NO, 0, SYSDATE", "      FROM ORD_DTL D,", "           ORD_HDR H", "     WHERE H.ORD_NO = D.ORD_NO", "       AND H.ORD_DT >= TRUNC(SYSDATE) - 1", "     GROUP BY D.ORD_NO;", "  END P_INIT;");
  put(205, "  PROCEDURE P_SUMMARY IS", "  BEGIN");
  put(212, "    MERGE INTO ORD_SUM T", "    USING (SELECT D.ORD_NO, SUM(D.AMT) AMT", "             FROM ORD_DTL D", "            WHERE D.STATUS = 'OK'", "            GROUP BY D.ORD_NO) S", "       ON (T.ORD_NO = S.ORD_NO)",
    "     WHEN MATCHED THEN", "          UPDATE SET T.TOT_AMT = NVL(S.AMT, 0), T.UPD_DT = SYSDATE", "     WHEN NOT MATCHED THEN", "          INSERT (ORD_NO, TOT_AMT, UPD_DT)", "          VALUES (S.ORD_NO, NVL(S.AMT, 0), SYSDATE);", "  END P_SUMMARY;");
  put(298, "  PROCEDURE P_PURGE IS", "  BEGIN");
  put(301, "    DELETE FROM ORD_SUM WHERE UPD_DT < ADD_MONTHS(SYSDATE, -6);", "  END P_PURGE;");
  put(320, "END PKG_ORD;");
  return L;
}
const VT_LOG = mockLog([
  ["APP.ORD_SUM(ORD_NO=A123).TOT_AMT", "분석 결과에서 ORD_SUM.TOT_AMT 에 값을 넣는 문장 2개를 찾음"],
  ["APP.PKG_ORD.P_SUMMARY 212줄 MERGE", "문장 원문 읽기 (저장된 본문이 잘림)", "SELECT text FROM all_source WHERE owner = :o AND name = :n AND type = :t AND line BETWEEN :a AND :b ORDER BY line", "38행", 41],
  ["APP.PKG_ORD.P_SUMMARY 212줄 MERGE · MERGE · 있으면 UPDATE", "길: TOT_AMT = S.AMT ← 서브쿼리 S"],
  ["APP.PKG_ORD.P_SUMMARY 212줄 MERGE · MERGE · 있으면 UPDATE", "이 키로 다시 계산", "SELECT * FROM (SELECT S.AMT AS V__, T.ORD_NO AS K1__ FROM ORD_SUM T, (…) S WHERE T.ORD_NO = S.ORD_NO) WHERE K1__ = :K1 AND ROWNUM <= 5", "1행", 128],
  ["APP.PKG_ORD.P_SUMMARY 212줄 MERGE · MERGE · 있으면 UPDATE", "값을 만든 원본 행과 그 키", "SELECT … FROM ORD_SUM T, (…) S WHERE … AND ROWNUM <= 20", "2행", 96],
  ["APP.PKG_ORD.P_SUMMARY 212줄 MERGE · MERGE · 있으면 UPDATE › 서브쿼리 S", "서브쿼리 안으로: TOT_AMT = NVL((SUM(D.AMT)), 0)"],
  ["APP.PKG_ORD.P_SUMMARY 212줄 MERGE · MERGE · 있으면 UPDATE › 서브쿼리 S", "이 키로 다시 계산", "SELECT * FROM (SELECT NVL((SUM(D.AMT)), 0) AS V__, (D.ORD_NO) AS K1__ FROM ORD_DTL D WHERE (D.STATUS = 'OK') GROUP BY D.ORD_NO) WHERE K1__ = :K1 AND ROWNUM <= 5", "0행", 77],
  ["APP.PKG_ORD.P_SUMMARY 212줄 MERGE · MERGE · 있으면 UPDATE › 서브쿼리 S", "0건 — 거르는 조건마다 통과 여부", "SELECT CASE WHEN (D.STATUS = 'OK') THEN 'O' ELSE 'X' END F1__ FROM ORD_DTL D WHERE (D.ORD_NO) = :K1", "3행", 52],
  ["APP.PKG_ORD.P_INIT 88줄 INSERT · INSERT … SELECT", "이 키로 다시 계산", "SELECT * FROM (SELECT 0 AS V__, D.ORD_NO AS K1__ FROM ORD_DTL D) WHERE K1__ = :K1 AND ROWNUM <= 5", "1행", 33],
  ["ORD_SUM.TOT_AMT", "ORD_SUM 에 행을 넣지만 TOT_AMT 은(는) 넣지 않는 문장 1개 — 이 키의 행을 만드는지 키 열로 다시 계산"],
  ["APP.PKG_IF.P_REG_ORD 30줄 INSERT · INSERT … VALUES", "이 키로 다시 계산", "SELECT * FROM (SELECT :P_ORD_NO AS V__ FROM DUAL) WHERE ROWNUM <= 5", "오류", 12, "ORA-01008: not all variables bound"],
]);
const RT_LOG = mockLog([
  ["APP.ORD_SUM(ORD_NO=A999)", "지금 이 키의 행이 있는지", "SELECT COUNT(*) AS N__ FROM APP.ORD_SUM WHERE ORD_NO = :K1", "0건", 18],
  ["APP.ORD_SUM(ORD_NO=A999)", "분석 결과에서 ORD_SUM 에 행을 넣는 문장 2개를 찾음"],
  ["APP.PKG_ORD.P_INIT 88줄 INSERT · INSERT … SELECT", "길: ORD_NO = D.ORD_NO"],
  ["APP.PKG_ORD.P_INIT 88줄 INSERT · INSERT … SELECT", "이 키로 다시 계산", "SELECT * FROM (SELECT D.ORD_NO AS V__, D.ORD_NO AS K1__ FROM ORD_DTL D, ORD_HDR H WHERE …) WHERE K1__ = :K1 AND ROWNUM <= 5", "0행", 64],
  ["APP.PKG_ORD.P_INIT 88줄 INSERT · INSERT … SELECT", "0건 — 거르는 조건마다 통과 여부", "SELECT CASE WHEN (H.ORD_DT >= TRUNC(SYSDATE) - 1) THEN 'O' ELSE 'X' END F1__ FROM ORD_DTL D, ORD_HDR H WHERE …", "0행", 40],
  ["APP.PKG_ORD.P_INIT 88줄 INSERT · INSERT … SELECT", "조인 단계: ORD_DTL D 에서 이 키", "SELECT COUNT(*) AS N__ FROM ORD_DTL D WHERE (D.ORD_NO) = :K1", "3건", 9],
  ["APP.PKG_ORD.P_INIT 88줄 INSERT · INSERT … SELECT", "조인 단계: + ORD_HDR H", "SELECT COUNT(*) AS N__ FROM ORD_DTL D, ORD_HDR H WHERE (D.ORD_NO) = :K1 AND (H.ORD_NO = D.ORD_NO)", "0건", 11],
  ["APP.PKG_ORD.P_INIT 88줄 INSERT · INSERT … SELECT", "ORD_HDR 에서 0건 — 여기서 끊김, 조인 단계 멈춤"],
  ["APP.PKG_ORD.P_SUMMARY 212줄 MERGE · MERGE · 없으면 INSERT", "이 키로 다시 계산", "SELECT * FROM (SELECT S.ORD_NO AS V__, S.ORD_NO AS K1__ FROM (…) S) WHERE K1__ = :K1 AND ROWNUM <= 5", "0행", 70],
  ["APP.PKG_ORD.P_SUMMARY 212줄 MERGE · MERGE · 없으면 INSERT › 서브쿼리 S", "서브쿼리 안으로: ORD_NO = (D.ORD_NO)"],
  ["APP.PKG_ORD.P_SUMMARY 212줄 MERGE · MERGE · 없으면 INSERT › 서브쿼리 S", "이 키로 다시 계산", "SELECT * FROM (SELECT (D.ORD_NO) AS V__ … WHERE (D.STATUS = 'OK') GROUP BY D.ORD_NO) WHERE K1__ = :K1 AND ROWNUM <= 5", "0행", 58],
  ["APP.PKG_ORD.P_SUMMARY 212줄 MERGE · MERGE · 없으면 INSERT › 서브쿼리 S", "0건 — 거르는 조건마다 통과 여부", "SELECT CASE WHEN (D.STATUS = 'OK') THEN 'O' ELSE 'X' END F1__ FROM ORD_DTL D WHERE (D.ORD_NO) = :K1", "3행", 21],
  ["ORD_SUM", "분석 결과에서 ORD_SUM 의 행을 지우는 문장 1개를 찾음"],
]);

/** 브라우저에서만 — 진짜 백엔드가 맡던 것 */
const LOCAL: Record<string, (a: Args) => unknown> = {
  analyze_sql: analyze,
  app_info: () => ({ version: "가짜 모드 (브라우저)", config_path: "(없음)" }),
  tns_list: () => ({ entries: [] }),
  list_providers: () => [{ name: "가짜 로컬", kind: "ollama", model: "qwen3.5:4b", remote: false, remote_ok: true, has_key: true,
    profile: { model: "qwen3.5:4b", tok_per_sec: 74.5, json: true, think: true, context: 262144 } }],
  ai_probe: async () => { await sleep(600); return { model: "qwen3.5:4b", tok_per_sec: 74.5, json: true, think: true, context: 262144 }; },
  ai_harness_opts: () => ({ table_pick: "auto", examples: true, examples_count: 3, examples_path: "(가짜)/ai-examples.jsonl" }),
  set_ai_harness_opts: () => null,
  ai_examples_clear: () => null,
  ai_remember_example: () => null,
  // 값 추적 흉내: MERGE 는 USING 서브쿼리 안에서 STATUS 조건에 걸려 값이 안 나옴, INSERT … SELECT 는 재현됨 + 원본 행 → 커서로 한 단계 더
  value_trace_start: async () => {
    await sleep(1800);
    const w = (o: Record<string, unknown>) => ({ notes: [], binds: [["K1", "A123"]], ran: true, values: [], dropped: [], unknown_vars: [], flag_conds: [], flag_rows: [], inputs: [], via: [], layers: [], keys: [], join_steps: [], ...o });
    return { target: { owner: "APP", table: "ORD_SUM", column: "TOT_AMT", keys: [["ORD_NO", "A123"]], value: "0", known: true, vars: [] }, analyzed: true, notes: [], default: null, log: VT_LOG,
      lacking: [{ node: "APP.PKG_IF.P_REG_ORD", unit: "APP.PKG_IF.PACKAGE_BODY", line: 30, kind: "INSERT", produces: false, binds: [] }],
      writers: [
      w({ node: "APP.PKG_ORD.P_INIT", unit: "APP.PKG_ORD.PACKAGE_BODY", line: 88, end_line: 95, kind: "INSERT", label: "INSERT … SELECT", expr: "NVL(SUM(D.AMT), 0)",
        source: "FROM ORD_DTL D GROUP BY D.ORD_NO", reproduced: true, values: ["0"], keys: [["ORD_NO", "D.ORD_NO", "A123"]],
        sql: "SELECT * FROM (SELECT NVL(SUM(D.AMT), 0) AS V__, D.ORD_NO AS K1__ FROM ORD_DTL D GROUP BY D.ORD_NO) WHERE K1__ = :K1 AND ROWNUM <= 5",
        inputs: [{ owner: null, table: "ORD_DTL", column: "AMT", alias: "D", key_cols: ["ORD_NO", "SEQ"], missing: 0, rows: [
          { keys: [["ORD_NO", "A123"], ["SEQ", "1"]], value: null }, { keys: [["ORD_NO", "A123"], ["SEQ", "2"]], value: null }] }] }),
      w({ node: "APP.PKG_ORD.P_SUMMARY", unit: "APP.PKG_ORD.PACKAGE_BODY", line: 212, end_line: 222, kind: "MERGE", label: "MERGE · 있으면 UPDATE", expr: "NVL(S.AMT, 0)",
        source: "FROM ORD_SUM T, (SELECT D.ORD_NO, SUM(D.AMT) AMT FROM ORD_DTL D WHERE D.STATUS = 'OK' GROUP BY D.ORD_NO) S WHERE T.ORD_NO = S.ORD_NO",
        keys: [["ORD_NO", "T.ORD_NO", "A123"]],
        sql: "SELECT * FROM (SELECT NVL(S.AMT, 0) AS V__, T.ORD_NO AS K1__ FROM ORD_SUM T, (…) S WHERE (T.ORD_NO = S.ORD_NO)) WHERE K1__ = :K1 AND ROWNUM <= 5",
        via: [{ kind: "문장", name: "", expr: "NVL(S.AMT, 0)" }, { kind: "서브쿼리", name: "S", expr: "NVL((SUM(D.AMT)), 0)" }],
        layers: [w({ label: "서브쿼리 S", via: [{ kind: "서브쿼리", name: "S", expr: "NVL((SUM(D.AMT)), 0)" }], expr: "NVL((SUM(D.AMT)), 0)",
          keys: [["ORD_NO", "(D.ORD_NO)", "A123"]], source: "FROM ORD_DTL D WHERE D.STATUS = 'OK' GROUP BY D.ORD_NO",
          sql: "SELECT * FROM (SELECT NVL((SUM(D.AMT)), 0) AS V__, (D.ORD_NO) AS K1__ FROM ORD_DTL D WHERE (D.STATUS = 'OK') GROUP BY D.ORD_NO) WHERE K1__ = :K1 AND ROWNUM <= 5",
          flag_conds: ["D.STATUS = 'OK'"], flag_rows: [["X"], ["X"], ["X"]] })] }),
    ] };
  },
  value_trace_step: async (a) => {
    await sleep(1200);
    const t = (a as { target: Record<string, unknown> }).target;
    return { target: { ...t, owner: "APP" }, analyzed: true, notes: [], log: mockLog([
      ["APP.ORD_DTL(ORD_NO=A123, SEQ=1).AMT", "분석 결과에서 ORD_DTL.AMT 에 값을 넣는 문장 1개를 찾음"],
      ["APP.PKG_IF.P_LOAD_DTL 41줄 INSERT · INSERT … VALUES ← 커서 C_SRC (12줄)", "길: AMT = R.AMT ← 커서 C_SRC (12줄)"],
      ["APP.PKG_IF.P_LOAD_DTL 41줄 INSERT · INSERT … VALUES ← 커서 C_SRC (12줄)", "이 키로 다시 계산", "SELECT * FROM (SELECT (S.AMT_TXT) AS V__ … FROM IF_ORD_DTL S WHERE S.IF_DT = V_BASE_DT) WHERE …", "오류", 15, "ORA-00904: \"V_BASE_DT\": invalid identifier"],
      ["APP.PKG_IF.P_LOAD_DTL 41줄 INSERT · INSERT … VALUES ← 커서 C_SRC (12줄)", "V_BASE_DT 은(는) 값을 모르는 PL/SQL 변수 — 이 변수를 쓰는 조건을 빼고 다시"],
      ["APP.PKG_IF.P_LOAD_DTL 41줄 INSERT · INSERT … VALUES ← 커서 C_SRC (12줄)", "이 키로 다시 계산 (모르는 변수 V_BASE_DT 의 조건을 빼고)", "SELECT * FROM (SELECT (S.AMT_TXT) AS V__, (S.ORD_NO) AS K1__, (S.SEQ) AS K2__ FROM IF_ORD_DTL S) WHERE K1__ = :K1 AND K2__ = :K2 AND ROWNUM <= 5", "1행", 210],
      ["APP.PKG_IF.P_LOAD_DTL 41줄 INSERT · INSERT … VALUES ← 커서 C_SRC (12줄)", "값을 만든 원본 행과 그 키", "SELECT … FROM IF_ORD_DTL S WHERE … AND ROWNUM <= 20", "1행", 180],
    ]), writers: [
      { node: "APP.PKG_IF.P_LOAD_DTL", unit: "APP.PKG_IF.PACKAGE_BODY", line: 41, end_line: 42, kind: "INSERT", label: "INSERT … VALUES ← 커서 C_SRC (12줄)", expr: "(S.AMT_TXT)",
        source: "FROM IF_ORD_DTL S WHERE S.IF_DT = V_BASE_DT", notes: [], binds: [["K1", "A123"], ["K2", "1"]], ran: true, values: [null], reproduced: true,
        dropped: ["S.IF_DT = V_BASE_DT"], unknown_vars: ["V_BASE_DT"], flag_conds: [], flag_rows: [], layers: [],
        keys: [["ORD_NO", "(S.ORD_NO)", "A123"], ["SEQ", "(S.SEQ)", "1"]],
        via: [{ kind: "문장", name: "", line: 41, expr: "R.AMT" }, { kind: "커서", name: "C_SRC", line: 12, expr: "(S.AMT_TXT)" }],
        inputs: [{ owner: null, table: "IF_ORD_DTL", column: "AMT_TXT", alias: "S", key_cols: ["IF_SEQ"], missing: 0, rows: [{ keys: [["IF_SEQ", "90017"]], value: null }] }], join_steps: [],
        sql: "SELECT * FROM (SELECT (S.AMT_TXT) AS V__, (S.ORD_NO) AS K1__, (S.SEQ) AS K2__ FROM IF_ORD_DTL S) WHERE K1__ = :K1 AND K2__ = :K2 AND ROWNUM <= 5" },
    ] };
  },
  // 행 추적 흉내: A999 는 ORD_DTL 엔 있지만 ORD_HDR 과 조인하면 끊기고, MERGE 는 서브쿼리 안 STATUS 조건에 걸림
  row_trace_prepare: (a) => ({ owner: "APP", table: String((a.table as string) || "ORD_SUM").toUpperCase(), key_cols: ["ORD_NO"], pk: ["ORD_NO"],
    columns: ["ORD_NO", "TOT_AMT", "UPD_DT", "STATUS"], values: [/ORD_NO\s*=\s*'([^']*)'/i.exec(String(a.sql ?? ""))?.[1] ?? null] }),
  trace_source: async (a) => { await sleep(200); return mockSource(String(a.unit)); },
  row_trace: async (a) => {
    await sleep(1500);
    const keys = a.keys as [string, string][];
    const v = keys[0]?.[1] ?? "";
    const w = (o: Record<string, unknown>) => ({ notes: [], binds: [["K1", v]], ran: true, values: [], dropped: [], unknown_vars: [], flag_conds: [], flag_rows: [], inputs: [], via: [], layers: [], keys: [], join_steps: [], ...o });
    return { target: { owner: "APP", table: a.table, column: "ORD_NO", keys, value: null, known: false, vars: [] }, analyzed: true, exists: false, log: RT_LOG,
      exists_sql: "SELECT COUNT(*) AS N__ FROM APP.ORD_SUM WHERE ORD_NO = :K1",
      writers: [
        w({ node: "APP.PKG_ORD.P_INIT", unit: "APP.PKG_ORD.PACKAGE_BODY", line: 88, end_line: 95, kind: "INSERT", label: "INSERT … SELECT", expr: "D.ORD_NO",
          source: "FROM ORD_DTL D, ORD_HDR H WHERE H.ORD_NO = D.ORD_NO AND H.ORD_DT >= TRUNC(SYSDATE) - 1 GROUP BY D.ORD_NO", keys: [["ORD_NO", "D.ORD_NO", v]],
          sql: "SELECT * FROM (SELECT D.ORD_NO AS V__, D.ORD_NO AS K1__ FROM ORD_DTL D, ORD_HDR H WHERE (H.ORD_NO = D.ORD_NO) AND (H.ORD_DT >= TRUNC(SYSDATE) - 1) GROUP BY D.ORD_NO) WHERE K1__ = :K1 AND ROWNUM <= 5",
          flag_conds: ["H.ORD_DT >= TRUNC(SYSDATE) - 1"],
          join_steps: [{ alias: "D", table: "ORD_DTL", on: [], rows: 3, sql: "SELECT COUNT(*) AS N__ FROM ORD_DTL D WHERE (D.ORD_NO) = :K1" },
            { alias: "H", table: "ORD_HDR", on: ["H.ORD_NO = D.ORD_NO"], rows: 0, sql: "SELECT COUNT(*) AS N__ FROM ORD_DTL D, ORD_HDR H WHERE (D.ORD_NO) = :K1 AND (H.ORD_NO = D.ORD_NO)" }] }),
        w({ node: "APP.PKG_ORD.P_SUMMARY", unit: "APP.PKG_ORD.PACKAGE_BODY", line: 212, end_line: 222, kind: "MERGE", label: "MERGE · 없으면 INSERT", expr: "S.ORD_NO",
          source: "FROM (SELECT D.ORD_NO, SUM(D.AMT) AMT FROM ORD_DTL D WHERE D.STATUS = 'OK' GROUP BY D.ORD_NO) S", keys: [["ORD_NO", "S.ORD_NO", v]],
          sql: "SELECT * FROM (SELECT S.ORD_NO AS V__, S.ORD_NO AS K1__ FROM (…) S) WHERE K1__ = :K1 AND ROWNUM <= 5",
          via: [{ kind: "문장", name: "", expr: "S.ORD_NO" }, { kind: "서브쿼리", name: "S", expr: "(D.ORD_NO)" }],
          layers: [w({ label: "서브쿼리 S", via: [{ kind: "서브쿼리", name: "S", expr: "(D.ORD_NO)" }], expr: "(D.ORD_NO)", keys: [["ORD_NO", "(D.ORD_NO)", v]],
            sql: "SELECT * FROM (SELECT (D.ORD_NO) AS V__, (D.ORD_NO) AS K1__ FROM ORD_DTL D WHERE (D.STATUS = 'OK') GROUP BY D.ORD_NO) WHERE K1__ = :K1 AND ROWNUM <= 5",
            flag_conds: ["D.STATUS = 'OK'"], flag_rows: [["X"], ["X"], ["X"]] })] }),
      ],
      deleters: [{ node: "APP.PKG_ORD.P_PURGE", unit: "APP.PKG_ORD.PACKAGE_BODY", line: 301, end_line: 301, kind: "DELETE", text: "DELETE FROM ORD_SUM WHERE UPD_DT < ADD_MONTHS(SYSDATE, -6)" }] };
  },
  ai_guides_info: () => ({ path: "(가짜)/oracle-guides.md", exists: false, builtin: 24, custom: 0 }),
  ai_guide_select: () => "auto",
  save_provider: () => null,
  // 하네스 흉내: 문맥 → 첫 답(없는 컬럼) → 지우고 고친 답 → 서버 파싱 통과
  ai_ask: async (a) => {
    const ch = a.onEvent as Channel<unknown>;
    const say = async (t: string) => { for (const w of t.match(/[\s\S]{1,6}/g) ?? []) { ch.onmessage({ type: "delta", text: w }); await sleep(15); } };
    if ((a.args as { task?: string }).task === "trace") {
      ch.onmessage({ type: "context", tables: [], plan: false, remote: false, guides: [], tokens: 640, budget: 8000, trimmed: [] });
      await sleep(900);
      const text = "1) 출처 경로: ORD_SUM.TOT_AMT(ORD_NO=A123) = 0 ← PKG_ORD.P_INIT 88줄 INSERT … SELECT [NVL(SUM(D.AMT), 0)] ← ORD_DTL.AMT(A123, 1·2) = NULL ← PKG_IF.P_LOAD_DTL 41줄 ← IF_ORD_DTL.AMT_TXT\n" +
        "2) IF_ORD_DTL.AMT_TXT 가 NULL 로 적재되어 ORD_DTL.AMT 두 행이 모두 NULL → SUM 이 NULL → NVL 로 0 이 되어 TOT_AMT 에 들어갔습니다.\n" +
        "3) MERGE(P_SUMMARY 212줄)는 STATUS = 'OK' 조건에 세 행이 모두 걸려 이 행을 만들지 않았습니다. P_LOAD_DTL 의 V_BASE_DT 조건은 값을 몰라 빼고 봤습니다.";
      await say(text);
      return { text, model: "qwen3.5:4b", refused: false, truncated: false, user_message: "", verify: { state: "none", repairs: 0 }, warnings: [] };
    }
    ch.onmessage({ type: "context", tables: ["SCOTT.EMP", "SCOTT.DEPT"], plan: false, remote: false, guides: ["ROWNUM 페이징"], tokens: 2140, budget: 8000, trimmed: ["컬럼 많은 테이블은 관련 컬럼만"],
      picked: "ai", examples: ["부서별 사원 수"] });
    await say("부서별 급여 합계입니다.\n```sql\nSELECT dept_no, SUM(sal) FROM emp GROUP BY dept_no;\n```");
    ch.onmessage({ type: "status", text: "확인에서 걸렸습니다 (ORA-00904: \"DEPT_NO\": invalid identifier) — 고쳐 달라고 다시 묻는 중 (1/2)" });
    await sleep(300);
    ch.onmessage({ type: "reset" });
    const text = "DEPT_NO 를 DEPTNO 로 고쳤습니다.\n```sql\nSELECT deptno, SUM(sal) AS total_sal\n  FROM emp\n GROUP BY deptno;\n```";
    await say(text);
    return { text, sql: "SELECT deptno, SUM(sal) AS total_sal FROM emp GROUP BY deptno;", model: "qwen3.5:4b", refused: false, truncated: false, output_tokens: 42, user_message: "## 요청\n" + (a.args as { question?: string }).question,
      verify: { state: "ok", repairs: 1, first_problem: "ORA-00904: \"DEPT_NO\": invalid identifier" }, warnings: [] };
  },
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
