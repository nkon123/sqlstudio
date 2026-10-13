// SQLStudio 화면.
//
// 탭 하나 = 에디터 하나 + 세션 하나 + 결과 영역 하나. 탭끼리는 아무것도 공유하지 않는다
// (한 탭의 긴 쿼리가 다른 탭을 묶지 않는다 — 세션마다 서버 쪽 스레드가 따로 있다).

import { invoke } from "@tauri-apps/api/core";
import { open as openDialog, save as saveDialog } from "@tauri-apps/plugin-dialog";
import { getCurrentWindow } from "@tauri-apps/api/window";
import {
  api, byteToUtf16, errOf, utf16ToByte,
  type Connected, type ErrView, type ExecView, type ObjectEntry, type Profile, type Provider, type TableDesc, type TnsEntry, type TnsView, type RowPage } from "./api";
import { AiPanel } from "./ai";
import { listen } from "@tauri-apps/api/event";
import { sqlCompletion } from "./completion";
import { AnalysisView } from "./analysis";
import { DebugView } from "./debugger";
import { MonitorView } from "./monitor";
import { SqlEditor } from "./editor";
import { DEFAULT_PREFS, formatSql, loadPrefs, savePrefs, type FormatPrefs } from "./format";
import type { GitPanel } from "./git";
import { ResultGrid } from "./grid";
import { guessTable, toCsv as toCsvText, toInserts as toInsertsText, unsupported as unsupportedCols } from "./export-text";
import { aliasMap, showColumns } from "./colpopup";
import { openHistory } from "./history";
import { openImport } from "./importer";
import { showShortcuts } from "./shortcuts";
import { DEFAULT_SNIPPETS, loadSnippets, parse as parseSnippets, saveSnippets, snippets, type Snip } from "./snippets";
import { alertBox, bindBox, confirmBox, field, h, modal, passwordBox, toast } from "./ui";

type Conn = Connected & { profile: string };
type Pane = "result" | "output" | "plan" | "desc";

let tabSeq = 1;

class Tab {
  readonly id = tabSeq++;
  readonly root: HTMLElement;
  readonly editor: SqlEditor;
  readonly grid: ResultGrid;
  readonly output = h("pre", { class: "pane output" });
  readonly plan = h("pre", { class: "pane plan" });
  readonly desc = h("div", { class: "pane desc" });
  readonly gridStatus = h("span", {});
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
  running = false;
  runStarted = 0;
  lastError: string | undefined;
  private editorParts: HTMLElement[] = [];
  /** 마지막으로 결과를 띄운 조회 (편집할 때 ROWID 를 붙여 다시 돈다) */
  lastQuery: { sql: string; binds: [string, string | null][] } | null = null;
  editTable: string | null = null;
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

  /** 디버거 화면 (탭의 접속을 쓴다) */
  debugger(): DebugView {
    if (!this.dbg) {
      this.dbg = new DebugView(() => this.conn?.id, () => this.showEditor());
      this.root.append(this.dbg.el);
    }
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
    for (const p of this.editorParts) p.hidden = true;
    if (this.dbg) this.dbg.el.hidden = true;
    if (this.an) this.an.el.hidden = true;
    return this.mon;
  }

  showEditor() {
    if (this.an) this.an.el.hidden = true;
    if (this.mon) this.mon.el.hidden = true;
    for (const p of this.editorParts) p.hidden = false;
    this.editor.focus();
  }

  constructor(app: App, text = "") {
    const editorHost = h("div", { class: "editor-host" });
    this.grid = new ResultGrid({
      onNeedMore: async () => (this.conn ? api.fetchMore(this.conn.id, 1000) : null),
      onStatus: (t) => { this.gridStatus.textContent = t; },
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
        this.filterIn, this.editBtn, this.delRowBtn, this.applyBtn, this.discardBtn,
        h("span", { class: "spacer" }), this.gridStatus,
        h("button", { class: "small", title: "가져온 행을 CSV 로 복사", onclick: () => this.copyCsv() }, "CSV 복사"),
        exportSel),
      gridPane, this.output, this.plan, this.desc);
    const splitter = h("div", { class: "splitter" });
    this.root = h("div", { class: "tab-body" }, editorHost, splitter, bottom);
    this.editorParts = [editorHost, splitter, bottom];
    this.editor = new SqlEditor(editorHost, {
      runStatement: () => app.runStatement(),
      runScript: () => app.runScript(),
      explain: () => app.explain(),
      save: () => app.save(),
      format: () => app.formatSql(),
      describe: () => app.describeAtCursor(),
      history: () => app.showHistory(),
    }, text, sqlCompletion(() => this.conn?.id));
    this.editor.view.dom.addEventListener("input", () => this.setDirty(true));
    dragSplit(splitter, editorHost);
    this.show("result");
    this.grid.clear("Ctrl+Enter: 커서 위치 문장 실행 · F5: 스크립트 실행 · Ctrl+E: 실행계획");
    this.tabEl = h("div", { class: "tab", onclick: () => app.activate(this) },
      h("span", { class: "tab-title" }), h("button", { class: "tab-close", title: "닫기", onclick: (e: Event) => { e.stopPropagation(); app.closeTab(this); } }, "×"));
    this.refreshTitle();
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

class App {
  private tabs: Tab[] = [];
  private active: Tab | null = null;
  private profiles: Profile[] = [];
  private tabStrip = h("div", { class: "tabs" });
  private tabBodies = h("div", { class: "tab-bodies" });
  private profileSel = h("select", { title: "접속 프로필" });
  private connBadge = h("span", { class: "conn-badge" });
  private txnBadge = h("span", { class: "txn-badge", hidden: true }, "트랜잭션 진행 중");
  private statusLeft = h("span", {});
  private statusRight = h("span", {});
  private compBadge = h("span", { class: "comp-badge", title: "자동완성 캐시 — Ctrl+Space 로 직접 띄운다" });
  private btn: Record<string, HTMLButtonElement> = {};
  private browserList = h("div", { class: "obj-list" });
  private browserOwner = h("select", { title: "스키마" });
  private browserType = h("select", { title: "종류" },
    ...["TABLE", "VIEW", "PACKAGE", "PROCEDURE", "FUNCTION", "TRIGGER", "SEQUENCE", "SYNONYM", "TYPE", "MATERIALIZED VIEW", "INDEX"].map((t) => h("option", { value: t }, t)),
    h("option", { value: "" }, "(전체)"));
  private browserFilter = h("input", { placeholder: "이름 필터 (예: ORD%)" });
  private browserObjs: ObjectEntry[] = [];
  private pickedTables = new Set<string>();
  private ai: AiPanel;
  /** 프로젝트(Git) 패널 — 처음 필요할 때 불러온다 (시작을 늦추지 않게) */
  private git: GitPanel | null = null;
  private gitPane = h("div", { class: "git" });
  private gitLoading: Promise<GitPanel> | null = null;
  private gitBadge = h("span", { class: "git-badge" });
  private layout!: SideLayout;
  private timer = 0;

  constructor(root: HTMLElement) {
    const b = (key: string, label: string, title: string, onclick: () => void, cls = "") =>
      (this.btn[key] = h("button", { class: cls, title, onclick }, label));
    const toolbar = h("header", { class: "toolbar" },
      this.profileSel,
      b("connect", "접속", "선택한 프로필로 현재 탭을 접속", () => this.connectActive()),
      b("disconnect", "끊기", "현재 탭의 접속을 끊는다", () => this.disconnectActive()),
      this.connBadge, this.txnBadge,
      h("span", { class: "sep" }),
      b("run", "▶ 실행", "커서 위치 문장 실행 (Ctrl+Enter / F9)", () => this.runStatement(), "primary"),
      b("script", "스크립트", "전체 실행 (F5)", () => this.runScript()),
      b("explain", "실행계획", "Ctrl+E", () => this.explain()),
      b("debug", "디버그", "PL/SQL 디버거 — 커서 위치 블록을 디버그로 연다 (탐색기에서 프로시저·패키지를 골라도 된다)", () => this.debugActive()),
      b("dfd", "흐름도", "데이터 흐름도를 별도 창으로 — 분석 결과에서 테이블 ← 처리 ← 테이블 흐름 (테이블 상세의 '흐름도'로 열면 그 테이블 기준)", () => this.openDfd()),
      b("analyze", "분석", "PL/SQL 분석 — 패키지·프로시저를 조각내어 (로컬) 모델로 분석하고 호출 관계·테이블 CRUD 를 통합한다", () => this.analyzeActive()),
      b("monitor", "세션", "세션·락 모니터 — 누가 무엇을 막고 있는지, 세션의 SQL·트랜잭션, 세션 종료(확인 후)", () => this.monitorActive()),
      b("cancel", "중지", "실행 중인 쿼리를 취소", () => this.cancel(), "danger"),
      h("span", { class: "sep" }),
      b("commit", "커밋", "COMMIT", () => this.txn("commit")),
      b("rollback", "롤백", "ROLLBACK", () => this.txn("rollback")),
      h("span", { class: "sep" }),
      b("new", "새 탭", "새 에디터 (같은 접속으로)", () => this.newTab(true)),
      b("open", "열기", "SQL 파일 열기", () => this.openFile()),
      b("history", "이력", "SQL 실행 이력 (F8) — 실행한 문장을 찾아 다시 쓴다", () => this.showHistory()),
      b("save", "저장", "Ctrl+S", () => this.save()),
      b("format", "포맷", "SQL 정리 (Ctrl+Shift+F) — 선택 영역, 없으면 커서 위치 문장", () => this.formatSql()),
      h("span", { class: "sep" }),
      b("toggleBrowser", "왼쪽", "왼쪽 영역(테이블·프로젝트) 접기/펼치기 (Ctrl+Shift+1) · 경계선을 끌면 너비 조절", () => this.layout.toggle("left")),
      b("toggleAi", "AI", "오른쪽 AI 영역 접기/펼치기 (Ctrl+Shift+3) · AI 입력칸으로 바로: Ctrl+K · 경계선을 끌면 너비 조절", () => this.layout.toggle("right")),
      h("span", { class: "spacer" }),
      b("keys", "단축키", "단축키 목록 (F1)", () => showShortcuts()),
      b("settings", "설정", "접속·AI·MCP 설정", () => this.settings()),
    );
    const tablePane = h("div", { class: "side-pane" },
      h("div", { class: "browser-head" }, this.browserOwner, this.browserType),
      this.browserFilter,
      this.browserList,
      h("small", { class: "hint" }, "클릭: 구조 · 우클릭: 컬럼 팝업 · 더블클릭: 이름 넣기 · 체크: AI 문맥에 포함"));
    // 왼쪽 영역: 테이블 | 프로젝트
    const sideTabs = h("div", { class: "side-tabs" });
    const panes: Record<string, HTMLElement> = { table: tablePane, git: this.gitPane };
    const showSide = (k: string) => {
      for (const [n, el] of Object.entries(panes)) el.hidden = n !== k;
      for (const b of sideTabs.children) b.classList.toggle("on", (b as HTMLElement).dataset.k === k);
      try { localStorage.setItem("side.tab", k); } catch { /* 없어도 된다 */ }
      if (k === "git") this.loadGit().then((g) => g.setVisible(true));
      else this.git?.setVisible(false);
    };
    for (const [k, label] of [["table", "테이블"], ["git", "프로젝트"]]) sideTabs.append(h("button", { "data-k": k, onclick: () => showSide(k) }, label));
    const browser = h("aside", { class: "browser" }, sideTabs, tablePane, this.gitPane);
    let sideTab = "table";
    try { sideTab = localStorage.getItem("side.tab") === "git" ? "git" : "table"; } catch { /* 기본 */ }
    showSide(sideTab);
    this.ai = new AiPanel({
      context: () => this.aiContext(),
      insertSql: (s) => this.active?.editor.insert(s),
      replaceSql: (s) => this.replaceStatement(s),
    });
    const center = h("section", { class: "center" }, this.tabStrip, this.tabBodies);
    const main = h("main", { class: "layout" });
    this.layout = new SideLayout(main, browser, center, this.ai.el, (side, open) => {
      this.btn[side === "left" ? "toggleBrowser" : "toggleAi"]?.classList.toggle("active", open);
    });
    root.append(toolbar, main,
      h("footer", { class: "statusbar" }, this.statusLeft, this.gitBadge, h("span", { class: "spacer" }), this.compBadge, this.statusRight));

    this.browserOwner.addEventListener("change", () => this.loadObjects());
    this.browserType.addEventListener("change", () => this.loadObjects());
    let ft = 0;
    this.browserFilter.addEventListener("input", () => { clearTimeout(ft); ft = window.setTimeout(() => this.loadObjects(), 250); });
    // Ctrl+K: AI 입력칸으로 (오른쪽 영역이 접혀 있으면 펼친다). AI 입력칸에서 Esc: 편집기로
    window.addEventListener("keydown", (e) => {
      // F1: 단축키 목록 (대화상자가 떠 있으면 무시)
      if (e.key === "F1" && !e.ctrlKey && !e.altKey && !document.querySelector("dialog[open]")) {
        e.preventDefault();
        e.stopPropagation();
        showShortcuts();
        return;
      }
      if ((e.ctrlKey || e.metaKey) && !e.shiftKey && !e.altKey && e.code === "KeyK") {
        e.preventDefault();
        e.stopPropagation();
        this.layout.toggle("right", true);
        this.ai.focus();
      }
    }, true);
    this.ai.el.addEventListener("keydown", (e) => {
      if (e.key === "Escape" && (e.target as HTMLElement).tagName === "TEXTAREA") { e.preventDefault(); this.active?.editor.view.focus(); }
    });
    window.addEventListener("keydown", (e) => {
      if (e.key === "Escape" && this.active?.running) this.cancel();
      if (e.ctrlKey && e.shiftKey && (e.code === "Digit1" || e.code === "Digit3")) {
        e.preventDefault();
        this.layout.toggle(e.code === "Digit1" ? "left" : "right");
      }
    });
  }

  /** 선택 영역, 없으면 커서 위치 문장을 정리한다. 실행 취소 한 번으로 되돌린다. */
  async formatSql() {
    const t = this.active;
    if (!t) return;
    const view = t.editor.view;
    if (view.state.selection.ranges.length > 1) return toast("여러 곳이 선택되어 있습니다 — Esc 로 하나로 만든 뒤 포맷하세요", "info");
    const main = view.state.selection.main;
    let from = main.from, to = main.to;
    if (main.empty) {
      const cur = await this.currentStatement(t);
      if (!cur) return toast("포맷할 문장이 없습니다", "info");
      ({ from, to } = cur);
    }
    const src = view.state.sliceDoc(from, to);
    const r = formatSql(src);
    if (!r.ok) return toast(r.reason, "error", 6000);
    if (r.text === src) return toast("이미 정리되어 있습니다", "info");
    view.dispatch({ changes: { from, to, insert: r.text }, selection: { anchor: from, head: from + r.text.length }, userEvent: "input.format", scrollIntoView: true });
    t.editor.focus();
  }

  async start() {
    // 자동완성 캐시 적재 진행 (프로필마다 백그라운드)
    listen<{ profile: string; phase: string; objects: number; columns: number; error?: string | null }>("completion-status", (e) => {
      if (this.active?.conn?.profile.toUpperCase() !== e.payload.profile.toUpperCase()) return;
      this.showCompletionStatus(e.payload);
    });
    const info = await api.appInfo();
    void loadSnippets(info.config_path);
    this.statusLeft.textContent = `SQLStudio ${info.version} · 설정: ${info.config_path}`;
    if (info.config_error) await alertBox("설정 파일 오류", `${info.config_error}\n\n기본 설정으로 시작합니다. 설정 화면에서 저장하면 파일을 다시 씁니다.`);
    await this.reloadProfiles();
    await this.ai.load();
    this.newTab(false);
    if (localStorage.getItem("git.last")) {
      const later = (f: () => void) => ("requestIdleCallback" in window ? requestIdleCallback(f, { timeout: 4000 }) : setTimeout(f, 2000));
      later(() => void this.loadGit());
    }
    this.refresh();
    getCurrentWindow().onCloseRequested(async (e) => {
      const pending = this.tabs.filter((t) => t.txnPending);
      const dirty = this.tabs.filter((t) => t.dirty);
      if (!pending.length && !dirty.length) return;
      const lines = [
        ...pending.map((t) => `· ${t.title()}: 커밋하지 않은 변경 (닫으면 롤백)`),
        ...dirty.map((t) => `· ${t.title()}: 저장하지 않은 편집`),
      ];
      if (!(await confirmBox("닫을까요?", lines.join("\n"), "닫기"))) e.preventDefault();
    });
    if (!this.profiles.length) {
      toast("접속 프로필이 없습니다 — 설정에서 추가하세요", "info", 6000);
      this.settings();
    }
  }

  async reloadProfiles() {
    this.profiles = await api.listProfiles();
    const prev = localStorage.getItem("profile");
    this.profileSel.replaceChildren(...this.profiles.map((p) =>
      h("option", { value: p.name, selected: p.name === prev }, `${p.name}${p.read_only ? " (읽기 전용)" : ""}`)));
  }

  // ── 탭 ───────────────────────────────────────────

  newTab(sameConnection: boolean, text = "") {
    const prev = this.active;
    const t = new Tab(this, text);
    this.tabs.push(t);
    this.tabStrip.append(t.tabEl);
    this.tabBodies.append(t.root);
    this.activate(t);
    if (sameConnection && prev?.conn) this.connect(t, prev.conn.profile);
    return t;
  }

  activate(t: Tab) {
    this.active = t;
    for (const x of this.tabs) {
      x.root.hidden = x !== t;
      x.tabEl.classList.toggle("active", x === t);
    }
    if (t.conn) this.profileSel.value = t.conn.profile;
    this.compBadge.textContent = "";
    if (t.conn) api.completionStatus(t.conn.id).then((s) => this.showCompletionStatus(s)).catch(() => {});
    this.refresh();
    if (t.conn) this.loadBrowser();
    else this.browserList.replaceChildren(h("div", { class: "empty" }, "접속하면 객체 목록이 나옵니다"));
    t.editor.focus();
  }

  async closeTab(t: Tab) {
    if (t.running && !(await confirmBox("실행 중", "실행 중인 쿼리가 있습니다. 세션을 버리고 닫을까요?", "닫기"))) return;
    if (t.txnPending && t.conn) {
      const r = await modal("커밋하지 않은 변경", h("p", {}, `${t.title()} 에 커밋하지 않은 변경이 있습니다.`), [
        { label: "취소", value: "cancel" }, { label: "롤백하고 닫기", value: "rollback", kind: "danger" }, { label: "커밋하고 닫기", value: "commit", kind: "primary" },
      ]);
      if (!r || r === "cancel") return;
      try { await (r === "commit" ? api.commit(t.conn.id) : api.rollback(t.conn.id)); }
      catch (e) { return toast(errOf(e).message, "error"); }
    }
    if (t.dirty && !(await confirmBox("저장하지 않음", `${t.title()} 의 편집을 버릴까요?`, "버리기"))) return;
    if (t.conn) (t.running ? api.abandon(t.conn.id) : api.disconnect(t.conn.id)).catch(() => {});
    t.root.remove();
    t.tabEl.remove();
    this.tabs = this.tabs.filter((x) => x !== t);
    if (!this.tabs.length) this.newTab(false);
    else if (this.active === t) this.activate(this.tabs[this.tabs.length - 1]);
  }

  // ── 접속 ─────────────────────────────────────────

  private connectActive() {
    if (this.active) this.connect(this.active, this.profileSel.value);
  }

  async connect(t: Tab, profileName: string) {
    const p = this.profiles.find((x) => x.name === profileName);
    if (!p) return toast("접속 프로필을 고르세요", "error");
    localStorage.setItem("profile", p.name);
    if (t.conn) await this.disconnect(t);
    this.status(`${p.name} 접속 중…`);
    let password: string | undefined;
    let remember = false;
    for (let attempt = 0; attempt < 3; attempt++) {
      try {
        const c = await api.connect(p.name, password, remember);
        t.conn = { ...c, profile: p.name };
        t.txnPending = false;
        t.refreshTitle();
        t.log(`접속: ${c.user}@${c.connect_string} — ${c.server_version}${c.read_only ? " (읽기 전용)" : ""}`, "ok");
        this.status(`${p.name} 접속됨`);
        this.refresh();
        if (t === this.active) {
          this.loadBrowser();
          api.completionStatus(c.id).then((s) => this.showCompletionStatus(s)).catch(() => {});
        }
        return;
      } catch (e) {
        const err = errOf(e);
        if (err.kind === "password_required" || err.ora_code === 1017) {
          if (err.ora_code === 1017) toast("비밀번호가 틀렸습니다 (ORA-01017)", "error");
          const pw = await passwordBox(p.name, p.user);
          if (pw == null) { this.status("접속 취소"); return; }
          password = pw.password;
          remember = pw.remember;
          continue;
        }
        this.status("접속 실패");
        await alertBox("접속 실패", hintFor(err));
        return;
      }
    }
  }

  private async disconnectActive() {
    if (this.active) await this.disconnect(this.active);
  }

  async disconnect(t: Tab) {
    if (!t.conn) return;
    if (t.txnPending) {
      const r = await modal("커밋하지 않은 변경", h("p", {}, "끊으면 롤백됩니다."), [
        { label: "취소", value: "cancel" }, { label: "롤백하고 끊기", value: "rollback", kind: "danger" }, { label: "커밋하고 끊기", value: "commit", kind: "primary" },
      ]);
      if (!r || r === "cancel") return;
      if (r === "commit") await api.commit(t.conn.id).catch((e) => toast(errOf(e).message, "error"));
    }
    await api.disconnect(t.conn.id).catch(() => {});
    t.log("접속 끊음");
    t.conn = null;
    t.txnPending = false;
    t.refreshTitle();
    this.refresh();
  }

  // ── 실행 ─────────────────────────────────────────

  private need(): Tab | null {
    const t = this.active;
    if (!t) return null;
    if (!t.conn) {
      toast("먼저 접속하세요", "error");
      return null;
    }
    if (t.running) {
      toast("실행 중입니다 — 끝나거나 중지한 뒤에 하세요", "error");
      return null;
    }
    return t;
  }

  /** 커서 문장(또는 선택 영역) 과 그 시작 위치(UTF-16) */
  private async currentStatement(t: Tab) {
    const text = t.editor.text();
    const sel = t.editor.selection();
    const a = await api.analyze(text, utf16ToByte(text, t.editor.cursor()), sel || undefined, t.conn?.id);
    if (!a.statement) return null;
    const baseText = sel || text;
    const selFrom = sel ? t.editor.view.state.selection.main.from : 0;
    const from = selFrom + byteToUtf16(baseText, a.statement.start);
    const to = selFrom + byteToUtf16(baseText, a.statement.end);
    return { a, from, to };
  }

  async runStatement() {
    const t = this.need();
    if (!t || !t.conn) return;
    const cur = await this.currentStatement(t);
    if (!cur) return toast("실행할 문장이 없습니다", "error");
    const { a, from, to } = cur;
    const stmt = a.statement!;
    let binds: [string, string | null][] = [];
    if (a.binds.length) {
      const b = await bindBox(a.binds);
      if (!b) return;
      binds = b;
    }
    let confirmed = false;
    if (a.confirm) {
      if (!(await confirmBox("확인이 필요한 문장", `${a.confirm}\n\n${stmt.text.slice(0, 400)}`))) return;
      confirmed = true;
    }
    t.editor.flash(from, to);
    t.lastError = undefined;
    await this.running(t, async () => {
      try {
        const r = await api.execute(t.conn!.id, stmt.text, binds, confirmed);
        if (r.outcome.type === "rows") {
          t.lastQuery = { sql: stmt.text, binds };
          this.endGridEdit(t);
        }
        this.showResult(t, r);
      } catch (e) {
        this.showError(t, errOf(e), stmt.text, from);
      }
    });
  }

  private showResult(t: Tab, r: ExecView) {
    t.txnPending = r.txn_pending;
    const ms = `${r.elapsed_ms.toLocaleString()} ms`;
    switch (r.outcome.type) {
      case "rows":
        t.grid.setPage(r.outcome);
        t.show("result");
        this.status(`${r.outcome.rows.length.toLocaleString()}행${r.outcome.has_more ? "+" : ""} · ${ms}`);
        break;
      case "affected":
        t.log(`${r.outcome.rows.toLocaleString()}행 처리 (${ms})${r.txn_pending ? " — 커밋 전" : ""}`, "ok");
        t.show("output");
        this.status(`${r.outcome.rows}행 처리 · ${ms}`);
        break;
      case "done":
        t.log(`완료 (${ms})`, "ok");
        t.show("output");
        this.status(`완료 · ${ms}`);
        break;
      case "skipped":
        t.log(r.outcome.reason);
        break;
    }
    if (r.output.length) {
      t.log("── DBMS_OUTPUT ──", "dim");
      for (const l of r.output) t.log(l);
      t.show("output");
    }
    this.refresh();
  }

  private showError(t: Tab, err: ErrView, sqlText: string, stmtFrom: number) {
    if (err.kind === "cancelled") {
      t.log("취소했습니다", "warn");
      this.status("취소됨");
      return;
    }
    if (err.kind === "connection_lost" || err.kind === "session_closed") {
      t.log(`접속이 끊겼습니다: ${err.message}`, "err");
      t.conn = null;
      t.txnPending = false;
      t.refreshTitle();
      this.refresh();
      toast("접속이 끊겼습니다 — 다시 접속하세요", "error", 6000);
      return;
    }
    t.lastError = `${err.message}\n\n실행한 SQL:\n${sqlText}`;
    t.log(err.message, "err");
    t.show("output");
    this.status(err.ora_code ? `ORA-${String(err.ora_code).padStart(5, "0")}` : "오류");
    if (err.offset != null && err.offset > 0) t.editor.markError(stmtFrom + byteToUtf16(sqlText, err.offset));
  }

  async runScript() {
    const t = this.need();
    if (!t || !t.conn) return;
    const sel = t.editor.selection();
    const script = sel || t.editor.text();
    const scriptFrom = sel ? t.editor.view.state.selection.main.from : 0;
    t.lastError = undefined;
    t.output.replaceChildren();
    t.show("output");
    const run = async (confirmed: boolean): Promise<void> => {
      try {
        let firstErrorLine = 0;
        const s = await api.executeScript(t.conn!.id, script, false, confirmed, (ev) => {
          if (ev.event === "started") {
            this.status(`스크립트 ${ev.index + 1}/${ev.total} 실행 중 (${ev.line}행)`);
            t.log(`[${ev.index + 1}/${ev.total}] ${ev.line}행: ${ev.preview}`, "dim");
          } else if (ev.event === "finished") {
            t.log(`    ${ev.summary} (${ev.elapsed_ms} ms)`, "ok");
            for (const l of ev.output) t.log(`    ${l}`);
          } else if (ev.event === "failed") {
            t.log(`    ${ev.error.message}`, "err");
            if (!firstErrorLine) {
              firstErrorLine = ev.line;
              t.lastError = ev.error.message;
            }
          } else if (ev.event === "rows") {
            t.grid.setPage(ev.page);
          }
        });
        t.txnPending = s.txn_pending;
        t.log(`── 완료: ${s.ok}개 성공, ${s.failed}개 실패${s.stopped ? ", 중단됨" : ""}${s.txn_pending ? " — 커밋 전" : ""}`, s.failed ? "err" : "ok");
        this.status(`스크립트: ${s.ok}/${s.total} 성공`);
        if (firstErrorLine) {
          const doc = t.editor.view.state.doc;
          const baseLine = doc.lineAt(scriptFrom).number - 1;
          const ln = Math.min(doc.lines, baseLine + firstErrorLine);
          t.editor.markError(doc.line(ln).from);
        }
      } catch (e) {
        const err = errOf(e);
        if (err.kind === "confirm_required" && !confirmed) {
          if (await confirmBox("확인이 필요한 문장이 있습니다", err.message)) return run(true);
          return;
        }
        this.showError(t, err, script, scriptFrom);
      }
      this.refresh();
    };
    await this.running(t, () => run(false));
  }

  async explain() {
    const t = this.need();
    if (!t || !t.conn) return;
    const cur = await this.currentStatement(t);
    if (!cur) return toast("실행계획을 볼 문장이 없습니다", "error");
    const stmt = cur.a.statement!;
    await this.running(t, async () => {
      try {
        const lines = await api.explain(t.conn!.id, stmt.text);
        t.plan.textContent = lines.join("\n");
        t.show("plan");
        this.status("실행계획");
      } catch (e) {
        this.showError(t, errOf(e), stmt.text, cur.from);
      }
    });
  }

  /** 실행 중 표시 + 오래 걸리면 중지/버리기 안내 */
  // ── 결과 편집 ─────────────────────────────────────

  endGridEdit(t: Tab) {
    t.editTable = null;
    t.editBtn.textContent = "편집";
    t.editBtn.classList.remove("editing");
    t.applyBtn.hidden = true;
    t.discardBtn.hidden = true;
    t.delRowBtn.hidden = true;
  }

  async toggleGridEdit(t: Tab) {
    if (!t.conn) return toast("먼저 접속하세요", "error");
    if (t.editTable) {
      if (t.grid.changeCount() && !(await confirmBox("편집 끝내기", "적용하지 않은 변경을 버릴까요?", "버리기"))) return;
      t.grid.setEditable(null);
      this.endGridEdit(t);
      return;
    }
    if (!t.lastQuery) return toast("먼저 테이블을 조회하세요 (SELECT … FROM 테이블)", "error");
    if (t.running) return;
    try {
      const e = await api.gridEditable(t.conn.id, t.lastQuery.sql);
      await this.running(t, async () => {
        const r = await api.execute(t.conn!.id, e.sql, t.lastQuery!.binds, false);
        if (r.outcome.type !== "rows") throw new Error("조회 결과가 없습니다");
        t.grid.setPage(r.outcome);
        const cols = r.outcome.columns;
        const rowidCol = cols.findIndex((c) => c.name === "SQLS_ROWID");
        const names = new Set(e.columns.map((c) => c.toUpperCase()));
        const bad = /LOB|^LONG|^RAW|^BFILE|ROWID|XMLTYPE|INTERVAL/i;
        const editable = new Set(cols.map((c, i) => (i !== rowidCol && names.has(c.name.toUpperCase()) && !bad.test(c.type_name) ? i : -1)).filter((i) => i >= 0));
        t.grid.setEditable({ rowidCol, editable });
        t.editTable = e.table;
        t.editBtn.textContent = "편집 끝";
        t.editBtn.classList.add("editing");
        t.applyBtn.hidden = false;
        t.discardBtn.hidden = false;
        t.delRowBtn.hidden = false;
        t.applyBtn.disabled = true;
        t.discardBtn.disabled = true;
        t.show("result");
        t.log(`${e.table} 편집 — 셀 더블클릭(또는 F2)으로 고치고, Delete 로 행 삭제 표시, '적용' 으로 문장 확인 후 실행. 커밋은 따로.`, "dim");
      });
    } catch (err) {
      toast(errOf(err).message, "error");
    }
  }

  async applyGridEdit(t: Tab) {
    if (!t.conn || !t.editTable) return;
    const ch = t.grid.changes();
    if (!ch.edits.length && !ch.deletes.length) return;
    try {
      const plan = await api.gridApply(t.conn.id, t.editTable, ch, true);
      const list = plan.done.map(([s]) => s);
      const shown = list.slice(0, 30).join(";\n") + (list.length > 30 ? `;\n… 외 ${list.length - 30}개` : ";");
      const prod = t.conn.production ? `⚠ 운영 DB (${t.conn.user}@${t.conn.connect_string}) 입니다.\n\n` : "";
      if (!(await confirmBox(`변경 ${list.length}건 실행`, `${prod}${shown}\n\n실행한 뒤 커밋해야 저장됩니다 (롤백으로 되돌릴 수 있습니다).`, "실행", !!prod))) return;
      const r = await api.gridApply(t.conn.id, t.editTable, ch, false);
      t.txnPending = r.txn_pending;
      for (const [s, n] of r.done) t.log(`${s} — ${n}행`, "ok");
      t.log(`${r.done.length}건 적용 — 커밋 전입니다`, "warn");
      t.grid.commitChanges();
      toast(`${r.done.length}건 적용 — 커밋하면 저장됩니다`, "ok");
    } catch (err) {
      t.txnPending = true;
      t.log(errOf(err).message, "err");
      toast(errOf(err).message, "error", 8000);
    }
    this.refresh();
  }

  private async running(t: Tab, f: () => Promise<void>) {
    t.running = true;
    t.runStarted = performance.now();
    this.refresh();
    clearInterval(this.timer);
    this.timer = window.setInterval(() => {
      if (t === this.active && t.running) this.statusRight.textContent = `실행 중 ${((performance.now() - t.runStarted) / 1000).toFixed(1)}s (Esc: 중지)`;
    }, 200);
    try {
      await f();
    } finally {
      t.running = false;
      clearInterval(this.timer);
      t.editor.clearRunning();
      this.refresh();
    }
  }

  async cancel() {
    const t = this.active;
    if (!t?.conn || !t.running) return;
    const id = t.conn.id;
    this.status("취소 요청…");
    api.cancel(id).catch(() => {});
    // 취소 신호가 서버에 닿지 않는 네트워크가 있다 (방화벽이 OOB 를 버림). 몇 초 뒤에도 그대로면 묻는다.
    setTimeout(async () => {
      if (!t.running || t.conn?.id !== id) return;
      const ok = await confirmBox("취소가 되지 않습니다",
        "서버가 취소 요청에 응답하지 않습니다 (네트워크 장비가 취소 신호를 막는 경우가 있습니다).\n\n" +
        "이 세션을 버리고 새로 접속할까요? 커밋하지 않은 변경은 롤백되고, 서버의 SQL 은 끝날 때까지 돌 수 있습니다.",
        "버리고 다시 접속");
      if (!ok || !t.running || t.conn?.id !== id) return;
      const profile = t.conn.profile;
      await api.abandon(id).catch(() => {});
      t.conn = null;
      t.running = false;
      t.txnPending = false;
      t.log("세션을 버렸습니다", "warn");
      await this.connect(t, profile);
    }, 5000);
  }

  private async debugActive() {
    const t = this.need();
    if (!t) return;
    const cur = await this.currentStatement(t);
    const k = cur?.a.statement?.kind;
    const text = cur && (k === "plsql" || k === "other") ? cur.a.statement!.text : "BEGIN\n  NULL;\nEND;";
    t.debugger().openBlock(text);
  }

  private monitorActive() {
    const t = this.need();
    if (!t) return;
    if (!t.conn) return toast("먼저 접속하세요", "error");
    t.monitor().open();
  }

  private analyzeActive(owner?: string, name?: string) {
    const t = this.need();
    if (!t) return;
    if (!t.conn) return toast("먼저 접속하세요", "error");
    t.analysis().open(owner, name);
  }

  private async txn(kind: "commit" | "rollback") {
    const t = this.need();
    if (!t || !t.conn) return;
    try {
      if (kind === "commit") {
        try {
          await api.commit(t.conn.id);
        } catch (e) {
          if (errOf(e).kind !== "confirm_required") throw e;
          if (!(await confirmBox("운영 DB 커밋", `${t.conn.user}@${t.conn.connect_string}\n\n${errOf(e).message}`, "커밋"))) return;
          await api.commit(t.conn.id, true);
        }
      } else await api.rollback(t.conn.id);
      t.txnPending = false;
      t.log(kind === "commit" ? "커밋했습니다" : "롤백했습니다", "ok");
      this.status(kind === "commit" ? "커밋" : "롤백");
    } catch (e) {
      toast(errOf(e).message, "error");
    }
    this.refresh();
  }

  private async replaceStatement(sql: string) {
    const t = this.active;
    if (!t) return;
    const cur = await this.currentStatement(t);
    const clean = sql.replace(/;\s*$/, "");
    if (!cur) return t.editor.insert(clean);
    t.editor.view.dispatch({ changes: { from: cur.from, to: cur.to, insert: clean } });
    t.editor.focus();
  }

  private aiContext() {
    const t = this.active;
    const sel = t?.editor.selection();
    let sql = sel || undefined;
    if (!sql && t) {
      // 커서 문장을 동기로 구할 수 없으니, 문서가 짧으면 전체, 길면 커서 줄 주변
      const text = t.editor.text();
      sql = text.length < 20000 ? text : text.slice(Math.max(0, t.editor.cursor() - 4000), t.editor.cursor() + 4000);
    }
    return { sessionId: t?.conn?.id, sql: sql?.trim() || undefined, error: t?.lastError, tables: [...this.pickedTables] };
  }

  // ── 객체 탐색기 / 자동완성 ─────────────────────────

  private async loadBrowser() {
    const t = this.active;
    if (!t?.conn) return;
    try {
      const schemas = await api.listSchemas(t.conn.id);
      const me = t.conn.user;
      this.browserOwner.replaceChildren(...schemas.map((s) => h("option", { value: s, selected: s === me }, s)));
      await this.loadObjects();
    } catch (e) {
      this.browserList.replaceChildren(h("div", { class: "empty" }, errOf(e).message));
    }
  }

  private async loadObjects() {
    const t = this.active;
    if (!t?.conn) return;
    const f = this.browserFilter.value.trim();
    try {
      const objs = await api.listObjects(t.conn.id, this.browserOwner.value || undefined, this.browserType.value || undefined,
        f ? (f.includes("%") ? f : `%${f}%`).toUpperCase() : undefined);
      this.browserObjs = objs;
      this.renderObjects();
    } catch (e) {
      this.browserList.replaceChildren(h("div", { class: "empty" }, errOf(e).message));
    }
  }

  private renderObjects() {
    const t = this.active;
    const frag = document.createDocumentFragment();
    for (const o of this.browserObjs.slice(0, 3000)) {
      const q = `${o.owner}.${o.name}`;
      const tableLike = o.object_type === "TABLE" || o.object_type === "VIEW" || o.object_type === "MATERIALIZED VIEW";
      const check = tableLike ? h("input", { type: "checkbox", checked: this.pickedTables.has(q), title: "AI 문맥에 포함",
        onclick: (e: Event) => { e.stopPropagation(); (e.target as HTMLInputElement).checked ? this.pickedTables.add(q) : this.pickedTables.delete(q); } }) : null;
      const row = h("div", { class: `obj ${o.status === "INVALID" ? "invalid" : ""}`, title: `${o.object_type} · ${o.status} · ${o.last_ddl_time}` },
        check, h("span", {}, o.name), this.browserType.value ? "" : h("small", { class: "obj-type" }, o.object_type));
      row.addEventListener("click", () => this.showObject(o));
      if (tableLike) row.addEventListener("contextmenu", (e) => { e.preventDefault(); this.popupColumns(`"${o.owner}"."${o.name}"`, { x: e.clientX, y: e.clientY }); });
      row.addEventListener("dblclick", () => t?.editor.insert(o.owner === t.conn?.user ? o.name : q));
      frag.append(row);
    }
    if (this.browserObjs.length > 3000) frag.append(h("div", { class: "empty" }, `… ${this.browserObjs.length - 3000}개 더 (필터를 좁히세요)`));
    if (!this.browserObjs.length) frag.append(h("div", { class: "empty" }, "없음"));
    this.browserList.replaceChildren(frag);
  }

  /** 스키마 브라우저 — 객체 하나의 상세 (구조 탭). 탭은 누를 때 읽는다 (사전 조회 세션). */
  private async showObject(o: { owner: string; name: string; object_type: string; status?: string }) {
    const t = this.active;
    if (!t?.conn) return;
    const id = t.conn.id;
    const type = o.object_type.toUpperCase();
    const q = `"${o.owner}"."${o.name}"`;
    const tableLike = ["TABLE", "VIEW", "MATERIALIZED VIEW"].includes(type);
    const code = ["PACKAGE", "PACKAGE BODY", "PROCEDURE", "FUNCTION", "TRIGGER", "TYPE", "TYPE BODY"].includes(type);
    let sections: { key: string; label: string; single: boolean }[];
    let desc: TableDesc | null = null;
    try {
      [sections, desc] = await Promise.all([
        invoke<{ key: string; label: string; single: boolean }[]>("object_sections", { objectType: type }),
        tableLike ? api.describe(id, q) : Promise.resolve(null),
      ]);
    } catch (e) {
      return toast(errOf(e).message, "error");
    }
    const tabs: { key: string; label: string; single?: boolean }[] = [
      ...(tableLike ? [{ key: "#columns", label: `컬럼 ${desc?.columns.length ?? ""}` }] : []),
      ...(code ? [{ key: "#source", label: "소스" }] : []),
      ...sections,
    ];
    const body = h("div", { class: "ob-body" });
    const nav = h("nav", { class: "ob-tabs" });
    const cache = new Map<string, HTMLElement>();
    let current = "";
    const open = async (key: string) => {
      current = key;
      for (const b of nav.children) b.classList.toggle("on", (b as HTMLElement).dataset.k === key);
      const hit = cache.get(key);
      if (hit) return body.replaceChildren(hit);
      body.replaceChildren(h("div", { class: "hint" }, "읽는 중…"));
      let el: HTMLElement;
      try {
        if (key === "#columns") el = this.columnsTable(desc!);
        else if (key === "#source") el = h("pre", { class: "ob-src" }, await api.getDdl(id, type, q));
        else {
          const sec = sections.find((x) => x.key === key)!;
          const page = await invoke<RowPage>("object_section", { id, owner: o.owner, name: o.name, section: key });
          el = this.sectionTable(page, sec.single, key);
        }
      } catch (e) {
        el = h("div", { class: "err" }, errOf(e).message);
      }
      cache.set(key, el);
      if (current === key) body.replaceChildren(el);
    };
    for (const tb of tabs) nav.append(h("button", { "data-k": tb.key, onclick: () => open(tb.key) }, tb.label));

    const cols = desc ? desc.columns.map((c) => c.name).join(", ") : "*";
    const btn = (label: string, f: () => void, title = "", cls = "") => h("button", { class: `small ${cls}`, title, onclick: f }, label);
    const actions: (HTMLElement | string)[] = [];
    if (tableLike) actions.push(
      btn("데이터", () => this.previewData(t, `${q}`), "앞 200행을 결과 그리드로 (편집하려면 결과의 '편집')", "primary"),
      btn("SELECT 넣기", () => t.editor.insert(`SELECT ${cols}\n  FROM ${o.owner}.${o.name}\n WHERE ROWNUM <= 100`)),
      btn("팝업", () => desc && this.openColumns(desc), "떠 있는 컬럼 창 (편집기에서 F4)"),
    );
    if (type === "TABLE") actions.push(btn("가져오기", () => openImport({
      id, owner: o.owner, table: o.name,
      done: (r) => { t.txnPending = r.txn_pending; if (r.inserted) t.log(`${o.owner}.${o.name}: ${r.inserted}행 가져옴 — 커밋 전`, "warn"); this.refresh(); },
    }), "엑셀·CSV 파일의 데이터를 이 테이블에 넣는다 (커밋은 따로)"));
    if (type === "TABLE" || type === "VIEW" || type === "PROCEDURE" || type === "FUNCTION")
      actions.push(btn("흐름도", () => this.openDfd(`${o.owner}.${o.name}`, type === "TABLE" || type === "VIEW" ? "up" : "both"),
        tableLike ? "이 테이블로 데이터가 어디서 오는지 (쓰는 처리 ← 그 입력 테이블) — 별도 창, 분석 결과 필요" : "이 처리의 입력·출력 테이블 — 별도 창, 분석 결과 필요"));
    if (type === "SEQUENCE") actions.push(btn("NEXTVAL 넣기", () => t.editor.insert(`${o.owner}.${o.name}.NEXTVAL`)));
    actions.push(btn(code ? "새 탭에서 열기" : "DDL", async () => this.newTab(true, await api.getDdl(id, type, q).catch((e) => `-- ${errOf(e).message}`))));
    if (code) {
      actions.push(btn("컴파일", () => this.compileObject(t, o.owner, o.name, type, () => { cache.delete("errors"); open("errors"); }), "ALTER … COMPILE — 오류는 '컴파일 오류' 탭"));
      actions.push(btn("디버그", () => t.debugger().openUnit(o.owner, o.name, type)));
      if (type !== "TYPE") actions.push(btn("분석", () => this.analyzeActive(o.owner, o.name), "PL/SQL 분석 화면에서 연다"));
    }
    t.desc.replaceChildren(
      h("div", { class: "desc-head" },
        h("strong", {}, `${type} ${o.owner}.${o.name}`),
        o.status === "INVALID" ? h("span", { class: "ob-invalid", title: "컴파일이 필요합니다" }, " INVALID") : "",
        desc?.comment ? h("span", {}, ` — ${desc.comment}`) : "",
        desc?.num_rows ? h("small", {}, ` · 통계 ${Number(desc.num_rows).toLocaleString()}행 (${desc.last_analyzed ?? "?"})`) : ""),
      h("div", { class: "desc-actions" }, ...actions),
      nav, body);
    t.show("desc");
    if (tabs.length) open(tabs[0].key);
  }

  private columnsTable(d: TableDesc) {
    const rows = d.columns.map((c, i) => h("tr", {},
      h("td", { class: "num" }, String(i + 1)),
      h("td", {}, d.primary_key.includes(c.name) ? h("span", { class: "cp-pk" }, "PK") : ""), h("td", {}, c.name), h("td", {}, c.data_type),
      h("td", {}, c.nullable ? "" : "NOT NULL"), h("td", {}, c.default ?? ""), h("td", {}, c.comment ?? "")));
    return h("table", { class: "desc-table" }, h("thead", {}, h("tr", {}, ...["#", "", "컬럼", "형식", "", "기본값", "설명"].map((x) => h("th", {}, x)))), h("tbody", {}, ...rows));
  }

  /** 탭 내용 표. 한 행짜리는 세로로. 객체 이름 칸(OWNER.NAME)은 더블클릭하면 그 객체로 간다 */
  private sectionTable(page: RowPage, single: boolean, key: string) {
    if (!page.rows.length) return h("div", { class: "hint" }, "없음");
    if (single) {
      const r = page.rows[0];
      return h("table", { class: "desc-table ob-kv" }, h("tbody", {}, ...page.columns.map((c, i) => h("tr", {}, h("th", {}, c.name), h("td", {}, r[i] ?? "")))));
    }
    const names = page.columns.map((c) => c.name);
    const objCol = names.findIndex((n) => ["객체", "테이블", "트리거", "동의어"].includes(n));
    const typeCol = names.indexOf("종류");
    const defaultType: Record<string, string> = { referenced_by: "TABLE", triggers: "TRIGGER", synonyms: "SYNONYM", trigger: "TABLE", synonym: "TABLE" };
    const tbody = h("tbody", {}, ...page.rows.map((r) => {
      const tr = h("tr", {}, ...r.map((v) => h("td", { title: v && v.length > 60 ? v : "" }, v ?? "")));
      const ref = objCol >= 0 ? r[objCol] : null;
      const otype = typeCol >= 0 && key !== "constraints" ? r[typeCol] : defaultType[key];
      if (ref && ref.includes(".") && otype) {
        tr.classList.add("ob-link");
        tr.title = `더블클릭: ${ref} 열기`;
        tr.ondblclick = () => { const [owner, ...rest] = ref.split("."); this.showObject({ owner, name: rest.join("."), object_type: otype }); };
      }
      return tr;
    }));
    return h("table", { class: "desc-table" }, h("thead", {}, h("tr", {}, ...names.map((n) => h("th", {}, n)))), tbody);
  }

  /** 데이터 흐름도 별도 창 */
  private openDfd(center?: string, dir?: string) {
    const t = this.active;
    if (!t?.conn) return toast("먼저 접속하세요 — 그 접속의 분석 결과로 그립니다", "error");
    invoke("open_dfd_window", { id: t.conn.id, center: center ?? null, dir: dir ?? null }).catch((e) => toast(errOf(e).message, "error"));
  }

  /** 앞 200행을 이 탭의 결과 그리드로 */
  private async previewData(t: Tab, q: string) {
    if (!t.conn) return;
    const sql = `SELECT * FROM ${q} WHERE ROWNUM <= 200`;
    await this.running(t, async () => {
      try {
        const r = await api.execute(t.conn!.id, sql, [], false);
        if (r.outcome.type === "rows") { t.lastQuery = { sql, binds: [] }; this.endGridEdit(t); }
        this.showResult(t, r);
      } catch (e) {
        this.showError(t, errOf(e), sql, 0);
      }
    });
  }

  /** ALTER … COMPILE (운영 DB 면 서버가 확인을 요구한다) */
  private async compileObject(t: Tab, owner: string, name: string, type: string, after: () => void) {
    if (!t.conn) return;
    const base = type.replace(/ BODY$/, "");
    const sql = `ALTER ${base} "${owner}"."${name}" COMPILE${type.endsWith(" BODY") ? " BODY" : ""}`;
    const run = (confirmed: boolean) => api.execute(t.conn!.id, sql, [], confirmed);
    try {
      try {
        await run(false);
      } catch (e) {
        if (errOf(e).kind !== "confirm_required") throw e;
        if (!(await confirmBox("컴파일", `${errOf(e).message}\n\n${sql}`, "컴파일"))) return;
        await run(true);
      }
      toast(`컴파일했습니다 — 오류가 있으면 '컴파일 오류' 탭에 나옵니다`, "ok");
    } catch (e) {
      toast(errOf(e).message, "error", 6000);
    }
    after();
    this.loadObjects();
  }

  // ── 컬럼 팝업 ────────────────────────────────────

  private openColumns(d: TableDesc, at?: { x: number; y: number }, alias?: string) {
    showColumns(d, {
      at, alias,
      insert: (s) => { const t = this.active; if (!t) return; t.editor.insert(s); t.editor.view.focus(); },
      ddl: async () => { const t = this.active; if (t?.conn) this.newTab(true, await api.getDdl(t.conn.id, d.object_type, `"${d.owner}"."${d.name}"`)); },
    });
  }

  /** 이름으로 찾아 팝업 (사전 조회 세션으로 — 작업 세션에서 쿼리가 돌고 있어도 기다리지 않는다) */
  private async popupColumns(name: string, at?: { x: number; y: number }, alias?: string) {
    const t = this.active;
    if (!t?.conn) return toast("먼저 접속하세요", "error");
    try {
      this.openColumns(await api.describe(t.conn.id, name), at, alias);
    } catch (e) {
      toast(errOf(e).message, "error");
    }
  }

  /** F4: 커서 위치의 테이블(또는 별칭·스키마.테이블·별칭.컬럼)의 컬럼 팝업 */
  async describeAtCursor() {
    const t = this.active;
    if (!t?.conn) return toast("먼저 접속하세요", "error");
    const view = t.editor.view;
    const pos = view.state.selection.main.head;
    const line = view.state.doc.lineAt(pos);
    const s = line.text;
    const ch = /[\w$#."]/;
    let a = pos - line.from, b = a;
    while (a > 0 && ch.test(s[a - 1])) a--;
    while (b < s.length && ch.test(s[b])) b++;
    const raw = s.slice(a, b);
    const parts = raw.replace(/"/g, "").split(".").filter(Boolean);
    if (!parts.length) return toast("커서를 테이블 이름(또는 별칭) 위에 두고 F4", "info");
    const cur = Math.min(parts.length - 1, (s.slice(a, pos).match(/\./g) ?? []).length);
    const doc = view.state.doc;
    const around = doc.sliceString(Math.max(0, pos - 30000), Math.min(doc.length, pos + 30000));
    const aliases = aliasMap(around);
    const word = parts[cur];
    // 시도 순서: 스키마.테이블 → 별칭 → (별칭.컬럼이면) 앞쪽 별칭/테이블 → 그 낱말
    const tries: { name: string; alias?: string }[] = [];
    if (cur >= 1) tries.push({ name: parts.slice(cur - 1, cur + 1).join(".") });
    if (aliases.has(word.toUpperCase())) tries.push({ name: aliases.get(word.toUpperCase())!, alias: word });
    if (cur >= 1) {
      const p = parts[cur - 1];
      tries.push(aliases.has(p.toUpperCase()) ? { name: aliases.get(p.toUpperCase())!, alias: p } : { name: p });
    }
    tries.push({ name: word });
    if (cur === 0 && parts.length > 1) tries.push({ name: parts.slice(0, 2).join(".") });
    const c = view.coordsAtPos(pos);
    const at = c ? { x: c.left, y: c.bottom } : undefined;
    let last: unknown = null;
    for (const x of tries) {
      try {
        const d = await api.describe(t.conn.id, x.name);
        return this.openColumns(d, at, x.alias);
      } catch (e) {
        last = e;
      }
    }
    toast(`'${raw}' 를 테이블·뷰로 찾지 못했습니다${last ? ` — ${errOf(last).message}` : ""}`, "error");
  }

  // ── 파일 ─────────────────────────────────────────

  async openFile() {
    const path = await openDialog({ multiple: false, filters: [{ name: "SQL", extensions: ["sql", "pls", "pks", "pkb", "prc", "fnc", "trg", "vw", "txt"] }] });
    if (!path || Array.isArray(path)) return;
    await this.openPath(path);
  }

  /** 프로젝트 패널 모듈을 불러와 붙인다 (한 번만) */
  private loadGit(): Promise<GitPanel> {
    this.gitLoading ??= import("./git").then(({ GitPanel }) => {
      this.git = new GitPanel({
        openPath: (p) => this.openPath(p),
        tabsUnder: (root) => {
          const r = normPath(root) + "/";
          return this.tabs.filter((t) => t.filePath && normPath(t.filePath).startsWith(r)).map((t) => ({
            path: t.filePath!, dirty: t.dirty, reload: () => this.reloadTab(t),
          }));
        },
        onState: (text, title) => { this.gitBadge.textContent = text; this.gitBadge.title = title; },
      }, this.gitPane);
      return this.git;
    });
    return this.gitLoading;
  }

  /** F8: 실행 이력 */
  showHistory() {
    const t = this.active;
    openHistory({
      profile: t?.conn?.profile,
      insert: (sql) => {
        const cur = this.active;
        if (!cur) return void this.newTab(true, sql);
        const v = cur.editor.view;
        const line = v.state.doc.lineAt(v.state.selection.main.head);
        cur.editor.insert(line.text.trim() ? `\n${sql}\n` : sql);
        v.focus();
      },
      newTab: (sql) => void this.newTab(true, sql),
    });
  }

  /** 파일을 탭으로 연다. 이미 열려 있으면 그 탭으로 */
  async openPath(path: string) {
    const open = this.tabs.find((t) => t.filePath && normPath(t.filePath) === normPath(path));
    if (open) return this.activate(open);
    try {
      const f = await api.readFile(path);
      const cur = this.active;
      const t = cur && !cur.dirty && !cur.editor.text().trim() && !cur.filePath ? cur : this.newTab(true);
      t.editor.setText(f.text);
      t.filePath = path;
      t.encoding = f.encoding;
      t.eol = f.text.includes("\r\n") ? "\r\n" : "\n";
      t.setDirty(false);
      t.refreshTitle();
      this.status(`열었습니다 (${f.encoding}${t.eol === "\r\n" ? " · CRLF" : ""})`);
    } catch (e) {
      toast(errOf(e).message, "error");
    }
  }

  /** 디스크에서 다시 읽는다 (Git 풀·브랜치 전환·되돌리기 뒤) */
  private async reloadTab(t: Tab) {
    if (!t.filePath) return;
    const f = await api.readFile(t.filePath);
    const eol = f.text.includes("\r\n") ? "\r\n" : "\n";
    if (f.text.replace(/\r\n/g, "\n") === t.editor.text() && eol === t.eol) return;
    t.editor.setText(f.text);
    t.encoding = f.encoding;
    t.eol = eol;
    t.setDirty(false);
    t.refreshTitle();
  }

  async save() {
    const t = this.active;
    if (!t) return;
    let path = t.filePath;
    if (!path) {
      path = await saveDialog({ filters: [{ name: "SQL", extensions: ["sql"] }], defaultPath: "query.sql" });
      if (!path) return;
    }
    try {
      const text = t.eol === "\r\n" ? t.editor.text().replace(/\r?\n/g, "\r\n") : t.editor.text();
      await api.writeFile(path, text, t.encoding);
      t.filePath = path;
      t.setDirty(false);
      t.refreshTitle();
      this.status(`저장했습니다 (${t.encoding})`);
      this.git?.refreshSoon();
    } catch (e) {
      toast(errOf(e).message, "error");
    }
  }

  // ── 상태 표시 ────────────────────────────────────

  private showCompletionStatus(s: { phase: string; objects: number; columns: number; error?: string | null }) {
    const label: Record<string, string> = {
      connecting: "자동완성 준비 중…", objects: "자동완성: 객체 읽음", columns: "자동완성: 컬럼 읽음",
      synonyms: "자동완성: 동의어 읽음", keys: "자동완성: 키 읽음", ready: "자동완성", error: "자동완성 꺼짐",
    };
    const n = s.objects ? ` · 객체 ${s.objects.toLocaleString()} · 컬럼 ${s.columns.toLocaleString()}` : "";
    this.compBadge.textContent = `${label[s.phase] ?? s.phase}${n}`;
    this.compBadge.title = s.error ? `메타 세션 오류: ${s.error}` : "자동완성 캐시 — Ctrl+Space 로 직접 띄운다";
    this.compBadge.classList.toggle("err", s.phase === "error");
  }

  status(text: string) {
    this.statusLeft.textContent = text;
  }

  refresh() {
    const t = this.active;
    const c = t?.conn;
    const busy = !!t?.running;
    this.btn.connect.disabled = busy;
    this.btn.disconnect.disabled = !c || busy;
    for (const k of ["run", "script", "explain", "debug", "analyze", "monitor"]) this.btn[k].disabled = !c || busy;
    this.btn.cancel.disabled = !busy;
    this.btn.commit.disabled = !c || busy || !t?.txnPending;
    this.btn.rollback.disabled = !c || busy || !t?.txnPending;
    this.txnBadge.hidden = !t?.txnPending;
    this.connBadge.textContent = c ? `${c.production ? "운영 · " : ""}${c.user}@${c.connect_string}${c.read_only ? " · 읽기 전용" : ""}` : "접속 안 됨";
    this.connBadge.classList.toggle("prod", !!c?.production);
    this.connBadge.style.background = c?.color ?? "";
    this.connBadge.classList.toggle("on", !!c);
    if (!busy) this.statusRight.textContent = c ? c.server_version : "";
    for (const x of this.tabs) x.refreshTitle();
  }

  // ── 설정 ─────────────────────────────────────────

  async settings() {
    const body = h("div", { class: "settings" });
    const nav = h("nav", { class: "settings-nav" });
    const pages: Record<string, () => Promise<HTMLElement>> = {
      "접속": () => this.profilesPage(),
      "AI": () => this.providersPage(),
      "MCP": () => this.mcpPage(),
      "편집기": async () => this.editorPage(),
      "정보": async () => this.aboutPage(),
    };
    const content = h("div", { class: "settings-content" });
    const showPage = async (name: string) => {
      for (const b of nav.children) b.classList.toggle("active", b.textContent === name);
      content.replaceChildren(await pages[name]());
    };
    for (const name of Object.keys(pages)) nav.append(h("button", { onclick: (e: Event) => { e.preventDefault(); showPage(name); } }, name));
    body.append(nav, content);
    showPage("접속");
    await modal("설정", body, [{ label: "닫기", value: true, kind: "primary" }], { wide: true });
    await this.reloadProfiles();
    await this.ai.load();
  }

  private async aboutPage() {
    const info = await api.appInfo();
    return h("div", { class: "about" },
      h("p", {}, h("strong", {}, `SQLStudio ${info.version}`)),
      h("p", { class: "hint" }, `설정 파일: ${info.config_path}`),
      h("h4", {}, "오픈소스 라이선스"),
      h("p", { class: "hint" }, "이 프로그램에 포함된 오픈소스 구성 요소와 라이선스입니다. 설치 폴더의 THIRD_PARTY_NOTICES.html 과 같은 내용입니다."),
      h("iframe", { class: "about-notices", src: "/third-party-notices.html", title: "오픈소스 라이선스 고지" }));
  }

  private editorPage() {
    const p = loadPrefs();
    const sel = (cur: string, opts: [string, string][]) =>
      h("select", {}, ...opts.map(([v, l]) => h("option", { value: v, selected: v === cur }, l))) as HTMLSelectElement;
    const kw = sel(p.keywordCase, [["upper", "대문자 (SELECT)"], ["lower", "소문자 (select)"], ["preserve", "그대로"]]);
    const ind = sel(String(p.indent), [["2", "2칸"], ["4", "4칸"]]);
    const logic = sel(p.logicalBefore ? "before" : "after", [["before", "줄 앞 (AND x = 1)"], ["after", "줄 끝 (x = 1 AND)"]]);
    const width = h("input", { type: "number", min: "20", max: "200", value: String(p.expressionWidth) }) as HTMLInputElement;
    const preview = h("pre", { class: "format-preview" });
    const sample = "select o.id, c.name, sum(o.amt) total from orders o join customers c on c.id = o.cust_id where o.dt >= trunc(sysdate) - 7 and c.grade in ('A','B') group by o.id, c.name order by total desc";
    const read = (): FormatPrefs => ({
      keywordCase: kw.value as FormatPrefs["keywordCase"],
      indent: ind.value === "4" ? 4 : 2,
      expressionWidth: Math.max(20, Math.min(200, Number(width.value) || DEFAULT_PREFS.expressionWidth)),
      logicalBefore: logic.value === "before",
    });
    const update = () => {
      const prefs = read();
      savePrefs(prefs);
      const r = formatSql(sample, prefs);
      preview.textContent = r.ok ? r.text : r.reason;
    };
    for (const el of [kw, ind, logic, width]) el.addEventListener("change", update);
    update();
    // 실행 이력 켜기/끄기·지우기
    const histOn = h("input", { type: "checkbox" });
    const histDir = h("small", {});
    const histBox = h("div", {},
      h("label", { class: "check" }, histOn, "실행한 SQL 을 이 PC 에 남기기 (바인드 값은 남기지 않음)"), histDir,
      h("div", { class: "row" }, h("button", { onclick: async (e: Event) => {
        e.preventDefault();
        if (!(await confirmBox("실행 이력 지우기", "남겨 둔 실행 이력을 모두 지웁니다. 되돌릴 수 없습니다.", "지우기"))) return;
        await invoke("history_clear").then(() => toast("지웠습니다", "ok")).catch((err) => toast(errOf(err).message, "error"));
      } }, "이력 모두 지우기")));
    invoke<{ enabled: boolean; keep_months: number; dir: string }>("history_settings").then((s) => {
      histOn.checked = s.enabled;
      histDir.textContent = `위치: ${s.dir} · 최근 ${s.keep_months}달만 남김`;
    }).catch(() => {});
    histOn.onchange = () => invoke("set_history_enabled", { enabled: histOn.checked }).catch((err) => toast(errOf(err).message, "error"));
    return h("div", { class: "form" },
      h("h3", {}, "SQL 포맷 (Ctrl+Shift+F)"),
      field("키워드", kw), field("들여쓰기", ind), field("AND / OR 위치", logic),
      field("괄호 식 한 줄 최대 길이", width, "이보다 길면 여러 줄로 나눕니다"),
      h("small", {}, "바꾸면 바로 저장됩니다. 포맷 결과가 공백·대소문자 말고 원본과 달라지면 적용하지 않습니다. PL/SQL 블록은 아직 포맷하지 않습니다."),
      field("미리 보기", preview),
      h("h3", {}, "여러 곳 편집 (열 편집)"),
      h("pre", { class: "format-preview" },
        "Alt + 드래그          사각형(열) 선택\n" +
        "Alt + Shift + 방향키   키보드로 사각형 선택\n" +
        "Ctrl + 클릭           커서 추가\n" +
        "Ctrl + D              같은 단어 하나 더 선택\n" +
        "Esc                   커서 하나로"),
      h("h3", {}, "SQL 실행 이력 (F8)"),
      histBox,
      h("h3", {}, "코드 조각"),
      this.snippetEditor(),
    );
  }

  /** 코드 조각 목록·편집·가져오기/내보내기 */
  private snippetEditor() {
    let items: Snip[] = snippets().map((s) => ({ ...s }));
    let cur = -1;
    const listEl = h("div", { class: "snip-list" });
    const key = h("input", { placeholder: "줄임말 (예: sel)", style: "width:120px" });
    const desc = h("input", { placeholder: "설명", style: "flex:1" });
    const body = h("textarea", { rows: 7, class: "snip-body", placeholder: "SELECT ${*}\n  FROM ${table}" });
    const persist = async (next: Snip[], msg = "저장했습니다") => {
      try { await saveSnippets(next); items = next.map((s) => ({ ...s })); render(); toast(msg, "ok"); }
      catch (err) { toast(errOf(err).message, "error"); }
    };
    const render = () => {
      listEl.replaceChildren(...items.map((s, i) => {
        const b = h("button", { class: `list-item${i === cur ? " active" : ""}`, onclick: (e: Event) => { e.preventDefault(); pick(i); } },
          h("code", {}, s.key), h("small", {}, ` ${s.desc}`));
        return b;
      }));
    };
    const pick = (i: number) => {
      cur = i;
      const s = items[i];
      key.value = s?.key ?? ""; desc.value = s?.desc ?? ""; body.value = s?.body ?? "";
      render();
    };
    const btn = (label: string, f: () => void, cls = "") => h("button", { class: cls, onclick: (e: Event) => { e.preventDefault(); f(); } }, label);
    const box = h("div", { class: "snip" },
      h("small", {}, "편집기에서 줄임말을 치면 자동완성에 뜹니다. 고르면 틀이 들어가고 ${이름} 칸을 Tab 으로 옮겨 다니며 채웁니다 (이름이 같은 칸은 함께 바뀜)."),
      h("div", { class: "snip-main" }, listEl,
        h("div", { class: "snip-form" },
          h("div", { class: "row" }, key, desc), body,
          h("div", { class: "row" },
            btn("저장", () => {
              const k = key.value.trim();
              if (!k || !body.value.trim()) return toast("줄임말과 내용을 채우세요", "error");
              const next = items.slice();
              const dup = next.findIndex((s, i) => i !== cur && s.key.toLowerCase() === k.toLowerCase());
              if (dup >= 0) return toast(`'${k}' 는 이미 있습니다`, "error");
              const s = { key: k, desc: desc.value.trim(), body: body.value };
              if (cur >= 0) next[cur] = s; else { next.push(s); cur = next.length - 1; }
              persist(next);
            }, "primary"),
            btn("새 조각", () => pick(-1)),
            btn("삭제", async () => {
              if (cur < 0) return;
              if (!(await confirmBox("조각 삭제", `'${items[cur].key}' 를 지울까요?`, "삭제"))) return;
              const next = items.filter((_, i) => i !== cur); cur = -1; key.value = desc.value = body.value = "";
              persist(next, "지웠습니다");
            }, "danger")))),
      h("div", { class: "row" },
        btn("내보내기", async () => {
          const path = await saveDialog({ filters: [{ name: "JSON", extensions: ["json"] }], defaultPath: "sqlstudio-snippets.json" });
          if (!path) return;
          await api.writeFile(path, JSON.stringify({ snippets: items }, null, 2), "utf-8").then(() => toast("내보냈습니다 — 팀원은 '가져오기'로 씁니다", "ok")).catch((err) => toast(errOf(err).message, "error"));
        }),
        btn("가져오기", async () => {
          const path = await openDialog({ multiple: false, filters: [{ name: "JSON", extensions: ["json"] }] });
          if (!path || Array.isArray(path)) return;
          try {
            const got = parseSnippets((await api.readFile(path)).text);
            // 같은 줄임말은 가져온 것으로 바꾸고 나머지는 더한다
            const next = items.filter((s) => !got.some((g) => g.key.toLowerCase() === s.key.toLowerCase())).concat(got);
            persist(next, `${got.length}개 가져왔습니다`);
          } catch (err) { toast(`가져오지 못했습니다: ${errOf(err).message}`, "error"); }
        }),
        btn("기본 조각으로", async () => {
          if (!(await confirmBox("기본 조각으로", "지금 조각을 지우고 기본 조각으로 되돌립니다.", "되돌리기"))) return;
          cur = -1; persist(DEFAULT_SNIPPETS.map((s) => ({ ...s })), "기본 조각으로 되돌렸습니다");
        })));
    render();
    return box;
  }

  private async profilesPage() {
    const list = h("div", { class: "list" });
    const form = h("div", { class: "form" });
    // tnsnames.ora 별칭 — 접속 문자열 칸에서 고른다
    const tns = await api.tnsList().catch(() => ({ entries: [] } as TnsView));
    const tnsList = h("datalist", { id: "tns-aliases" }, ...tns.entries.map((e) => h("option", { value: e.alias }, [e.host, e.port].filter(Boolean).join(":") + (e.service ? `/${e.service}` : ""))));
    const where = (e: TnsEntry) => `${e.host ?? "?"}${e.port ? `:${e.port}` : ""}${e.service ? `/${e.service}` : ""}`;
    const edit = (p?: Profile) => {
      const name = h("input", { value: p?.name ?? "" });
      const user = h("input", { value: p?.user ?? "" });
      const cs = h("input", { value: p?.connect_string ?? "", placeholder: tns.entries.length ? `TNS 별칭 (${tns.entries.length}개에서 고르기) 또는 host:1521/SERVICE` : "host:1521/SERVICE 또는 TNS 별칭" });
      cs.setAttribute("list", "tns-aliases");
      const csHint = h("small", { class: "tns-hint" });
      const showHint = () => {
        const e = tns.entries.find((x) => x.alias === cs.value.trim().toUpperCase());
        csHint.textContent = e ? `TNS ${e.alias} → ${where(e)}` : "";
      };
      cs.addEventListener("input", () => {
        showHint();
        // 새 접속에서 별칭을 고르면 이름도 채운다
        const e = tns.entries.find((x) => x.alias === cs.value.trim().toUpperCase());
        if (e && !p && !name.value.trim()) name.value = e.alias;
      });
      showHint();
      const ro = h("input", { type: "checkbox", checked: !!p?.read_only });
      const prod = h("input", { type: "checkbox", checked: !!p?.production });
      const color = h("input", { type: "color", value: p?.color ?? "#3a7bd5" });
      const useColor = h("input", { type: "checkbox", checked: !!p?.color });
      const timeout = h("input", { type: "number", min: "0", value: p?.call_timeout_secs ?? "" , placeholder: "없음" });
      form.replaceChildren(
        field("이름", name), field("사용자", user), field("접속 문자열", cs, "11g 는 Instant Client 가 필요합니다 (19c 권장)"), csHint,
        h("label", { class: "check" }, ro, "읽기 전용 (SELECT 만, 서버에서도 막음)"),
        h("label", { class: "check", title: "INSERT·UPDATE·DELETE·DDL·PL/SQL 블록·COMMIT 을 실행 전에 항상 확인합니다" }, prod, "운영 DB (바꾸는 문장·커밋 전에 항상 확인)"),
        h("label", { class: "check" }, useColor, "탭 색 표시 (운영 DB 는 빨강 권장)", color),
        field("호출 시간 상한(초)", timeout, "Oracle Client 18 이상에서만 동작"),
        h("small", {}, "비밀번호는 설정 파일에 쓰지 않습니다. 접속할 때 묻거나(‘비밀번호 저장’ 을 고르면 OS 자격 증명 관리자에), 환경변수 SQLSTUDIO_PW_<이름> 을 씁니다."),
        p?.has_saved_password ? h("div", { class: "row" }, h("span", {}, "저장된 비밀번호가 있습니다"),
          h("button", { class: "small", onclick: async (e: Event) => {
            e.preventDefault();
            await api.forgetPassword(p.name).catch((err) => toast(errOf(err).message, "error"));
            toast("저장된 비밀번호를 지웠습니다", "ok");
            await refreshList();
            (e.target as HTMLElement).parentElement?.remove();
          } }, "지우기")) : "",
        h("div", { class: "row" },
          h("button", { class: "primary", onclick: async (e: Event) => {
            e.preventDefault();
            try {
              await api.saveProfile({
                name: name.value.trim(), user: user.value.trim(), connect_string: cs.value.trim(),
                read_only: ro.checked, production: prod.checked, color: useColor.checked ? color.value : null,
                call_timeout_secs: timeout.value ? Number(timeout.value) : null,
              }, p?.name);
              toast("저장했습니다", "ok");
              await refreshList();
            } catch (err) { toast(errOf(err).message, "error"); }
          } }, "저장"),
          p ? h("button", { class: "danger", onclick: async (e: Event) => {
            e.preventDefault();
            if (!(await confirmBox("삭제", `${p.name} 프로필을 지울까요?`, "삭제"))) return;
            await api.deleteProfile(p.name).catch((err) => toast(errOf(err).message, "error"));
            form.replaceChildren();
            await refreshList();
          } }, "삭제") : null));
    };
    const refreshList = async () => {
      const ps = await api.listProfiles();
      list.replaceChildren(
        ...ps.map((p) => h("button", { class: "list-item", onclick: (e: Event) => { e.preventDefault(); edit(p); } },
          h("span", { class: "dot", style: `background:${p.color ?? "transparent"}` }), `${p.name}`, h("small", {}, ` ${p.user}@${p.connect_string}`))),
        h("button", { class: "list-item add", onclick: (e: Event) => { e.preventDefault(); edit(); } }, "+ 새 접속"));
    };
    await refreshList();
    if (!this.profiles.length) edit();
    const pickTns = async (e: Event) => {
      e.preventDefault();
      const d = await openDialog({ directory: true, title: "tnsnames.ora 가 있는 폴더" });
      if (!d || Array.isArray(d)) return;
      try {
        await api.setTnsAdmin(d);
        toast("저장했습니다 — 프로그램을 다시 시작하면 이 폴더의 별칭으로 접속합니다", "ok", 6000);
      } catch (err) { toast(errOf(err).message, "error"); }
    };
    const tnsBox = h("div", { class: "tns-box" },
      h("strong", {}, "tnsnames.ora "),
      tns.dir ? h("span", { title: tns.dir }, `${tns.dir} · 별칭 ${tns.entries.length}개${tns.configured ? "" : " (자동으로 찾음)"}`)
        : h("span", { class: "warn" }, "찾지 못했습니다 — TNS_ADMIN·ORACLE_HOME·Instant Client 폴더를 봤습니다"),
      tns.error ? h("span", { class: "warn" }, ` · 읽기 실패: ${tns.error}`) : "",
      h("button", { class: "small", onclick: pickTns }, "폴더 지정"),
      tns.configured ? h("button", { class: "small", onclick: async (e: Event) => { e.preventDefault(); await api.setTnsAdmin(null); toast("지정을 지웠습니다 — 다시 시작하면 기본 위치를 씁니다", "ok"); } }, "지정 해제") : "");
    return h("div", {}, tnsBox, tnsList, h("div", { class: "split" }, list, form));
  }

  private async providersPage() {
    const list = h("div", { class: "list" });
    const form = h("div", { class: "form" });
    const edit = (p?: Provider) => {
      const name = h("input", { value: p?.name ?? "" });
      const kind = h("select", {},
        h("option", { value: "ollama", selected: p?.kind === "ollama" }, "Ollama (로컬)"),
        h("option", { value: "openai_compatible", selected: p?.kind === "openai_compatible" }, "OpenAI 호환 (LM Studio, vLLM, OpenAI…)"),
        h("option", { value: "anthropic", selected: p?.kind === "anthropic" }, "Anthropic (Claude)"));
      const base = h("input", { value: p?.base_url ?? "", placeholder: "비우면 기본값" });
      const model = h("input", { value: p?.model ?? "" });
      const keyEnv = h("input", { value: p?.api_key_env ?? "", placeholder: "예: ANTHROPIC_API_KEY" });
      const key = h("input", { type: "password", placeholder: p?.has_key ? "(설정됨)" : "API 키" });
      const keyRemember = h("input", { type: "checkbox" });
      const numCtx = h("input", { type: "number", value: p?.num_ctx ?? "", placeholder: "16384" });
      const effort = h("select", {}, ...["", "low", "medium", "high", "xhigh", "max"].map((x) => h("option", { value: x, selected: (p?.effort ?? "") === x }, x || "(기본)")));
      const fallbacks = h("input", { type: "checkbox", checked: p?.fallbacks ?? true });
      const maxTokens = h("input", { type: "number", value: p?.max_tokens ?? 8192 });
      const result = h("pre", { class: "modal-text" });
      const collect = (): Provider => ({
        name: name.value.trim(), kind: kind.value as Provider["kind"], base_url: base.value.trim() || null,
        model: model.value.trim(), api_key_env: keyEnv.value.trim() || null, num_ctx: numCtx.value ? Number(numCtx.value) : null,
        effort: effort.value || null, fallbacks: fallbacks.checked, max_tokens: Number(maxTokens.value) || 8192,
      });
      const persist = async () => {
        const np = collect();
        await api.saveProvider(np, p?.name);
        if (key.value) await api.setApiKey(np.name, key.value, keyRemember.checked);
        return np;
      };
      form.replaceChildren(
        field("이름", name), field("종류", kind), field("주소", base, "Ollama: http://127.0.0.1:11434 · 사내 GPU 서버 주소도 됩니다"),
        field("모델", model, "예: qwen2.5-coder:14b, gemma3:12b"),
        field("API 키 환경변수", keyEnv), field("API 키", key, "설정 파일에는 쓰지 않습니다"),
        h("label", { class: "check" }, keyRemember, "OS 자격 증명 관리자에 저장 (끄면 이 실행 동안만)"),
        field("num_ctx (Ollama)", numCtx, "기본 컨텍스트(2~4K)는 스키마 문맥을 조용히 잘라 버립니다"),
        field("effort (Claude)", effort),
        h("label", { class: "check" }, fallbacks, "거절 시 대체 모델로 이어서 답하기 (Claude API 직통일 때만)"),
        field("max_tokens", maxTokens),
        h("div", { class: "row" },
          h("button", { class: "primary", onclick: async (e: Event) => {
            e.preventDefault();
            try { await persist(); toast("저장했습니다", "ok"); await refreshList(); } catch (err) { toast(errOf(err).message, "error"); }
          } }, "저장"),
          h("button", { onclick: async (e: Event) => {
            e.preventDefault();
            try {
              const np = await persist();
              result.textContent = "연결 확인 중…";
              const models = await api.testProvider(np.name);
              result.textContent = `연결됨 · 모델 ${models.length}개\n${models.slice(0, 30).join("\n")}`;
            } catch (err) { result.textContent = errOf(err).message; }
          } }, "연결 테스트")),
        result);
    };
    const refreshList = async () => {
      const ps = await api.listProviders();
      list.replaceChildren(
        ...ps.map((p) => h("button", { class: "list-item", onclick: (e: Event) => { e.preventDefault(); edit(p); } },
          `${p.name}`, h("small", {}, ` ${p.model}${p.remote ? " · 외부" : " · 로컬"}`))),
        h("button", { class: "list-item add", onclick: (e: Event) => { e.preventDefault(); edit(); } }, "+ 새 공급자"));
    };
    await refreshList();
    return h("div", { class: "split" }, list, form);
  }

  private async mcpPage() {
    const [s, profiles] = await Promise.all([api.getMcp(), api.listProfiles()]);
    const checks = profiles.map((p) => ({ p, c: h("input", { type: "checkbox", checked: s.allowed_connections.some((a) => a.toUpperCase() === p.name.toUpperCase()) }) }));
    const explain = h("input", { type: "checkbox", checked: s.allow_explain });
    const timeout = h("input", { type: "number", value: s.call_timeout_secs });
    const snippet = h("textarea", { class: "snippet", rows: 12, readOnly: true });
    const refreshSnippet = async () => { snippet.value = await api.mcpSnippet(); };
    await refreshSnippet();
    return h("div", { class: "form" },
      h("p", {}, "AI 에이전트(Claude Desktop, Claude Code, Cursor 등)가 sqlstudio-mcp 로 체크한 접속의 ",
        h("strong", {}, "스키마 정보(객체 목록·테이블 구조·DDL)만"), " 읽습니다. ",
        h("strong", {}, "AI 는 SQL 을 실행할 수 없고 테이블 데이터도 받지 못합니다."), " AI 가 만든 SQL 은 사람이 에디터에서 확인하고 실행합니다."),
      ...checks.map(({ p, c }) => h("label", { class: "check" }, c, `${p.name} (${p.user}@${p.connect_string})`) as Node),
      field("사전 조회 시간 상한(초)", timeout),
      h("label", { class: "check" }, explain, "explain_plan 허용 — AI 가 쓴 SQL 을 파서에 넘겨 실행계획만 봅니다 (실행·데이터 반환 없음)"),
      h("div", { class: "row" }, h("button", { class: "primary", onclick: async (e: Event) => {
        e.preventDefault();
        try {
          await api.setMcp({ allowed_connections: checks.filter((x) => x.c.checked).map((x) => x.p.name), call_timeout_secs: Number(timeout.value), allow_explain: explain.checked });
          await refreshSnippet();
          toast("저장했습니다", "ok");
        } catch (err) { toast(errOf(err).message, "error"); }
      } }, "저장"),
      h("button", { onclick: (e: Event) => { e.preventDefault(); navigator.clipboard.writeText(snippet.value).then(() => toast("복사했습니다", "ok")); } }, "설정 복사")),
      field("MCP 호스트 설정 (claude_desktop_config.json 등에 붙여 넣기)", snippet, "<비밀번호> 를 실제 값으로 바꾸세요"));
  }
}

/** 자주 나는 접속 오류에 해결 방법을 붙인다 */
function hintFor(err: ErrView): string {
  const m = err.message;
  const tips: [RegExp, string][] = [
    [/DPI-1047|Cannot locate a 64-bit Oracle Client/i, "Oracle Instant Client 를 찾을 수 없습니다.\nInstant Client 19 (64비트) 를 설치하고 PATH 에 넣거나, config.toml 의 [oracle] client_lib_dir 에 폴더를 지정하세요."],
    [/ORA-12154|TNS:could not resolve/i, "TNS 별칭을 찾을 수 없습니다. host:port/서비스명 형식으로 쓰거나 TNS_ADMIN 을 확인하세요."],
    [/ORA-12514/i, "리스너가 그 서비스 이름을 모릅니다. 서비스명(SID 아님)을 확인하세요."],
    [/ORA-12541|ORA-12170|TNS:Connect timeout/i, "DB 서버에 닿지 않습니다. 주소·포트·방화벽을 확인하세요."],
    [/ORA-28000/i, "계정이 잠겼습니다. DBA 에게 문의하세요."],
    [/ORA-28001/i, "비밀번호가 만료되었습니다."],
  ];
  const tip = tips.find(([re]) => re.test(m))?.[1];
  return tip ? `${m}\n\n${tip}` : m;
}

/** 왼쪽(테이블)·오른쪽(AI) 영역: 경계선을 끌어 너비 조절, 접기/펼치기, 상태는 다음 실행에도 유지 */
/** 경로 비교용: 구분자·대소문자 무시 (Windows) */
function normPath(p: string) {
  return p.replace(/\\/g, "/").replace(/\/+$/, "").toLowerCase();
}

class SideLayout {
  private w = { left: 240, right: 360 };
  private open = { left: true, right: true };

  constructor(private root: HTMLElement, left: HTMLElement, center: HTMLElement, right: HTMLElement,
    private onChange: (side: "left" | "right", open: boolean) => void) {
    try {
      const s = JSON.parse(localStorage.getItem("layout.sides") ?? "{}");
      if (s.w) this.w = { ...this.w, ...s.w };
      if (s.open) this.open = { ...this.open, ...s.open };
    } catch { /* 기본값 */ }
    root.append(left, this.splitter("left"), center, this.splitter("right"), right);
    this.apply();
  }

  toggle(side: "left" | "right", open = !this.open[side]) {
    this.open[side] = open;
    this.apply();
  }

  private splitter(side: "left" | "right") {
    const btn = h("button", { class: "vsplit-btn", title: "접기/펼치기 (경계선 더블클릭도 됨)", onclick: (e: Event) => { e.stopPropagation(); this.toggle(side); } });
    const el = h("div", { class: `vsplit ${side}`, title: "끌어서 너비 조절 · 더블클릭: 접기/펼치기" }, btn);
    el.addEventListener("dblclick", () => this.toggle(side));
    el.addEventListener("mousedown", (e) => {
      if (e.target === btn) return;
      e.preventDefault();
      const x0 = e.clientX;
      const w0 = this.open[side] ? this.w[side] : 0;
      document.body.classList.add("col-resizing");
      const move = (ev: MouseEvent) => {
        const dx = side === "left" ? ev.clientX - x0 : x0 - ev.clientX;
        const max = Math.max(200, Math.min(800, window.innerWidth * 0.5));
        const nw = w0 + dx;
        if (nw < 80) {
          this.open[side] = false;
        } else {
          this.open[side] = true;
          this.w[side] = Math.round(Math.min(max, Math.max(160, nw)));
        }
        this.apply(false);
      };
      const up = () => {
        document.body.classList.remove("col-resizing");
        window.removeEventListener("mousemove", move);
        window.removeEventListener("mouseup", up);
        this.apply();
      };
      window.addEventListener("mousemove", move);
      window.addEventListener("mouseup", up);
    });
    return el;
  }

  private apply(save = true) {
    const l = this.open.left ? `${this.w.left}px` : "0px";
    const r = this.open.right ? `${this.w.right}px` : "0px";
    this.root.style.gridTemplateColumns = `${l} 6px minmax(0, 1fr) 6px ${r}`;
    this.root.classList.toggle("left-closed", !this.open.left);
    this.root.classList.toggle("right-closed", !this.open.right);
    for (const s of ["left", "right"] as const) {
      const b = this.root.querySelector(`.vsplit.${s} .vsplit-btn`);
      // 펼쳐져 있으면 접는 방향, 접혀 있으면 펼치는 방향
      if (b) b.textContent = (s === "left") === this.open[s] ? "‹" : "›";
      this.onChange(s, this.open[s]);
    }
    if (save) localStorage.setItem("layout.sides", JSON.stringify({ w: this.w, open: this.open }));
  }
}

function dragSplit(handle: HTMLElement, top: HTMLElement) {
  handle.addEventListener("mousedown", (e) => {
    e.preventDefault();
    const y0 = e.clientY;
    const h0 = top.getBoundingClientRect().height;
    const move = (ev: MouseEvent) => { top.style.flexBasis = `${Math.max(80, h0 + ev.clientY - y0)}px`; };
    const up = () => { window.removeEventListener("mousemove", move); window.removeEventListener("mouseup", up); };
    window.addEventListener("mousemove", move);
    window.addEventListener("mouseup", up);
  });
}

const app = new App(document.getElementById("app")!);
app.start().catch((e) => alertBox("시작 오류", errOf(e).message));
