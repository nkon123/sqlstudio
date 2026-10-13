// 컬럼 영향도 — "이 컬럼을 바꾸면 어디가 깨지나".
// 사전 의존성으로 이 테이블을 쓰는 객체를 찾고, 그 소스·뷰 정의에서 컬럼 이름이 나오는 줄을 보여 준다.
// 이름이 안 보여도 테이블을 쓰는 객체(SELECT *, %ROWTYPE, 동적 SQL)는 따로 — 그래도 영향을 받을 수 있다.

import { invoke } from "@tauri-apps/api/core";
import { errOf } from "./api";
import { h, modal, toast } from "./ui";

interface Line { line: number; text: string; in_comment: boolean }
interface Obj { owner: string; name: string; kind: string; via?: string | null; lines: Line[] }
interface Usage { table: string; column: string; hits: Obj[]; silent: Obj[]; indexes: string[]; constraints: string[]; referenced_by: string[] }

const KIND: Record<string, string> = {
  VIEW: "뷰", "MATERIALIZED VIEW": "구체화 뷰", PACKAGE: "패키지", "PACKAGE BODY": "패키지 본문", PROCEDURE: "프로시저",
  FUNCTION: "함수", TRIGGER: "트리거", TYPE: "타입", "TYPE BODY": "타입 본문", SYNONYM: "동의어",
};

/** openSource: 그 객체 소스를 새 탭으로 */
export async function showUsage(id: number, table: string, column: string, openSource: (owner: string, name: string, kind: string) => void) {
  const body = h("div", { class: "usage" }, h("div", { class: "hint" }, `${table}.${column} 을 쓰는 곳을 찾는 중…`));
  const started = performance.now();
  const shown = modal(`컬럼 영향도 — ${table}.${column}`, body, [{ label: "닫기", value: true }], { wide: true });
  let u: Usage;
  try {
    u = await invoke<Usage>("column_usage", { id, table, column });
  } catch (e) {
    body.replaceChildren(h("div", { class: "err" }, errOf(e).message));
    return shown;
  }
  const secs = ((performance.now() - started) / 1000).toFixed(1);
  const codeLines = u.hits.reduce((n, o) => n + o.lines.filter((l) => !l.in_comment).length, 0);
  const summary = `${u.table}.${u.column}: 컬럼이 나오는 객체 ${u.hits.length}개 (${codeLines}줄)` +
    ` · 테이블만 쓰는 객체 ${u.silent.length}개 · 인덱스 ${u.indexes.length} · 제약 ${u.constraints.length} · 이 컬럼을 가리키는 외래 키 ${u.referenced_by.length}`;
  const objEl = (o: Obj, withLines: boolean) => h("details", { class: "usage-obj", open: withLines && o.lines.length <= 12 },
    h("summary", {},
      h("span", { class: "badge" }, KIND[o.kind] ?? o.kind), " ", h("strong", {}, `${o.owner}.${o.name}`),
      o.via ? h("span", { class: "dim" }, ` (동의어 ${o.via})`) : "",
      withLines ? h("span", { class: "dim" }, ` · ${o.lines.length}줄`) : "",
      h("button", { class: "small", style: "margin-left:8px", title: "소스를 새 탭으로", onclick: (e: Event) => { e.preventDefault(); openSource(o.owner, o.name, o.kind); } }, "열기")),
    withLines ? h("table", { class: "usage-lines" }, h("tbody", {}, ...o.lines.map((l) => h("tr", { class: l.in_comment ? "dim" : "" },
      h("td", { class: "num" }, String(l.line)), h("td", {}, h("code", {}, l.text.trim())), h("td", { class: "dim" }, l.in_comment ? "주석" : "")))))
      : "");
  const list = (title: string, xs: string[]) => xs.length ? h("div", {}, h("h4", {}, `${title} ${xs.length}`), h("ul", { class: "an-ul" }, ...xs.map((x) => h("li", {}, h("code", {}, x))))) : "";
  body.replaceChildren(
    h("div", { class: "row" }, h("span", {}, summary), h("span", { class: "spacer" }),
      h("button", { class: "small", onclick: () => navigator.clipboard.writeText(report(u)).then(() => toast("영향도 목록을 복사했습니다", "info", 1500)) }, "목록 복사")),
    h("div", { class: "dim", style: "font-size:12px;margin:4px 0 8px" }, `${secs}초 · 사전 의존성과 소스·뷰 정의로 찾았습니다. 동적 SQL 문자열 안의 이름과 다른 DB(DB 링크)는 찾지 못합니다.`),
    u.hits.length ? h("h4", {}, `컬럼이 나오는 곳 ${u.hits.length}`) : h("p", { class: "hint" }, "소스·뷰 정의에 이 컬럼 이름이 나오는 객체가 없습니다."),
    ...u.hits.map((o) => objEl(o, true)),
    u.silent.length ? h("h4", {}, `테이블은 쓰지만 컬럼 이름이 안 보이는 곳 ${u.silent.length}`) : "",
    u.silent.length ? h("p", { class: "hint" }, "SELECT *, %ROWTYPE, INSERT … VALUES (컬럼 목록 없이), 동적 SQL 이면 컬럼을 바꿀 때 영향을 받을 수 있습니다.") : "",
    ...u.silent.map((o) => objEl(o, false)),
    list("인덱스", u.indexes), list("제약조건", u.constraints), list("이 컬럼을 가리키는 외래 키", u.referenced_by));
  return shown;
}

/** 붙여 넣을 목록 */
function report(u: Usage): string {
  const out = [`${u.table}.${u.column} 영향도`];
  for (const o of u.hits) {
    out.push(`- ${KIND[o.kind] ?? o.kind} ${o.owner}.${o.name}${o.via ? ` (동의어 ${o.via})` : ""}`);
    for (const l of o.lines) out.push(`    ${l.line}: ${l.text.trim()}${l.in_comment ? "  (주석)" : ""}`);
  }
  if (u.silent.length) {
    out.push("테이블만 쓰는 곳 (SELECT *, %ROWTYPE, 동적 SQL 확인):");
    for (const o of u.silent) out.push(`- ${KIND[o.kind] ?? o.kind} ${o.owner}.${o.name}`);
  }
  for (const [t, xs] of [["인덱스", u.indexes], ["제약조건", u.constraints], ["외래 키", u.referenced_by]] as const) if (xs.length) out.push(`${t}: ${xs.join(", ")}`);
  return out.join("\n");
}
