// SQL 포맷 — 스타일 셋 중 하나로 정리하고, 결과가 원본과 같은 SQL 인지 확인한다.
//
//   오른쪽 맞춤 (기본) : 절 키워드를 SELECT 끝에 맞추고 쉼표를 앞에 — sqlfmt.ts
//   간결              : 오른쪽 맞춤 + 목록을 한 줄에 채움 — sqlfmt.ts
//   왼쪽 정렬          : 키워드 한 줄, 내용은 들여쓰기 — sql-formatter(PL/SQL 방언)
// sqlfmt 가 다루지 않는 문장(DDL·INSERT ALL 등)은 sql-formatter 로 한다.
//
// 포맷터가 힌트 주석·문자열·연산자를 잘못 건드리면 실행 결과가 달라진다. 그래서 포맷 전후를
// 토큰으로 쪼개 공백·줄바꿈·키워드 대소문자 말고 달라진 게 있으면 적용하지 않는다.
// PL/SQL 블록(BEGIN/DECLARE/CREATE PACKAGE ...)은 건드리지 않는다.

import { format, type FormatOptionsWithLanguage } from "sql-formatter";
import { formatRiver } from "./sqlfmt";

export type FormatStyle = "river" | "compact" | "left";
type Case = "upper" | "lower" | "preserve";

export interface FormatPrefs {
  style: FormatStyle;
  keywordCase: Case;
  /** 테이블·컬럼 이름 */
  identCase: Case;
  /** 목록의 쉼표를 줄 앞에 (오른쪽 맞춤·간결) */
  commaBefore: boolean;
  /** AND / OR 를 줄 앞에 */
  logicalBefore: boolean;
  /** 왼쪽 정렬의 들여쓰기 */
  indent: 2 | 4;
  /** 왼쪽 정렬: 한 줄에 둘 괄호 식의 최대 길이 · 간결: 한 줄 너비 */
  expressionWidth: number;
}

export const STYLE_PRESETS: Record<FormatStyle, { label: string; prefs: Partial<FormatPrefs> }> = {
  river: { label: "오른쪽 맞춤 · 앞 쉼표 (Toad·오렌지)", prefs: { commaBefore: true, logicalBefore: true } },
  compact: { label: "간결 (목록을 한 줄에 채움)", prefs: { commaBefore: false, logicalBefore: true, expressionWidth: 100 } },
  left: { label: "왼쪽 정렬 (키워드 한 줄, 내용 들여쓰기)", prefs: { commaBefore: false, logicalBefore: true, indent: 2, expressionWidth: 60 } },
};

export const DEFAULT_PREFS: FormatPrefs = {
  style: "river", keywordCase: "upper", identCase: "preserve", commaBefore: true, logicalBefore: true, indent: 2, expressionWidth: 60,
};

const KEY = "format.prefs";

export function loadPrefs(): FormatPrefs {
  try {
    return { ...DEFAULT_PREFS, ...JSON.parse(localStorage.getItem(KEY) ?? "{}") };
  } catch {
    return { ...DEFAULT_PREFS };
  }
}

export function savePrefs(p: FormatPrefs) {
  try { localStorage.setItem(KEY, JSON.stringify(p)); } catch { /* 저장 못 해도 이번에는 쓴다 */ }
}

/** 쓰는 동안 자동 포맷 — 모두 기본은 끔 (파일 메뉴 > 자동 포맷, 설정 > 편집기) */
export interface AutoFormat {
  /** 키워드를 다 치면(띄어쓰기·줄바꿈·괄호 …) 그 낱말을 설정의 키워드 대소문자로 */
  caseOnType: boolean;
  /** 줄 끝에서 ; 를 치면 그 문장을 정리 */
  onSemicolon: boolean;
  /** SQL 문장을 붙여 넣으면 정리 */
  onPaste: boolean;
  /** 실행(Ctrl+Enter·F9)할 때 그 문장을 정리 */
  onRun: boolean;
}

const AUTO_KEY = "format.auto";
const AUTO_OFF: AutoFormat = { caseOnType: false, onSemicolon: false, onPaste: false, onRun: false };
let autoCache: AutoFormat | null = null;

export function autoFormat(): AutoFormat {
  if (!autoCache) {
    try { autoCache = { ...AUTO_OFF, ...JSON.parse(localStorage.getItem(AUTO_KEY) ?? "{}") }; } catch { autoCache = { ...AUTO_OFF }; }
  }
  return autoCache!;
}

export function setAutoFormat(a: Partial<AutoFormat>) {
  autoCache = { ...autoFormat(), ...a };
  try { localStorage.setItem(AUTO_KEY, JSON.stringify(autoCache)); } catch { /* 이번 실행에서만 */ }
}

/** 자동으로 정리할 만한 SQL 문장으로 시작하는지 (PL/SQL 조각·DDL 은 건드리지 않는다) */
export function startsWithDml(sql: string): boolean {
  return /^\s*(--[^\n]*\n\s*|\/\*[\s\S]*?\*\/\s*)*\(?\s*(SELECT|WITH|INSERT|UPDATE|DELETE|MERGE)\b/i.test(sql);
}

export type FormatResult ={ ok: true; text: string } | { ok: false; reason: string };

const PLSQL_START = /^\s*(BEGIN|DECLARE|CREATE\s+(OR\s+REPLACE\s+)?(EDITIONABLE\s+|NONEDITIONABLE\s+)?(PACKAGE|PROCEDURE|FUNCTION|TRIGGER|TYPE)\b)/i;

/** 주석·공백을 건너뛰고 본문이 PL/SQL 블록으로 시작하는지 */
function isPlsql(sql: string) {
  const body = sql.replace(/^(\s+|--[^\n]*\n?|\/\*[\s\S]*?\*\/)+/, "");
  return PLSQL_START.test(body) || /^\s*\/\s*$/m.test(sql);
}

interface Tok { k: "w" | "s" | "c" | "p"; v: string }

/** 비교용 토큰: 단어(대문자로), 문자열·따옴표 이름(그대로), 주석(공백만 정리), 기호 */
export function tokens(sql: string): Tok[] {
  const out: Tok[] = [];
  let i = 0;
  const n = sql.length;
  while (i < n) {
    const ch = sql[i];
    if (/\s/.test(ch)) { i++; continue; }
    if (ch === "-" && sql[i + 1] === "-") {
      const e = sql.indexOf("\n", i);
      const end = e < 0 ? n : e;
      out.push({ k: "c", v: sql.slice(i, end).replace(/\s+/g, " ").trim() });
      i = end;
      continue;
    }
    if (ch === "/" && sql[i + 1] === "*") {
      const e = sql.indexOf("*/", i + 2);
      const end = e < 0 ? n : e + 2;
      out.push({ k: "c", v: sql.slice(i, end).replace(/\s+/g, " ") });
      i = end;
      continue;
    }
    // q'[...]' 대체 따옴표
    if ((ch === "q" || ch === "Q" || ((ch === "n" || ch === "N") && /[qQ]/.test(sql[i + 1] ?? ""))) && /['"]/.test(sql[i + (ch === "n" || ch === "N" ? 2 : 1)] ?? "")) {
      const q = i + (ch === "n" || ch === "N" ? 2 : 1);
      const open = sql[q + 1];
      const close = ({ "[": "]", "(": ")", "{": "}", "<": ">" } as Record<string, string>)[open] ?? open;
      const e = sql.indexOf(close + "'", q + 2);
      const end = e < 0 ? n : e + 2;
      out.push({ k: "s", v: sql.slice(i, end) });
      i = end;
      continue;
    }
    if (ch === "'" || ch === '"') {
      let j = i + 1;
      while (j < n) {
        if (sql[j] === ch) {
          if (sql[j + 1] === ch) { j += 2; continue; }
          break;
        }
        j++;
      }
      out.push({ k: "s", v: sql.slice(i, j + 1) });
      i = j + 1;
      continue;
    }
    const m = /^[A-Za-z0-9_$#.]+|^[가-힣\w$#]+/.exec(sql.slice(i));
    if (m) {
      // 점으로 이은 이름·숫자는 한 덩어리 — 포맷터가 "a.b" 를 "a. b" 로 바꾸는 걸 잡는다
      out.push({ k: "w", v: m[0].toUpperCase() });
      i += m[0].length;
      continue;
    }
    // 두 글자 연산자는 한 토큰 — ">=" 가 "> =" 로 갈라지면 다른 SQL 이다
    const op = /^(>=|<=|<>|!=|\^=|~=|\|\||:=|=>|\*\*|\.\.)/.exec(sql.slice(i));
    if (op) {
      out.push({ k: "p", v: op[0] });
      i += op[0].length;
      continue;
    }
    out.push({ k: "p", v: ch });
    i++;
  }
  return out;
}

/** 같은 SQL 인지 (공백·줄바꿈·키워드 대소문자만 다른지).
 *  주석은 자리가 조금 옮겨져도(앞 쉼표 앞으로 등) 같은 주석이 모두 남아 있으면 된다 — 힌트(/*+ … *\/)는 자리도 같아야 한다 */
export function sameSql(a: string, b: string): boolean {
  const split = (s: string) => {
    const code: Tok[] = [];
    const notes: string[] = [];
    for (const t of tokens(s)) {
      if (t.k === "c" && !t.v.startsWith("/*+")) notes.push(t.v);
      else code.push(t);
    }
    return { code, notes: notes.sort() };
  };
  const x = split(a);
  const y = split(b);
  if (x.code.length !== y.code.length || x.notes.join("\n") !== y.notes.join("\n")) return false;
  return x.code.every((t, i) => t.k === y.code[i].k && t.v === y.code[i].v);
}

function formatLeft(sql: string, prefs: FormatPrefs): string {
  const opts: FormatOptionsWithLanguage = {
    language: "plsql",
    keywordCase: prefs.keywordCase,
    dataTypeCase: prefs.keywordCase,
    functionCase: prefs.keywordCase,
    identifierCase: prefs.identCase,
    tabWidth: prefs.indent,
    expressionWidth: prefs.expressionWidth,
    logicalOperatorNewline: prefs.logicalBefore ? "before" : "after",
    linesBetweenQueries: 1,
  };
  return format(sql, opts);
}

export function formatSql(sql: string, prefs: FormatPrefs = loadPrefs()): FormatResult {
  if (!sql.trim()) return { ok: false, reason: "포맷할 SQL 이 없습니다" };
  if (isPlsql(sql)) {
    return { ok: false, reason: "PL/SQL 블록(BEGIN·DECLARE·CREATE PACKAGE 등)과 '/' 구분 스크립트는 아직 포맷하지 않습니다. SQL 문만 선택해서 포맷하세요." };
  }
  // 오른쪽 맞춤·간결이 다루지 못하는 문장(DDL·INSERT ALL …)이면 왼쪽 정렬로
  let out: string | null = null;
  if (prefs.style !== "left") {
    out = formatRiver(sql, {
      commaBefore: prefs.commaBefore, logicalBefore: prefs.logicalBefore, fill: prefs.style === "compact",
      width: prefs.expressionWidth, keywordCase: prefs.keywordCase, identCase: prefs.identCase,
    });
    if (out !== null && !sameSql(sql, out)) out = null;
  }
  if (out === null) {
    try {
      out = formatLeft(sql, prefs);
    } catch (e) {
      return { ok: false, reason: `이 SQL 은 포맷할 수 없습니다: ${(e as Error).message.split("\n")[0]}` };
    }
  }
  // 원본 끝의 세미콜론 유무는 그대로
  if (!/;\s*$/.test(sql)) out = out.replace(/;\s*$/, "");
  if (!sameSql(sql, out)) {
    return { ok: false, reason: "포맷 결과가 원본과 다른 SQL 이 될 수 있어 적용하지 않았습니다 (공백·대소문자 외의 차이 발견)." };
  }
  // 앞뒤 공백은 원본 모양을 따른다
  const lead = /^\s*/.exec(sql)![0];
  const trail = /\s*$/.exec(sql)![0];
  return { ok: true, text: lead.replace(/[^\n]/g, "") + out.trim() + trail };
}

const CASE_NAME = { upper: "대문자", lower: "소문자", preserve: "그대로" } as const;
const caseOf = (ws: string[]): Case =>
  ws.every((w) => w === w.toUpperCase()) ? "upper" : ws.every((w) => w === w.toLowerCase()) ? "lower" : "preserve";

/** 예시 SQL 의 모양을 읽어 설정을 맞춘다 — 키워드 정렬, 쉼표·AND 위치, 대소문자 */
export function detectPrefs(sample: string): { prefs: Partial<FormatPrefs>; found: string[] } {
  const lines = sample.replace(/\r/g, "").split("\n").filter((l) => l.trim());
  const found: string[] = [];
  const p: Partial<FormatPrefs> = {};
  // 키워드만 있는 줄(SELECT / FROM 단독)이면 왼쪽 정렬, 키워드 뒤에 내용이 오면 오른쪽 맞춤
  const alone = lines.filter((l) => /^\s*(SELECT|FROM|WHERE|GROUP BY|ORDER BY)\s*$/i.test(l)).length;
  const inline = lines.filter((l) => /^\s*(SELECT|FROM|WHERE)\s+\S/i.test(l)).length;
  if (alone > inline) {
    p.style = "left";
    const ind = lines.map((l) => /^( +)\S/.exec(l)?.[1].length ?? 0).filter((n) => n > 0);
    if (ind.length) p.indent = Math.min(...ind) >= 4 ? 4 : 2;
    found.push("왼쪽 정렬");
  } else if (inline) {
    // SELECT 줄에 쉼표가 둘 이상이면 목록을 채우는 간결
    const sel = lines.find((l) => /^\s*SELECT\s/i.test(l)) ?? "";
    p.style = (sel.match(/,/g)?.length ?? 0) >= 2 ? "compact" : "river";
    found.push(p.style === "compact" ? "간결 (목록을 한 줄에)" : "오른쪽 맞춤");
  }
  const lead = lines.filter((l) => /^\s*,/.test(l)).length;
  const trail = lines.filter((l) => /,\s*(--.*)?$/.test(l)).length;
  if (lead || trail) {
    p.commaBefore = lead >= trail;
    found.push(p.commaBefore ? "쉼표를 줄 앞에" : "쉼표를 줄 끝에");
  }
  const andBefore = lines.filter((l) => /^\s*(AND|OR)\b/i.test(l)).length;
  const andAfter = lines.filter((l) => /\b(AND|OR)\s*$/i.test(l)).length;
  if (andBefore || andAfter) {
    p.logicalBefore = andBefore >= andAfter;
    found.push(p.logicalBefore ? "AND·OR 를 줄 앞에" : "AND·OR 를 줄 끝에");
  }
  // 대소문자 — 문자열·주석·따옴표 이름을 빼고 본다
  const body = sample.replace(/'(?:[^']|'')*'|"[^"]*"|--[^\n]*|\/\*[\s\S]*?\*\//g, " ");
  const words = [...body.matchAll(/\b[A-Za-z_][\w$#]*\b/g)].map((m) => m[0]).filter((w) => /[A-Za-z]/.test(w));
  const KW = /^(select|from|where|and|or|not|in|is|null|as|on|join|inner|left|right|full|outer|cross|group|by|order|having|union|all|minus|distinct|case|when|then|else|end|insert|into|values|update|set|delete|with|asc|desc|like|between|exists)$/i;
  const kws = words.filter((w) => KW.test(w));
  if (kws.length) {
    p.keywordCase = caseOf(kws);
    found.push(`키워드 ${CASE_NAME[p.keywordCase]}`);
  }
  const ids = words.filter((w) => !KW.test(w));
  if (ids.length) {
    p.identCase = caseOf(ids);
    found.push(`이름 ${CASE_NAME[p.identCase]}`);
  }
  return { prefs: p, found };
}

