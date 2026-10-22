// 탭 하나 — 에디터 + 세션 + 결과 영역 (main.ts 에서 나눔)

import { save as saveDialog } from "@tauri-apps/plugin-dialog";
import { api, errOf, type Connected } from "./api";
import { sqlCompletion } from "./completion";
import { AnalysisView } from "./analysis";
import { DebugView } from "./debugger";
import { MonitorView } from "./monitor";
import { SqlEditor } from "./editor";
import { ResultGrid } from "./grid";
import { guessTable, toCsv as toCsvText, toInserts as toInsertsText, unsupported as unsupportedCols } from "./export-text";
import { SchemaCompareView } from "./schemacmp";
import { type Snapshot } from "./resultdiff";
import { field, h, modal, toast } from "./ui";
import type { App } from "./main";
import { dragSplit } from "./layout";

export type Conn = Connected & { profile: string; /** 접속한 때 (ms) */ at?: number };
export type Pane = "result" | "output" | "plan" | "desc";

let tabSeq = 1;

export class Tab {
  readonly id = tabSeq++;
  readonly root: HTMLElement;
  readonly editor: SqlEditor;
  readonly grid: ResultGrid;
  readonly output = h("pre", { class: "pane output" });
  readonly plan = h("pre", { class: "pane plan" });
  readonly desc = h("div", { class: "pane desc" });
  readonly gridStatus = h("span", { class: "grid-status" });
  readonly tabEl: HTMLElement;
  private paneBtns = new Map<Pane, HTMLElement>();
  private panes = new Map<Pane, HTMLElement>();
  conn: Conn | null = null;
  filePath: string | null = null;
  encoding = "utf-8";
  /** 읽은 파일의 줄 끝 — 저장할 때 되돌린다 (편집기 안은 \n). CRLF 파일을 LF 로 바꿔 Git 에서 모든 줄이 바뀐 것처럼 보이지 않게 */
  eol: "\n" | "\r\n" = "\n";
  dirty = false;
  txnPending = false;
  /** 끊긴 접속을 다시 여는 중 */
  recovering = false;
  running = false;
  runStarted = 0;
  /** 실행 중 표시를 갱신하는 타이머 (탭마다 — 두 탭이 동시에 돌아도 각자 센다) */
  runTimer = 0;
  /** 결과 영역 위에 뜨는 '실행 중' 표시 — 1초 넘게 걸릴 때만 */
  readonly runBox = h("div", { class: "run-overlay", hidden: true });
  private runText = h("span", { class: "run-text" });
  lastError: string | undefined;
  private editorParts: HTMLElement[] = [];
  /** 마지막으로 결과를 띄운 조회 (편집할 때 ROWID 를 붙여 다시 돈다) */
  lastQuery: { sql: string; binds: [string, string | null][] } | null = null;
  editTable: string | null = null;
  readonly notesBtn = h("button", { class: "small", title: "결과 머리를 컬럼 설명(COMMENT)으로 — 다시 누르면 이름으로" }, "설명");
  readonly filterIn = h("input", { class: "grid-filter", placeholder: "거르기: 값 · 열=값 · 열~포함 · 열>값 (AND 로 여럿)" });
  readonly editBtn = h("button", { class: "small", title: "단일 테이블 조회 결과를 고친다 (ROWID 로 다시 조회)" }, "편집");
  readonly applyBtn = h("button", { class: "small primary", hidden: true }, "적용");
  readonly discardBtn = h("button", { class: "small", hidden: true, title: "고친 것을 버린다" }, "되돌리기");
  readonly delRowBtn = h("button", { class: "small", hidden: true, title: "선택한 행을 지울 행으로 표시 (다시 누르면 풀림, Delete 키)" }, "행 삭제");
  private dbg: DebugView | null = null;
  /** 글을 새 탭으로 (INSERT 문 내보내기) */
  private openText: (text: string) => void = () => {};
  private an: AnalysisView | null = null;
  private mon: MonitorView | null = null;
  private cmp: SchemaCompareView | null = null;
  private app: App;

  /** 디버거 화면 (탭의 접속을 쓴다) */
  debugger(): DebugView {
    if (!this.dbg) {
      this.dbg = new DebugView(() => this.conn?.id, () => this.showEditor());
      this.root.append(this.dbg.el);
    }
    if (this.cmp) this.cmp.el.hidden = true;
    for (const p of this.editorParts) p.hidden = true;
    if (this.an) this.an.el.hidden = true;
    if (this.mon) this.mon.el.hidden = true;
    return this.dbg;
  }

  /** PL/SQL 분석 화면 (탭의 접속 프로필 기준) */
  analysis(): AnalysisView {
    if (!this.an) {
      this.an = new AnalysisView(() => this.conn?.id, () => this.showEditor());
      this.root.append(this.an.el);
    }
    if (this.cmp) this.cmp.el.hidden = true;
    for (const p of this.editorParts) p.hidden = true;
    if (this.dbg) this.dbg.el.hidden = true;
    if (this.mon) this.mon.el.hidden = true;
    return this.an;
  }

  /** 세션·락 모니터 */
  monitor(): MonitorView {
    if (!this.mon) {
      this.mon = new MonitorView(() => this.conn?.id, () => this.showEditor());
      this.root.append(this.mon.el);
    }
    if (this.cmp) this.cmp.el.hidden = true;
    for (const p of this.editorParts) p.hidden = true;
    if (this.dbg) this.dbg.el.hidden = true;
    if (this.an) this.an.el.hidden = true;
    return this.mon;
  }

  /** 스키마 비교 (탭의 접속이 원본) */
  compare(): SchemaCompareView {
    if (!this.cmp) {
      this.cmp = new SchemaCompareView(() => this.conn?.id, () => this.conn?.profile, () => this.showEditor(), (text) => void this.app.newTab(false, text));
      this.root.append(this.cmp.el);
    }
    for (const p of this.editorParts) p.hidden = true;
    if (this.dbg) this.dbg.el.hidden = true;
    if (this.an) this.an.el.hidden = true;
    if (this.mon) this.mon.el.hidden = true;
    return this.cmp;
  }

  showEditor() {
    if (this.an) this.an.el.hidden = true;
    if (this.mon) this.mon.el.hidden = true;
    if (this.cmp) this.cmp.el.hidden = true;
    for (const p of this.editorParts) p.hidden = false;
    this.editor.focus();
  }

  constructor(app: App, text = "") {
    this.app = app;
    const editorHost = h("div", { class: "editor-host" });
    this.grid = new ResultGrid({
      onNeedMore: async () => (this.conn ? api.fetchMore(this.conn.id, 1000) : null),
      // 한 줄로 — 길면 줄임표, 전체는 풍선으로 (여러 줄로 늘어나 탭 줄이 높아지던 것)
      onStatus: (t) => { this.gridStatus.textContent = t; this.gridStatus.title = t; },
      onContextMenu: (r, c, e) => app.gridMenu(this, e, r[c] ?? null, c),
      onDirty: (n) => {
        this.applyBtn.textContent = n ? `적용 (${n})` : "적용";
        this.applyBtn.disabled = !n;
        this.discardBtn.disabled = !n;
      },
    });
    let ft = 0;
    this.filterIn.addEventListener("input", () => {
      clearTimeout(ft);
      ft = window.setTimeout(() => this.grid.setFilter(this.filterIn.value), 150);
    });
    this.filterIn.addEventListener("keydown", (e) => { if (e.key === "Escape") { this.filterIn.value = ""; this.grid.setFilter(""); } });
    this.editBtn.onclick = () => app.toggleGridEdit(this);
    // 머리를 설명으로 — 이 PC 에 기억한다
    let notesOn = false;
    try { notesOn = localStorage.getItem("grid.notes") === "1"; } catch { /* 기본은 이름 */ }
    this.grid.setShowNotes(notesOn);
    this.notesBtn.classList.toggle("on", notesOn);
    this.notesBtn.disabled = true;
    this.notesBtn.onclick = () => {
      notesOn = !notesOn;
      this.grid.setShowNotes(notesOn);
      this.notesBtn.classList.toggle("on", notesOn);
      try { localStorage.setItem("grid.notes", notesOn ? "1" : "0"); } catch { /* 기억 못 해도 된다 */ }
    };
    this.applyBtn.onclick = () => app.applyGridEdit(this);
    this.discardBtn.onclick = () => this.grid.discardChanges();
    this.delRowBtn.onclick = () => this.grid.toggleDeleteSelected();
    const gridPane = h("div", { class: "pane result" }, this.grid.el);
    // 내보내기: 엑셀 · CSV · INSERT 문 (보이는 열·정렬·거르기 그대로)
    const exportSel = h("select", { class: "small export-sel", title: "결과 내보내기 — 보이는 열·정렬·거르기 그대로" },
      h("option", { value: "" }, "내보내기…"), h("option", { value: "xlsx" }, "엑셀 (.xlsx)"), h("option", { value: "csv" }, "CSV 파일"),
      h("option", { value: "insert-tab" }, "INSERT 문 → 새 탭"), h("option", { value: "insert-file" }, "INSERT 문 → 파일 (.sql)"));
    exportSel.onchange = () => { const v = exportSel.value as "xlsx" | "csv" | "insert-tab" | "insert-file"; exportSel.value = ""; if (v) this.exportResult(v); };
    this.openText = (text) => void app.newTab(true, text);
    this.panes.set("result", gridPane).set("output", this.output).set("plan", this.plan).set("desc", this.desc);
    const btn = (p: Pane, label: string) => {
      const b = h("button", { class: "pane-tab", onclick: () => this.show(p) }, label);
      this.paneBtns.set(p, b);
      return b;
    };
    const bottom = h("section", { class: "bottom" },
      h("nav", { class: "pane-tabs" }, btn("result", "결과"), btn("output", "출력"), btn("plan", "실행계획"), btn("desc", "구조"),
        this.filterIn, this.notesBtn, this.editBtn, this.delRowBtn, this.applyBtn, this.discardBtn,
        h("span", { class: "spacer" }), this.gridStatus,
        h("button", { class: "small", title: "가져온 행을 CSV 로 복사", onclick: () => this.copyCsv() }, "CSV 복사"),
        exportSel,
        h("button", { class: "small", title: "결과 비교 — 이 결과를 기준으로 담고, 고친 쿼리·다른 DB 의 결과와 행·열 단위로 비교", onclick: (e: Event) => app.compareMenu(this, e as MouseEvent) }, "비교"),
        h("span", { class: "pane-size" },
          h("button", { class: "small", title: "결과 영역 키우기 (에디터를 줄인다) — 경계선을 끌어도 된다", onclick: () => this.resizePane("grow") }, "▲"),
          h("button", { class: "small", title: "결과 영역 줄이기", onclick: () => this.resizePane("shrink") }, "▼"),
          h("button", { class: "small", title: "원래 비율로 (경계선을 더블클릭해도 된다)", onclick: () => this.resizePane("reset") }, "↕"))),
      gridPane, this.output, this.plan, this.desc, this.runBox);
    this.runBox.append(h("span", { class: "spinner" }), this.runText,
      h("button", { class: "small danger", title: "실행 중인 쿼리를 취소 (Esc)", onclick: () => void app.cancel() }, "중지"));
    const splitter = h("div", { class: "splitter" });
    this.root = h("div", { class: "tab-body" }, editorHost, splitter, bottom);
    this.editorParts = [editorHost, splitter, bottom];
    this.editor = new SqlEditor(editorHost, {
      runStatement: () => app.runStatement(),
      runScript: () => app.runScript(),
      explain: () => app.explain(),
      save: () => app.save(),
      saveAs: () => app.saveAs(),
      format: () => app.formatSql(),
      describe: () => app.describeAtCursor(),
      history: () => app.showHistory(),
      // 붙여 넣기·되돌리기처럼 DOM input 이 나지 않는 편집도 — 탭의 ● (저장하지 않은 편집)
      edited: () => this.setDirty(true),
    }, text, sqlCompletion(() => this.conn?.id));
    this.editor.view.dom.addEventListener("contextmenu", (e) => void app.editorMenu(this, e));
    dragSplit(splitter, editorHost);
    this.editorHostEl = editorHost;
    splitter.title = "끌어서 높이 조절 · 더블클릭: 원래 비율";
    splitter.addEventListener("dblclick", () => this.resizePane("reset"));
    try {
      const saved = Number(localStorage.getItem("pane.editor"));
      if (saved >= 15 && saved <= 85) editorHost.style.flexBasis = `${saved}%`;
    } catch { /* 기본 비율 */ }
    this.show("result");
    this.grid.clear("Ctrl+Enter: 커서 위치 문장 실행 · F5: 스크립트 실행 · Ctrl+E: 실행계획");
    this.tabEl = h("div", { class: "tab", onclick: () => { if (!app.tabJustDragged) app.activate(this); } },
      h("span", { class: "tab-title" }),
      h("span", { class: "tab-txn", hidden: true, title: "커밋하지 않은 변경이 있습니다 (INSERT·UPDATE·DELETE·MERGE 등) — 커밋 또는 롤백하세요" }, "커밋 전"),
      h("button", { class: "tab-close", title: "닫기", onclick: (e: Event) => { e.stopPropagation(); app.closeTab(this); } }, "×"));
    this.refreshTitle();
  }

  private editorHostEl!: HTMLElement;

  /** 에디터와 결과 영역의 높이 — 키우기·줄이기는 15%씩, 원래는 45%. 비율로 두어 창을 줄여도 경계선이 화면 밖으로 나가지 않는다 */
  resizePane(how: "grow" | "shrink" | "reset") {
    const host = this.editorHostEl;
    const total = this.root.getBoundingClientRect().height || 1;
    const cur = (host.getBoundingClientRect().height / total) * 100;
    const next = how === "reset" ? 45 : how === "grow" ? cur - 15 : cur + 15;
    const pct = Math.round(Math.max(15, Math.min(85, next)));
    host.style.flexBasis = `${pct}%`;
    try { localStorage.setItem("pane.editor", String(pct)); } catch { /* 기억 못 해도 된다 */ }
  }

  /** 오래 걸리는 쿼리 표시 — 결과 영역 위 안내, 탭의 도는 표시. secs 가 null 이면 끝 */
  showRunning(secs: number | null) {
    this.tabEl.classList.toggle("running", secs != null);
    if (secs == null || secs < 1) {
      this.runBox.hidden = true;
      return;
    }
    const whole = Math.floor(secs);
    const t = whole >= 60 ? `${Math.floor(whole / 60)}분 ${whole % 60}초` : `${secs.toFixed(1)}초`;
    this.runText.textContent = whole >= 10
      ? `서버가 실행 중입니다 · ${t} — 응답을 기다리는 중 (오래 걸리는 쿼리입니다)`
      : `서버가 실행 중입니다 · ${t}`;
    this.runText.title = "응답이 올 때까지 정상적으로 기다리는 중입니다. 너무 오래 걸리면 중지(Esc)하고 실행계획(Ctrl+E)이나 세션 모니터(세션)에서 원인을 보세요.";
    this.runBox.hidden = false;
  }

  show(p: Pane) {
    for (const [k, el] of this.panes) el.hidden = k !== p;
    for (const [k, b] of this.paneBtns) b.classList.toggle("active", k === p);
  }

  title() {
    const file = this.filePath ? this.filePath.split(/[\\/]/).pop() : `새 문서 ${this.id}`;
    return `${this.dirty ? "● " : ""}${file}${this.conn ? ` — ${this.conn.profile}` : ""}`;
  }

  refreshTitle() {
    (this.tabEl.querySelector(".tab-title") as HTMLElement).textContent = this.title();
    this.tabEl.style.borderTopColor = this.conn?.color ?? "transparent";
    // 커밋하지 않은 변경 — 다른 탭을 보고 있어도 알 수 있게 탭과 에디터 위에
    const txn = !!this.conn && this.txnPending;
    this.tabEl.classList.toggle("txn", txn);
    (this.tabEl.querySelector(".tab-txn") as HTMLElement).hidden = !txn;
    this.root.classList.toggle("txn", txn);
  }

  setDirty(d: boolean) {
    if (this.dirty !== d) {
      this.dirty = d;
      this.refreshTitle();
    }
  }

  log(line: string, cls = "") {
    const span = h("span", { class: cls }, line + "\n");
    this.output.append(span);
    this.output.scrollTop = this.output.scrollHeight;
  }

  /** 비교용으로 지금 결과를 담는다 (보이는 열·정렬·거르기 그대로). 서버에 행이 남았으면 끝까지 가져올지 묻는다 */
  async snapshot(): Promise<Snapshot | null> {
    if (!this.grid.columnAt(0)) {
      toast("비교할 결과가 없습니다 — 먼저 조회하세요", "info");
      return null;
    }
    const LIMIT = 500_000;
    if (this.grid.hasMoreRows()) {
      const choice = await modal<"all" | "loaded">("결과 비교",
        h("pre", { class: "modal-text" },
          `지금 ${this.grid.rowCount().toLocaleString()}행을 가져왔고, 서버에 행이 더 남아 있습니다.\n\n` +
          `전체를 비교하려면 남은 행을 모두 가져옵니다 (${LIMIT.toLocaleString()}행까지, 행이 많으면 시간이 걸립니다).`),
        [{ label: "가져온 행만", value: "loaded" }, { label: "전체 가져와서", value: "all", kind: "primary" }]);
      if (!choice) return null;
      if (choice === "all") {
        let stop = false;
        const cancel = h("button", { class: "small", onclick: () => { stop = true; } }, "중지");
        this.gridStatus.after(cancel);
        try {
          const r = await this.grid.loadAll(LIMIT, (n) => { this.gridStatus.textContent = `${n.toLocaleString()}행 가져오는 중…`; }, () => stop);
          if (r === "cancelled") { toast("멈췄습니다 (가져온 행은 그리드에 남아 있습니다)", "info"); return null; }
        } catch (e) {
          toast(errOf(e).message, "error");
          return null;
        } finally {
          cancel.remove();
        }
      }
    }
    const d = this.grid.exportData();
    return { label: `${this.title()} ${new Date().toTimeString().slice(0, 8)}`, columns: d.columns, rows: d.rows, partial: this.grid.hasMoreRows() };
  }

  /** 결과 내보내기: 엑셀 · CSV 파일 · INSERT 문(새 탭 / 파일). 보이는 열·정렬·거르기 그대로 */
  async exportResult(kind: "xlsx" | "csv" | "insert-tab" | "insert-file") {
    if (!this.grid.rowCount()) return toast("내보낼 결과가 없습니다", "info");
    const LIMIT = kind === "xlsx" ? 1_048_575 : kind === "insert-tab" ? 50_000 : 5_000_000;
    const what = { xlsx: "엑셀", csv: "CSV", "insert-tab": "INSERT 문", "insert-file": "INSERT 문" }[kind];
    if (this.grid.hasMoreRows()) {
      const choice = await modal<"all" | "loaded">(`${what}로 내보내기`,
        h("pre", { class: "modal-text" },
          `지금 ${this.grid.rowCount().toLocaleString()}행을 가져왔고, 서버에 행이 더 남아 있습니다.\n\n` +
          `전체를 내보내려면 남은 행을 모두 가져옵니다 (행이 많으면 시간이 걸립니다).\n${what}는 ${LIMIT.toLocaleString()}행까지 내보냅니다.`),
        [{ label: "가져온 행만", value: "loaded" }, { label: "전체 가져와서 내보내기", value: "all", kind: "primary" }]);
      if (!choice) return;
      if (choice === "all") {
        let stop = false;
        const cancel = h("button", { class: "small", onclick: () => { stop = true; } }, "중지");
        this.gridStatus.after(cancel);
        try {
          const r = await this.grid.loadAll(LIMIT, (n) => { this.gridStatus.textContent = `${n.toLocaleString()}행 가져오는 중…`; }, () => stop);
          if (r === "cancelled") return toast("내보내기를 멈췄습니다 (가져온 행은 그리드에 남아 있습니다)", "info");
          if (r === "limit") toast(`${LIMIT.toLocaleString()}행까지만 내보냅니다`, "info", 6000);
        } catch (e) {
          return toast(errOf(e).message, "error");
        } finally {
          cancel.remove();
        }
      }
    }
    const data = this.grid.exportData();
    if (!data.rows.length) return toast("필터에 맞는 행이 없습니다", "info");
    const rows = data.rows.length > LIMIT ? data.rows.slice(0, LIMIT) : data.rows;
    const stamp = new Date().toISOString().slice(0, 16).replace(/[-:]/g, "").replace("T", "_");
    const base = this.filePath ? this.filePath.split(/[\\/]/).pop()!.replace(/\.[^.]+$/, "") : "result";
    try {
      if (kind === "xlsx") {
        const path = await saveDialog({ filters: [{ name: "Excel", extensions: ["xlsx"] }], defaultPath: `${base}_${stamp}.xlsx` });
        if (!path) return;
        this.gridStatus.textContent = `${rows.length.toLocaleString()}행을 엑셀로 쓰는 중…`;
        const n = await api.exportXlsx(path, data.columns, rows, this.lastQuery?.sql ?? null);
        return void toast(`${n.toLocaleString()}행을 엑셀로 저장했습니다`, "ok");
      }
      if (kind === "csv") {
        const delim = h("select", {}, h("option", { value: "," }, "쉼표 ,"), h("option", { value: "\t" }, "탭"), h("option", { value: ";" }, "세미콜론 ;"));
        const enc = h("select", {}, h("option", { value: "utf-8-bom" }, "UTF-8 (BOM — 엑셀에서 한글이 안 깨짐)"), h("option", { value: "utf-8" }, "UTF-8"), h("option", { value: "cp949" }, "CP949 (EUC-KR)"));
        const head = h("input", { type: "checkbox", checked: true });
        const ok = await modal("CSV 로 내보내기", h("div", { class: "form" },
          field("구분자", delim), field("인코딩", enc), h("label", { class: "check" }, head, "첫 줄에 컬럼 이름"),
          h("small", {}, `${rows.length.toLocaleString()}행 × ${data.columns.length}열 · 날짜는 화면에 보이는 형식(YYYY-MM-DD HH24:MI:SS) 그대로`)),
          [{ label: "취소", value: false }, { label: "저장", value: true, kind: "primary" }]);
        if (!ok) return;
        const ext = delim.value === "\t" ? "tsv" : "csv";
        const path = await saveDialog({ filters: [{ name: "CSV", extensions: [ext, "txt"] }], defaultPath: `${base}_${stamp}.${ext}` });
        if (!path) return;
        await api.writeFile(path, toCsvText(data.columns, rows, { delimiter: delim.value, header: head.checked }), enc.value);
        return void toast(`${rows.length.toLocaleString()}행을 CSV 로 저장했습니다`, "ok");
      }
      // INSERT 문
      const table = h("input", { value: this.editTable ?? guessTable(this.lastQuery?.sql), placeholder: "스키마.테이블", style: "width:100%" });
      const bad = unsupportedCols(data.columns);
      const ok = await modal("INSERT 문으로 내보내기", h("div", { class: "form" },
        field("넣을 테이블", table, "다른 DB·스키마로 옮길 때 바꿔 쓰세요"),
        h("small", {}, `${rows.length.toLocaleString()}행 · 날짜는 TO_DATE/TO_TIMESTAMP, 4000바이트가 넘는 글자는 TO_CLOB 로`),
        bad.length ? h("small", { class: "warn" }, `바이너리 열은 NULL 로 넣습니다: ${bad.join(", ")}`) : ""),
        [{ label: "취소", value: false }, { label: kind === "insert-tab" ? "새 탭에 넣기" : "저장", value: true, kind: "primary" }]);
      if (!ok || !table.value.trim()) return;
      const text = toInsertsText(table.value.trim(), data.columns, rows);
      if (kind === "insert-tab") return void this.openText(`-- ${rows.length.toLocaleString()}행 · ${new Date().toLocaleString()}\n${text}`);
      const path = await saveDialog({ filters: [{ name: "SQL", extensions: ["sql"] }], defaultPath: `${table.value.trim().replace(/[\\/:*?"<>|]/g, "_")}_${stamp}.sql` });
      if (!path) return;
      await api.writeFile(path, text, "utf-8");
      toast(`${rows.length.toLocaleString()}행을 INSERT 문으로 저장했습니다`, "ok");
    } catch (e) {
      toast(errOf(e).message, "error");
    } finally {
      this.grid.setFilter(this.filterIn.value);
    }
  }

  copyCsv() {
    if (!this.grid.rowCount()) return;
    navigator.clipboard.writeText(this.grid.toCsv()).then(() => toast(`${this.grid.rowCount()}행을 CSV 로 복사했습니다`, "ok"));
  }
}
