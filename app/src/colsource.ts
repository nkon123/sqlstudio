// 값의 출처 — 결과 그리드의 열이 어느 테이블.열인지, 그 열에 값을 넣는 곳(INSERT·UPDATE·MERGE)이 어디인지.
// 열은 쿼리를 자동완성 캐시로 풀어 찾고(DB 왕복 없음), 넣는 곳은 저장된 분석 결과(문장 목록)에서 찾는다.
// 우클릭으로 고를 때만 돈다 — 조회·스크롤에는 아무 일도 더하지 않는다.

import { invoke } from "@tauri-apps/api/core";
import { errOf, type Column } from "./api";
import { showMenu } from "./ctxmenu";
import { ResultGrid } from "./grid";
import { h, modal, toast } from "./ui";

type Origin =
  | { kind: "column"; owner: string | null; table: string; column: string }
  | { kind: "computed"; expr: string }
  | { kind: "unknown"; reason: string };
interface ColWrite { node: string; unit: string; line: number; end_line: number; kind: string; how: "set" | "all" | "maybe"; text: string }
interface ColumnSource { origin: Origin; analyzed: boolean; writes: ColWrite[] }

/** 분석 단위 키(OWNER.NAME.PACKAGE_BODY) → DB 객체. Pro*C·XML 처럼 DB 에 없는 것은 null */
function dbObject(unit: string): { owner: string; name: string; kind: string } | null {
  const parts = unit.split(".");
  if (parts.length < 3) return null;
  const kind = parts.slice(2).join(".").replace(/_/g, " ");
  if (!["PACKAGE BODY", "PACKAGE", "PROCEDURE", "FUNCTION", "TRIGGER", "TYPE BODY"].includes(kind)) return null;
  return { owner: parts[0], name: parts[1], kind };
}

const HOW: Record<ColWrite["how"], string> = { set: "", all: " · 열 목록 없음(모든 열)", maybe: " · 본문이 길어 잘림 — 확인 필요" };

export interface SourceHooks {
  /** DB 객체 소스를 새 탭으로 (line 이 있으면 그 줄로) */
  openSource: (owner: string, name: string, kind: string, line?: number) => void;
  /** 사전으로 찾는 컬럼 영향도 (모든 사용처) */
  showUsage: (table: string, column: string) => void;
  /** 분석 화면 열기 */
  openAnalysis: () => void;
}

export async function showColumnSource(id: number, sql: string, pos: number | null, col: Column, hooks: SourceHooks) {
  const body = h("div", { class: "usage" }, h("div", { class: "hint" }, `${col.name} 의 출처를 찾는 중…`));
  const shown = modal(`값의 출처 — ${col.name}`, body, [{ label: "닫기", value: true }], { wide: true });
  const started = performance.now();
  let r: ColumnSource;
  try {
    r = await invoke<ColumnSource>("column_source", { id, sql, pos, name: col.name });
  } catch (e) {
    body.replaceChildren(h("div", { class: "err" }, errOf(e).message));
    return shown;
  }
  const o = r.origin;
  if (o.kind !== "column") {
    body.replaceChildren(
      h("p", {}, o.kind === "computed" ? `${col.name} 은 테이블의 열이 아니라 쿼리에서 계산한 값입니다:` : o.reason),
      o.kind === "computed" ? h("pre", { class: "ob-src" }, o.expr) : "",
      h("p", { class: "dim", style: "font-size:12px" }, "계산에 쓴 열을 결과에 따로 꺼내면 그 열의 출처를 볼 수 있습니다."));
    return shown;
  }
  const full = `${o.owner ? `${o.owner}.` : ""}${o.table}.${o.column}`;
  const usageBtn = h("button", { class: "small", title: "DB 사전으로 이 열이 나오는 모든 소스·뷰를 찾는다 (읽는 곳 포함)", onclick: () => hooks.showUsage(o.owner ? `"${o.owner}"."${o.table}"` : o.table, o.column) }, "모든 사용처 (컬럼 영향도)");
  const head = h("div", { class: "row" }, h("span", {}, "결과 열 ", h("b", {}, col.name), " ← ", h("b", {}, full)), h("span", { class: "spacer" }), usageBtn);
  if (!r.analyzed) {
    body.replaceChildren(head,
      h("p", {}, "이 접속의 분석 결과가 없어 값을 넣는 곳은 찾지 못했습니다."),
      h("p", { class: "dim", style: "font-size:12px" }, "분석 탭에서 PL/SQL(또는 코드 폴더)을 한 번 분석해 두면, 이 열에 INSERT·UPDATE·MERGE 하는 프로시저와 줄이 여기 나옵니다. 모델 없이 정적 분석만 해도 됩니다."),
      h("div", { class: "row" }, h("button", { class: "small", onclick: () => hooks.openAnalysis() }, "분석 화면 열기")));
    return shown;
  }
  const writes = [...r.writes].sort((a, b) => Number(a.how !== "set") - Number(b.how !== "set"));
  const secs = ((performance.now() - started) / 1000).toFixed(1);
  const sure = writes.filter((w) => w.how === "set").length;
  const summary = `${full} 에 값을 넣는 문장 ${sure}개${writes.length > sure ? ` · 확인할 것 ${writes.length - sure}개` : ""} · 객체 ${new Set(writes.map((w) => w.node)).size}개`;
  const c = (name: string, type_name = "VARCHAR2"): Column => ({ name, type_name, nullable: true });
  const columns = [c("구분"), c("객체"), c("줄", "NUMBER"), c("내용")];
  const rows = writes.map((w) => [w.kind + HOW[w.how], w.node, String(w.line), w.text]);
  const open = (i: number) => {
    const w = writes[i];
    const d = w && dbObject(w.unit);
    if (!d) return false;
    hooks.openSource(d.owner, d.name, d.kind, w.line);
    return true;
  };
  const idx = (row: (string | null)[]) => rows.indexOf(row as string[]);
  const grid: ResultGrid = new ResultGrid({
    // DB 객체면 소스를 그 줄로 연다. Pro*C·XML 은 문장 본문을 창으로 (기본 동작)
    onDoubleClick: (row) => open(idx(row)),
    onContextMenu: (row, _c, e) => {
      const i = idx(row);
      const d = writes[i] && dbObject(writes[i].unit);
      showMenu(e.clientX, e.clientY, [
        ...(d ? [{ label: "소스 열기", hint: "더블클릭", run: () => open(i) }, { sep: true as const }] : []),
        { label: "고른 셀 복사", hint: "Ctrl+C", run: () => navigator.clipboard.writeText(grid.selectionText()).catch(() => {}) },
        { label: "머리 포함 복사", hint: "Ctrl+Shift+C", run: () => navigator.clipboard.writeText(grid.selectionText(true)).catch(() => {}) },
      ]);
    },
  });
  grid.setPage({ columns, rows, has_more: false, fetched_total: rows.length });
  const filter = h("input", { placeholder: "거르기 (예: UPDATE, 패키지 이름)", style: "width:260px" }) as HTMLInputElement;
  filter.addEventListener("input", () => grid.setFilter(filter.value));
  filter.addEventListener("keydown", (e) => { if (e.key === "ArrowDown") { e.preventDefault(); grid.el.focus(); } });
  const copy = () => navigator.clipboard.writeText([`${full} 에 값을 넣는 곳`, ...writes.map((w) => `- ${w.kind}${HOW[w.how]} ${w.node}:${w.line}  ${w.text.slice(0, 200)}`)].join("\n"))
    .then(() => toast("목록을 복사했습니다", "info", 1500));
  body.replaceChildren(
    head,
    h("div", { class: "row", style: "margin-top:4px" }, h("span", {}, summary), h("span", { class: "spacer" }), writes.length ? h("button", { class: "small", onclick: copy }, "목록 복사") : ""),
    h("div", { class: "dim", style: "font-size:12px;margin:4px 0 6px" }, `${secs}초 · 저장된 분석 결과로 찾았습니다 (분석한 뒤 바뀐 소스, 동적 SQL, 트리거의 :NEW 대입은 나오지 않습니다). 더블클릭하면 소스를 그 줄로 엽니다.`),
    writes.length ? h("div", { class: "row", style: "margin-bottom:6px" }, filter) : h("p", { class: "hint" }, "분석한 소스에서는 이 열에 값을 넣는 문장을 찾지 못했습니다. 다른 시스템이 넣거나, 동적 SQL·트리거일 수 있습니다 — '모든 사용처' 로 더 넓게 찾아 보세요."),
    writes.length ? h("div", { class: "usage-grid" }, grid.el) : "");
  return shown;
}
