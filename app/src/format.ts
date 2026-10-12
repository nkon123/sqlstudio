// SQL 포맷 — sql-formatter(PL/SQL 방언) 를 감싸고, 결과가 원본과 같은 SQL 인지 확인한다.
//
// 포맷터가 힌트 주석·문자열·연산자를 잘못 건드리면 실행 결과가 달라진다. 그래서 포맷 전후를
// 토큰으로 쪼개 공백·줄바꿈·키워드 대소문자 말고 달라진 게 있으면 적용하지 않는다.
// PL/SQL 블록(BEGIN/DECLARE/CREATE PACKAGE ...)은 포맷터가 아직 잘 못 다뤄서 건드리지 않는다.

import { format, type FormatOptionsWithLanguage } from "sql-formatter";

export interface FormatPrefs {
  keywordCase: "upper" | "lower" | "preserve";
  indent: 2 | 4;
  /** 한 줄에 둘 괄호 식의 최대 길이 */
  expressionWidth: number;
  /** AND / OR 를 줄 앞에 둘지 */
  logicalBefore: boolean;
}

export const DEFAULT_PREFS: FormatPrefs = { keywordCase: "upper", indent: 2, expressionWidth: 60, logicalBefore: true };

const KEY = "format.prefs";

export function loadPrefs(): FormatPrefs {
  try {
    return { ...DEFAULT_PREFS, ...JSON.parse(localStorage.getItem(KEY) ?? "{}") };
  } catch {
    return { ...DEFAULT_PREFS };
  }
}

export function savePrefs(p: FormatPrefs) {
  localStorage.setItem(KEY, JSON.stringify(p));
}

export type FormatResult = { ok: true; text: string } | { ok: false; reason: string };

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

/** 같은 SQL 인지 (공백·줄바꿈·키워드 대소문자만 다른지) */
export function sameSql(a: string, b: string): boolean {
  const x = tokens(a);
  const y = tokens(b);
  if (x.length !== y.length) return false;
  return x.every((t, i) => t.k === y[i].k && t.v === y[i].v);
}

export function formatSql(sql: string, prefs: FormatPrefs = loadPrefs()): FormatResult {
  if (!sql.trim()) return { ok: false, reason: "포맷할 SQL 이 없습니다" };
  if (isPlsql(sql)) {
    return { ok: false, reason: "PL/SQL 블록(BEGIN·DECLARE·CREATE PACKAGE 등)과 '/' 구분 스크립트는 아직 포맷하지 않습니다. SQL 문만 선택해서 포맷하세요." };
  }
  const opts: FormatOptionsWithLanguage = {
    language: "plsql",
    keywordCase: prefs.keywordCase,
    dataTypeCase: prefs.keywordCase,
    tabWidth: prefs.indent,
    expressionWidth: prefs.expressionWidth,
    logicalOperatorNewline: prefs.logicalBefore ? "before" : "after",
    linesBetweenQueries: 1,
  };
  let out: string;
  try {
    out = format(sql, opts);
  } catch (e) {
    return { ok: false, reason: `이 SQL 은 포맷할 수 없습니다: ${(e as Error).message.split("\n")[0]}` };
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
