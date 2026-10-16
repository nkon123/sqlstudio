// 값의 출처 — 결과 그리드의 열이 어느 테이블의 어느 열에서 어떤 식으로 나왔는지(계보),
// 그리고 그 테이블 열에 값을 넣는 곳(INSERT·UPDATE·MERGE)이 어디인지.
// 계보는 쿼리를 자동완성 캐시로 풀어 찾고(DB 왕복 없음), 넣는 곳은 저장된 분석 결과(문장 목록)에서 찾는다.
// 우클릭으로 고를 때만 돈다 — 조회·스크롤에는 아무 일도 더하지 않는다.

import { invoke } from "@tauri-apps/api/core";
import { errOf, type Column } from "./api";
import { showMenu } from "./ctxmenu";
import { ResultGrid } from "./grid";
import { h, modal, toast } from "./ui";

interface SourceCol { owner: string | null; table: string; column: string; alias?: string; data_type?: string; comment?: string }
type Kind = "column" | "view" | "computed" | "aggregate" | "analytic" | "constant" | "union" | "subquery" | "unknown";
interface Lineage {
  name: string; kind: Kind; expr?: string; via?: string; source?: SourceCol;
  from?: string; filter?: string; reason?: string; inputs?: Lineage[];
}
interface ColWrite { node: string; unit: string; line: number; end_line: number; kind: string; how: "set" | "all" | "maybe"; text: string }
interface ColumnWrites { owner: string | null; table: string; column: string; writes: ColWrite[] }
interface ColumnSource { lineage: Lineage; analyzed: boolean; writes: ColumnWrites[] }

const KIND: Record<Kind, string> = {
  column: "테이블 열", view: "뷰를 거침", computed: "계산", aggregate: "집계", analytic: "분석 함수",
  constant: "상수·의사 열", union: "합침", subquery: "서브쿼리", unknown: "모름",
};

/** 분석 단위 키(OWNER.NAME.PACKAGE_BODY) → DB 객체. Pro*C·XML 처럼 DB 에 없는 것은 null */
function dbObject(unit: string): { owner: string; name: string; kind: string } | null {
  const parts = unit.split(".");
  if (parts.length < 3) return null;
  const kind = parts.slice(2).join(".").replace(/_/g, " ");
  if (!["PACKAGE BODY", "PACKAGE", "PROCEDURE", "FUNCTION", "TRIGGER", "TYPE BODY"].includes(kind)) return null;
  return { owner: parts[0], name: parts[1], kind };
}

const HOW: Record<ColWrite["how"], string> = { set: "", all: " · 열 목록 없음(모든 열)", maybe: " · 본문이 길어 잘림 — 확인 필요" };

const fullName = (s: { owner: string | null; table: string; column: string }) => `${s.owner ? `${s.owner}.` : ""}${s.table}.${s.column}`;

export interface SourceHooks {
  /** DB 객체 소스를 새 탭으로 (line 이 있으면 그 줄로) */
  openSource: (owner: string, name: string, kind: string, line?: number) => void;
  /** 사전으로 찾는 컬럼 영향도 (모든 사용처) */
  showUsage: (table: string, column: string) => void;
  /** 분석 화면 열기 */
  openAnalysis: () => void;
}

/** 계보를 글로 (붙여 넣기용) */
function lineageText(l: Lineage, depth = 0): string[] {
  const pad = "  ".repeat(depth);
  const what = l.source ? `${fullName(l.source)}${l.source.data_type ? ` ${l.source.data_type}` : ""}${l.source.comment ? ` (${l.source.comment})` : ""}`
    : l.expr ?? l.reason ?? "";
  const out = [`${pad}${depth ? "← " : ""}${l.via ? `[${l.via}] ` : ""}${l.name} : ${KIND[l.kind]}${what ? ` — ${what}` : ""}`];
  if (l.filter) out.push(`${pad}    조건: ${l.filter}`);
  for (const i of l.inputs ?? []) out.push(...lineageText(i, depth + 1));
  return out;
}

export async function showColumnSource(id: number, sql: string, pos: number | null, col: Column, hooks: SourceHooks) {
  const body = h("div", { class: "usage colsrc" }, h("div", { class: "hint" }, `${col.name} 의 출처를 찾는 중…`));
  const shown = modal(`값의 출처 — ${col.name}`, body, [{ label: "닫기", value: true }], { wide: true });
  const started = performance.now();
  let r: ColumnSource;
  try {
    r = await invoke<ColumnSource>("column_source", { id, sql, pos, name: col.name });
  } catch (e) {
    body.replaceChildren(h("div", { class: "err" }, errOf(e).message));
    return shown;
  }
  const secs = ((performance.now() - started) / 1000).toFixed(1);
  const writesOf = new Map(r.writes.map((w) => [fullName(w), w]));
  // 아래 '값을 넣는 곳' 에 보일 테이블 열
  const wPane = h("div", { class: "colsrc-writes" });
  const leafEls = new Map<string, HTMLElement[]>();
  let picked = "";
  const pick = (key: string) => {
    picked = key;
    for (const [k, els] of leafEls) for (const el of els) el.classList.toggle("on", k === key);
    renderWrites(wPane, key, writesOf.get(key), r.analyzed, hooks);
  };

  // ── 계보 나무 ──
  const nodeEl = (l: Lineage): HTMLElement => {
    const badge = h("span", { class: `lin-kind lin-${l.kind}` }, KIND[l.kind]);
    const head = h("div", { class: "lin-head" }, badge);
    if (l.via) head.append(h("span", { class: "lin-via" }, l.via));
    head.append(h("b", {}, l.name));
    if (l.source) {
      const s = l.source;
      const key = fullName(s);
      const n = writesOf.get(key)?.writes.filter((w) => w.how === "set").length;
      const leaf = h("span", { class: "lin-src", title: "눌러서 이 열에 값을 넣는 곳 보기", onclick: () => pick(key) },
        "← ", h("b", {}, key), s.alias ? h("span", { class: "dim" }, ` (별칭 ${s.alias})`) : "",
        s.data_type ? h("span", { class: "lin-type" }, s.data_type) : "",
        s.comment ? h("span", { class: "lin-note" }, s.comment) : "",
        r.analyzed ? h("span", { class: "lin-count" }, n ? `넣는 곳 ${n}` : "넣는 곳 없음") : "");
      leafEls.set(key, [...(leafEls.get(key) ?? []), leaf]);
      head.append(leaf);
    }
    const el = h("li", {}, head);
    if (l.expr && l.kind !== "column") el.append(h("code", { class: "lin-expr" }, l.expr));
    if (l.reason) el.append(h("div", { class: "lin-reason" }, l.reason));
    if (l.from || l.filter) {
      el.append(h("div", { class: "lin-scope" },
        l.from ? h("span", {}, h("span", { class: "dim" }, "FROM "), l.from) : "",
        l.filter ? h("span", {}, h("span", { class: "dim" }, "  WHERE "), l.filter) : ""));
    }
    if (l.inputs?.length) el.append(h("ul", {}, ...l.inputs.map(nodeEl)));
    return el;
  };
  const tree = h("ul", { class: "lin-tree" }, nodeEl(r.lineage));

  const sources = r.writes.length ? r.writes.map(fullName) : [...leafEls.keys()];
  const summary = sources.length
    ? `${col.name} 은(는) 테이블 열 ${sources.length}개에서 나왔습니다: ${sources.join(", ")}`
    : `${col.name} 은(는) 테이블 열에서 오지 않았습니다 (식·상수이거나 찾지 못함)`;
  const first = sources.find((k) => writesOf.get(k)?.writes.length) ?? sources[0];
  const usageKey = () => picked || first;
  const usageBtn = h("button", { class: "small", title: "DB 사전으로 고른 테이블 열이 나오는 모든 소스·뷰를 찾는다 (읽는 곳 포함)", onclick: () => {
    const s = r.writes.find((w) => fullName(w) === usageKey()) ?? findSource(r.lineage, usageKey());
    if (s) hooks.showUsage(s.owner ? `"${s.owner}"."${s.table}"` : s.table, s.column);
  } }, "모든 사용처 (컬럼 영향도)");
  usageBtn.disabled = !sources.length;
  const copy = () => navigator.clipboard.writeText(lineageText(r.lineage).join("\n")).then(() => toast("계보를 복사했습니다", "info", 1500));
  body.replaceChildren(
    h("div", { class: "row" }, h("span", {}, summary), h("span", { class: "spacer" }), h("button", { class: "small", onclick: copy }, "계보 복사"), usageBtn),
    h("div", { class: "dim", style: "font-size:12px;margin:4px 0" }, `${secs}초 · 쿼리를 풀어 결과 열에서 테이블 열까지 단계마다 보입니다. 테이블 열을 누르면 아래에 그 열에 값을 넣는 곳이 나옵니다.`),
    h("div", { class: "lin-box" }, tree),
    sources.length ? wPane : "");
  if (first) pick(first);
  return shown;
}

function findSource(l: Lineage, key: string): SourceCol | undefined {
  if (l.source && fullName(l.source) === key) return l.source;
  for (const i of l.inputs ?? []) { const s = findSource(i, key); if (s) return s; }
  return undefined;
}

/** 고른 테이블 열에 값을 넣는 문장들 */
function renderWrites(pane: HTMLElement, key: string, cw: ColumnWrites | undefined, analyzed: boolean, hooks: SourceHooks) {
  const title = h("h4", { class: "colsrc-title" }, `${key} 에 값을 넣는 곳`);
  if (!analyzed) {
    pane.replaceChildren(title,
      h("p", {}, "이 접속의 분석 결과가 없어 값을 넣는 곳은 찾지 못했습니다."),
      h("p", { class: "dim", style: "font-size:12px" }, "분석 탭에서 PL/SQL(또는 코드 폴더)을 한 번 분석해 두면, 이 열에 INSERT·UPDATE·MERGE 하는 프로시저와 줄이 여기 나옵니다. 모델 없이 정적 분석만 해도 됩니다."),
      h("div", { class: "row" }, h("button", { class: "small", onclick: () => hooks.openAnalysis() }, "분석 화면 열기")));
    return;
  }
  const writes = [...(cw?.writes ?? [])].sort((a, b) => Number(a.how !== "set") - Number(b.how !== "set"));
  if (!writes.length) {
    pane.replaceChildren(title, h("p", { class: "hint" }, "분석한 소스에서는 이 열에 값을 넣는 문장을 찾지 못했습니다. 다른 시스템이 넣거나, 동적 SQL·트리거일 수 있습니다 — '모든 사용처' 로 더 넓게 찾아 보세요."));
    return;
  }
  const sure = writes.filter((w) => w.how === "set").length;
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
  const filter = h("input", { placeholder: "거르기 (예: UPDATE, 패키지 이름)", style: "width:240px" }) as HTMLInputElement;
  filter.addEventListener("input", () => grid.setFilter(filter.value));
  filter.addEventListener("keydown", (e) => { if (e.key === "ArrowDown") { e.preventDefault(); grid.el.focus(); } });
  pane.replaceChildren(
    h("div", { class: "row" }, title, h("span", { class: "dim" }, `문장 ${sure}개${writes.length > sure ? ` · 확인할 것 ${writes.length - sure}개` : ""} · 객체 ${new Set(writes.map((w) => w.node)).size}개`), h("span", { class: "spacer" }), filter),
    h("div", { class: "dim", style: "font-size:12px;margin:2px 0 6px" }, "저장된 분석 결과로 찾았습니다 (분석한 뒤 바뀐 소스, 동적 SQL, 트리거의 :NEW 대입은 나오지 않습니다). 더블클릭하면 소스를 그 줄로 엽니다."),
    h("div", { class: "usage-grid colsrc-grid" }, grid.el));
}
