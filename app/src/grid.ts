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
    this.body.addEventListener("dblclick", (e) => this.onDblClick(e));
    this.measureCtx.font = "12px ui-monospace, Consolas, monospace";
  }

  clear(message = "") {
    this.columns = [];
    this.rows = [];
    this.hasMore = false;
    this.sel = null;
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
    this.update();
    this.status();
  }

  rowCount() {
    return this.rows.length;
  }

  private status() {
    this.opts.onStatus?.(`${this.rows.length.toLocaleString()}행${this.hasMore ? " (더 있음 — 스크롤하면 가져옵니다)" : ""}`);
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

  private totalWidth() {
    return 56 + this.widths.reduce((a, b) => a + b, 0);
  }

  private renderHeader() {
    const row = div("grid-row grid-head");
    row.style.width = `${this.totalWidth()}px`;
    const num = div("grid-cell grid-num");
    num.textContent = "#";
    row.append(num);
    this.columns.forEach((c, i) => {
      const cell = div("grid-cell");
      cell.style.width = `${this.widths[i]}px`;
      cell.textContent = c.name;
      cell.title = `${c.name}  ${c.type_name}${c.nullable ? "" : " NOT NULL"}`;
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
    const n = this.rows.length;
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
      const num = div("grid-cell grid-num");
      num.textContent = String(r + 1);
      row.append(num);
      const data = this.rows[r];
      for (let c = 0; c < this.columns.length; c++) {
        const cell = div("grid-cell");
        cell.style.width = `${this.widths[c]}px`;
        const v = data[c];
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
    const v = this.rows[at.r][at.c];
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
      if (this.rows.length) this.sel = { r0: 0, c0: 0, r1: this.rows.length - 1, c1: this.columns.length - 1 };
      this.update();
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
    if (withHeader) lines.push(this.columns.slice(c0, c1 + 1).map((c) => c.name).join("\t"));
    for (let r = r0; r <= r1; r++) lines.push(this.rows[r].slice(c0, c1 + 1).map(esc).join("\t"));
    return lines.join("\n");
  }

  /** 전체 결과를 CSV 로 (가져온 행까지) */
  toCsv(): string {
    const esc = (v: string | null) => (v == null ? "" : /[,\n"]/.test(v) ? `"${v.replace(/"/g, '""')}"` : v);
    const out = [this.columns.map((c) => esc(c.name)).join(",")];
    for (const r of this.rows) out.push(r.map(esc).join(","));
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
