// PL/SQL 구조 읽기 — 패키지·프로시저·함수·트리거와 BEGIN…END · IF…END IF · LOOP…END LOOP · CASE…END 의 짝.
// 구조 보기(Ctrl+Shift+O), 현재 위치 표시, 블록 접기가 쓴다. 문법 검사가 아니라 낱말만 보고 짐작한다 —
// 짝이 안 맞아도 끝까지 읽고, 스크립트의 "/" 줄에서 열린 것을 모두 닫는다.

export type NavKind =
  | "package" | "package body" | "type body" | "procedure" | "function" | "trigger"
  | "type" | "cursor" | "declare" | "begin" | "if" | "loop" | "case"
  // 쿼리 한 문장 (PL/SQL 안의 DML · Pro*C 의 EXEC SQL)
  | "sql"
  // Pro*C (procnav.ts)
  | "c function" | "declare section" | "struct" | "c block";

export interface NavNode {
  kind: NavKind;
  name: string;
  /** 여는 낱말 자리 */
  from: number;
  /** 끝 (END …; 뒤) */
  to: number;
  /** 닫는 END 자리 — 선언만 있거나 닫히지 않았으면 -1 */
  end: number;
  /** 몸체 없는 선언 (패키지 명세의 PROCEDURE p(…); 등) */
  decl: boolean;
  children: NavNode[];
}

/** 구조 보기에 나오는 것 (BEGIN·IF 같은 블록은 빼고) */
export const UNIT_KINDS: ReadonlySet<NavKind> = new Set(["package", "package body", "type body", "procedure", "function", "trigger", "type", "cursor", "c function", "declare section", "struct"]);

/** 쿼리의 짧은 이름 — SELECT ORD · UPDATE ORD · OPEN C1 FOR … (words: 문장의 낱말, 쓴 그대로) */
export function sqlLabel(words: string[]): string {
  let k = 0;
  const W = (i: number) => (words[i] ?? "").toUpperCase();
  if (W(0) === "AT") k = 2;
  const head = W(k);
  const next = (i: number) => words[i] ?? "";
  const after = (word: string) => { for (let i = k + 1; i < words.length - 1; i++) if (W(i) === word) return words[i + 1]; return ""; };
  const join = (...xs: string[]) => xs.filter(Boolean).join(" ");
  switch (head) {
    case "SELECT": case "WITH": return join("SELECT", after("FROM"));
    case "INSERT": return join("INSERT", W(k + 1) === "ALL" || W(k + 1) === "FIRST" ? "ALL" : after("INTO"));
    case "UPDATE": return join("UPDATE", next(k + 1));
    case "DELETE": return join("DELETE", W(k + 1) === "FROM" ? next(k + 2) : next(k + 1));
    case "MERGE": return join("MERGE", after("INTO"));
    case "EXECUTE":
      if (W(k + 1) === "IMMEDIATE") return "EXECUTE IMMEDIATE";
      return W(k + 1) === "BEGIN" || W(k + 1) === "DECLARE" ? "EXECUTE (PL/SQL)" : join("EXECUTE", next(k + 1));
    case "OPEN": return W(k + 2) === "FOR" ? join("OPEN", next(k + 1), "FOR", W(k + 3) === "SELECT" ? after("FROM") : "") : join("OPEN", next(k + 1));
    case "FETCH": case "CLOSE": case "PREPARE": case "DESCRIBE": return join(head, next(k + 1));
    case "FORALL": { const i = words.findIndex((x, j) => j > k && ["INSERT", "UPDATE", "DELETE", "MERGE"].includes(x.toUpperCase())); return join("FORALL", i < 0 ? "" : sqlLabel(words.slice(i))); }
    default: return head || "SQL";
  }
}

/** 쿼리로 보는 문장의 첫 낱말 */
const SQL_HEAD = new Set(["SELECT", "WITH", "INSERT", "UPDATE", "DELETE", "MERGE", "FORALL"]);
/** 이 낱말 뒤는 새 문장 */
const STMT_AFTER = new Set(["BEGIN", "THEN", "ELSE", "LOOP"]);

interface Tok { t: "w" | "q" | ";" | "(" | ")" | "/" | "." | "%" | ","; v: string; from: number; to: number }

const isStart = (c: number) => (c >= 65 && c <= 90) || (c >= 97 && c <= 122) || c === 95 || c >= 0x80;
const isPart = (c: number) => isStart(c) || (c >= 48 && c <= 57) || c === 36 || c === 35;

/** 낱말·기호로 — 주석·문자열은 건너뛴다 */
function lex(s: string): Tok[] {
  const out: Tok[] = [];
  const n = s.length;
  let lineStart = true;
  for (let i = 0; i < n;) {
    const c = s.charCodeAt(i);
    if (c === 10) { lineStart = true; i++; continue; }
    if (c === 32 || c === 9 || c === 13) { i++; continue; }
    const atLineStart = lineStart;
    lineStart = false;
    // -- 줄 주석
    if (c === 45 && s.charCodeAt(i + 1) === 45) { const e = s.indexOf("\n", i); i = e < 0 ? n : e; continue; }
    // /* 블록 주석 */ — Oracle 은 겹치지 않는다
    if (c === 47 && s.charCodeAt(i + 1) === 42) { const e = s.indexOf("*/", i + 2); i = e < 0 ? n : e + 2; continue; }
    // 문자열 — q'[…]' · n'…' · '…''…'
    let q = i;
    if ((c === 110 || c === 78) && (s.charCodeAt(i + 1) === 113 || s.charCodeAt(i + 1) === 81) && s.charCodeAt(i + 2) === 39) q = i + 1;
    const qc = s.charCodeAt(q);
    if ((qc === 113 || qc === 81) && s.charCodeAt(q + 1) === 39 && q + 2 < n && !/\s/.test(s[q + 2])) {
      const open = s[q + 2];
      const close = ({ "[": "]", "{": "}", "<": ">", "(": ")" } as Record<string, string>)[open] ?? open;
      const e = s.indexOf(`${close}'`, q + 3);
      i = e < 0 ? n : e + 2;
      continue;
    }
    if ((c === 110 || c === 78) && s.charCodeAt(i + 1) === 39) q = i + 1;
    if (s.charCodeAt(q) === 39) {
      let j = q + 1;
      for (;;) {
        const e = s.indexOf("'", j);
        if (e < 0) { j = n; break; }
        if (s.charCodeAt(e + 1) === 39) { j = e + 2; continue; }
        j = e + 1;
        break;
      }
      i = j;
      continue;
    }
    if (c === 34) {
      const e = s.indexOf('"', i + 1);
      const to = e < 0 ? n : e + 1;
      out.push({ t: "q", v: s.slice(i + 1, e < 0 ? n : e), from: i, to });
      i = to;
      continue;
    }
    if (isStart(c)) {
      let j = i + 1;
      while (j < n && isPart(s.charCodeAt(j))) j++;
      out.push({ t: "w", v: s.slice(i, j).toUpperCase(), from: i, to: j });
      i = j;
      continue;
    }
    if (c >= 48 && c <= 57) { let j = i + 1; while (j < n && (isPart(s.charCodeAt(j)) || s.charCodeAt(j) === 46 && s.charCodeAt(j + 1) !== 46)) j++; i = j; continue; }
    if (c === 47 && atLineStart) {
      // 줄에 "/" 만 — 스크립트의 블록 끝
      let j = i + 1;
      while (j < n && (s.charCodeAt(j) === 32 || s.charCodeAt(j) === 9 || s.charCodeAt(j) === 13)) j++;
      if (j >= n || s.charCodeAt(j) === 10) { out.push({ t: "/", v: "/", from: i, to: i + 1 }); i = j; continue; }
    }
    const sym = s[i];
    if (sym === ";" || sym === "(" || sym === ")" || sym === "." || sym === "%" || sym === ",") out.push({ t: sym, v: sym, from: i, to: i + 1 });
    i++;
  }
  return out;
}

/** 이런 말로 시작하는 문장의 PROCEDURE·PACKAGE 는 정의가 아니다 */
const NOT_DEF = new Set(["DROP", "ALTER", "GRANT", "REVOKE", "AUDIT", "NOAUDIT", "COMMENT"]);
/** 몸체를 기다리는 것 — BEGIN 이 오면 새 블록이 아니라 이것의 몸체 */
const WAITS_BODY = new Set<NavKind>(["procedure", "function", "package body", "type body", "trigger", "declare"]);

/** 문서의 구조 — 맨 위 단위들 */
export function parseNav(src: string): NavNode[] {
  const toks = lex(src);
  const root: NavNode = { kind: "begin", name: "", from: 0, to: src.length, end: -1, decl: false, children: [] };
  const stack: (NavNode & { begun?: boolean })[] = [root];
  const top = () => stack[stack.length - 1];
  const add = (x: NavNode) => { top().children.push(x); return x; };
  const word = (k: number) => (toks[k]?.t === "w" ? toks[k].v : "");
  const nameAt = (k: number): [string, number] => {
    // schema.name · "Name"
    let name = "";
    let j = k;
    while (toks[j] && (toks[j].t === "w" || toks[j].t === "q")) {
      // 이름은 쓴 그대로
      name += toks[j].t === "q" ? toks[j].v : src.slice(toks[j].from, toks[j].to);
      if (toks[j + 1]?.t === "." && (toks[j + 2]?.t === "w" || toks[j + 2]?.t === "q")) { name += "."; j += 2; } else { j++; break; }
    }
    return [name, j];
  };
  /** k 부터 괄호 밖의 ; (또는 / 줄) 까지 — 그 ; 자리 */
  const toSemi = (k: number) => {
    for (let d = 0; k < toks.length; k++) {
      const x = toks[k];
      if (x.t === "(") d++;
      else if (x.t === ")") d = Math.max(0, d - 1);
      else if (x.t === "/" || (x.t === ";" && d === 0)) return k;
    }
    return toks.length - 1;
  };
  const close = (x: NavNode, end: number, to: number) => { x.end = end; x.to = to; };
  let first = "";        // 지금 문장의 첫 낱말
  let loopHead = -1;     // FOR·WHILE … LOOP 의 FOR 자리
  let loopWord = "";
  for (let i = 0; i < toks.length; i++) {
    const tk = toks[i];
    if (tk.t === ";") { first = ""; loopHead = -1; continue; }
    if (tk.t === "/") {
      while (stack.length > 1) close(stack.pop()!, -1, tk.from);
      first = ""; loopHead = -1;
      continue;
    }
    if (tk.t !== "w") continue;
    const w = tk.v;
    const prev = toks[i - 1];
    const prevW = prev?.t === "w" ? prev.v : "";
    if (!first) first = w;
    // 쿼리 한 문장 — 프로시저·블록 안에서 문장 처음의 DML · EXECUTE IMMEDIATE · OPEN … FOR · FOR r IN (SELECT …)
    const atStmt = !prev || prev.t === ";" || (prev.t === "w" && STMT_AFTER.has(prev.v));
    const inParen = prev?.t === "(" && toks[i - 2]?.t === "w" && toks[i - 2].v === "IN" && (w === "SELECT" || w === "WITH");
    if (stack.length > 1 && ((atStmt && (SQL_HEAD.has(w) || (w === "EXECUTE" && word(i + 1) === "IMMEDIATE") || (w === "OPEN" && word(i + 2) === "FOR"))) || inParen)) {
      // 끝: ; (괄호 밖) — FOR … IN ( 안이면 닫는 ) 앞
      let e = i;
      for (let d = 0; e < toks.length; e++) {
        const x = toks[e];
        if (x.t === "(") d++;
        else if (x.t === ")") { if (d === 0 && inParen) { e--; break; } d = Math.max(0, d - 1); }
        else if (x.t === "/" || (x.t === ";" && d === 0 && !inParen)) break;
      }
      const last = toks[Math.min(e, toks.length - 1)];
      const words = toks.slice(i, e + 1).filter((x) => x.t === "w" || x.t === "q").map((x) => (x.t === "q" ? x.v : src.slice(x.from, x.to)));
      add({ kind: "sql", name: sqlLabel(words), from: tk.from, to: last?.t === "/" ? last.from : (last?.to ?? src.length), end: -1, decl: false, children: [] });
      // 안의 CASE … END 는 블록이 아니다 — 문장 끝으로 건너뛴다 (; 는 다음 차례에 읽는다)
      i = (last?.t === ";" || last?.t === "/") && !inParen ? e - 1 : e;
      continue;
    }
    switch (w) {
      case "PROCEDURE": case "FUNCTION": {
        if (NOT_DEF.has(first) || prev?.t === "." || prev?.t === "%") break;
        const [name, j] = nameAt(i + 1);
        if (!name) break;
        // 괄호 밖의 IS·AS 가 ; 보다 먼저면 몸체, 아니면 선언
        let k = j;
        for (let d = 0; k < toks.length; k++) {
          const x = toks[k];
          if (x.t === "(") d++;
          else if (x.t === ")") d = Math.max(0, d - 1);
          else if (d === 0 && (x.t === ";" || x.t === "/" || (x.t === "w" && (x.v === "IS" || x.v === "AS" || x.v === "BEGIN" || x.v === "END")))) break;
        }
        const x = toks[k];
        const kind = w === "PROCEDURE" ? "procedure" : "function";
        if (x?.t === "w" && (x.v === "IS" || x.v === "AS") && !["LANGUAGE", "EXTERNAL"].includes(word(k + 1))) {
          stack.push(add({ kind, name, from: tk.from, to: src.length, end: -1, decl: false, children: [] }));
          i = k;
        } else if (x?.t === ";" || x?.t === "/" || x?.t === "w" && (x.v === "IS" || x.v === "AS")) {
          const e = x.t === ";" ? k : toSemi(k);
          add({ kind, name, from: tk.from, to: toks[e]?.to ?? src.length, end: -1, decl: true, children: [] });
          i = e - (toks[e]?.t === "/" ? 1 : 0);
        }
        break;
      }
      case "PACKAGE": case "TYPE": {
        if (NOT_DEF.has(first) || prev?.t === "%" || prev?.t === ".") break;
        const body = word(i + 1) === "BODY";
        const [name, j] = nameAt(i + (body ? 2 : 1));
        if (!name) break;
        if (w === "TYPE" && !body) {
          // TYPE t IS TABLE OF … ; · CREATE TYPE t AS OBJECT (…);
          const e = toSemi(j);
          add({ kind: "type", name, from: tk.from, to: toks[e]?.to ?? src.length, end: -1, decl: true, children: [] });
          i = e - (toks[e]?.t === "/" ? 1 : 0);
          break;
        }
        const kind: NavKind = w === "TYPE" ? "type body" : body ? "package body" : "package";
        stack.push(add({ kind, name, from: tk.from, to: src.length, end: -1, decl: false, children: [] }));
        // AUTHID … IS 까지는 건너뛴다
        let k = j;
        while (k < toks.length && !(toks[k].t === "w" && (toks[k].v === "IS" || toks[k].v === "AS")) && toks[k].t !== ";" && toks[k].t !== "/") k++;
        if (toks[k]?.t === "w") i = k;
        break;
      }
      case "TRIGGER": {
        if (NOT_DEF.has(first)) break;
        const [name] = nameAt(i + 1);
        if (!name) break;
        stack.push(add({ kind: "trigger", name, from: tk.from, to: src.length, end: -1, decl: false, children: [] }));
        break;
      }
      case "CURSOR": {
        if (prevW === "REF" || prev?.t === "%") break;
        const [name, j] = nameAt(i + 1);
        if (!name) break;
        const e = toSemi(j);
        add({ kind: "cursor", name, from: tk.from, to: toks[e]?.to ?? src.length, end: -1, decl: true, children: [] });
        i = e - (toks[e]?.t === "/" ? 1 : 0);
        break;
      }
      case "DECLARE": {
        const t0 = top();
        if (t0.kind === "trigger" && !t0.begun) break;
        stack.push(add({ kind: "declare", name: "DECLARE", from: tk.from, to: src.length, end: -1, decl: false, children: [] }));
        break;
      }
      case "BEGIN": {
        const t0 = top();
        if (stack.length > 1 && WAITS_BODY.has(t0.kind) && !t0.begun) { t0.begun = true; break; }
        stack.push(add({ kind: "begin", name: "BEGIN", from: tk.from, to: src.length, end: -1, decl: false, children: [] }));
        break;
      }
      case "FOR": case "WHILE":
        if (loopHead < 0) { loopHead = tk.from; loopWord = w; }
        break;
      case "IF": case "LOOP": case "CASE": {
        // END IF · 23ai 의 IF [NOT] EXISTS
        if (prevW === "END" || (w === "IF" && (word(i + 1) === "EXISTS" || word(i + 1) === "NOT" && word(i + 2) === "EXISTS"))) break;
        const kind = w.toLowerCase() as NavKind;
        const from = w === "LOOP" && loopHead >= 0 ? loopHead : tk.from;
        if (w === "LOOP") loopHead = -1;
        const name = w === "LOOP" && from !== tk.from ? `${loopWord} LOOP` : w;
        const x = add({ kind, name, from, to: src.length, end: -1, decl: false, children: [] });
        // FOR r IN (SELECT …) LOOP — 머리의 쿼리는 이 LOOP 안으로
        const sib = top().children;
        while (sib.length > 1 && sib[sib.length - 2].kind === "sql" && sib[sib.length - 2].from >= from) x.children.unshift(sib.splice(sib.length - 2, 1)[0]);
        stack.push(x);
        break;
      }
      case "END": {
        if (stack.length <= 1) break;
        const nx = word(i + 1);
        let k = i;
        let at = stack.length - 1;
        if (nx === "IF" || nx === "LOOP" || nx === "CASE") {
          k = i + 1;
          const kind = nx.toLowerCase();
          let f = stack.length - 1;
          while (f > 0 && stack[f].kind !== kind) f--;
          if (f > 0) at = f;
        }
        // END 이름 ;
        if (toks[k + 1]?.t === "w" && toks[k + 2]?.t === ";") k++;
        const to = toks[k + 1]?.t === ";" ? toks[k + 1].to : toks[k].to;
        while (stack.length - 1 > at) close(stack.pop()!, tk.from, to);
        close(stack.pop()!, tk.from, to);
        i = k;
        break;
      }
    }
  }
  return root.children;
}

/** pos 를 감싼 마디들 — 바깥부터 */
export function navPath(nodes: NavNode[], pos: number): NavNode[] {
  const out: NavNode[] = [];
  for (let list = nodes; ;) {
    const x = list.find((n) => n.from <= pos && pos <= n.to);
    if (!x) return out;
    out.push(x);
    list = x.children;
  }
}

/** 구조 보기 목록 — 단위만, 깊이와 함께. sql: 쿼리(한 문장)까지 */
export function navOutline(nodes: NavNode[], sql = false): { node: NavNode; depth: number }[] {
  const out: { node: NavNode; depth: number }[] = [];
  const walk = (list: NavNode[], depth: number) => {
    for (const x of list) {
      const unit = UNIT_KINDS.has(x.kind) || (sql && x.kind === "sql");
      if (unit) out.push({ node: x, depth });
      walk(x.children, unit ? depth + 1 : depth);
    }
  };
  walk(nodes, 0);
  return out;
}

/** 화면에 쓸 이름 */
export const KIND_LABEL: Record<NavKind, string> = {
  package: "패키지", "package body": "패키지 본문", "type body": "타입 본문", procedure: "프로시저", function: "함수",
  trigger: "트리거", type: "타입", cursor: "커서", declare: "블록", begin: "블록", if: "IF", loop: "LOOP", case: "CASE",
  sql: "SQL", "c function": "함수", "declare section": "선언부", struct: "구조체", "c block": "블록",
};
