// 결과 그리드의 셀 값을 조회 쿼리의 조건으로 붙인다 — 맨 끝에 AND 를 이어 붙이면 ORDER BY·GROUP BY 뒤라 깨지므로,
// 최상위(괄호 밖)의 WHERE 끝 — 곧 WHERE 가 없으면 새로, GROUP BY·ORDER BY·CONNECT BY·FOR UPDATE 같은 다음 절 바로 앞에 넣는다.

import type { Column } from "./api";

export type CondOp = "eq" | "like";

interface Scan {
  /** 괄호 밖(깊이 0)의 낱말: 대문자, 시작 위치(sql 안) */
  words: { w: string; at: number }[];
  /** 마지막 글자(공백·세미콜론 앞) 위치 + 1 */
  end: number;
}

/** 문자열·따옴표 이름·주석을 건너뛰며 깊이 0 의 낱말을 모은다 */
function scan(sql: string): Scan {
  const words: { w: string; at: number }[] = [];
  let depth = 0;
  let i = 0;
  const n = sql.length;
  while (i < n) {
    const c = sql[i];
    if (c === "-" && sql[i + 1] === "-") { while (i < n && sql[i] !== "\n") i++; continue; }
    if (c === "/" && sql[i + 1] === "*") { const j = sql.indexOf("*/", i + 2); i = j < 0 ? n : j + 2; continue; }
    if (c === "'") { i++; while (i < n) { if (sql[i] === "'") { if (sql[i + 1] === "'") { i += 2; continue; } break; } i++; } i++; continue; }
    if (c === '"') { const j = sql.indexOf('"', i + 1); i = j < 0 ? n : j + 1; continue; }
    if (c === "(") { depth++; i++; continue; }
    if (c === ")") { depth = Math.max(0, depth - 1); i++; continue; }
    if (/[A-Za-z_]/.test(c)) {
      const s = i;
      while (i < n && /[\w$#]/.test(sql[i])) i++;
      if (depth === 0) words.push({ w: sql.slice(s, i).toUpperCase(), at: s });
      continue;
    }
    i++;
  }
  // 끝: 뒤의 공백·세미콜론·슬래시 줄을 뺀다
  let end = n;
  while (end > 0 && /[\s;/]/.test(sql[end - 1])) end--;
  return { words, end };
}

/** 다음 절의 시작 — WHERE 뒤에 올 수 있는 절들 */
const AFTER_WHERE = new Set(["CONNECT", "START", "GROUP", "HAVING", "ORDER", "FETCH", "OFFSET", "MODEL"]);
const SET_OPS = new Set(["UNION", "INTERSECT", "MINUS"]);

/** SQL 에 조건 하나를 붙인다. 못 붙이면 이유 */
export function addCondition(sql: string, cond: string): { sql: string } | { error: string } {
  const { words, end } = scan(sql);
  const first = words[0]?.w;
  if (first !== "SELECT" && first !== "WITH") return { error: "조회(SELECT) 문에만 조건을 붙일 수 있습니다" };
  if (words.some((x) => SET_OPS.has(x.w))) return { error: "UNION·INTERSECT·MINUS 가 있어 어느 SELECT 에 붙일지 알 수 없습니다 — 직접 넣어 주세요" };
  // 최상위 SELECT 의 FROM (WITH 절의 SELECT 는 괄호 안이라 깊이 0 에 없다)
  const sel = words.findIndex((x) => x.w === "SELECT");
  const from = words.findIndex((x, k) => k > sel && x.w === "FROM");
  if (sel < 0 || from < 0) return { error: "FROM 을 찾지 못했습니다" };
  const where = words.findIndex((x, k) => k > from && x.w === "WHERE");
  // WHERE 다음(없으면 FROM 다음)에 오는 첫 절
  const startK = where >= 0 ? where : from;
  let next = words.findIndex((x, k) => k > startK && (AFTER_WHERE.has(x.w) || (x.w === "FOR" && words[k + 1]?.w === "UPDATE")));
  // START WITH 의 WITH, ORDER BY 의 BY 같은 것은 위 집합에 없으니 괜찮다. 단 'CONNECT BY' 앞에 WHERE 가 올 수 있다
  const stop = next >= 0 ? words[next].at : end;
  const head = sql.slice(0, stop).replace(/\s+$/, "");
  const tail = next >= 0 ? sql.slice(stop) : sql.slice(end);
  if (where >= 0) {
    // 기존 조건에 최상위 OR 가 있으면 괄호로 묶는다 (AND 가 OR 보다 먼저 묶이므로)
    const wAt = words[where].at + "WHERE".length;
    const body = head.slice(wAt);
    const hasOr = scan(body).words.some((x) => x.w === "OR");
    const newHead = hasOr ? `${head.slice(0, wAt)} (${body.trim()})` : head;
    return { sql: `${newHead}\n  AND ${cond}${tail ? (next >= 0 ? `\n${tail}` : tail) : ""}` };
  }
  return { sql: `${head}\nWHERE ${cond}${tail ? (next >= 0 ? `\n${tail}` : tail) : ""}` };
}

/** 결과 열 이름에 해당하는 SELECT 목록의 식 — `o.status` 처럼 별칭이 붙은 이름이면 그대로 써서 조인에서 모호하지 않게. 못 찾으면 null */
export function selectExpr(sql: string, colName: string): string | null {
  const { words } = scan(sql);
  const sel = words.findIndex((x) => x.w === "SELECT");
  const from = words.findIndex((x, k) => k > sel && x.w === "FROM");
  if (sel < 0 || from < 0) return null;
  const list = sql.slice(words[sel].at + 6, words[from].at);
  // 깊이 0 의 쉼표로 항목을 나눈다
  const items: string[] = [];
  let depth = 0, cur = "", q = "";
  for (let i = 0; i < list.length; i++) {
    const c = list[i];
    if (q) { cur += c; if (c === q) q = ""; continue; }
    if (c === "'" || c === '"') { q = c; cur += c; continue; }
    if (c === "(") depth++;
    else if (c === ")") depth--;
    if (c === "," && depth === 0) { items.push(cur); cur = ""; continue; }
    cur += c;
  }
  items.push(cur);
  const want = colName.toUpperCase();
  const hits: string[] = [];
  for (let item of items) {
    item = item.replace(/--[^\n]*/g, "").replace(/\/\*[\s\S]*?\*\//g, "").trim().replace(/^(DISTINCT|UNIQUE|ALL)\s+/i, "");
    // 단순한 [별칭.]열 [AS] [별칭] 만
    const m = /^((?:"[^"]+"|\w[\w$#]*)(?:\.(?:"[^"]+"|\w[\w$#]*))?)(?:\s+(?:AS\s+)?("[^"]+"|\w[\w$#]*))?$/i.exec(item);
    if (!m) continue;
    const expr = m[1];
    const out = (m[2] ?? expr.split(".").pop()!).replace(/"/g, "").toUpperCase();
    if (out === want) hits.push(expr);
  }
  // 같은 이름이 둘 이상이면(다른 별칭의 같은 열) 어느 것인지 모른다
  return hits.length === 1 ? hits[0] : null;
}

const NUMERIC = /^(NUMBER|FLOAT|BINARY_FLOAT|BINARY_DOUBLE|INTEGER|INT|SMALLINT|DECIMAL|NUMERIC|REAL|DOUBLE)/i;
const TS = /^(\d{4}-\d{2}-\d{2})[ T](\d{2}:\d{2}:\d{2})(\.\d+)?/;

const q = (s: string) => `'${s.replace(/'/g, "''")}'`;
/** 식별자 — 따옴표가 필요한 이름만 따옴표로 */
const ident = (s: string) => (/^[A-Z][A-Z0-9_$#]*$/.test(s) ? s : `"${s.replace(/"/g, '""')}"`);

/** 값 하나의 SQL 리터럴 (열 종류에 맞게). 못 만들면 null */
function literal(col: Column, v: string): string | null {
  const t = col.type_name || "";
  // .5 · -.5 · 1. 처럼 앞뒤 0 이 빠진 숫자도 그대로
  if (NUMERIC.test(t)) return /^-?(\d+(\.\d*)?|\.\d+)([eE][-+]?\d+)?$/.test(v.trim()) ? v.trim() : q(v);
  if (/^DATE/i.test(t) || /^TIMESTAMP/i.test(t)) {
    const m = TS.exec(v);
    if (!m) return null;
    const base = `${m[1]} ${m[2]}`;
    if (/^DATE/i.test(t)) return `TO_DATE('${base}', 'YYYY-MM-DD HH24:MI:SS')`;
    // 시간대가 있는 열은 값의 시간대(+09:00)까지 — 빼면 세션 시간대로 읽혀 같은 값이 안 맞을 수 있다
    const tz = /TIME ZONE/i.test(t) ? /\s*([+-]\d{2}:\d{2})\s*$/.exec(v)?.[1] : undefined;
    if (tz) return `TO_TIMESTAMP_TZ('${base}${m[3] ?? ""} ${tz}', 'YYYY-MM-DD HH24:MI:SS${m[3] ? ".FF9" : ""} TZH:TZM')`;
    return m[3] ? `TO_TIMESTAMP('${base}${m[3]}', 'YYYY-MM-DD HH24:MI:SS.FF9')` : `TO_TIMESTAMP('${base}', 'YYYY-MM-DD HH24:MI:SS')`;
  }
  return q(v);
}

/** 셀 값으로 만든 조건 한 줄. exprOverride: SELECT 목록의 식 (없으면 열 이름) */
export function cellCondition(col: Column, value: string | null, op: CondOp, exprOverride?: string | null): { cond: string } | { error: string } {
  const name = exprOverride ?? ident(col.name);
  if (value == null) return op === "eq" ? { cond: `${name} IS NULL` } : { error: "빈 값(NULL)에는 LIKE 를 쓸 수 없습니다" };
  if (op === "eq") {
    const lit = literal(col, value);
    return lit == null ? { error: `'${col.name}' 의 값을 조건으로 바꿀 수 없습니다 (${col.type_name})` } : { cond: `${name} = ${lit}` };
  }
  // LIKE — 글자 열만 (숫자·날짜는 같은 값만 의미가 있다)
  if (NUMERIC.test(col.type_name || "") || /^(DATE|TIMESTAMP)/i.test(col.type_name || "")) return { error: "LIKE 는 글자 열에만 씁니다 — 숫자·날짜는 '같다(=)' 를 쓰세요" };
  const needsEscape = /[%_\\]/.test(value);
  const body = needsEscape ? value.replace(/([%_\\])/g, "\\$1") : value;
  return { cond: `${name} LIKE ${q(`%${body}%`)}${needsEscape ? " ESCAPE '\\'" : ""}` };
}

// ── 여러 셀(한 열)의 값 목록 → IN 절·조건·복사 문자열 ─────────────────────────────

/** 중복 없이(처음 나온 순서), NULL 은 따로 */
function distinct(values: (string | null)[]): { vals: string[]; hasNull: boolean } {
  const seen = new Set<string>();
  const vals: string[] = [];
  let hasNull = false;
  for (const v of values) {
    if (v == null) { hasNull = true; continue; }
    if (!seen.has(v)) { seen.add(v); vals.push(v); }
  }
  return { vals, hasNull };
}

export type ListCond = "in" | "notin" | "like";
export type ListText = "in" | "cond" | "comma" | "quoted" | "lines";

/** 조건 한 줄: IN / NOT IN / LIKE 여러 개(OR). NULL 을 정확히 다룬다 —
 *  IN: 고른 것에 NULL 이 있으면 `OR 열 IS NULL`. NOT IN: 'NULL 이 아닌 다른 값' 이 되도록 NULL 행은 남기거나(고르지 않았으면) 뺀다(골랐으면) */
export function listCondition(col: Column, values: (string | null)[], kind: ListCond, exprOverride?: string | null): { cond: string } | { error: string } {
  const name = exprOverride ?? ident(col.name);
  const { vals, hasNull } = distinct(values);
  if (!vals.length && !hasNull) return { error: "고른 값이 없습니다" };
  if (kind === "like") {
    if (NUMERIC.test(col.type_name || "") || /^(DATE|TIMESTAMP)/i.test(col.type_name || "")) return { error: "LIKE 는 글자 열에만 씁니다 — 숫자·날짜는 IN 을 쓰세요" };
    if (!vals.length) return { error: "빈 값(NULL)에는 LIKE 를 쓸 수 없습니다" };
    const parts = vals.map((v) => {
      const esc = /[%_\\]/.test(v);
      return `${name} LIKE ${q(`%${esc ? v.replace(/([%_\\])/g, "\\$1") : v}%`)}${esc ? " ESCAPE '\\'" : ""}`;
    });
    const all = hasNull ? [...parts, `${name} IS NULL`] : parts;
    return { cond: all.length === 1 ? all[0] : `(${all.join(" OR ")})` };
  }
  const lits: string[] = [];
  for (const v of vals) {
    const l = literal(col, v);
    if (l == null) return { error: `'${col.name}' 의 값 '${v}' 을 조건으로 바꿀 수 없습니다 (${col.type_name})` };
    lits.push(l);
  }
  if (kind === "in") {
    if (!lits.length) return { cond: `${name} IS NULL` };
    const inn = lits.length === 1 ? `${name} = ${lits[0]}` : `${name} IN (${lits.join(", ")})`;
    return { cond: hasNull ? `(${inn} OR ${name} IS NULL)` : inn };
  }
  // NOT IN
  if (!lits.length) return { cond: `${name} IS NOT NULL` };
  const not = lits.length === 1 ? `${name} <> ${lits[0]}` : `${name} NOT IN (${lits.join(", ")})`;
  return { cond: hasNull ? `${name} IS NOT NULL AND ${not}` : `(${not} OR ${name} IS NULL)` };
}

/** 복사할 글: IN 절 `('A','B')`, 조건까지 `COL IN ('A','B')`, 쉼표 `A,B`, 따옴표 쉼표 `'A','B'`, 줄마다 하나 */
export function listText(col: Column, values: (string | null)[], form: ListText): { text: string } | { error: string } {
  const { vals, hasNull } = distinct(values);
  if (!vals.length) return { error: hasNull ? "고른 값이 모두 빈 값(NULL)입니다" : "고른 값이 없습니다" };
  if (form === "comma") return { text: vals.join(",") };
  if (form === "lines") return { text: vals.join("\n") };
  if (form === "quoted") return { text: vals.map(q).join(",") };
  const lits: string[] = [];
  for (const v of vals) {
    const l = literal(col, v);
    if (l == null) return { error: `'${col.name}' 의 값 '${v}' 을 SQL 로 바꿀 수 없습니다 (${col.type_name})` };
    lits.push(l);
  }
  if (form === "in") return { text: `(${lits.join(", ")})` };
  const c = listCondition(col, values, "in");
  return "error" in c ? c : { text: c.cond };
}

// ── 계산한 열(DECODE·함수·집계·별칭)·UNION 은 WHERE 에 별칭을 쓸 수 없다 → SELECT * FROM (원본) WHERE 조건 으로 감싼다 ──────────

const WORD_SET_OPS = new Set(["UNION", "INTERSECT", "MINUS"]);

/** 첫 SELECT 의 목록 항목 (깊이 0 의 쉼표로 나눈 글) — 못 찾으면 null */
function selectList(sql: string): string[] | null {
  const { words } = scan(sql);
  const sel = words.findIndex((x) => x.w === "SELECT");
  const from = words.findIndex((x, k) => k > sel && x.w === "FROM");
  if (sel < 0 || from < 0) return null;
  const list = sql.slice(words[sel].at + 6, words[from].at);
  const items: string[] = [];
  let depth = 0, cur = "", quote = "";
  for (let i = 0; i < list.length; i++) {
    const c = list[i];
    if (quote) { cur += c; if (c === quote) quote = ""; continue; }
    if (c === "'" || c === '"') { quote = c; cur += c; continue; }
    if (c === "(") depth++;
    else if (c === ")") depth--;
    if (c === "," && depth === 0) { items.push(cur); cur = ""; continue; }
    cur += c;
  }
  items.push(cur);
  return items.map((s) => s.replace(/--[^\n]*/g, "").replace(/\/\*[\s\S]*?\*\//g, "").trim().replace(/^(DISTINCT|UNIQUE|ALL)\s+/i, ""));
}

const SIMPLE_ITEM = /^((?:"[^"]+"|\w[\w$#]*)(?:\.(?:"[^"]+"|\w[\w$#]*))?)(?:\s+(?:AS\s+)?("[^"]+"|\w[\w$#]*))?$/i;

/** 이 결과 열이 SELECT 목록의 단순한 열인지 — simple: `[별칭.]열 [AS 이름]` 이라 WHERE 에 그대로 쓸 수 있다,
 *  star: `*` 로 가져온 것(그 이름으로 쓴다), complex: DECODE·함수·집계·식 이거나 어느 항목인지 모르겠다 → 감싸야 한다 */
export function columnKind(sql: string, colName: string): "simple" | "star" | "complex" {
  const items = selectList(sql);
  if (!items) return "complex";
  const want = colName.toUpperCase();
  const hits = items.filter((item) => {
    const m = SIMPLE_ITEM.exec(item);
    if (m) return (m[2] ?? m[1].split(".").pop()!).replace(/"/g, "").toUpperCase() === want;
    // 단순하지 않은 항목: 별칭이 같으면 그 항목이 이 열이다 (예: DECODE(…) AS ST_NM)
    const alias = /\s(?:AS\s+)?("[^"]+"|\w[\w$#]*)$/i.exec(item);
    return !!alias && alias[1].replace(/"/g, "").toUpperCase() === want;
  });
  if (hits.length === 1) return SIMPLE_ITEM.test(hits[0]) ? "simple" : "complex";
  if (hits.length === 0 && items.some((i) => /(^|\.)\*$/.test(i))) return "star";
  return "complex";
}

/** SELECT 목록의 출력 이름들(대문자)과 `식 → 이름` */
function outputNames(sql: string): { names: Set<string>; byExpr: Map<string, string>; star: boolean } {
  const names = new Set<string>();
  const byExpr = new Map<string, string>();
  let star = false;
  for (const item of selectList(sql) ?? []) {
    if (/(^|\.)\*$/.test(item)) { star = true; continue; }
    const m = SIMPLE_ITEM.exec(item);
    if (m) {
      const name = (m[2] ?? m[1].split(".").pop()!).replace(/"/g, "").toUpperCase();
      names.add(name);
      byExpr.set(m[1].replace(/"/g, "").toUpperCase(), name);
      continue;
    }
    const alias = /\s(?:AS\s+)?("[^"]+"|\w[\w$#]*)$/i.exec(item);
    if (alias) names.add(alias[1].replace(/"/g, "").toUpperCase());
  }
  return { names, byExpr, star };
}

/** SQL 을 `SELECT * FROM (원본) WHERE 조건` 으로 감싼다. 끝의 ORDER BY 는 출력 열 이름·번호로 쓴 것만 바깥으로 옮긴다
 *  (안쪽의 ORDER BY 는 순서가 보장되지 않는다). FOR UPDATE 는 감쌀 수 없다 */
export function wrapCondition(sql: string, cond: string): { sql: string } | { error: string } {
  const { words, end } = scan(sql);
  const first = words[0]?.w;
  if (first !== "SELECT" && first !== "WITH") return { error: "조회(SELECT) 문에만 조건을 붙일 수 있습니다" };
  if (words.some((x, k) => x.w === "FOR" && words[k + 1]?.w === "UPDATE")) return { error: "FOR UPDATE 가 있는 쿼리는 감쌀 수 없습니다 — 조건을 직접 넣어 주세요" };
  const tailStart = (() => {
    const k = words.findIndex((x) => x.w === "FETCH" || x.w === "OFFSET");
    return k >= 0 ? words[k].at : end;
  })();
  // 마지막 ORDER BY (깊이 0) — FETCH·OFFSET 이 뒤에 오면 옮기지 않는다 (먼저 N 개를 자르고 거르는 뜻이 달라진다)
  let ob = -1;
  words.forEach((x, k) => { if (x.w === "ORDER" && words[k + 1]?.w === "BY") ob = k; });
  let inner = sql.slice(0, end).replace(/\s+$/, "");
  let lifted = "";
  if (ob >= 0 && tailStart === end) {
    const at = words[ob].at;
    const clause = sql.slice(at + "ORDER BY".length, end);
    const { names, byExpr, star } = outputNames(sql);
    const parts: string[] = [];
    let depth = 0, cur = "";
    for (const c of clause) {
      if (c === "(") depth++;
      else if (c === ")") depth--;
      if (c === "," && depth === 0) { parts.push(cur); cur = ""; continue; }
      cur += c;
    }
    parts.push(cur);
    const out: string[] = [];
    for (const p of parts) {
      const m = /^\s*(\S+?)((?:\s+(?:ASC|DESC))?(?:\s+NULLS\s+(?:FIRST|LAST))?)\s*$/i.exec(p);
      if (!m) { out.length = 0; break; }
      const key = m[1].replace(/"/g, "").toUpperCase();
      if (/^\d+$/.test(key)) out.push(`${m[1]}${m[2]}`);
      else if (!key.includes(".") && (names.has(key) || star)) out.push(`${m[1]}${m[2]}`);
      else if (byExpr.has(key)) out.push(`${byExpr.get(key)}${m[2]}`);
      else { out.length = 0; break; }
    }
    if (out.length === parts.length) {
      lifted = `ORDER BY ${out.map((s) => s.trim()).join(", ")}`;
      inner = sql.slice(0, at).replace(/\s+$/, "");
    }
  }
  const tail = sql.slice(end); // 끝의 세미콜론·빈 줄
  return { sql: `SELECT *\nFROM (\n${inner}\n)\nWHERE ${cond}${lifted ? `\n${lifted}` : ""}${tail}` };
}

/** 셀 값 조건을 쿼리에 놓는다 — 단순한 열은 WHERE 끝에(인덱스를 탈 수 있고 쿼리가 그대로 보인다),
 *  계산한 열·집계·UNION 은 감싸서 (별칭은 WHERE 에 쓸 수 없고, 집계는 WHERE 가 아니다) */
export function placeCondition(
  sql: string,
  col: Column,
  make: (expr: string | null) => { cond: string } | { error: string },
): { sql: string; cond: string; wrapped: boolean } | { error: string } {
  const setOp = scan(sql).words.some((x) => WORD_SET_OPS.has(x.w));
  // 분석 함수(… OVER (…))·ROWNUM 이 SELECT 목록에 있으면 WHERE 에 넣는 순간 그 값(순번·누계)이 달라진다 — 감싼다
  const windowed = (selectList(sql) ?? []).some((i) => /\bOVER\s*\(|\bROWNUM\b/i.test(i));
  const kind = setOp || windowed ? "complex" : columnKind(sql, col.name);
  if (kind === "complex") {
    const c = make(null);
    if ("error" in c) return c;
    const w = wrapCondition(sql, c.cond);
    return "error" in w ? w : { sql: w.sql, cond: c.cond, wrapped: true };
  }
  const c = make(kind === "simple" ? selectExpr(sql, col.name) : null);
  if ("error" in c) return c;
  const a = addCondition(sql, c.cond);
  return "error" in a ? a : { sql: a.sql, cond: c.cond, wrapped: false };
}
