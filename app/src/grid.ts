// 가상 스크롤 결과 그리드.
//
// 보이는 행만 DOM 으로 만든다. 10만 행이어도 DOM 노드는 화면에 보이는 30~50행뿐이라
// 스크롤이 끊기지 않는다. 아래 끝에 가까워지면 서버에서 다음 페이지를 가져온다 (Toad 방식).

import type { Column, RowPage } from "./api";

const ROW_H = 22;
const OVERSCAN = 8;
const MIN_W = 48;
const MAX_W = 420;

export interface GridOptions {
  /** 아래 끝 근처에서 불린다. 다음 페이지를 돌려주면 이어 붙인다. */
  onNeedMore?: () => Promise<RowPage | null>;
  onStatus?: (text: string) => void;
  /** 편집 중 바뀐 셀·지울 행 수가 달라질 때 */
  onDirty?: (changes: number) => void;
}

/** 편집 모드 설정: ROWID 열 위치와 고칠 수 있는 열 */
export interface EditSetup { rowidCol: number; editable: Set<number> }

export interface GridChanges {
  edits: { rowid: string; cells: { column: string; type_name: string; value: string | null }[] }[];
  deletes: string[];
}

const NUMERIC = /^(NUMBER|FLOAT|BINARY_|INTEGER)/i;

/** 필터 식: "값" (아무 열 포함) · "열=값" · "열~포함" · "열>값" / "열<값" · "열 is null" */
function compileFilter(text: string, cols: Column[]): ((r: (string | null)[]) => boolean) | null {
  const t = text.trim();
  if (!t) return null;
  const parts = t.split(/\s+AND\s+|\s*&&\s*/i).map((x) => x.trim()).filter(Boolean);
  const preds = parts.map((p) => {
    const m = p.match(/^([A-Za-z_][\w$#]*)\s*(=|!=|~|>=|<=|>|<|\s+is\s+(?:not\s+)?null)\s*(.*)$/i);
    const ci = m ? cols.findIndex((c) => c.name.toUpperCase() === m[1].toUpperCase()) : -1;
    if (!m || ci < 0) {
      const needle = p.toUpperCase();
      return (r: (string | null)[]) => r.some((v) => v != null && v.toUpperCase().includes(needle));
    }
    const op = m[2].trim().toUpperCase().replace(/\s+/g, " ");
    const want = m[3];
    const num = NUMERIC.test(cols[ci].type_name);
    const cmp = (v: string) => (num ? Number(v) - Number(want) : v.localeCompare(want));
    return (r: (string | null)[]) => {
      const v = r[ci];
      switch (op) {
        case "IS NULL": return v == null;
        case "IS NOT NULL": return v != null;
        case "=": return v != null && (num ? Number(v) === Number(want) : v === want);
        case "!=": return v == null || (num ? Number(v) !== Number(want) : v !== want);
        case "~": return v != null && v.toUpperCase().includes(want.toUpperCase());
        case ">": return v != null && cmp(v) > 0;
        case "<": return v != null && cmp(v) < 0;
        case ">=": return v != null && cmp(v) >= 0;
        case "<=": return v != null && cmp(v) <= 0;
      }
      return true;
    };
  });
  return (r) => preds.every((f) => f(r));
}

export class ResultGrid {
  readonly el: HTMLElement;
  private header: HTMLElement;
  private viewport: HTMLElement;
  private spacer: HTMLElement;
  private body: HTMLElement;
  private columns: Column[] = [];
  private rows: (string | null)[][] = [];
  private widths: number[] = [];
  private hasMore = false;
  private loading = false;
  private sel: { r0: number; c0: number; r1: number; c1: number } | null = null;
  private raf = 0;
  private measureCtx = document.createElement("canvas").getContext("2d")!;
  /** 화면 순서 → rows 인덱스 (정렬·필터) */
  private view: number[] = [];
  private sortCol = -1;
  private sortDir: 1 | -1 = 1;
  private filterText = "";
  private filter: ((r: (string | null)[]) => boolean) | null = null;
  private edit: EditSetup | null = null;
  /** rows 인덱스 → (열 → 새 값) */
  private dirty = new Map<number, Map<number, string | null>>();
  private deleted = new Set<number>();

  constructor(private opts: GridOptions = {}) {
    this.el = document.createElement("div");
    this.el.className = "grid";
    this.el.tabIndex = 0;
    this.header = div("grid-header");
    this.viewport = div("grid-viewport");
    this.spacer = div("grid-spacer");
    this.body = div("grid-body");
    this.viewport.append(this.spacer, this.body);
    this.el.append(this.header, this.viewport);
    this.viewport.addEventListener("scroll", () => this.schedule(), { passive: true });
    new ResizeObserver(() => this.schedule()).observe(this.viewport);
    this.el.addEventListener("keydown", (e) => this.onKey(e));
    this.body.addEventListener("mousedown", (e) => this.onMouseDown(e));
    // 더블클릭: 첫 클릭이 셀을 다시 그려 dblclick 이 사라지므로 mousedown 의 두 번째 클릭으로 받는다
    this.measureCtx.font = "12px ui-monospace, Consolas, monospace";
  }

  clear(message = "") {
    this.columns = [];
    this.rows = [];
    this.hasMore = false;
    this.sel = null;
    this.view = [];
    this.edit = null;
    this.dirty.clear();
    this.deleted.clear();
    this.header.replaceChildren();
    this.body.replaceChildren();
    this.spacer.style.height = "0px";
    if (message) {
      const m = div("grid-empty");
      m.textContent = message;
      this.body.append(m);
    }
  }

  setPage(page: RowPage) {
    this.columns = page.columns;
    this.rows = page.rows.slice();
    this.hasMore = page.has_more;
    this.sel = null;
    this.sortCol = -1;
    this.edit = null;
    this.dirty.clear();
    this.deleted.clear();
    // 같은 열이 있으면 필터를 이어 쓴다 (다시 실행했을 때)
    this.filter = compileFilter(this.filterText, this.columns);
    this.rebuildView();
    this.widths = this.measure();
    this.renderHeader();
    this.viewport.scrollTop = 0;
    this.viewport.scrollLeft = 0;
    this.update();
    this.status();
  }

  append(page: RowPage) {
    for (const r of page.rows) this.rows.push(r);
    this.hasMore = page.has_more;
    this.rebuildView();
    this.update();
    this.status();
  }

  rowCount() {
    return this.rows.length;
  }

  private status() {
    const shown = this.view.length !== this.rows.length ? `${this.view.length.toLocaleString()} / ` : "";
    const sorted = this.sortCol >= 0 && this.hasMore ? " · 가져온 행만 정렬됨 (전체는 ORDER BY)" : "";
    const edit = this.edit ? ` · 편집 중${this.changeCount() ? ` — 변경 ${this.changeCount()}` : ""}` : "";
    this.opts.onStatus?.(`${shown}${this.rows.length.toLocaleString()}행${this.hasMore ? " (더 있음 — 스크롤하면 가져옵니다)" : ""}${sorted}${edit}`);
  }

  // ── 정렬·필터 ─────────────────────────────────────

  private rebuildView() {
    let v = this.rows.map((_, i) => i);
    if (this.filter) v = v.filter((i) => this.filter!(this.rows[i]));
    if (this.sortCol >= 0) {
      const c = this.sortCol;
      const num = NUMERIC.test(this.columns[c]?.type_name ?? "");
      const d = this.sortDir;
      v.sort((a, b) => {
        const x = this.rows[a][c], y = this.rows[b][c];
        if (x == null && y == null) return a - b;
        if (x == null) return 1; // NULL 은 뒤로 (Oracle 의 오름차순과 같게)
        if (y == null) return -1;
        const r = num ? Number(x) - Number(y) : x < y ? -1 : x > y ? 1 : 0;
        return r * d || a - b;
      });
    }
    this.view = v;
  }

  /** 필터 적용 ("" 이면 해제) */
  setFilter(text: string) {
    this.filterText = text;
    this.filter = compileFilter(text, this.columns);
    this.sel = null;
    this.rebuildView();
    this.viewport.scrollTop = 0;
    this.update();
    this.status();
  }

  private toggleSort(c: number) {
    if (this.sortCol !== c) {
      this.sortCol = c;
      this.sortDir = 1;
    } else if (this.sortDir === 1) {
      this.sortDir = -1;
    } else {
      this.sortCol = -1;
    }
    this.sel = null;
    this.rebuildView();
    this.renderHeader();
    this.update();
    this.status();
  }

  // ── 편집 ─────────────────────────────────────────

  /** 편집 모드 켜기 (ROWID 를 붙여 다시 조회한 결과에서) */
  setEditable(e: EditSetup | null) {
    this.edit = e;
    this.dirty.clear();
    this.deleted.clear();
    this.renderHeader();
    this.update();
    this.status();
  }

  isEditing() {
    return this.edit != null;
  }

  changeCount() {
    let n = this.deleted.size;
    for (const [r, m] of this.dirty) if (!this.deleted.has(r)) n += m.size;
    return n;
  }

  private notifyDirty() {
    this.opts.onDirty?.(this.changeCount());
    this.status();
  }

  /** 바뀐 것 (ROWID 기준) */
  changes(): GridChanges {
    const e = this.edit;
    if (!e) return { edits: [], deletes: [] };
    const edits: GridChanges["edits"] = [];
    for (const [r, m] of this.dirty) {
      if (this.deleted.has(r) || !m.size) continue;
      edits.push({
        rowid: this.rows[r][e.rowidCol] ?? "",
        cells: [...m].map(([c, v]) => ({ column: this.columns[c].name, type_name: this.columns[c].type_name, value: v })),
      });
    }
    const deletes = [...this.deleted].map((r) => this.rows[r][e.rowidCol] ?? "");
    return { edits, deletes };
  }

  /** 적용이 끝났다 — 바뀐 값을 결과에 반영하고 지운 행을 뺀다 */
  commitChanges() {
    for (const [r, m] of this.dirty) for (const [c, v] of m) this.rows[r][c] = v;
    const del = this.deleted;
    if (del.size) this.rows = this.rows.filter((_, i) => !del.has(i));
    this.dirty.clear();
    this.deleted.clear();
    this.rebuildView();
    this.update();
    this.notifyDirty();
  }

  discardChanges() {
    this.dirty.clear();
    this.deleted.clear();
    this.update();
    this.notifyDirty();
  }

  /** 선택한 행들을 지울 행으로 표시 (다시 하면 풀린다) */
  toggleDeleteSelected() {
    if (!this.edit || !this.sel) return;
    const [a, b] = [Math.min(this.sel.r0, this.sel.r1), Math.max(this.sel.r0, this.sel.r1)];
    const rs = this.view.slice(a, b + 1);
    const all = rs.every((r) => this.deleted.has(r));
    for (const r of rs) (all ? this.deleted.delete(r) : this.deleted.add(r));
    this.update();
    this.notifyDirty();
  }

  private value(r: number, c: number): string | null {
    const m = this.dirty.get(r);
    return m && m.has(c) ? m.get(c)! : this.rows[r][c];
  }

  private startEdit(viewRow: number, c: number, cellEl: HTMLElement) {
    const e = this.edit;
    if (!e || !e.editable.has(c)) return false;
    const r = this.view[viewRow];
    if (this.deleted.has(r)) return true;
    const cur = this.value(r, c);
    const input = document.createElement("input");
    input.className = "grid-input";
    input.value = cur ?? "";
    input.placeholder = "비우면 NULL";
    input.style.width = cellEl.style.width;
    cellEl.replaceChildren(input);
    input.focus();
    input.select();
    let done = false;
    const finish = (save: boolean) => {
      if (done) return;
      done = true;
      if (save) {
        // Oracle 에서 빈 문자열은 NULL 이다
        const nv = input.value === "" ? null : input.value;
        const orig = this.rows[r][c];
        const m = this.dirty.get(r) ?? new Map<number, string | null>();
        if (nv === orig) m.delete(c);
        else m.set(c, nv);
        if (m.size) this.dirty.set(r, m);
        else this.dirty.delete(r);
        this.notifyDirty();
      }
      this.update();
      this.el.focus();
    };
    input.addEventListener("keydown", (ev) => {
      ev.stopPropagation();
      if (ev.key === "Enter") finish(true);
      else if (ev.key === "Escape") finish(false);
      else if (ev.key === "Tab") {
        ev.preventDefault();
        finish(true);
        // 오른쪽 편집 가능한 칸으로
        const next = [...e.editable].filter((x) => x > c).sort((a, b) => a - b)[0];
        if (next != null) {
          const el = this.body.querySelector(`.grid-cell[data-r="${viewRow}"][data-c="${next}"]`) as HTMLElement | null;
          if (el) this.startEdit(viewRow, next, el);
        }
      }
    });
    input.addEventListener("blur", () => finish(true));
    return true;
  }

  /** 열 너비: 헤더와 앞쪽 200행의 글자 폭으로 정한다 */
  private measure(): number[] {
    const sample = this.rows.slice(0, 200);
    return this.columns.map((c, i) => {
      let w = this.measureCtx.measureText(c.name).width + 28;
      for (const r of sample) {
        const v = r[i];
        const t = v == null ? "(null)" : v.length > 80 ? v.slice(0, 80) : v;
        w = Math.max(w, this.measureCtx.measureText(t).width + 16);
      }
      return Math.round(Math.min(MAX_W, Math.max(MIN_W, w)));
    });
  }

  private hiddenCol(i: number) {
    return this.edit?.rowidCol === i;
  }

  private totalWidth() {
    return 56 + this.widths.reduce((a, b, i) => a + (this.hiddenCol(i) ? 0 : b), 0);
  }

  private renderHeader() {
    const row = div("grid-row grid-head");
    row.style.width = `${this.totalWidth()}px`;
    const num = div("grid-cell grid-num");
    num.textContent = "#";
    row.append(num);
    this.columns.forEach((c, i) => {
      const cell = div("grid-cell");
      cell.style.width = `${this.hiddenCol(i) ? 0 : this.widths[i]}px`;
      if (this.hiddenCol(i)) cell.style.display = "none";
      const arrow = this.sortCol === i ? (this.sortDir === 1 ? " ▲" : " ▼") : "";
      cell.textContent = c.name + arrow;
      if (this.edit && !this.edit.editable.has(i)) cell.classList.add("ro");
      cell.title = `${c.name}  ${c.type_name}${c.nullable ? "" : " NOT NULL"} — 눌러서 정렬${this.edit ? (this.edit.editable.has(i) ? " · 더블클릭으로 고치기" : " · 고칠 수 없는 열") : ""}`;
      cell.addEventListener("click", (e) => {
        if ((e.target as HTMLElement).classList.contains("grid-grip")) return;
        this.toggleSort(i);
      });
      const grip = div("grid-grip");
      grip.addEventListener("mousedown", (e) => this.startResize(e, i));
      cell.append(grip);
      row.append(cell);
    });
    this.header.replaceChildren(row);
  }

  private startResize(e: MouseEvent, i: number) {
    e.preventDefault();
    e.stopPropagation();
    const x0 = e.clientX;
    const w0 = this.widths[i];
    const move = (ev: MouseEvent) => {
      this.widths[i] = Math.max(MIN_W, w0 + ev.clientX - x0);
      this.renderHeader();
      this.update();
    };
    const up = () => {
      window.removeEventListener("mousemove", move);
      window.removeEventListener("mouseup", up);
    };
    window.addEventListener("mousemove", move);
    window.addEventListener("mouseup", up);
  }

  private schedule() {
    if (!this.raf) this.raf = requestAnimationFrame(() => { this.raf = 0; this.update(); });
  }

  private update() {
    // 헤더는 가로 스크롤만 따라간다
    this.header.scrollLeft = this.viewport.scrollLeft;
    const n = this.view.length;
    this.spacer.style.height = `${n * ROW_H}px`;
    this.spacer.style.width = `${this.totalWidth()}px`;
    if (!this.columns.length) return;
    const top = this.viewport.scrollTop;
    const h = this.viewport.clientHeight;
    const first = Math.max(0, Math.floor(top / ROW_H) - OVERSCAN);
    const last = Math.min(n, Math.ceil((top + h) / ROW_H) + OVERSCAN);
    const frag = document.createDocumentFragment();
    const tw = `${this.totalWidth()}px`;
    for (let r = first; r < last; r++) {
      const row = div("grid-row");
      row.style.transform = `translateY(${r * ROW_H}px)`;
      row.style.width = tw;
      if (r % 2) row.classList.add("odd");
      const ri = this.view[r];
      if (this.deleted.has(ri)) row.classList.add("deleted");
      const num = div("grid-cell grid-num");
      num.textContent = String(r + 1);
      row.append(num);
      const changed = this.dirty.get(ri);
      for (let c = 0; c < this.columns.length; c++) {
        if (this.hiddenCol(c)) continue;
        const cell = div("grid-cell");
        cell.style.width = `${this.widths[c]}px`;
        if (changed?.has(c)) cell.classList.add("dirty");
        const v = this.value(ri, c);
        if (v == null) {
          cell.textContent = "(null)";
          cell.classList.add("null");
        } else {
          cell.textContent = v.length > 500 ? v.slice(0, 500) + "…" : v;
        }
        if (this.inSel(r, c)) cell.classList.add("sel");
        cell.dataset.r = String(r);
        cell.dataset.c = String(c);
        row.append(cell);
      }
      frag.append(row);
    }
    this.body.replaceChildren(frag);
    // 끝에서 두 화면 안쪽이면 다음 페이지
    if (this.hasMore && !this.loading && top + h > n * ROW_H - h * 2) this.loadMore();
  }

  private async loadMore() {
    if (!this.opts.onNeedMore) return;
    this.loading = true;
    this.opts.onStatus?.(`${this.rows.length.toLocaleString()}행 — 더 가져오는 중…`);
    try {
      const page = await this.opts.onNeedMore();
      if (page) this.append(page);
      else this.hasMore = false;
    } catch (e) {
      this.hasMore = false;
      this.opts.onStatus?.(`더 가져오기 실패: ${(e as { message?: string }).message ?? e}`);
    } finally {
      this.loading = false;
    }
  }

  // ── 선택과 복사 ─────────────────────────────────────

  private inSel(r: number, c: number) {
    const s = this.sel;
    if (!s) return false;
    return r >= Math.min(s.r0, s.r1) && r <= Math.max(s.r0, s.r1) && c >= Math.min(s.c0, s.c1) && c <= Math.max(s.c0, s.c1);
  }

  private cellAt(e: MouseEvent): { r: number; c: number } | null {
    const t = (e.target as HTMLElement).closest(".grid-cell") as HTMLElement | null;
    if (!t || t.dataset.r == null) return null;
    return { r: Number(t.dataset.r), c: Number(t.dataset.c) };
  }

  private onMouseDown(e: MouseEvent) {
    const at = this.cellAt(e);
    if (!at) return;
    if (e.detail >= 2) {
      e.preventDefault();
      this.onDblClick(e);
      return;
    }
    this.el.focus();
    if (e.shiftKey && this.sel) {
      this.sel.r1 = at.r;
      this.sel.c1 = at.c;
    } else {
      this.sel = { r0: at.r, c0: at.c, r1: at.r, c1: at.c };
    }
    this.update();
    const move = (ev: MouseEvent) => {
      const p = this.cellAt(ev);
      if (p && this.sel && (p.r !== this.sel.r1 || p.c !== this.sel.c1)) {
        this.sel.r1 = p.r;
        this.sel.c1 = p.c;
        this.update();
      }
    };
    const up = () => {
      this.body.removeEventListener("mousemove", move);
      window.removeEventListener("mouseup", up);
    };
    this.body.addEventListener("mousemove", move);
    window.addEventListener("mouseup", up);
  }

  private onDblClick(e: MouseEvent) {
    const at = this.cellAt(e);
    if (!at) return;
    const cellEl = (e.target as HTMLElement).closest(".grid-cell") as HTMLElement;
    if (this.edit && this.startEdit(at.r, at.c, cellEl)) return;
    const v = this.value(this.view[at.r], at.c);
    // 긴 값(CLOB 등)은 창으로 본다
    const dlg = document.createElement("dialog");
    dlg.className = "cell-viewer";
    const pre = document.createElement("textarea");
    pre.readOnly = true;
    pre.value = v ?? "(null)";
    const close = document.createElement("button");
    close.textContent = "닫기";
    close.onclick = () => dlg.close();
    dlg.append(heading(`${this.columns[at.c].name} — ${at.r + 1}행`), pre, close);
    dlg.addEventListener("close", () => dlg.remove());
    document.body.append(dlg);
    dlg.showModal();
  }

  private onKey(e: KeyboardEvent) {
    if ((e.ctrlKey || e.metaKey) && e.key === "c") {
      e.preventDefault();
      navigator.clipboard.writeText(this.selectionText(e.shiftKey)).catch(() => {});
    } else if ((e.ctrlKey || e.metaKey) && e.key === "a") {
      e.preventDefault();
      if (this.view.length) this.sel = { r0: 0, c0: 0, r1: this.view.length - 1, c1: this.columns.length - 1 };
      this.update();
    } else if (this.edit && (e.key === "Delete" || (e.ctrlKey && e.key === "Delete"))) {
      e.preventDefault();
      this.toggleDeleteSelected();
    } else if (this.edit && (e.key === "F2" || e.key === "Enter") && this.sel) {
      e.preventDefault();
      const el = this.body.querySelector(`.grid-cell[data-r="${this.sel.r1}"][data-c="${this.sel.c1}"]`) as HTMLElement | null;
      if (el) this.startEdit(this.sel.r1, this.sel.c1, el);
    }
  }

  /** 선택 영역을 TSV 로 (엑셀에 바로 붙는다). Ctrl+Shift+C 는 헤더 포함. */
  selectionText(withHeader = false): string {
    const s = this.sel;
    if (!s) return "";
    const [r0, r1] = [Math.min(s.r0, s.r1), Math.max(s.r0, s.r1)];
    const [c0, c1] = [Math.min(s.c0, s.c1), Math.max(s.c0, s.c1)];
    const esc = (v: string | null) => {
      if (v == null) return "";
      return /[\t\n"]/.test(v) ? `"${v.replace(/"/g, '""')}"` : v;
    };
    const lines: string[] = [];
    const cols: number[] = [];
    for (let c = c0; c <= c1; c++) if (!this.hiddenCol(c)) cols.push(c);
    if (withHeader) lines.push(cols.map((c) => this.columns[c].name).join("\t"));
    for (let r = r0; r <= r1; r++) lines.push(cols.map((c) => esc(this.value(this.view[r], c))).join("\t"));
    return lines.join("\n");
  }

  /** 전체 결과를 CSV 로 (가져온 행까지) */
  toCsv(): string {
    const esc = (v: string | null) => (v == null ? "" : /[,\n"]/.test(v) ? `"${v.replace(/"/g, '""')}"` : v);
    const cols = this.columns.map((_, i) => i).filter((i) => !this.hiddenCol(i));
    const out = [cols.map((c) => esc(this.columns[c].name)).join(",")];
    for (const r of this.view) out.push(cols.map((c) => esc(this.rows[r][c])).join(","));
    return out.join("\r\n");
  }
}

function div(cls: string) {
  const d = document.createElement("div");
  d.className = cls;
  return d;
}

function heading(t: string) {
  const h = document.createElement("h3");
  h.textContent = t;
  return h;
}
