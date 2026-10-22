// SQL 포맷 — 오른쪽 맞춤(강줄기) 스타일. 국내에서 흔한 모양:
//
//   SELECT A.JOB
//        , A.SAL
//     FROM emp A
//     JOIN dept D
//       ON D.DEPTNO = A.DEPTNO
//    WHERE A.SAL > 1000
//      AND A.JOB = 'CLERK'
//
// 절 키워드(SELECT·FROM·WHERE …)를 6칸(SELECT 길이)에 오른쪽으로 맞추고, 목록은 쉼표를 앞(또는 뒤)에,
// 조건은 AND·OR 를 키워드처럼 맞춘다. 괄호 안의 서브쿼리는 여는 괄호 바로 뒤를 기준으로 같은 모양을 다시 짠다.
// SELECT·WITH·INSERT(ALL·FIRST 포함)·UPDATE·DELETE·MERGE 와 CREATE TABLE·VIEW … AS SELECT 만 다룬다 —
// 그 밖(DDL 등)은 null 을 돌려 부르는 쪽이 다른 포맷터를 쓴다.
// 결과가 같은 SQL 인지는 부르는 쪽(format.ts)이 토큰으로 다시 확인한다.

export interface RiverOptions {
  /** 목록의 쉼표를 줄 앞에 */
  commaBefore: boolean;
  /** AND / OR 를 줄 앞에 */
  logicalBefore: boolean;
  /** 목록을 한 줄에 채운다 (넘치면 다음 줄) — 간결 */
  fill: boolean;
  /** 채울 때 한 줄 너비 */
  width: number;
  keywordCase: "upper" | "lower" | "preserve";
  identCase: "upper" | "lower" | "preserve";
}

type TT = "word" | "qid" | "str" | "num" | "bind" | "op" | "(" | ")" | "," | "." | ";" | "lc" | "bc";
interface Tok { t: TT; v: string; /** 앞에 줄바꿈이 있었나 */ nl: boolean; /** 앞에 공백이 있었나 */ sp: boolean }

/** 화면 너비 — 한글·한자 등은 두 칸 */
export function vw(s: string): number {
  let w = 0;
  for (const ch of s) w += /[ᄀ-ᅟ⺀-꓏가-힣豈-﫿︰-﹏＀-｠￠-￦]/.test(ch) ? 2 : 1;
  return w;
}

export function tokenize(sql: string): Tok[] {
  const out: Tok[] = [];
  let i = 0, nl = false, sp = false;
  const n = sql.length;
  const push = (t: TT, v: string) => { out.push({ t, v, nl, sp }); nl = false; sp = false; };
  while (i < n) {
    const ch = sql[i];
    if (/\s/.test(ch)) { if (ch === "\n") nl = true; sp = true; i++; continue; }
    if (ch === "-" && sql[i + 1] === "-") {
      const e = sql.indexOf("\n", i);
      const end = e < 0 ? n : e;
      push("lc", sql.slice(i, end).replace(/\s+$/, ""));
      i = end;
      continue;
    }
    if (ch === "/" && sql[i + 1] === "*") {
      const e = sql.indexOf("*/", i + 2);
      if (e < 0) throw new Error("닫히지 않은 주석");
      push("bc", sql.slice(i, e + 2));
      i = e + 2;
      continue;
    }
    // q'[...]', n'...', nq'[...]'
    const pre = /^(n?q|n)'/i.exec(sql.slice(i, i + 3));
    if (pre && /q/i.test(pre[1])) {
      const q = i + pre[1].length;
      const open = sql[q + 1];
      const close = ({ "[": "]", "(": ")", "{": "}", "<": ">" } as Record<string, string>)[open] ?? open;
      const e = sql.indexOf(close + "'", q + 2);
      if (e < 0) throw new Error("닫히지 않은 문자열");
      push("str", sql.slice(i, e + 2));
      i = e + 2;
      continue;
    }
    if (ch === "'" || (pre && pre[1].toLowerCase() === "n")) {
      let j = ch === "'" ? i + 1 : i + 2;
      for (;;) {
        if (j >= n) throw new Error("닫히지 않은 문자열");
        if (sql[j] === "'") { if (sql[j + 1] === "'") { j += 2; continue; } break; }
        j++;
      }
      push("str", sql.slice(i, j + 1));
      i = j + 1;
      continue;
    }
    if (ch === '"') {
      const e = sql.indexOf('"', i + 1);
      if (e < 0) throw new Error("닫히지 않은 따옴표 이름");
      push("qid", sql.slice(i, e + 1));
      i = e + 1;
      continue;
    }
    const num = /^(\d+(\.\d*)?|\.\d+)([eE][+-]?\d+)?[fFdD]?/.exec(sql.slice(i, i + 40));
    if (num) { push("num", num[0]); i += num[0].length; continue; }
    if (ch === ":" && /[\w가-힣$#]/.test(sql[i + 1] ?? "")) {
      const m = /^:[\w가-힣$#]+/.exec(sql.slice(i))!;
      push("bind", m[0]);
      i += m[0].length;
      continue;
    }
    const w = /^[A-Za-z_가-힣][\w$#가-힣]*/.exec(sql.slice(i, i + 200));
    if (w) { push("word", w[0]); i += w[0].length; continue; }
    if ("(),.;".includes(ch)) { push(ch as TT, ch); i++; continue; }
    const op = /^(>=|<=|<>|!=|\^=|~=|\|\||:=|=>|\*\*|\.\.)/.exec(sql.slice(i, i + 2));
    if (op) { push("op", op[0]); i += op[0].length; continue; }
    push("op", ch);
    i++;
  }
  return out;
}

export const KEYWORDS = new Set(("SELECT FROM WHERE AND OR NOT IN IS NULL LIKE LIKEC BETWEEN EXISTS CASE WHEN THEN ELSE END AS ON JOIN INNER LEFT RIGHT FULL OUTER CROSS " +
  "NATURAL APPLY GROUP BY ORDER HAVING UNION ALL MINUS INTERSECT EXCEPT DISTINCT UNIQUE INSERT INTO VALUES UPDATE SET DELETE MERGE USING MATCHED WITH " +
  "CONNECT START PRIOR LEVEL NOCYCLE SIBLINGS ASC DESC NULLS FIRST LAST FOR NOWAIT WAIT SKIP LOCKED OF ROWNUM ROWID SYSDATE SYSTIMESTAMP " +
  "CURRENT_DATE CURRENT_TIMESTAMP OVER PARTITION ROWS ROW RANGE UNBOUNDED PRECEDING FOLLOWING CURRENT FETCH NEXT ONLY OFFSET TIES PIVOT UNPIVOT " +
  "WITHIN KEEP ANY SOME ESCAPE DEFAULT RETURNING RETURN INTERVAL DATE TIMESTAMP DAY MONTH YEAR HOUR MINUTE SECOND TO LATERAL TRUE FALSE " +
  "MATERIALIZE IGNORE RESPECT DENSE_RANK GROUPING SETS ERRORS REJECT LIMIT UNLIMITED").split(" "));

export const FUNCS = new Set(("COUNT SUM AVG MIN MAX NVL NVL2 COALESCE DECODE TO_CHAR TO_DATE TO_NUMBER TO_TIMESTAMP TO_CLOB TO_NCHAR TO_SINGLE_BYTE TO_MULTI_BYTE " +
  "TRUNC ROUND SUBSTR SUBSTRB INSTR INSTRB LENGTH LENGTHB UPPER LOWER INITCAP TRIM LTRIM RTRIM LPAD RPAD REPLACE TRANSLATE CONCAT CHR ASCII " +
  "ABS CEIL FLOOR MOD POWER SQRT SIGN EXP LN LOG GREATEST LEAST ADD_MONTHS MONTHS_BETWEEN LAST_DAY NEXT_DAY EXTRACT CAST ROW_NUMBER RANK " +
  "DENSE_RANK LAG LEAD FIRST_VALUE LAST_VALUE NTH_VALUE NTILE CUME_DIST PERCENT_RANK RATIO_TO_REPORT LISTAGG XMLAGG XMLELEMENT REGEXP_LIKE " +
  "REGEXP_SUBSTR REGEXP_REPLACE REGEXP_INSTR REGEXP_COUNT NULLIF SYS_CONNECT_BY_PATH CONNECT_BY_ROOT MEDIAN STDDEV VARIANCE JSON_VALUE " +
  "JSON_QUERY JSON_TABLE JSON_OBJECT JSON_ARRAY JSON_ARRAYAGG JSON_OBJECTAGG NUMTODSINTERVAL NUMTOYMINTERVAL SYS_GUID USERENV SYS_CONTEXT " +
  "FROM_TZ SYS_EXTRACT_UTC NLSSORT EMPTY_CLOB EMPTY_BLOB DUMP VSIZE BITAND WIDTH_BUCKET ROLLUP CUBE GROUPING MULTISET XMLTABLE " +
  "XMLQUERY XMLEXISTS").split(" "));

/** 이 낱말 뒤의 여는 괄호는 띄운다 (함수 호출이 아니다) */
const SPACE_PAREN = new Set(("IN EXISTS AS VALUES OVER AND OR NOT ON USING THEN ELSE WHEN FROM JOIN SELECT WHERE BY ALL ANY SOME INTO SET IS " +
  "HAVING CASE PIVOT UNPIVOT FOR GROUP KEEP WITHIN RETURN RETURNING LATERAL APPLY TABLE").split(" "));

/** CREATE … TABLE·VIEW … AS 의 머리에서 키워드로 쓰는 낱말 (이름은 아니다) */
const CREATE_WORDS = new Set(("CREATE OR REPLACE FORCE NOFORCE EDITIONABLE NONEDITIONABLE MATERIALIZED VIEW TABLE GLOBAL TEMPORARY PRIVATE " +
  "BUILD IMMEDIATE DEFERRED REFRESH FAST COMPLETE ON DEMAND COMMIT NOLOGGING LOGGING PARALLEL NOPARALLEL COMPRESS NOCOMPRESS " +
  "TABLESPACE ENABLE DISABLE QUERY REWRITE PRESERVE DELETE ROWS AS").split(" "));

/** 값으로 쓰이는 키워드 — 뒤의 -/+ 는 빼기·더하기 */
const VALUE_WORDS = new Set("NULL LEVEL ROWNUM ROWID SYSDATE SYSTIMESTAMP CURRENT_DATE CURRENT_TIMESTAMP TRUE FALSE".split(" "));

type Kind = "list" | "cond" | "join" | "flat" | "setop" | "with" | "insert" | "update" | "delete" | "pivot" | "merge" | "multi" | "tail";
interface Clause { kw: string; n: number; kind: Kind }
interface Snap { l: number; cur: string; pos: number; prev: Tok | null; mb: boolean; un: boolean }

class Fmt {
  private lines: string[] = [];
  private cur = "";
  private pos = 0;
  private prev: Tok | null = null;
  private mustBreak = false;
  private unary = false;
  private cont = 0;
  private allowWith = true;

  constructor(private toks: Tok[], private o: RiverOptions) {}

  // ── 쓰기 ─────────────────────────────────────────
  private col() { return vw(this.cur); }
  private blank() { return this.cur.trim() === ""; }
  private nl(col: number) {
    if (!this.blank()) this.lines.push(this.cur.replace(/\s+$/, ""));
    this.cur = " ".repeat(Math.max(0, col));
    this.prev = null;
    this.mustBreak = false;
  }
  private emit(text: string, space: boolean) {
    if (this.mustBreak) this.nl(this.cont);
    if (space && !this.blank() && !this.cur.endsWith(" ") && !this.cur.endsWith("(")) this.cur += " ";
    this.cur += text;
  }
  /** 줄을 바꾸기 전에 같은 줄에 붙어 있던 주석은 이 줄 끝에 둔다 */
  private brk(col: number) {
    while (this.isComment(this.at()) && !this.at()!.nl) this.comment();
    this.nl(col);
  }
  private snap(): Snap { return { l: this.lines.length, cur: this.cur, pos: this.pos, prev: this.prev, mb: this.mustBreak, un: this.unary }; }
  private restore(s: Snap) {
    this.lines.length = s.l; this.cur = s.cur; this.pos = s.pos; this.prev = s.prev; this.mustBreak = s.mb; this.unary = s.un;
  }

  // ── 토큰 ─────────────────────────────────────────
  private at(k = 0): Tok | undefined { return this.toks[this.pos + k]; }
  private up(k = 0) { const t = this.at(k); return t?.t === "word" ? t.v.toUpperCase() : ""; }
  private isComment(t?: Tok) { return t?.t === "lc" || t?.t === "bc"; }
  private kw(s: string) { return this.o.keywordCase === "lower" ? s.toLowerCase() : this.o.keywordCase === "upper" ? s.toUpperCase() : s; }
  /** 레이아웃이 쓰는 키워드 — 원래 글자를 이어서 대소문자만 맞춘다 */
  private kwFrom(n: number) {
    return this.toks.slice(this.pos, this.pos + n).map((t) => (t.t === "word" ? this.kw(t.v) : t.v)).join(" ");
  }

  private text(i: number): string {
    const t = this.toks[i];
    if (t.t !== "word") return t.v;
    const u = t.v.toUpperCase();
    const dotted = this.toks[i - 1]?.t === "." || this.toks[i + 1]?.t === ".";
    if (!dotted && (KEYWORDS.has(u) || (FUNCS.has(u) && this.toks[i + 1]?.t === "("))) return this.kw(t.v);
    return this.o.identCase === "upper" ? t.v.toUpperCase() : this.o.identCase === "lower" ? t.v.toLowerCase() : t.v;
  }

  private space(prev: Tok | null, t: Tok): boolean {
    if (!prev) return true;
    if (this.unary) return false;
    if (prev.t === "(" || prev.t === ".") return false;
    if (t.t === ")" || t.t === "," || t.t === "." || t.t === ";") return false;
    if (prev.v === "@" || t.v === "@" || prev.v === "&" || prev.v === "%" || t.v === "%") return false;
    if (t.t === "(") {
      if (prev.t === "word") {
        const u = prev.v.toUpperCase();
        if (FUNCS.has(u)) return false;
        if (SPACE_PAREN.has(u)) return true;
        return t.sp;
      }
      if (prev.t === "qid") return t.sp;
    }
    return true;
  }

  private tok() {
    const t = this.at()!;
    const i = this.pos;
    this.emit(this.text(i), this.space(this.prev, t));
    const p = this.prev;
    const unaryCtx = !p || p.t === "op" || p.t === "(" || p.t === "," || (p.t === "word" && KEYWORDS.has(p.v.toUpperCase()) && !VALUE_WORDS.has(p.v.toUpperCase()));
    this.unary = (t.v === "-" || t.v === "+") && t.t === "op" && unaryCtx;
    this.prev = t;
    this.pos++;
  }

  private comment() {
    const t = this.at()!;
    if (t.nl && !this.blank()) this.nl(this.cont);
    this.emit(t.v, true);
    this.prev = t;
    this.unary = false;
    if (t.t === "lc") this.mustBreak = true;
    this.pos++;
  }

  // ── 절 ───────────────────────────────────────────
  private clauseAt(): Clause | null {
    // 낱말 사이의 /* 힌트 */ 는 건너 본다 (MERGE /*+ APPEND */ INTO …) — 키워드 글에는 그대로 남는다
    const idx: number[] = [];
    for (let k = 0; idx.length < 6 && this.at(k); k++) if (this.at(k)!.t !== "bc") idx.push(k);
    const w = (k: number) => (idx[k] == null ? "" : this.up(idx[k]));
    const c = (kw: number, kind: Kind): Clause => {
      const n = idx[kw - 1] + 1;
      return { kw: this.kwFrom(n), n, kind };
    };
    switch (w(0)) {
      case "SELECT": return c(1, "list");
      case "FROM": return c(1, "list");
      case "WHERE": case "HAVING": case "ON": return c(1, "cond");
      case "GROUP": return w(1) === "BY" ? c(2, "list") : null;
      case "ORDER": return w(1) === "BY" ? c(2, "list") : w(1) === "SIBLINGS" && w(2) === "BY" ? c(3, "list") : null;
      case "CONNECT": return w(1) === "BY" ? c(2, "cond") : null;
      case "START": return w(1) === "WITH" ? c(2, "cond") : null;
      case "UNION": return c(w(1) === "ALL" ? 2 : 1, "setop");
      case "MINUS": case "INTERSECT": case "EXCEPT": return c(w(1) === "ALL" ? 2 : 1, "setop");
      case "JOIN": return c(1, "join");
      case "INNER": case "LEFT": case "RIGHT": case "FULL": case "CROSS": case "NATURAL": case "OUTER": {
        if (this.at(1)?.t === "(") return null;
        let k = 1;
        while (["OUTER", "INNER", "LEFT", "RIGHT", "FULL"].includes(w(k))) k++;
        return w(k) === "JOIN" || w(k) === "APPLY" ? c(k + 1, "join") : null;
      }
      case "USING": case "VALUES": case "RETURNING": case "RETURN": return c(1, "flat");
      case "INTO": return c(1, "list");
      case "SET": return c(1, "list");
      case "FOR": return w(1) === "UPDATE" ? c(2, "flat") : null;
      case "FETCH": return w(1) === "FIRST" || w(1) === "NEXT" ? c(1, "flat") : null;
      case "OFFSET": return ["num", "bind"].includes(this.at(1)?.t ?? "") ? c(1, "flat") : null;
      case "WITH": return this.allowWith ? c(1, "with") : null;
      case "INSERT":
        if (w(1) === "INTO") return c(2, "insert");
        if (w(1) === "ALL" || w(1) === "FIRST") return c(2, "multi");
        throw new Error("INSERT");
      case "LOG": return w(1) === "ERRORS" ? c(2, "tail") : null;
      case "UPDATE": return this.allowWith ? c(1, "update") : null;
      case "DELETE": return this.allowWith ? c(w(1) === "FROM" ? 2 : 1, "delete") : null;
      case "MERGE": return w(1) === "INTO" ? c(2, "merge") : null;
      // PIVOT ( … ) · PIVOT XML ( … ) · UNPIVOT INCLUDE NULLS ( … ) — 이름으로 쓴 PIVOT 은 아니다
      case "PIVOT": case "UNPIVOT":
        return this.at(1)?.t === "(" || ["XML", "INCLUDE", "EXCLUDE"].includes(w(1)) ? c(1, "pivot") : null;
    }
    return null;
  }

  /** 절 키워드를 놓을 칸: 6칸 안은 오른쪽 맞춤, 긴 것(GROUP BY …)은 한 칸 들여, 조인은 FROM 과 같은 칸 */
  private kwStart(c: Clause, base: number) {
    const R = base + 6;
    if (["with", "insert", "update", "delete", "setop", "merge", "multi"].includes(c.kind)) return base;
    if (c.kind === "join") return R - 4;
    const len = vw(c.kw);
    return len <= 6 ? R - len : base + 1;
  }

  /** 문장(또는 괄호 안 서브쿼리) 하나 — base 는 SELECT 가 시작하는 칸 */
  query(base: number): void {
    const saveCont = this.cont, saveWith = this.allowWith;
    this.allowWith = true;
    this.cont = base;
    let first = true;
    for (;;) {
      const t = this.at();
      if (!t || t.t === ")" || t.t === ";") break;
      if (this.isComment(t)) {
        if (!first || !this.blank()) { if (t.nl && !this.blank()) this.nl(base); }
        this.comment();
        if (this.at() && !this.isComment(this.at())) this.nl(base);
        continue;
      }
      const c = this.clauseAt();
      if (!c) {
        if (!first && !this.blank()) this.brk(base);
        this.cont = base;
        this.expr({ clauses: true });
        first = false;
        continue;
      }
      const start = this.kwStart(c, base);
      if (this.blank()) this.cur = " ".repeat(start);
      else if (!(first && this.col() === base && start === base && !this.mustBreak)) this.brk(start);
      this.emit(c.kw, true);
      this.pos += c.n;
      this.prev = null;
      this.allowWith = c.kind === "insert" || c.kind === "multi";
      const R = base + 6;
      switch (c.kind) {
        case "list": case "with": this.list(this.col() + 1); break;
        case "cond": this.cond(R); break;
        case "setop": break;
        case "pivot": this.pivot(); break;
        case "merge": this.merge(base); break;
        case "multi": this.insertMulti(base); break;
        // LOG ERRORS INTO … REJECT LIMIT … — 끝까지 한 줄
        case "tail": this.cont = this.col() + 1; this.expr({}); break;
        default: this.cont = this.col() + 1; this.expr({ clauses: true }); break;
      }
      first = false;
    }
    this.cont = saveCont;
    this.allowWith = saveWith;
  }

  private list(itemCol: number, stops?: Set<string>) {
    const commaCol = itemCol - 2;
    this.cont = itemCol;
    this.expr({ clauses: true, list: true, stops });
    while (this.at()?.t === ",") {
      this.pos++;
      if (this.o.fill) {
        const s = this.snap();
        const before = this.lines.length;
        this.emit(",", false);
        this.prev = { t: ",", v: ",", nl: false, sp: false };
        this.expr({ clauses: true, list: true, stops });
        if (this.lines.length === before && this.col() <= this.o.width && !this.mustBreak) continue;
        this.restore(s);
      }
      if (this.o.commaBefore) {
        this.brk(commaCol);
        this.emit(",", false);
      } else {
        this.emit(",", false);
        this.brk(itemCol);
      }
      this.prev = { t: ",", v: ",", nl: false, sp: false };
      this.cont = itemCol;
      this.expr({ clauses: true, list: true, stops });
    }
  }

  /** 절 키워드(낱말 n 개)를 그 칸에 — 줄을 바꿔 col 에서 */
  private kwAt(col: number, n = 1) {
    this.brk(col);
    this.emit(this.kwFrom(n), true);
    this.pos += n;
    this.prev = null;
  }

  /** MERGE INTO 대상 USING 원본 ON (조건) WHEN [NOT] MATCHED THEN … — 갈래 안의 UPDATE·INSERT 는 한 칸 들인 문장처럼:
   *
   *    MERGE INTO emp t
   *     USING (SELECT …) s
   *        ON (t.empno = s.empno)
   *      WHEN MATCHED THEN
   *           UPDATE
   *              SET t.sal = s.sal
   *            WHERE t.sal <> s.sal
   *           DELETE
   *            WHERE t.sal = 0
   *      WHEN NOT MATCHED THEN
   *           INSERT (empno, sal)
   *           VALUES (s.empno, s.sal)
   */
  private merge(base: number) {
    const R = base + 6;
    const WHEN = new Set(["WHEN"]);
    this.cont = this.col() + 1;
    this.expr({ clauses: true });
    if (this.up() !== "USING") throw new Error("MERGE 에 USING 이 없습니다");
    this.kwAt(R - 5);
    this.cont = this.col() + 1;
    this.expr({ stops: new Set(["ON"]) });
    if (this.up() !== "ON") throw new Error("MERGE 에 ON 이 없습니다");
    this.kwAt(R - 2);
    this.cond(R, WHEN);
    // 갈래 안 문장의 SELECT 칸
    const B = R + 1, BR = B + 6;
    while (this.up() === "WHEN") {
      const n = this.up(1) === "NOT" ? 4 : 3;
      if (this.up(n - 2) !== "MATCHED" || this.up(n - 1) !== "THEN") throw new Error("WHEN [NOT] MATCHED THEN");
      this.kwAt(R - 4, n);
      const what = this.up();
      if (what === "UPDATE") {
        this.kwAt(B);
        if (this.up() !== "SET") throw new Error("UPDATE 에 SET 이 없습니다");
        this.kwAt(BR - 3);
        this.list(this.col() + 1, new Set(["WHEN", "WHERE", "DELETE"]));
        if (this.up() === "WHERE") { this.kwAt(BR - 5); this.cond(BR, new Set(["WHEN", "DELETE"])); }
        if (this.up() === "DELETE") {
          this.kwAt(B);
          if (this.up() === "WHERE") { this.kwAt(BR - 5); this.cond(BR, WHEN); }
        }
      } else if (what === "INSERT") {
        this.kwAt(B);
        this.cont = this.col() + 1;
        this.expr({ stops: new Set(["VALUES", "WHEN", "WHERE"]) });
        if (this.up() === "VALUES") { this.kwAt(BR - 6); this.cont = this.col() + 1; this.expr({ stops: new Set(["WHEN", "WHERE"]) }); }
        if (this.up() === "WHERE") { this.kwAt(BR - 5); this.cond(BR, WHEN); }
      } else {
        throw new Error("MERGE 갈래는 UPDATE·INSERT");
      }
    }
  }

  /** WITH 의 재귀 쿼리 뒤 SEARCH DEPTH FIRST BY … SET … · CYCLE … SET … TO … DEFAULT … — 다음 줄에 한 줄로 */
  private searchClause() {
    this.brk(this.cont);
    this.emit(this.kw(this.at()!.v), true);
    this.prev = this.at()!;
    this.pos++;
    for (;;) {
      const t = this.at();
      const u = this.up();
      if (!t || t.t === "," || t.t === ")" || t.t === ";" || u === "SELECT" || u === "WITH" || u === "CYCLE") return;
      if (t.t === "(") { this.paren(); continue; }
      if (["SEARCH", "DEPTH", "BREADTH", "FIRST", "BY", "SET", "TO", "DEFAULT", "CYCLE"].includes(u)) {
        this.emit(this.kw(t.v), true);
        this.prev = t;
        this.pos++;
        continue;
      }
      this.tok();
    }
  }

  /** INSERT ALL · INSERT FIRST — INTO 마다 한 줄, 조건(WHEN … THEN · ELSE) 아래의 INTO 는 한 칸 들여:
   *
   *    INSERT FIRST
   *      WHEN x > 10 THEN
   *           INTO t1 (a) VALUES (x)
   *      ELSE
   *           INTO t3 (a) VALUES (x)
   *    SELECT x
   *      FROM src
   */
  private insertMulti(base: number) {
    const R = base + 6;
    const STOP = new Set(["INTO", "VALUES", "WHEN", "ELSE", "SELECT", "WITH"]);
    let intoCol = R - 4;
    for (;;) {
      const u = this.up();
      if (u === "WHEN") {
        this.kwAt(R - 4);
        this.cont = this.col() + 1;
        this.expr({ stops: new Set(["THEN"]) });
        if (this.up() !== "THEN") throw new Error("WHEN 에 THEN 이 없습니다");
        this.tok();
        intoCol = R + 1;
      } else if (u === "ELSE") {
        this.kwAt(R - 4);
        intoCol = R + 1;
      } else if (u === "INTO") {
        this.kwAt(intoCol);
        this.cont = this.col() + 1;
        this.expr({ stops: STOP });
        if (this.up() === "VALUES") {
          this.emit(this.kw(this.at()!.v), true);
          this.prev = this.at()!;
          this.pos++;
          this.expr({ stops: STOP });
        }
      } else {
        return;
      }
    }
  }

  /** 여는 괄호 하나 — 없으면 읽지 못한 것 */
  private open() {
    const t = this.at();
    if (t?.t !== "(") throw new Error("괄호가 와야 합니다");
    this.emit("(", true);
    this.prev = t;
    this.unary = false;
    this.pos++;
  }

  private close() {
    const t = this.at();
    if (t?.t !== ")") throw new Error("괄호 짝이 맞지 않습니다");
    this.emit(")", false);
    this.prev = t;
    this.pos++;
  }

  /** PIVOT (집계 목록 FOR 열 IN (값 목록)) [별칭] — UNPIVOT 은 집계 대신 값 열.
   *  집계·값은 SELECT 목록처럼 한 줄에 하나, FOR 는 첫 집계와 같은 칸:
   *
   *     PIVOT (SUM(sal) AS s
   *          , COUNT(*) AS n
   *            FOR deptno IN (10 AS d10
   *                         , 20 AS d20))
   */
  private pivot() {
    // PIVOT XML · UNPIVOT INCLUDE NULLS — 키워드 대소문자로
    while (this.at()?.t === "word") {
      this.emit(this.kw(this.at()!.v), true);
      this.prev = this.at()!;
      this.pos++;
    }
    this.open();
    const itemCol = this.col();
    this.list(itemCol, new Set(["FOR"]));
    if (this.up() !== "FOR") throw new Error("PIVOT 에 FOR 가 없습니다");
    this.brk(itemCol);
    this.tok();
    this.cont = this.col() + 1;
    this.expr({ stops: new Set(["IN"]) });
    if (this.up() !== "IN") throw new Error("PIVOT 에 IN 이 없습니다");
    this.tok();
    this.open();
    // PIVOT XML 의 IN (ANY) · IN (SELECT …)
    if (this.up() === "SELECT" || this.up() === "WITH") this.query(this.col());
    else this.list(this.col());
    this.close();
    this.close();
    // 별칭
    const a = this.at();
    if ((a?.t === "word" || a?.t === "qid") && !this.clauseAt() && !KEYWORDS.has(this.up())) this.tok();
  }

  private cond(R: number, stops?: Set<string>) {
    this.cont = R + 1;
    for (;;) {
      this.expr({ clauses: true, cond: true, stops });
      const op = this.up();
      if (op !== "AND" && op !== "OR") break;
      const text = this.kw(this.at()!.v);
      if (this.o.logicalBefore) {
        this.brk(R - op.length);
        this.emit(text, true);
      } else {
        this.emit(text, true);
        this.pos++;
        this.brk(R + 1);
        this.prev = null;
        this.cont = R + 1;
        continue;
      }
      this.pos++;
      this.prev = this.toks[this.pos - 1];
      this.cont = R + 1;
    }
  }

  /** 식 — 멈출 곳(쉼표·AND/OR·절 키워드·닫는 괄호·CASE 의 WHEN 등)까지 한 줄로, 서브쿼리·CASE 는 줄을 나눈다 */
  private expr(ctx: { clauses?: boolean; list?: boolean; cond?: boolean; stops?: Set<string> }) {
    let between = false;
    for (;;) {
      const t = this.at();
      if (!t || t.t === ")" || t.t === ";") return;
      if (this.isComment(t)) { this.comment(); continue; }
      if (t.t === ",") {
        if (ctx.list) return;
        this.emit(",", false);
        this.prev = t;
        this.pos++;
        continue;
      }
      const u = this.up();
      if (ctx.stops?.has(u)) return;
      if (ctx.cond && (u === "AND" || u === "OR")) {
        if (u === "AND" && between) between = false;
        else return;
      }
      if ((u === "SEARCH" || u === "CYCLE") && this.prev?.t === ")") { this.searchClause(); continue; }
      if (ctx.clauses && t.t === "word" && this.clauseAt()) return;
      if (u === "BETWEEN") between = true;
      if (t.t === "(") { this.paren(); continue; }
      if (u === "CASE") { this.caseExpr(); continue; }
      this.tok();
    }
  }

  private paren() {
    const open = this.at()!;
    this.emit("(", this.space(this.prev, open));
    this.prev = open;
    this.unary = false;
    this.pos++;
    const u = this.up();
    if (u === "SELECT" || u === "WITH") {
      this.query(this.col());
    } else {
      const saveCont = this.cont;
      this.cont = this.col();
      this.expr({});
      this.cont = saveCont;
    }
    const close = this.at();
    if (close?.t !== ")") throw new Error("괄호 짝이 맞지 않습니다");
    this.emit(")", false);
    this.prev = close;
    this.unary = false;
    this.pos++;
  }

  private caseExpr() {
    // 갈래(WHEN·ELSE)가 둘 이상이면 줄을 나눈다
    let depth = 0, parts = 0;
    for (let k = this.pos; k < this.toks.length; k++) {
      const u = this.toks[k].t === "word" ? this.toks[k].v.toUpperCase() : "";
      if (u === "CASE") depth++;
      else if (u === "END") { if (--depth === 0) break; }
      else if (depth === 1 && (u === "WHEN" || u === "ELSE")) parts++;
    }
    const multi = parts >= 2;
    this.tok(); // CASE
    const caseCol = this.col() - 4;
    const saveCont = this.cont;
    if (this.up() !== "WHEN") this.expr({ stops: new Set(["WHEN"]) });
    let whenCol = -1;
    for (;;) {
      const u = this.up();
      if (u === "WHEN" || u === "ELSE") {
        if (whenCol < 0) { this.tok(); whenCol = this.col() - u.length; }
        else if (multi) { this.brk(whenCol); this.tok(); }
        else this.tok();
        this.cont = whenCol + 5;
        this.expr({ stops: new Set(["THEN", "WHEN", "ELSE", "END"]) });
        if (this.up() === "THEN") { this.tok(); this.expr({ stops: new Set(["WHEN", "ELSE", "END"]) }); }
        continue;
      }
      if (u === "END") {
        if (multi) this.brk(caseCol);
        this.tok();
        break;
      }
      throw new Error("CASE 가 END 로 끝나지 않습니다");
    }
    this.cont = saveCont;
  }

  /** CREATE [OR REPLACE] … TABLE·VIEW … AS — AS 까지 한 줄로 쓰고 참. 아니면 아무것도 쓰지 않고 거짓 */
  private createHead(): boolean {
    let k = this.pos;
    while (this.isComment(this.toks[k])) k++;
    if (this.toks[k]?.t !== "word" || this.toks[k].v.toUpperCase() !== "CREATE") return false;
    // 같은 깊이의 AS 뒤에 SELECT·WITH·( 가 오고, 그 앞에 TABLE 이나 VIEW 가 있어야
    let depth = 0, kind = false;
    for (let i = k; i < this.toks.length; i++) {
      const t = this.toks[i];
      if (t.t === "(") depth++;
      else if (t.t === ")") depth--;
      else if (t.t === ";") return false;
      if (depth || t.t !== "word") continue;
      const u = t.v.toUpperCase();
      if (u === "TABLE" || u === "VIEW") kind = true;
      if (u === "AS" && kind) {
        const nx = this.toks[i + 1];
        if (!(nx?.t === "(" || (nx?.t === "word" && ["SELECT", "WITH"].includes(nx.v.toUpperCase())))) return false;
        while (this.pos <= i) {
          const h = this.at()!;
          if (h.t === "(") this.paren();
          else if (h.t === "word" && CREATE_WORDS.has(h.v.toUpperCase()) && this.prev?.t !== ".") {
            this.emit(this.kw(h.v), this.space(this.prev, h));
            this.prev = h;
            this.pos++;
          } else this.tok();
        }
        this.nl(0);
        return true;
      }
    }
    return false;
  }

  run(): string | null {
    // CREATE TABLE·VIEW … AS SELECT 는 머리를 쓰고 쿼리로
    if (this.createHead()) {
      this.query(0);
      const t = this.at();
      if (t && t.t !== ";") return null;
      if (t) { this.emit(";", false); this.pos++; }
      if (this.pos < this.toks.length && this.toks.slice(this.pos).some((x) => !this.isComment(x))) return null;
      while (this.at()) this.comment();
      this.nl(0);
      return this.lines.join("\n");
    }
    // 다루는 문장인지
    const first = this.toks.find((t) => !this.isComment(t));
    if (!first) return null;
    const u = first.t === "word" ? first.v.toUpperCase() : first.t;
    if (!["SELECT", "WITH", "INSERT", "UPDATE", "DELETE", "MERGE", "("].includes(u)) return null;
    while (this.pos < this.toks.length) {
      this.query(0);
      const t = this.at();
      if (!t) break;
      if (t.t === ")") throw new Error("괄호 짝이 맞지 않습니다");
      this.emit(";", false);
      this.pos++;
      while (this.isComment(this.at()) && !this.at()!.nl) this.comment();
      if (this.pos < this.toks.length) {
        this.nl(0);
        this.lines.push("");
        const nu = this.toks.slice(this.pos).find((x) => !this.isComment(x));
        const nuu = nu?.t === "word" ? nu.v.toUpperCase() : nu?.t;
        if (nu && !["SELECT", "WITH", "INSERT", "UPDATE", "DELETE", "MERGE", "("].includes(nuu ?? "")) return null;
      }
    }
    this.nl(0);
    return this.lines.join("\n");
  }
}

/** 오른쪽 맞춤 스타일로 포맷. 다루지 않는 문장이거나 읽지 못하면 null */
export function formatRiver(sql: string, o: RiverOptions): string | null {
  try {
    return new Fmt(tokenize(sql), o).run();
  } catch {
    return null;
  }
}
