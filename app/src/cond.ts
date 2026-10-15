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
  if (NUMERIC.test(t)) return /^-?\d+(\.\d+)?([eE][-+]?\d+)?$/.test(v.trim()) ? v.trim() : q(v);
  if (/^DATE/i.test(t) || /^TIMESTAMP/i.test(t)) {
    const m = TS.exec(v);
    if (!m) return null;
    const base = `${m[1]} ${m[2]}`;
    if (/^DATE/i.test(t)) return `TO_DATE('${base}', 'YYYY-MM-DD HH24:MI:SS')`;
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
