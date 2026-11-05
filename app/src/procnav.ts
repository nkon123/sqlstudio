// Pro*C 구조 읽기 — C 함수 · if/for/while 블록 · EXEC SQL 문장 · 커서 · DECLARE SECTION.
// PL/SQL 구조(plsqlnav)와 같은 NavNode 나무를 만든다 — 개요·위치 줄·접기·정의로 이동이 그대로 쓴다.
// 문법 검사가 아니라 낱말·괄호로 짐작한다 (K&R 함수 머리도 받는다).

import { type NavKind, type NavNode, sqlLabel } from "./plsqlnav";

/** Pro*C 로 볼 글인가 — EXEC SQL 이 있고 C 의 흔적(#include · 중괄호)이 있으면 */
export function isProC(text: string): boolean {
  return /\bEXEC\s+(SQL|ORACLE|TOOLS)\b/i.test(text) && (/^\s*#\s*(include|define)\b/m.test(text) || /[{}]/.test(text));
}

interface Tok { t: "w" | "p"; v: string; from: number; to: number }

const isStart = (c: number) => (c >= 65 && c <= 90) || (c >= 97 && c <= 122) || c === 95;
const isPart = (c: number) => isStart(c) || (c >= 48 && c <= 57) || c === 36 || c === 35;

/** 낱말·기호로 — 주석·문자열·전처리 줄은 건너뛴다 */
function lex(s: string): Tok[] {
  const out: Tok[] = [];
  const n = s.length;
  let lineStart = true;
  for (let i = 0; i < n;) {
    const c = s.charCodeAt(i);
    if (c === 10) { lineStart = true; i++; continue; }
    if (c === 32 || c === 9 || c === 13 || c === 12) { i++; continue; }
    const atLine = lineStart;
    lineStart = false;
    // # 전처리 — 줄 끝의 \ 는 다음 줄로 이어진다
    if (c === 35 && atLine) {
      let j = i;
      for (;;) {
        const e = s.indexOf("\n", j);
        if (e < 0) { j = n; break; }
        if (s[e - 1] === "\\" || (s[e - 1] === "\r" && s[e - 2] === "\\")) { j = e + 1; continue; }
        j = e;
        break;
      }
      i = j;
      continue;
    }
    if (c === 47 && s.charCodeAt(i + 1) === 47) { const e = s.indexOf("\n", i); i = e < 0 ? n : e; continue; }
    if (c === 47 && s.charCodeAt(i + 1) === 42) { const e = s.indexOf("*/", i + 2); i = e < 0 ? n : e + 2; continue; }
    // SQL 의 -- 주석 (C 에서 -- 뒤에 - 가 또 오는 일은 거의 없다 — x-- 는 앞이 낱말이다)
    if (c === 45 && s.charCodeAt(i + 1) === 45 && out.length && out[out.length - 1].t === "p" && !/[\w)\]]/.test(s[i - 1] ?? "")) {
      const e = s.indexOf("\n", i); i = e < 0 ? n : e; continue;
    }
    if (c === 34 || c === 39) {
      // "…" · '…' — \ 이스케이프, SQL 의 '' 도 그대로 넘어간다
      let j = i + 1;
      while (j < n && s.charCodeAt(j) !== c && s.charCodeAt(j) !== 10) j += s.charCodeAt(j) === 92 ? 2 : 1;
      i = j + 1;
      continue;
    }
    if (isStart(c)) {
      let j = i + 1;
      while (j < n && isPart(s.charCodeAt(j))) j++;
      out.push({ t: "w", v: s.slice(i, j).toUpperCase(), from: i, to: j });
      i = j;
      continue;
    }
    if (c >= 48 && c <= 57) { let j = i + 1; while (j < n && /[\w.]/.test(s[j])) j++; i = j; continue; }
    out.push({ t: "p", v: s[i], from: i, to: i + 1 });
    i++;
  }
  return out;
}

const CTRL = new Set(["IF", "FOR", "WHILE", "SWITCH"]);
/** 이름표를 달지 않는 EXEC SQL (선언·설정) */
const QUIET = new Set(["INCLUDE", "WHENEVER", "VAR", "TYPE", "CONTEXT", "ENABLE", "ALLOCATE", "FREE"]);

export function parseProC(src: string): NavNode[] {
  const toks = lex(src);
  const raw = (t: Tok) => src.slice(t.from, t.to);
  const root: NavNode = { kind: "begin", name: "", from: 0, to: src.length, end: -1, decl: false, children: [] };
  const stack: NavNode[] = [root];
  const top = () => stack[stack.length - 1];
  const add = (x: NavNode) => { top().children.push(x); return x; };
  const node = (kind: NavKind, name: string, from: number): NavNode => ({ kind, name, from, to: src.length, end: -1, decl: false, children: [] });
  /** 지금 C 선언(문장)이 시작한 자리 — 맨 바깥에서 함수 머리의 처음 */
  let declStart = -1;
  /** 바깥에서 본 "이름(…)" — 뒤에 { 가 오면 함수 */
  let pending: { name: string; from: number } | null = null;
  let section: NavNode | null = null;
  const depth = () => stack.length - 1;
  for (let i = 0; i < toks.length; i++) {
    const tk = toks[i];
    if (declStart < 0) declStart = tk.from;
    // EXEC SQL … ; (EXECUTE BEGIN … END-EXEC; 는 안의 ; 를 넘는다)
    if (tk.t === "w" && tk.v === "EXEC" && toks[i + 1]?.t === "w" && ["SQL", "ORACLE", "TOOLS"].includes(toks[i + 1].v)) {
      const k = i + 2;
      const plsql = toks[k]?.v === "EXECUTE" && ["BEGIN", "DECLARE"].includes(toks[k + 1]?.v ?? "");
      let e = k;
      if (plsql) {
        while (e < toks.length && !(toks[e].v === "END" && toks[e + 1]?.v === "-" && toks[e + 2]?.v === "EXEC")) e++;
        e += 2;
      }
      while (e < toks.length && toks[e].v !== ";") e++;
      const to = toks[e]?.to ?? src.length;
      const w0 = toks[k]?.v ?? "";
      if (w0 === "BEGIN" && toks[k + 1]?.v === "DECLARE") {
        section = add(node("declare section", "DECLARE SECTION", tk.from));
        stack.push(section);
      } else if (w0 === "END" && toks[k + 1]?.v === "DECLARE") {
        if (section && top() === section) { stack.pop(); section.end = tk.from; section.to = to; }
        section = null;
      } else if (w0 === "DECLARE" && toks[k + 2]?.v === "CURSOR") {
        add({ ...node("cursor", raw(toks[k + 1]), tk.from), to, decl: true });
      } else if (w0 === "DECLARE" && (toks[k + 2]?.v === "STATEMENT" || toks[k + 2]?.v === "TABLE")) {
        // DECLARE s STATEMENT · DECLARE t TABLE — 이름표 없이
      } else if (!QUIET.has(w0)) {
        add({ ...node("sql", sqlLabel(toks.slice(k, e).filter((x) => x.t === "w").map(raw)), tk.from), to });
      }
      i = e;
      declStart = -1;
      pending = null;
      continue;
    }
    if (tk.t !== "p") {
      // 이름( — 함수 머리 후보 (맨 바깥, 또는 DECLARE SECTION 안이 아닐 때)
      if (depth() === 0 && toks[i + 1]?.v === "(" && !CTRL.has(tk.v) && tk.v !== "SIZEOF" && tk.v !== "RETURN") {
        let d = 0, j = i + 1;
        for (; j < toks.length; j++) { if (toks[j].v === "(") d++; else if (toks[j].v === ")" && --d === 0) break; }
        // 바로 ; 나 , 면 원형 선언, = 면 변수
        const nx = toks[j + 1]?.v;
        pending = nx === ";" || nx === "," || nx === "=" ? null : { name: raw(tk), from: declStart };
        i = j;
      }
      continue;
    }
    switch (tk.v) {
      case ";":
        if (depth() === 0 && pending) {
          // K&R 인자 선언 (int f(a) int a; {) — ; 가 와도 { 를 기다린다
        } else declStart = -1;
        break;
      case "{": {
        if (pending && depth() === 0) {
          stack.push(add(node("c function", pending.name, pending.from)));
          pending = null;
          break;
        }
        // 블록의 머리: if (…) { · for · while · switch · else · do · struct x {
        let kind: NavKind = "c block";
        let name = "{";
        let from = tk.from;
        const prev = toks[i - 1];
        if (prev?.v === ")") {
          let d = 0, j = i - 1;
          for (; j >= 0; j--) { if (toks[j].v === ")") d++; else if (toks[j].v === "(" && --d === 0) break; }
          const h = toks[j - 1];
          if (h?.t === "w" && CTRL.has(h.v)) { name = h.v.toLowerCase(); from = h.from; if (h.v === "IF" && toks[j - 2]?.v === "ELSE") { name = "else if"; from = toks[j - 2].from; } }
        } else if (prev?.t === "w" && (prev.v === "ELSE" || prev.v === "DO")) { name = prev.v.toLowerCase(); from = prev.from; }
        else if (depth() === 0) {
          // struct / union / enum [이름] { — 맨 바깥만 이름을 단다
          let j = i - 1;
          while (j >= 0 && toks[j].t === "w" && !["STRUCT", "UNION", "ENUM"].includes(toks[j].v) && i - j < 3) j--;
          if (toks[j]?.t === "w" && ["STRUCT", "UNION", "ENUM"].includes(toks[j].v)) {
            kind = "struct";
            name = `${toks[j].v.toLowerCase()}${toks[j + 1] && toks[j + 1] !== tk ? ` ${raw(toks[j + 1])}` : ""}`;
            from = declStart >= 0 ? declStart : toks[j].from;
          }
        }
        stack.push(add(node(kind, name, from)));
        break;
      }
      case "}": {
        if (depth() === 0) break;
        // 닫히지 않은 DECLARE SECTION 은 건너 닫는다
        const x = stack.pop()!;
        x.end = tk.from;
        x.to = toks[i + 1]?.v === ";" ? toks[i + 1].to : tk.to;
        if (depth() === 0) {
          declStart = -1;
          // typedef struct { … } name;
          if (x.kind === "struct" && toks[i + 1]?.t === "w" && x.name === "struct") x.name = `struct ${raw(toks[i + 1])}`;
        }
        break;
      }
    }
  }
  return root.children;
}
