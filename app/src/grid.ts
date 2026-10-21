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
  /** 화면에 보이지 않는 열 (행을 찾는 키 등) */
  hiddenCols?: number[];
  /** 셀을 눌렀을 때 (선택한 뒤). 행 값과 열 번호 */
  onCellClick?: (row: (string | null)[], col: number, e: MouseEvent) => void;
  /** 우클릭. 선택 밖을 누르면 그 셀을 고른 뒤 부른다 */
  onContextMenu?: (row: (string | null)[], col: number, e: MouseEvent) => void;
  /** 더블클릭. 참을 돌려주면 값 보기 창을 띄우지 않는다 */
  onDoubleClick?: (row: (string | null)[], col: number) => boolean;
  /** 행·셀에 덧붙일 CSS 클래스 (결과 비교 등) — 없으면 그리는 데 아무것도 더하지 않는다 */
  rowClass?: (row: (string | null)[]) => string | null;
  cellClass?: (row: (string | null)[], col: number) => string | null;
  /** 그리드가 키를 받기 전에. 참을 돌려주면 그리드는 그 키를 쓰지 않는다 */
  onKeyDown?: (e: KeyboardEvent) => boolean;
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

const NAV_KEYS = new Set(["ArrowUp", "ArrowDown", "ArrowLeft", "ArrowRight", "PageUp", "PageDown", "Home", "End", " "]);

/** 그리드가 쓰는 이동 키 이름. 한글 등 입력기가 켜져 있으면 key 가 "Process" 로 오기도 해 물리 키(code)로 대신한다 */
export function navKeyName(e: Pick<KeyboardEvent, "key" | "code">): string {
  if (NAV_KEYS.has(e.key)) return e.key;
  if (e.code === "Space") return " ";
  return NAV_KEYS.has(e.code) ? e.code : e.key;
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
  /** 컬럼 이름 → 설명(COMMENT) */
  private notes: Record<string, string> = {};
  /** 머리에 이름 대신 설명을 */
  private showNotes = false;
  /** Ctrl+클릭으로 더한 셀들 (화면 행 * 10000 + 열) — 사각형(sel) 과 합쳐서 선택이 된다 */
  private picked = new Set<number>();
  /** 키보드 커서 (화면 행·열) — 방향키가 여기서 움직인다. Ctrl+방향키는 선택을 두고 이것만 옮긴다 (Space 로 더하기) */
  private cur: { r: number; c: number } | null = null;
  /** 고른 행만 보기(keep) / 고른 행 빼고 보기 — 화면에서만 거른다. 값은 rows 인덱스 */
  private rowMask: { keep: boolean; ids: Set<number> } | null = null;
  /** 머리 끝의 빈칸 — 본문의 세로 스크롤바 폭만큼. 없으면 오른쪽 끝에서 머리가 그만큼 덜 스크롤되어 열과 어긋난다 */
  private filler = div("grid-filler");
  private fillerW = -1;

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
    this.body.addEventListener("contextmenu", (e) => {
      const at = this.cellAt(e);
      if (!at || !this.opts.onContextMenu) return;
      e.preventDefault();
      if (!this.inSel(at.r, at.c)) {
        this.sel = { r0: at.r, c0: at.c, r1: at.r, c1: at.c };
        this.picked.clear();
        this.update();
      }
      this.opts.onContextMenu(this.rows[this.view[at.r]], at.c, e);
    });
    // 더블클릭: 첫 클릭이 셀을 다시 그려 dblclick 이 사라지므로 mousedown 의 두 번째 클릭으로 받는다
    this.measureCtx.font = "12px ui-monospace, Consolas, monospace";
  }

  clear(message = "") {
    this.columns = [];
    this.rows = [];
    this.hasMore = false;
    this.sel = null;
    this.cur = null;
    this.picked.clear();
    this.rowMask = null;
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
    this.cur = null;
    this.picked.clear();
    this.rowMask = null;
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

  /** 행만 바꾼다 — 정렬·필터·선택·스크롤은 그대로 (목록 상태가 바뀌었을 때) */
  replaceRows(rows: (string | null)[][]) {
    this.rows = rows.slice();
    this.rebuildView();
    this.picked.clear();
    if (this.sel) {
      const max = this.view.length - 1;
      if (max < 0) this.sel = null;
      else {
        this.sel.r0 = Math.min(this.sel.r0, max);
        this.sel.r1 = Math.min(this.sel.r1, max);
      }
    }
    this.update();
    this.status();
  }

  /** 지금 보이는 행 전부 (필터·정렬 반영, 화면 순서) */
  viewRows(): (string | null)[][] {
    return this.view.map((i) => this.rows[i]);
  }

  /** 선택 영역에 걸친 행들 (화면 순서) */
  selectedRows(): (string | null)[][] {
    return this.selRowIdx().map((r) => this.rows[this.view[r]]);
  }

  /** 컬럼 설명 — 머리 풍선에, '설명' 을 켜면 머리 글자로 */
  setNotes(notes: Record<string, string>) {
    this.notes = notes;
    this.renderHeader();
  }

  setShowNotes(on: boolean) {
    this.showNotes = on;
    this.renderHeader();
  }

  hasNotes() {
    return this.columns.some((c) => this.notes[c.name]);
  }

  append(page: RowPage) {
    for (const r of page.rows) this.rows.push(r);
    this.hasMore = page.has_more;
    // 정렬 중이면 새 행이 사이사이에 끼어 화면 행 번호가 바뀐다 — 고른 셀이 엉뚱한 행을 가리키지 않게 푼다
    if (this.sortCol >= 0) { this.sel = null; this.picked.clear(); }
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
    const mask = this.rowMask ? (this.rowMask.keep ? " · 고른 행만 보는 중" : ` · 고른 행 ${this.rowMask.ids.size}개 뺀 것을 보는 중`) : "";
    const n = this.selectionCount();
    const pick = n > 1 ? ` · ${n.toLocaleString()}개 선택` : "";
    this.opts.onStatus?.(`${shown}${this.rows.length.toLocaleString()}행${this.hasMore ? " · 더 있음 (스크롤하면 가져옴)" : ""}${sorted}${edit}${mask}${pick}`);
  }

  // ── 정렬·필터 ─────────────────────────────────────

  private rebuildView() {
    let v = this.rows.map((_, i) => i);
    if (this.filter) v = v.filter((i) => this.filter!(this.rows[i]));
    if (this.rowMask) {
      const m = this.rowMask;
      v = v.filter((i) => m.ids.has(i) === m.keep);
    }
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
    this.picked.clear();
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
    this.picked.clear();
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
    if (!this.edit) return;
    // 사각형 선택과 Ctrl+클릭으로 고른 셀의 행 모두
    const rs = this.selRowIdx().map((r) => this.view[r]);
    if (!rs.length) return;
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
    return this.edit?.rowidCol === i || !!this.opts.hiddenCols?.includes(i);
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
      const note = this.notes[c.name];
      cell.textContent = (this.showNotes && note ? note : c.name) + arrow;
      if (note) cell.classList.add("noted");
      if (this.edit && !this.edit.editable.has(i)) cell.classList.add("ro");
      cell.title = `${c.name}${note ? ` (${note})` : ""}  ${c.type_name}${c.nullable ? "" : " NOT NULL"} — 눌러서 정렬${this.edit ? (this.edit.editable.has(i) ? " · 더블클릭으로 고치기" : " · 고칠 수 없는 열") : ""}`;
      cell.addEventListener("click", (e) => {
        if ((e.target as HTMLElement).classList.contains("grid-grip")) return;
        this.toggleSort(i);
      });
      const grip = div("grid-grip");
      grip.addEventListener("mousedown", (e) => this.startResize(e, i));
      cell.append(grip);
      row.append(cell);
    });
    row.append(this.filler);
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
    // 헤더는 가로 스크롤만 따라간다 — 본문의 세로 스크롤바 폭만큼 머리 끝을 늘려 끝까지 같이 가게
    const sbw = this.viewport.offsetWidth - this.viewport.clientWidth;
    if (sbw !== this.fillerW) {
      this.fillerW = sbw;
      this.filler.style.width = `${sbw}px`;
    }
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
    const { rowClass, cellClass } = this.opts;
    for (let r = first; r < last; r++) {
      const row = div("grid-row");
      row.style.transform = `translateY(${r * ROW_H}px)`;
      row.style.width = tw;
      if (r % 2) row.classList.add("odd");
      const ri = this.view[r];
      if (this.deleted.has(ri)) row.classList.add("deleted");
      const rc = rowClass?.(this.rows[ri]);
      if (rc) row.classList.add(rc);
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
        if (cellClass) { const cc = cellClass(this.rows[ri], c); if (cc) cell.classList.add(cc); }
        if (this.inSel(r, c)) cell.classList.add("sel");
        if (this.cur && this.cur.r === r && this.cur.c === c) cell.classList.add("cur");
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
    if (this.picked.has(r * 10000 + c)) return true;
    const s = this.sel;
    if (!s) return false;
    return r >= Math.min(s.r0, s.r1) && r <= Math.max(s.r0, s.r1) && c >= Math.min(s.c0, s.c1) && c <= Math.max(s.c0, s.c1);
  }

  /** 고른 셀 수 (사각형 + Ctrl 로 더한 셀 — 겹치면 두 번 셀 수 있다) */
  selectionCount(): number {
    const s = this.sel;
    let n = this.picked.size;
    if (s) {
      let cols = 0;
      for (let c = Math.min(s.c0, s.c1); c <= Math.max(s.c0, s.c1); c++) if (!this.hiddenCol(c)) cols++;
      n += (Math.abs(s.r1 - s.r0) + 1) * cols;
    }
    return n;
  }

  /** 고른 셀 전부 (행 순서 → 열 순서) — 숨긴 열은 뺀다 */
  private selCells(): { r: number; c: number }[] {
    const keys = new Set<number>(this.picked);
    const s = this.sel;
    if (s) {
      for (let r = Math.min(s.r0, s.r1); r <= Math.max(s.r0, s.r1); r++) {
        for (let c = Math.min(s.c0, s.c1); c <= Math.max(s.c0, s.c1); c++) if (!this.hiddenCol(c)) keys.add(r * 10000 + c);
      }
    }
    return [...keys].sort((a, b) => a - b).map((k) => ({ r: Math.floor(k / 10000), c: k % 10000 }));
  }

  /** 고른 셀이 걸친 화면 행 번호 (오름차순, 중복 없음) */
  private selRowIdx(): number[] {
    const set = new Set<number>();
    const s = this.sel;
    if (s) for (let r = Math.min(s.r0, s.r1); r <= Math.max(s.r0, s.r1); r++) set.add(r);
    for (const k of this.picked) set.add(Math.floor(k / 10000));
    return [...set].sort((a, b) => a - b);
  }

  /** 고른 셀의 값 — 한 열에서만 골랐으면 그 열과 값들(행 순서). `max` 개를 넘으면 값을 읽지 않고 알린다 */
  selectedValues(max: number): { kind: "none" } | { kind: "many"; count: number } | { kind: "multi"; count: number } | { kind: "col"; col: number; column: Column; values: (string | null)[] } {
    const n = this.selectionCount();
    if (n === 0) return { kind: "none" };
    if (n > max) return { kind: "many", count: n };
    const cells = this.selCells();
    const col = cells[0]?.c;
    if (col == null) return { kind: "none" };
    if (cells.some((x) => x.c !== col)) return { kind: "multi", count: cells.length };
    return { kind: "col", col, column: this.columns[col], values: cells.map((x) => this.value(this.view[x.r], col)) };
  }

  /** 고른 행만 보기(keep=true) / 고른 행을 뺀 나머지 보기 — 화면에서만 거른다 (서버 쿼리는 그대로) */
  maskSelectedRows(keep: boolean) {
    const idx = this.selRowIdx();
    if (!idx.length) return;
    const ids = new Set(idx.map((r) => this.view[r]));
    const cur = this.rowMask;
    if (keep) this.rowMask = { keep: true, ids };
    else if (cur?.keep) this.rowMask = { keep: true, ids: new Set([...cur.ids].filter((i) => !ids.has(i))) };
    else this.rowMask = { keep: false, ids: new Set([...(cur?.ids ?? []), ...ids]) };
    this.sel = null;
    this.picked.clear();
    this.rebuildView();
    this.viewport.scrollTop = 0;
    this.update();
    this.status();
  }

  hasRowMask(): boolean {
    return this.rowMask != null;
  }

  clearRowMask() {
    if (!this.rowMask) return;
    this.rowMask = null;
    this.sel = null;
    this.picked.clear();
    this.rebuildView();
    this.update();
    this.status();
  }

  private cellAt(e: MouseEvent): { r: number; c: number } | null {
    const t = (e.target as HTMLElement).closest(".grid-cell") as HTMLElement | null;
    if (!t || t.dataset.r == null) return null;
    return { r: Number(t.dataset.r), c: Number(t.dataset.c) };
  }

  private onMouseDown(e: MouseEvent) {
    const at = this.cellAt(e);
    if (!at) return;
    // 고치는 중인 셀의 입력 칸은 브라우저 기본 동작대로 (커서 놓기·글자 고르기)
    if ((e.target as HTMLElement).closest("input, textarea")) return;
    // 기본 동작(포커스 옮기기·글자 드래그 선택)을 막는다 — 아래 update() 가 누른 셀을 다시 그려 문서에서 빠지면
    // 브라우저가 포커스를 그리드에서 빼 방향키가 안 먹고, 드래그하면 글자 선택이 셀 선택과 겹쳐 보였다
    e.preventDefault();
    if (e.detail >= 2) {
      this.onDblClick(e);
      return;
    }
    this.el.focus();
    // 이미 고른 셀 위의 오른쪽 클릭은 선택을 그대로 둔다 (고른 값으로 메뉴를 열려고)
    if (e.button === 2 && this.inSel(at.r, at.c)) return;
    this.cur = { ...at };
    if ((e.ctrlKey || e.metaKey) && e.button === 0) {
      // Ctrl+클릭: 셀을 하나씩 더하거나 뺀다 (떨어진 셀·행도). 지금 사각형은 그 셀들로 바꿔 둔다
      for (const x of this.selCells()) this.picked.add(x.r * 10000 + x.c);
      this.sel = null;
      const k = at.r * 10000 + at.c;
      if (this.picked.has(k)) this.picked.delete(k);
      else this.picked.add(k);
      this.update();
      this.status();
      return;
    }
    this.picked.clear();
    if (e.shiftKey && this.sel) {
      this.sel.r1 = at.r;
      this.sel.c1 = at.c;
    } else {
      this.sel = { r0: at.r, c0: at.c, r1: at.r, c1: at.c };
    }
    this.update();
    this.status();
    if (e.button === 0 && !e.shiftKey) this.opts.onCellClick?.(this.rows[this.view[at.r]], at.c, e);
    const move = (ev: MouseEvent) => {
      const p = this.cellAt(ev);
      if (p && this.sel && (p.r !== this.sel.r1 || p.c !== this.sel.c1)) {
        this.sel.r1 = p.r;
        this.sel.c1 = p.c;
        this.cur = { ...p };
        this.update();
        this.status();
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
    if (this.opts.onDoubleClick?.(this.rows[this.view[at.r]], at.c)) return;
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
    if (this.opts.onKeyDown?.(e)) return;
    if (this.navKey(e)) return;
    // 물리 키(code)로 비교 — Shift 를 누르면 key 가 "C" 가 되고, 한글 입력 상태에서는 "ㅊ" 이 된다
    if ((e.ctrlKey || e.metaKey) && e.code === "KeyC") {
      e.preventDefault();
      navigator.clipboard.writeText(this.selectionText(e.shiftKey)).catch(() => {});
    } else if ((e.ctrlKey || e.metaKey) && e.code === "KeyA") {
      e.preventDefault();
      this.picked.clear();
      if (this.view.length) this.sel = { r0: 0, c0: 0, r1: this.view.length - 1, c1: this.columns.length - 1 };
      this.update();
      this.status();
    } else if (this.edit && (e.key === "Delete" || (e.ctrlKey && e.key === "Delete"))) {
      e.preventDefault();
      this.toggleDeleteSelected();
    } else if (this.edit && (e.key === "F2" || e.key === "Enter") && this.sel) {
      e.preventDefault();
      const el = this.body.querySelector(`.grid-cell[data-r="${this.sel.r1}"][data-c="${this.sel.c1}"]`) as HTMLElement | null;
      if (el) this.startEdit(this.sel.r1, this.sel.c1, el);
    }
  }

  // ── 키보드 이동 ─────────────────────────────────────

  /** 방향키·PgUp/PgDn·Home/End·Space. 쓴 키면 참.
   *  방향키: 그 셀 하나를 고른다 · Shift+: 넓힌다 · Ctrl+: 커서만 옮긴다 (고른 것은 그대로) · Space: 커서 셀을 더하거나 뺀다 (Ctrl+클릭) */
  private navKey(e: KeyboardEvent): boolean {
    if (e.altKey || !this.view.length || !this.columns.length) return false;
    // 한글 입력 상태에서는 key 가 "Process" 로 올 수 있다 — 물리 키(code)로 알아본다
    const key = navKeyName(e);
    const cols = this.columns.map((_, i) => i).filter((i) => !this.hiddenCol(i));
    if (!cols.length) return false;
    const last = this.view.length - 1;
    const page = Math.max(1, Math.floor(this.viewport.clientHeight / ROW_H) - 1);
    if (key === " " && !e.ctrlKey && !e.shiftKey) {
      if (!this.cur) return false;
      e.preventDefault();
      // 사각형은 낱낱의 셀로 바꿔 두고 커서 셀을 뒤집는다
      for (const x of this.selCells()) this.picked.add(x.r * 10000 + x.c);
      this.sel = null;
      const k = this.cur.r * 10000 + this.cur.c;
      if (this.picked.has(k)) this.picked.delete(k);
      else this.picked.add(k);
      this.update();
      this.status();
      return true;
    }
    const from = this.cur ?? (this.sel ? { r: this.sel.r1, c: this.sel.c1 } : null);
    let r = Math.min(from?.r ?? 0, last);
    let ci = Math.max(0, cols.indexOf(from?.c ?? cols[0]));
    switch (key) {
      case "ArrowUp": r = Math.max(0, r - 1); break;
      case "ArrowDown": r = Math.min(last, r + 1); break;
      case "ArrowLeft": ci = Math.max(0, ci - 1); break;
      case "ArrowRight": ci = Math.min(cols.length - 1, ci + 1); break;
      case "PageUp": r = Math.max(0, r - page); break;
      case "PageDown": r = Math.min(last, r + page); break;
      case "Home": if (e.ctrlKey) r = 0; else ci = 0; break;
      case "End": if (e.ctrlKey) r = last; else ci = cols.length - 1; break;
      default: return false;
    }
    e.preventDefault();
    // 처음 누른 키는 커서를 첫 셀에 놓기만 한다
    if (!from) { r = 0; ci = 0; }
    const c = cols[ci];
    this.cur = { r, c };
    const moveOnly = e.ctrlKey && key !== "Home" && key !== "End";
    if (e.shiftKey) {
      if (!this.sel) this.sel = { r0: from?.r ?? r, c0: from?.c ?? c, r1: r, c1: c };
      this.sel.r1 = r;
      this.sel.c1 = c;
    } else if (!moveOnly) {
      this.picked.clear();
      this.sel = { r0: r, c0: c, r1: r, c1: c };
    }
    this.reveal(r, c);
    this.update();
    this.status();
    return true;
  }

  /** 그 셀이 보이게 스크롤 */
  private reveal(r: number, c: number) {
    const vp = this.viewport;
    const top = r * ROW_H;
    if (top < vp.scrollTop) vp.scrollTop = top;
    else if (top + ROW_H > vp.scrollTop + vp.clientHeight) vp.scrollTop = top + ROW_H - vp.clientHeight;
    const NUM_W = 56;
    let left = NUM_W;
    for (let i = 0; i < c; i++) if (!this.hiddenCol(i)) left += this.widths[i];
    const right = left + this.widths[c];
    if (left - NUM_W < vp.scrollLeft) vp.scrollLeft = left - NUM_W;
    else if (right > vp.scrollLeft + vp.clientWidth) vp.scrollLeft = right - vp.clientWidth;
  }

  /** 화면 행 `r` 의 셀 하나를 고르고 커서를 둔다 (거르기 칸에서 ↓ 로 내려왔을 때 등) */
  selectRow(r: number) {
    if (!this.view.length) return;
    r = Math.max(0, Math.min(this.view.length - 1, r));
    const c = this.columns.findIndex((_, i) => !this.hiddenCol(i));
    if (c < 0) return;
    this.picked.clear();
    this.sel = { r0: r, c0: c, r1: r, c1: c };
    this.cur = { r, c };
    this.reveal(r, c);
    this.update();
    this.status();
  }

  /** 키보드 커서 행의 화면 위치 (없으면 null) — 그 옆에 메뉴를 띄우려고 */
  cursorRect(): { left: number; top: number; right: number; bottom: number } | null {
    const r = this.cur?.r ?? this.sel?.r1;
    if (r == null) return null;
    const vp = this.viewport.getBoundingClientRect();
    const top = Math.max(vp.top, Math.min(vp.bottom - ROW_H, vp.top + r * ROW_H - this.viewport.scrollTop));
    return { left: vp.left, top, right: vp.right, bottom: top + ROW_H };
  }

  /** 선택 영역을 TSV 로 (엑셀에 바로 붙는다). Ctrl+Shift+C 는 헤더 포함. */
  selectionText(withHeader = false): string {
    if (this.picked.size) return this.pickedText(withHeader);
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

  /** Ctrl+클릭으로 고른 셀(+사각형)을 TSV 로 — 걸친 행·열의 상자에서 고르지 않은 칸은 비워 둔다 */
  private pickedText(withHeader: boolean): string {
    const cells = this.selCells();
    const have = new Set(cells.map((x) => x.r * 10000 + x.c));
    const rows = [...new Set(cells.map((x) => x.r))].sort((a, b) => a - b);
    const cols = [...new Set(cells.map((x) => x.c))].sort((a, b) => a - b);
    const esc = (v: string | null) => (v == null ? "" : /[\t\n"]/.test(v) ? `"${v.replace(/"/g, '""')}"` : v);
    const lines: string[] = [];
    if (withHeader) lines.push(cols.map((c) => this.columns[c].name).join("\t"));
    for (const r of rows) lines.push(cols.map((c) => (have.has(r * 10000 + c) ? esc(this.value(this.view[r], c)) : "")).join("\t"));
    return lines.join("\n");
  }

  /** 열 정의 (화면 열 번호 — 숨긴 열 포함) */
  columnAt(c: number): Column | undefined {
    return this.columns[c];
  }

  hasMoreRows() {
    return this.hasMore;
  }

  /** 서버에 남은 행을 `limit` 행까지 모두 가져온다. 멈춘 이유를 돌려준다. */
  async loadAll(limit: number, onProgress?: (n: number) => void, cancelled?: () => boolean): Promise<"done" | "limit" | "cancelled"> {
    if (!this.opts.onNeedMore) return "done";
    while (this.loading) await new Promise((r) => setTimeout(r, 50));
    this.loading = true;
    try {
      while (this.hasMore) {
        if (this.rows.length >= limit) return "limit";
        if (cancelled?.()) return "cancelled";
        const page = await this.opts.onNeedMore();
        if (!page) { this.hasMore = false; break; }
        for (const r of page.rows) this.rows.push(r);
        this.hasMore = page.has_more;
        onProgress?.(this.rows.length);
      }
      return "done";
    } finally {
      this.loading = false;
      this.rebuildView();
      this.update();
      this.status();
    }
  }

  /** 화면에 보이는 그대로 (정렬·필터·숨긴 열·고친 값 반영) */
  exportData(): { columns: { name: string; type_name: string }[]; rows: (string | null)[][] } {
    const cols = this.columns.map((_, i) => i).filter((i) => !this.hiddenCol(i));
    return {
      columns: cols.map((c) => ({ name: this.columns[c].name, type_name: this.columns[c].type_name })),
      rows: this.view.map((r) => cols.map((c) => this.value(r, c))),
    };
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
