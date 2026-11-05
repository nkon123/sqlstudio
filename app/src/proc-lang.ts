// Pro*C 구문 강조 — C 와 그 안의 EXEC SQL … ; (EXECUTE … END-EXEC;) 를 나눠 칠한다.
// SQL 낱말은 Oracle 방언(oracle-dialect)의 키워드·함수를 쓴다. 호스트 변수(:v_name)는 따로.
// 구조(함수·쿼리·커서)는 procnav.ts 가 읽는다 — 여기는 색만.

import { LanguageSupport, StreamLanguage, type StringStream } from "@codemirror/language";
import { tags as t } from "@lezer/highlight";
import { ORACLE } from "./oracle-dialect";

const words = (s: string) => new Set(s.toUpperCase().split(/\s+/).filter(Boolean));
const SQL_KW = words(`${ORACLE.spec.keywords ?? ""} exec sql oracle tools whenever sqlerror sqlwarning continue do stop goto include
  declare section connect identified using release work commit rollback cursor open fetch close execute immediate end-exec prepare describe
  bind variables descriptor allocate free context enable threads var type reference`);
const SQL_FN = words(ORACLE.spec.builtin ?? "");
const SQL_TYPES = words(`${ORACLE.spec.types ?? ""} varchar2 number date timestamp clob blob raw rowid`);
const C_KW = words(`auto break case const continue default do else enum extern for goto if inline register restrict return sizeof static
  struct switch typedef union volatile while`);
const C_TYPES = words("void char short int long float double signed unsigned bool size_t FILE varchar VARCHAR sqlca SQLCA");

interface State { sql: boolean; plsql: boolean; comment: boolean }

const proc = StreamLanguage.define<State>({
  name: "proc",
  startState: () => ({ sql: false, plsql: false, comment: false }),
  copyState: (s) => ({ ...s }),
  token(stream: StringStream, st: State): string | null {
    if (st.comment) {
      if (stream.skipTo("*/")) { stream.next(); stream.next(); st.comment = false; } else stream.skipToEnd();
      return "comment";
    }
    if (stream.sol() && !st.sql && stream.match(/^\s*#/)) { stream.skipToEnd(); return "meta"; }
    if (stream.eatSpace()) return null;
    if (stream.match("/*")) {
      if (stream.skipTo("*/")) { stream.next(); stream.next(); } else { stream.skipToEnd(); st.comment = true; }
      return "comment";
    }
    if (stream.match("//")) { stream.skipToEnd(); return "comment"; }
    if (st.sql && stream.match("--")) { stream.skipToEnd(); return "comment"; }
    const ch = stream.peek()!;
    if (ch === '"') {
      stream.next();
      for (let c = stream.next(); c != null && c !== '"'; c = stream.next()) if (c === "\\") stream.next();
      return st.sql ? "quotedIdentifier" : "string";
    }
    if (ch === "'") {
      stream.next();
      if (st.sql) {
        // SQL 문자열 — '' 는 따옴표 하나
        for (;;) { const c = stream.next(); if (c == null) break; if (c === "'") { if (stream.peek() === "'") { stream.next(); continue; } break; } }
      } else for (let c = stream.next(); c != null && c !== "'"; c = stream.next()) if (c === "\\") stream.next();
      return "string";
    }
    if (st.sql && ch === ":" && stream.match(/^:[A-Za-z_][\w$#]*/)) {
      stream.match(/^(?:\.[A-Za-z_]\w*|->[A-Za-z_]\w*|\[[^\]]*\])*/);
      return "hostVar";
    }
    if (stream.match(/^(?:0x[\da-f]+|\d+(?:\.\d*)?(?:e[+-]?\d+)?)[ulf]*/i)) return "number";
    const w = stream.match(/^[A-Za-z_][\w$#]*/) as RegExpMatchArray | null;
    if (w) {
      const u = w[0].toUpperCase();
      if (u === "EXEC" && stream.match(/^\s+(?:SQL|ORACLE|TOOLS)\b/i, false)) { st.sql = true; return "keyword"; }
      if (st.sql) {
        // EXECUTE BEGIN … END-EXEC; — 안의 ; 로는 끝나지 않는다
        if (u === "EXECUTE" && stream.match(/^\s+(?:BEGIN|DECLARE)\b/i, false)) st.plsql = true;
        if (u === "END" && stream.match(/^-EXEC\b/i)) { st.plsql = false; return "keyword"; }
        if (SQL_KW.has(u)) return "keyword";
        if (SQL_TYPES.has(u)) return "typeName";
        if (SQL_FN.has(u)) return "builtin";
        return "variableName";
      }
      if (C_KW.has(u) && w[0] === w[0].toLowerCase()) return "keyword";
      if (C_TYPES.has(w[0]) || C_TYPES.has(u) && w[0] === w[0].toLowerCase()) return "typeName";
      if (stream.match(/^\s*\(/, false)) return "function";
      return "variableName";
    }
    stream.next();
    if (ch === ";" && st.sql && !st.plsql) { st.sql = false; return "punctuation"; }
    if ("{}()[];,".includes(ch)) return "punctuation";
    return /[-+*/%=<>!&|^~?:.]/.test(ch) ? "operator" : null;
  },
  languageData: { commentTokens: { line: "//", block: { open: "/*", close: "*/" } } },
  tokenTable: {
    hostVar: t.special(t.variableName),
    function: t.function(t.variableName),
    builtin: t.standard(t.name),
    quotedIdentifier: t.special(t.string),
  },
});

/** 편집기에 붙일 Pro*C 언어 */
export function procLanguage(): LanguageSupport {
  return new LanguageSupport(proc);
}
