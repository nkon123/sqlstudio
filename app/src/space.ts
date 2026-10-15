// 용량 — 테이블별 크기(테이블+인덱스+LOB), 테이블스페이스별 사용량. 둘 다 그리드로 (정렬·거르기·셀 복사).
// 조회는 읽기 전용 사전 조회 세션으로 하니 작업 탭을 막지 않는다.

import { invoke } from "@tauri-apps/api/core";
import { errOf, type Column } from "./api";
import { showMenu } from "./ctxmenu";
import { ResultGrid } from "./grid";
import { h, modal, toast } from "./ui";

interface SpaceGrid { columns: [string, boolean][]; rows: (string | null)[][]; note?: string | null }

type Tab = "tables" | "spaces";

const size = (mb: number) => (mb >= 1048576 ? `${(mb / 1048576).toFixed(2)} TB` : mb >= 1024 ? `${(mb / 1024).toFixed(2)} GB` : `${mb.toFixed(1)} MB`);

/** user: 접속 계정 (스키마 칸의 기본값) */
export function showSpace(id: number, user: string, owner?: string): Promise<unknown> {
  let tab: Tab = "tables";
  let data: SpaceGrid | null = null;
  const ownerIn = h("input", { value: owner ?? user, placeholder: "스키마", style: "width:140px", title: "스키마 — DBA 권한이 있으면 다른 스키마도 볼 수 있다" }) as HTMLInputElement;
  const nameIn = h("input", { placeholder: "이름 (LIKE, 예: ORD%)", style: "width:150px" }) as HTMLInputElement;
  const filterIn = h("input", { placeholder: "거르기 (결과 안에서)", style: "width:180px" }) as HTMLInputElement;
  const loadBtn = h("button", { class: "primary" }, "조회");
  const statusEl = h("span", { class: "dim", style: "font-size:12px" });
  const noteEl = h("div", { class: "hint", style: "margin:4px 0", hidden: true });
  const tabBtn = (t: Tab, label: string) => h("button", { class: "pane-tab", onclick: () => { tab = t; sync(); void load(); } }, label);
  const tabs = { tables: tabBtn("tables", "테이블별"), spaces: tabBtn("spaces", "테이블스페이스별") };

  const grid: ResultGrid = new ResultGrid({
    onStatus: () => summarize(),
    onDoubleClick: () => true,
    onContextMenu: (_r, _c, e) => {
      const n = grid.selectedRows().length;
      showMenu(e.clientX, e.clientY, [
        { label: n > 1 ? `고른 셀 복사 (${n}행)` : "고른 셀 복사", hint: "Ctrl+C", run: () => navigator.clipboard.writeText(grid.selectionText()).catch(() => {}) },
        { label: "머리 포함 복사", hint: "Ctrl+Shift+C", run: () => navigator.clipboard.writeText(grid.selectionText(true)).catch(() => {}) },
        { label: "이름 복사", run: () => navigator.clipboard.writeText(grid.selectedRows().map((r) => r[0] ?? "").join("\n")).catch(() => {}) },
      ]);
    },
  });

  /** 보이는 행의 합계 (거르면 거른 것만) */
  const summarize = () => {
    if (!data) return;
    const rows = grid.viewRows();
    const idx = data.columns.findIndex(([n]) => n === (tab === "tables" ? "합계(MB)" : "사용(MB)"));
    const sum = idx < 0 ? null : rows.reduce((a, r) => a + (Number(r[idx]) || 0), 0);
    const total = data.rows.length;
    statusEl.textContent = `${rows.length === total ? `${total}행` : `${rows.length}/${total}행`}${sum == null ? "" : ` · ${tab === "tables" ? "합계" : "사용"} ${size(sum)}`}${tab === "tables" && total >= 3000 ? " · 큰 것부터 3,000개만" : ""}`;
  };

  const sync = () => {
    for (const [k, b] of Object.entries(tabs)) b.classList.toggle("active", k === tab);
    ownerIn.hidden = nameIn.hidden = tab !== "tables";
  };

  const load = async () => {
    loadBtn.disabled = true;
    statusEl.textContent = "조회 중…";
    noteEl.hidden = true;
    try {
      const r = tab === "tables"
        ? await invoke<SpaceGrid>("table_sizes", { id, owner: ownerIn.value.trim() || null, nameLike: nameIn.value.trim() || null })
        : await invoke<SpaceGrid>("tablespace_usage", { id });
      data = r;
      const columns: Column[] = r.columns.map(([name, num]) => ({ name, type_name: num ? "NUMBER" : "VARCHAR2", nullable: true }));
      grid.setPage({ columns, rows: r.rows, has_more: false, fetched_total: r.rows.length });
      grid.setFilter(filterIn.value);
      noteEl.hidden = !r.note;
      noteEl.textContent = r.note ?? "";
      summarize();
    } catch (e) {
      statusEl.textContent = "";
      toast(errOf(e).message, "error", 7000);
    } finally {
      loadBtn.disabled = false;
    }
  };

  loadBtn.onclick = () => void load();
  for (const i of [ownerIn, nameIn]) i.addEventListener("keydown", (e) => { if (e.key === "Enter") void load(); });
  filterIn.addEventListener("input", () => { grid.setFilter(filterIn.value); summarize(); });
  filterIn.addEventListener("keydown", (e) => { if (e.key === "ArrowDown") { e.preventDefault(); grid.el.focus(); } });

  const body = h("div", { class: "space-view" },
    h("div", { class: "row", style: "display:flex;gap:6px;align-items:center;margin-bottom:6px;flex-wrap:wrap" },
      tabs.tables, tabs.spaces, h("span", { class: "sep" }), ownerIn, nameIn, loadBtn, filterIn, h("span", { style: "flex:1" }), statusEl),
    noteEl,
    h("div", { class: "usage-grid space-grid" }, grid.el));
  sync();
  const shown = modal("용량", body, [{ label: "닫기", value: true }], { wide: true });
  void load();
  return shown;
}
