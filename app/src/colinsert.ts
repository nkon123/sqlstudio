// 컬럼 팝업에서 고른 컬럼을 SQL 에 넣는다.
//
// Enter: 커서가 있던 절에 맞춰 커서 자리에 (SELECT·ORDER BY·GROUP BY 는 쉼표로, WHERE·ON·HAVING 은 "열 = " 조건으로).
// 휠·메뉴: 커서와 상관없이 그 문장의 최상위 절에 (없으면 절을 만든다).

import { addCondition, scan } from "./cond";

export type Target = "select" | "where" | "order" | "group";

export const TARGET_LABEL: Record<Target, string> = { select: "SELECT", where: "WHERE", order: "ORDER BY", group: "GROUP BY" };

/** 넣은 결과: 문장 안에서 바꿀 범위와 글, 넣은 뒤 커서 (모두 문장 기준 위치) */
export interface Placed { from: number; to: number; insert: string; cursor: number }

/** 이 낱말이 오면 그 절이 끝난다 — 커서 뒤에 이것이 있으면 쉼표·AND 를 뒤에 붙이지 않는다 */
const CLAUSE_WORDS = new Set(["SELECT", "FROM", "WHERE", "GROUP", "ORDER", "HAVING", "UNION", "INTERSECT", "MINUS", "CONNECT", "START",
  "FETCH", "OFFSET", "FOR", "INTO", "JOIN", "LEFT", "RIGHT", "INNER", "FULL", "CROSS", "ON", "AND", "OR", "AS", "MODEL", "PARTITION", "WINDOW", "THEN", "ELSE", "END", "WHEN"]);

/** WHERE 다음에 올 수 있는 절의 첫 낱말 */
const WHERE_ENDS = new Set(["CONNECT", "START", "GROUP", "HAVING", "ORDER", "FETCH", "OFFSET", "MODEL", "FOR", "UNION", "INTERSECT", "MINUS"]);

/** 커서가 있는 절. 괄호마다 따로 본다 — 서브쿼리 안이면 그 서브쿼리의 절, 함수 괄호 안이면 바깥 절 */
export function clauseAt(sql: string, pos: number): Target | "other" {
  type C = Target | "other";
  const stack: C[] = ["other"];
  let i = 0;
  const n = Math.min(pos, sql.length);
  let prev = "";
  while (i < n) {
    const c = sql[i];
    if (c === "-" && sql[i + 1] === "-") { while (i < n && sql[i] !== "\n") i++; continue; }
    if (c === "/" && sql[i + 1] === "*") { const j = sql.indexOf("*/", i + 2); i = j < 0 ? n : j + 2; continue; }
    if (c === "'") { i++; while (i < n) { if (sql[i] === "'") { if (sql[i + 1] === "'") { i += 2; continue; } break; } i++; } i++; continue; }
    if (c === '"') { const j = sql.indexOf('"', i + 1); i = j < 0 ? n : j + 1; continue; }
    if (c === "(") { stack.push(stack[stack.length - 1]); i++; continue; }
    if (c === ")") { if (stack.length > 1) stack.pop(); i++; continue; }
    if (/[A-Za-z_]/.test(c)) {
      const s = i;
      while (i < n && /[\w$#]/.test(sql[i])) i++;
      const w = sql.slice(s, i).toUpperCase();
      let to: C | null = null;
      switch (w) {
        case "SELECT": to = "select"; break;
        case "WHERE": case "ON": case "HAVING": to = "where"; break;
        case "BY": to = prev === "ORDER" || prev === "SIBLINGS" ? "order" : prev === "GROUP" || prev === "PARTITION" ? "group" : prev === "CONNECT" ? "where" : null; break;
        case "WITH": to = prev === "START" ? "where" : "other"; break;
        case "FROM": case "JOIN": case "INTO": case "SET": case "VALUES": case "UNION": case "INTERSECT": case "MINUS": case "UPDATE": case "DELETE": case "INSERT": case "MERGE": case "USING":
        case "FETCH": case "OFFSET": case "FOR": to = "other"; break;
      }
      if (to) stack[stack.length - 1] = to;
      prev = w;
      continue;
    }
    i++;
  }
  return stack[stack.length - 1];
}

/** 커서 앞(공백 빼고)의 마지막 글자와 낱말 */
function before(sql: string, pos: number) {
  const head = sql.slice(0, pos).replace(/\s+$/, "");
  const ch = head.slice(-1);
  const word = /[\w$#]+$/.exec(head)?.[0].toUpperCase() ?? "";
  return { ch, word, touching: pos > 0 && !/\s/.test(sql[pos - 1]) };
}

/** 커서 뒤에 같은 절의 항목이 이어지는가 (그러면 쉼표·AND 를 뒤에 붙인다) */
function continues(sql: string, pos: number): boolean {
  const tail = sql.slice(pos).replace(/^\s+/, "");
  if (!tail || /^[,);]/.test(tail) || tail.startsWith("--") || tail.startsWith("/*")) return false;
  const w = /^[A-Za-z_][\w$#]*/.exec(tail)?.[0].toUpperCase();
  if (w) return !CLAUSE_WORDS.has(w);
  return /^["'(\d:]/.test(tail);
}

/** Enter: 커서 자리(`from`~`to` 는 고른 글자 — 바꾼다)에 그 절에 맞게. 절을 모르면 null (메뉴로 고르게) */
export function placeAtCursor(sql: string, from: number, to: number, cols: string[]): Placed | null {
  if (!cols.length) return null;
  const clause = clauseAt(sql, from);
  if (clause === "other") return null;
  const b = before(sql, from);
  const space = b.touching && b.ch !== "(" ? " " : "";
  const where = clause === "where";
  const fresh = !b.ch || b.ch === "(" || (where
    ? ["WHERE", "AND", "OR", "ON", "HAVING", "NOT", "BY", "WITH"].includes(b.word)
    : b.ch === "," || ["SELECT", "BY", "DISTINCT", "UNIQUE", "ALL"].includes(b.word));
  // 앞 항목 뒤에 이어 쓸 때는 앞 항목 바로 뒤에 붙이고, 사이의 공백·줄바꿈은 넣은 글 뒤로 보낸다 ("a.X , a.ID" 가 되지 않게)
  let at = from, ws = "";
  if (!fresh && !b.touching) {
    at = sql.slice(0, from).replace(/\s+$/, "").length;
    ws = sql.slice(at, from);
  }
  const lead = fresh ? (b.ch === "," && b.touching ? " " : space) : where ? " AND " : ", ";
  const body = where ? cols.map((c) => `${c} = `).join("\n   AND ") : cols.join(", ");
  const trail = continues(sql, to) ? (where ? "AND" : ",") + (ws || gap(sql, to) ? "" : " ") : "";
  const insert = lead + body + trail + ws;
  // 조건이면 첫 "= " 뒤에서 값을 쓰게
  const first = where ? `${cols[0]} = `.length : body.length;
  return { from: at, to, insert, cursor: at + lead.length + first };
}

/** 커서 뒤가 공백이면 덧붙이는 쉼표·AND 뒤에 빈칸을 또 넣지 않는다 */
const gap = (sql: string, pos: number) => /\s/.test(sql[pos] ?? "");

/** 휠·메뉴: 문장의 최상위 절에 넣는다 (없으면 만든다). 못 넣으면 이유 */
export function placeInClause(sql: string, target: Target, cols: string[]): Placed | { error: string } {
  if (!cols.length) return { error: "넣을 컬럼을 고르세요" };
  const { words, end } = scan(sql);
  const first = words[0]?.w;
  if (first !== "SELECT" && first !== "WITH") return { error: "조회(SELECT) 문에만 넣을 수 있습니다 — 커서를 SELECT 문에 두세요" };
  const sel = words.findIndex((x) => x.w === "SELECT");
  const from = words.findIndex((x, k) => k > sel && x.w === "FROM");
  if (sel < 0 || from < 0) return { error: "FROM 을 찾지 못했습니다" };
  const setOp = words.some((x) => x.w === "UNION" || x.w === "INTERSECT" || x.w === "MINUS");
  const body = cols.join(", ");

  if (target === "select") {
    if (setOp) return { error: "UNION·INTERSECT·MINUS 가 있어 어느 SELECT 에 넣을지 알 수 없습니다 — 커서 자리에 넣으세요 (Enter)" };
    const listFrom = words[sel].at + "SELECT".length;
    const listTo = words[from].at;
    const list = sql.slice(listFrom, listTo);
    // SELECT * 이면 * 를 고른 컬럼으로 바꾼다 (Oracle 은 "*, 열" 을 못 쓴다)
    const star = /^(\s*(?:DISTINCT\s+|UNIQUE\s+|ALL\s+)?)\*(\s*)$/i.exec(list);
    if (star) {
      const at = listFrom + star[1].length;
      return { from: at, to: at + 1, insert: body, cursor: at + body.length };
    }
    const at = listFrom + list.replace(/\s+$/, "").length;
    // 목록이 비었으면 (SELECT 만 쓰고 연 것) 쉼표 없이
    const lead = list.trim() && !/^(DISTINCT|UNIQUE|ALL)$/i.test(list.trim()) ? ", " : " ";
    return { from: at, to: at, insert: lead + body, cursor: at + lead.length + body.length };
  }

  if (target === "where") {
    // 첫 조건의 "= " 뒤에 커서를 둔다 — 표시 글자를 넣어 자리를 찾고 뺀다
    const MARK = "\u0001";
    const cond = cols.map((c, k) => `${c} = ${k === 0 ? MARK : ""}`).join("\n  AND ");
    // WHERE 만 써 두고 비어 있으면 그 뒤에 (addCondition 은 "WHERE AND …" 를 만든다)
    const w = words.findIndex((x, k) => k > from && x.w === "WHERE");
    if (w >= 0) {
      const wEnd = words[w].at + "WHERE".length;
      const nx = words.findIndex((x, k) => k > w && WHERE_ENDS.has(x.w));
      if (!sql.slice(wEnd, nx >= 0 ? words[nx].at : end).trim()) {
        return { from: wEnd, to: wEnd, insert: ` ${cond.replace(MARK, "")}`, cursor: wEnd + 1 + cond.indexOf(MARK) };
      }
    }
    const r = addCondition(sql, cond);
    if ("error" in r) return r;
    const at = r.sql.indexOf(MARK);
    const out = r.sql.replace(MARK, "");
    return { from: 0, to: sql.length, insert: out, cursor: at };
  }

  // ORDER BY · GROUP BY
  if (target === "group" && setOp) return { error: "UNION·INTERSECT·MINUS 가 있어 어느 SELECT 를 묶을지 알 수 없습니다 — 커서 자리에 넣으세요 (Enter)" };
  const kw = target === "order" ? "ORDER" : "GROUP";
  const head = words.findIndex((x, k) => k > from && x.w === kw && (words[k + 1]?.w === "BY" || words[k + 1]?.w === "SIBLINGS"));
  // 이 절 다음에 올 수 있는 절
  const after = target === "order" ? new Set(["FETCH", "OFFSET"]) : new Set(["HAVING", "ORDER", "FETCH", "OFFSET", "MODEL", "UNION", "INTERSECT", "MINUS"]);
  const isNext = (k: number) => after.has(words[k].w) || (words[k].w === "FOR" && words[k + 1]?.w === "UPDATE");
  const start = head >= 0 ? head : from;
  const next = words.findIndex((_, k) => k > start && isNext(k));
  const stop = next >= 0 ? words[next].at : end;
  const at = sql.slice(0, stop).replace(/\s+$/, "").length;
  if (head >= 0) {
    // "ORDER BY" 만 써 둔 것이면 쉼표 없이
    const by = words[head + 1]?.w === "BY" ? words[head + 1] : words[head + 2];
    const lead = by && !sql.slice(by.at + 2, stop).trim() ? " " : ", ";
    return { from: at, to: at, insert: lead + body, cursor: at + lead.length + body.length };
  }
  const clause = `\n${kw} BY ${body}`;
  return { from: at, to: at, insert: clause, cursor: at + clause.length };
}
