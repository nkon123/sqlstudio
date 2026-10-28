// SQLStudio 화면.
//
// 탭 하나 = 에디터 하나 + 세션 하나 + 결과 영역 하나. 탭끼리는 아무것도 공유하지 않는다
// (한 탭의 긴 쿼리가 다른 탭을 묶지 않는다 — 세션마다 서버 쪽 스레드가 따로 있다).

import "./mock/install"; // 가짜 DB 모드 (npm run mock) — 맨 먼저
import { type Area, areaNavOpen, showAreaNav } from "./areanav";
import { flushUiState } from "./uistate"; // 화면 상태를 데이터 폴더 파일에도 — localStorage 를 쓰기 전에
import { invoke } from "@tauri-apps/api/core";
import { open as openDialog, save as saveDialog } from "@tauri-apps/plugin-dialog";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { api, byteToUtf16, errOf, utf16ToByte, type Column, type ErrView, type ExecView, type Group, type ObjectEntry, type Profile, type TableDesc, type RowPage } from "./api";
import { AiPanel } from "./ai";
import { listen } from "@tauri-apps/api/event";
import { showSpace } from "./space";
import { enableTabDrag } from "./tabdrag";
import { cellCondition, listCondition, listText, placeCondition, type CondOp, type ListCond, type ListText } from "./cond";
import { changeCase } from "./editor";
import { autoFormat, formatSql, loadPrefs, startsWithDml } from "./format";
import { findList, wrapList } from "./listwrap";
import { vw } from "./sqlfmt";
import type { GitPanel } from "./git";
import { condOf, placeAtCursor, placeInClause, TARGET_LABEL, type Placed, type Target } from "./colinsert";
import { aliasMap, showColumns } from "./colpopup";
import { showMenu, type MenuItem } from "./ctxmenu";
import { showUsage } from "./usage";
import { showColumnSource } from "./colsource";
import { showValueTrace } from "./valuetrace";
import { showDiff, type Snapshot } from "./resultdiff";
import { openCopyData } from "./copydata";
import { openHistory } from "./history";
import { openImport } from "./importer";
import { showShortcuts } from "./shortcuts";
import { fromLineStart, hasManualLayout, manualLayout } from "./autofmt";
import { initSession, loadSession, restoreEnabled, saveSession } from "./tabsession";
import { loadSnippets } from "./snippets";
import { alertBox, applyTheme, bindBox, confirmBox, h, modal, passwordBox, toast } from "./ui";
import { Tab } from "./tab";
import { SideLayout } from "./layout";
import { connMenu, fileMenu, palette, runMenu, toolsMenu } from "./menus";
import { settings } from "./settings";
import { profileOptions } from "./conngroups";
import { editToSelect } from "./editcmd";

export class App {
  private tabs: Tab[] = [];
  active: Tab | null = null;
  profiles: Profile[] = [];
  groups: Group[] = [];
  private tabStrip = h("div", { class: "tabs" });
  /** 탭 옆의 "+" — 새 탭 (지금 탭과 같은 접속으로) */
  private plusBtn = h("button", { class: "tab-plus", title: "새 탭 (같은 접속으로)", onclick: () => this.newTab(true) }, "+");
  /** 탭을 끌어 옮긴 직후의 클릭은 탭을 고르는 것이 아니다 */
  tabJustDragged = false;
  private tabBodies = h("div", { class: "tab-bodies" });
  profileSel = h("select", { title: "접속 프로필" });
  /** 접속 단추 — 프로필·상태를 보이고, 누르면 프로필 고르기·끊기 메뉴 */
  private connName = h("span", { class: "conn-name" });
  private connSub = h("span", { class: "conn-sub" });
  private connPill = h("button", { class: "conn-pill" }, h("span", { class: "conn-dot" }), this.connName, this.connSub, h("span", { class: "caret" }, "▾"));
  /** 실행 단추 묶음 (실행 중에는 중지 단추로 바뀐다) */
  private runGroup!: HTMLElement;
  /** 커밋하지 않은 변경이 있을 때만 */
  private txnGroup = h("span", { class: "txn-group", hidden: true }, h("span", { class: "txn-label", title: "이 탭에 커밋하지 않은 변경이 있습니다 — 탭을 닫거나 프로그램을 끄면 묻습니다" }, "커밋 안 된 변경"));
  private statusLeft = h("span", {});
  private statusRight = h("span", {});
  private compBadge = h("span", { class: "comp-badge", title: "자동완성 캐시 — Ctrl+Space 로 직접 띄운다" });
  btn: Record<string, HTMLButtonElement> = {};
  private browserList = h("div", { class: "obj-list" });
  private browserOwner = h("select", { title: "스키마" });
  private browserType = h("select", { title: "종류" },
    ...["TABLE", "VIEW", "PACKAGE", "PROCEDURE", "FUNCTION", "TRIGGER", "SEQUENCE", "SYNONYM", "TYPE", "MATERIALIZED VIEW", "INDEX"].map((t) => h("option", { value: t }, t)),
    h("option", { value: "" }, "(전체)"));
  private browserFilter = h("input", { placeholder: "이름 필터 (예: ORD%)" });
  /** 왼쪽 영역 (테이블·프로젝트) — 영역 이동이 포커스를 둘 곳을 찾는다 */
  private browserEl!: HTMLElement;
  /** 영역 이동(Ctrl+Shift+2)으로 떠나온 영역 — 한 번 더 누르면 돌아간다 */
  private lastArea: Area | null = null;
  private browserObjs: ObjectEntry[] = [];
  private pickedTables = new Set<string>();
  ai: AiPanel;
  /** 프로젝트(Git) 패널 — 처음 필요할 때 불러온다 (시작을 늦추지 않게) */
  private git: GitPanel | null = null;
  private gitPane = h("div", { class: "git" });
  private gitLoading: Promise<GitPanel> | null = null;
  private gitBadge = h("span", { class: "git-badge" });
  layout!: SideLayout;

  constructor(root: HTMLElement) {
    const b = (key: string, label: string, title: string, onclick: () => void, cls = "") =>
      (this.btn[key] = h("button", { class: cls, title, onclick }, label));
    // 단추는 모두 만들어 두고 (쓸 수 있는지는 refresh 가 정한다), 툴바에는 자주 쓰는 것만 — 나머지는 메뉴·명령 검색이 같은 단추를 본다
    b("connect", "접속", "선택한 프로필로 접속 — 이미 접속한 탭에서 다른 접속을 고르면 새 탭에 접속합니다 (지금 탭은 그대로)", () => void this.connectActive());
    b("disconnect", "끊기", "현재 탭의 접속을 끊는다", () => this.disconnectActive());
    b("run", "▶ 실행", "커서 위치 문장 실행 (Ctrl+Enter / F9)", () => this.runStatement(), "primary run-main");
    b("script", "스크립트", "전체 실행 (F5)", () => this.runScript());
    b("explain", "실행계획", "실행계획 (Ctrl+E)", () => this.explain());
    b("debug", "디버그", "PL/SQL 디버거 — 커서 위치 블록을 디버그로 연다 (탐색기에서 프로시저·패키지를 골라도 된다)", () => this.debugActive());
    b("dfd", "흐름도", "데이터 흐름도를 별도 창으로 — 분석 결과에서 테이블 ← 처리 ← 테이블 흐름 (테이블 상세의 '흐름도'로 열면 그 테이블 기준)", () => this.openDfd());
    b("analyze", "분석", "PL/SQL 분석 — 패키지·프로시저를 조각내어 (로컬) 모델로 분석하고 호출 관계·테이블 CRUD 를 통합한다", () => this.analyzeActive());
    b("compare", "비교", "스키마 비교 — 이 탭(원본)과 다른 접속(대상)의 테이블·뷰·시퀀스·소스를 맞대고 맞추는 스크립트를 만든다", () => this.compareActive());
    b("copy", "데이터 복사", "다른 접속으로 데이터 복사 — 이 탭(원본)의 테이블 행을 다른 접속 탭(대상)의 테이블로 (추가 · 지우고 넣기 · MERGE). 커밋은 대상 탭에서", () => this.copyDataFrom(this.active));
    b("space", "용량", "용량 — 테이블별 크기(테이블+인덱스+LOB)와 테이블스페이스별 사용량을 그리드로", () => this.spaceActive());
    b("monitor", "세션", "세션·락 모니터 — 누가 무엇을 막고 있는지, 세션의 SQL·트랜잭션, 세션 종료(확인 후)", () => this.monitorActive());
    b("cancel", "■ 중지", "실행 중인 쿼리를 취소 (Esc)", () => this.cancel(), "danger");
    b("commit", "커밋", "COMMIT", () => this.txn("commit"), "primary");
    b("rollback", "롤백", "ROLLBACK", () => this.txn("rollback"));
    b("new", "새 탭", "새 에디터 (같은 접속으로)", () => this.newTab(true));
    b("open", "열기", "SQL 파일 열기", () => this.openFile());
    b("history", "이력", "SQL 실행 이력 (F8) — 실행한 문장을 찾아 다시 쓴다", () => this.showHistory());
    b("save", "저장", "저장 (Ctrl+S)", () => this.save());
    b("saveAs", "다른 이름", "다른 이름으로 저장 (Ctrl+Shift+S)", () => this.saveAs());
    b("format", "포맷", "SQL 정리 (Ctrl+Shift+F) — 선택 영역, 없으면 커서 위치 문장", () => this.formatSql());
    b("toggleBrowser", "◧", "왼쪽 영역(테이블·프로젝트) 접기/펼치기 (Ctrl+Shift+1) · 경계선을 끌면 너비 조절", () => this.layout.toggle("left"), "icon");
    b("toggleAi", "AI", "오른쪽 AI 영역 접기/펼치기 (Ctrl+Shift+3) · AI 입력칸으로 바로: Ctrl+K · 경계선을 끌면 너비 조절", () => this.layout.toggle("right"), "icon");
    b("keys", "?", "단축키 목록 (F1)", () => showShortcuts(), "icon");
    b("settings", "⚙", "설정 — 접속·편집기·AI·MCP·데이터 폴더", () => this.settings(), "icon");
    const drop = (label: string, title: string, items: () => MenuItem[], cls = "") => {
      const el: HTMLButtonElement = h("button", { class: `drop ${cls}`, title, onclick: () => {
        const r = el.getBoundingClientRect();
        showMenu(r.left, r.bottom + 2, items());
      } }, label);
      return el;
    };
    this.runGroup = h("span", { class: "run-split" }, this.btn.run,
      drop("▾", "실행 방법 고르기 — 스크립트 전체·실행계획·디버그", () => runMenu(this), "primary split-more"));
    this.btn.cancel.hidden = true;
    const toolbar = h("header", { class: "toolbar" },
      this.connPill,
      h("span", { class: "sep" }),
      this.runGroup, this.btn.cancel, this.btn.explain,
      this.txnGroup,
      h("span", { class: "sep" }),
      drop("파일 ▾", "새 탭·열기·저장·인코딩·실행 이력·포맷", () => fileMenu(this)),
      drop("도구 ▾", "분석·흐름도·디버그·스키마 비교·데이터 복사·용량·세션", () => toolsMenu(this)),
      h("span", { class: "spacer" }),
      h("button", { class: "pal-open", title: "모든 명령을 이름으로 찾아 실행 (Ctrl+Shift+P)", onclick: () => this.palette() },
        h("span", {}, "🔍 명령 검색"), h("kbd", {}, "Ctrl+Shift+P")),
      h("span", { class: "spacer" }),
      this.btn.toggleBrowser, this.btn.toggleAi, this.btn.keys, this.btn.settings,
    );
    this.connPill.addEventListener("click", () => {
      const r = this.connPill.getBoundingClientRect();
      showMenu(r.left, r.bottom + 2, connMenu(this));
    });
    this.txnGroup.append(this.btn.commit, this.btn.rollback);
    const tablePane = h("div", { class: "side-pane" },
      h("div", { class: "browser-head" }, this.browserOwner, this.browserType),
      this.browserFilter,
      this.browserList,
      h("small", { class: "hint" }, "클릭: 구조 · 우클릭: 메뉴 · 더블클릭: 이름 넣기 · 체크: AI 문맥에 포함"));
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
    this.browserEl = browser;
    let sideTab = "table";
    try { sideTab = localStorage.getItem("side.tab") === "git" ? "git" : "table"; } catch { /* 기본 */ }
    showSide(sideTab);
    this.ai = new AiPanel({
      context: () => this.aiContext(),
      insertSql: (s) => this.active?.editor.insert(s),
      replaceSql: (s) => this.replaceStatement(s),
    });
    const center = h("section", { class: "center" }, h("div", { class: "tabs-row" }, this.tabStrip, this.plusBtn), this.tabBodies);
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
      // 웹뷰의 기본 새로고침(Ctrl+R · Ctrl+Shift+R)은 화면을 통째로 다시 불러와 쓰던 글이 날아간다 — 막는다
      if ((e.ctrlKey || e.metaKey) && e.code === "KeyR") {
        e.preventDefault();
        e.stopPropagation();
        return;
      }
      // F1: 단축키 목록 (대화상자가 떠 있으면 무시)
      if (e.key === "F1" && !e.ctrlKey && !e.altKey && !document.querySelector("dialog[open]")) {
        e.preventDefault();
        e.stopPropagation();
        showShortcuts();
        return;
      }
      // Ctrl+Shift+P: 명령 검색
      if ((e.ctrlKey || e.metaKey) && e.shiftKey && !e.altKey && e.code === "KeyP" && !document.querySelector("dialog[open]")) {
        e.preventDefault();
        e.stopPropagation();
        this.palette();
        return;
      }
      // Ctrl+Shift+2: 영역 이동 (4방향 표시) — 떠 있을 때의 키는 표시가 받는다
      if ((e.ctrlKey || e.metaKey) && e.shiftKey && !e.altKey && e.code === "Digit2" && !areaNavOpen() && !document.querySelector("dialog[open]")) {
        e.preventDefault();
        e.stopPropagation();
        this.areaNav();
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

  /** 마지막으로 저장한 탭 상태 — 문서 객체(고치면 새 객체)·파일·탭 순서가 같으면 다시 쓰지 않는다 */
  private savedTabsSig: unknown[] = [];
  private savingTabs = false;

  /** 탭을 데이터 폴더의 tabs.json 에 (바뀐 것이 있을 때만, force 면 늘) */
  private async saveTabs(force = false) {
    if (this.savingTabs || !restoreEnabled() || !this.tabs.length) return;
    const active = Math.max(0, this.active ? this.tabs.indexOf(this.active) : 0);
    const sig = [active, ...this.tabs.flatMap((t) => [t.editor.view.state.doc, t.filePath, t.dirty, t.encoding, t.conn?.profile ?? null])];
    if (!force && sig.length === this.savedTabsSig.length && sig.every((x, i) => x === this.savedTabsSig[i])) return;
    this.savingTabs = true;
    try {
      await saveSession(active, this.tabs.map((t) => ({
        text: t.editor.text(), filePath: t.filePath, encoding: t.encoding, eol: t.eol, dirty: t.dirty,
        cursor: t.editor.cursor(), profile: t.conn?.profile ?? null,
      })));
      this.savedTabsSig = sig;
    } catch { /* 다음 차례에 다시 */ } finally {
      this.savingTabs = false;
    }
  }

  /** 지난번 탭을 되살린다 — 없으면 빈 탭 하나 */
  private async restoreTabs() {
    const s = await loadSession();
    if (!s) {
      this.newTab(false);
      return;
    }
    let unsaved = 0;
    for (const st of s.tabs) {
      let { text, encoding, eol, dirty } = st;
      // 파일로 저장해 둔 탭은 디스크에서 다시 (밖에서 고쳤을 수 있다). 파일이 없어졌으면 남긴 내용을 저장 안 함으로
      if (st.filePath && !st.dirty) {
        try {
          const f = await api.readFile(st.filePath);
          text = f.text;
          encoding = f.encoding;
          eol = f.text.includes("\r\n") ? "\r\n" : "\n";
        } catch { dirty = true; }
      }
      const t = this.newTab(false, text);
      t.filePath = st.filePath;
      t.encoding = encoding;
      t.eol = eol;
      t.setDirty(dirty);
      t.refreshTitle();
      t.editor.view.dispatch({ selection: { anchor: Math.min(st.cursor, t.editor.view.state.doc.length) }, scrollIntoView: true });
      if (dirty) unsaved++;
    }
    const act = this.tabs[s.active];
    if (act) this.activate(act);
    // 접속은 되살리지 않는다 (운영 DB 일 수 있다) — 그 탭의 프로필만 골라 둔다
    const prof = s.tabs[s.active]?.profile;
    if (prof && this.profiles.some((p) => p.name === prof)) {
      this.profileSel.value = prof;
      this.refresh();
    }
    if (unsaved) toast(`지난번 저장하지 않은 탭 ${unsaved}개를 되살렸습니다 (접속은 다시 하세요)`, "info", 6000);
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
    // 첫 글자 앞 들여쓰기도 같이 (앞에 공백만 있을 때)
    from = fromLineStart(view.state, from);
    const src = view.state.sliceDoc(from, to);
    const r = formatSql(src);
    if (!r.ok) return toast(r.reason, "error", 6000);
    if (r.text === src) return toast("이미 정리되어 있습니다", "info");
    // 직접 포맷하면 손으로 배치한 표시도 지운다 (다시 자동 포맷의 대상이 된다)
    view.dispatch({ changes: { from, to, insert: r.text }, selection: { anchor: from, head: from + r.text.length }, effects: manualLayout(from, from + r.text.length, false), userEvent: "input.format", scrollIntoView: true });
    t.editor.focus();
  }

  /** Ctrl+Alt+L — SELECT 목록(선택 영역, 없으면 커서 문장)을 몇 줄로 나눌지 묻고 칸을 맞춰 배치한다 */
  async wrapSelectList() {
    const t = this.active;
    if (!t) return;
    const view = t.editor.view;
    if (view.state.selection.ranges.length > 1) return toast("여러 곳이 선택되어 있습니다 — Esc 로 하나로 만든 뒤 하세요", "info");
    const main = view.state.selection.main;
    let from = main.from, to = main.to;
    if (main.empty) {
      const cur = await this.currentStatement(t);
      if (!cur) return toast("나눌 SELECT 목록이 없습니다", "info");
      ({ from, to } = cur);
    }
    const span = findList(view.state.sliceDoc(from, to));
    if (typeof span === "string") return toast(span, "error", 6000);
    if (span.items.length < 2) return toast("나눌 항목이 하나뿐입니다", "info");
    const a = from + span.from, b = from + span.to;
    const line = view.state.doc.lineAt(a);
    const lead = line.text.slice(0, a - line.from);
    const col = vw(lead);
    const comma = loadPrefs().commaBefore;
    const n = span.items.length;
    let saved = 0;
    try { saved = Number(localStorage.getItem("listwrap.rows")) || 0; } catch { /* 없으면 기본 */ }
    const input = h("input", { type: "number", min: "1", max: String(n), value: String(Math.min(n, saved || Math.ceil(n / 4))) }) as HTMLInputElement;
    const preview = h("pre", { class: "format-preview" });
    const rows = () => Math.max(1, Math.min(n, Number(input.value) || 1));
    const show = () => { preview.textContent = lead + wrapList(span.items, rows(), col, comma); };
    input.addEventListener("input", show);
    show();
    const body = h("div", { class: "form" },
      h("label", { class: "field" }, h("span", {}, `몇 줄로 나눌까요? (항목 ${n}개 — 1 ~ ${n})`), input,
        h("small", {}, "줄마다 같은 수씩 놓고 칸을 맞춥니다. 쉼표 위치는 포맷 설정을 따릅니다.")),
      preview);
    setTimeout(() => { input.focus(); input.select(); }, 0);
    if (!(await modal("목록 나누기", body, [{ label: "취소", value: false }, { label: "나누기", value: true, kind: "primary" }], { wide: true }))) return;
    try { localStorage.setItem("listwrap.rows", String(rows())); } catch { /* 기억 못 해도 된다 */ }
    const text = wrapList(span.items, rows(), col, comma);
    // 자동 포맷(실행할 때·; 칠 때)이 다시 한 줄에 하나로 풀지 않게 표시해 둔다
    view.dispatch({ changes: { from: a, to: b, insert: text }, selection: { anchor: a, head: a + text.length }, effects: manualLayout(a, a + text.length), userEvent: "input.format", scrollIntoView: true });
    t.editor.focus();
  }

  async start(tModules = 0, tBuilt = 0) {
    applyTheme();
    // 자동완성 캐시 적재 진행 (프로필마다 백그라운드)
    listen<{ profile: string; phase: string; objects: number; columns: number; error?: string | null }>("completion-status", (e) => {
      if (this.active?.conn?.profile.toUpperCase() !== e.payload.profile.toUpperCase()) return;
      this.showCompletionStatus(e.payload);
    });
    // Ctrl+A: 입력칸이 아니면 화면 전체가 아니라 마지막으로 누른 글 상자(소스·출력·계획·분석 코드)만 고른다
    let lastPointer: Element | null = null;
    document.addEventListener("mousedown", (e) => { lastPointer = e.target as Element; }, true);
    document.addEventListener("keydown", (e) => {
      if (e.defaultPrevented || !(e.ctrlKey || e.metaKey) || e.altKey || e.shiftKey || e.key.toLowerCase() !== "a") return;
      const a = document.activeElement as HTMLElement | null;
      if (a && (a.isContentEditable || a.tagName === "INPUT" || a.tagName === "TEXTAREA" || a.tagName === "SELECT")) return;
      e.preventDefault();
      const block = lastPointer?.closest("pre, .ob-body, .desc-table, .an-content");
      if (!block) return;
      const r = document.createRange();
      r.selectNodeContents(block);
      const sel = window.getSelection();
      sel?.removeAllRanges();
      sel?.addRange(r);
    });
    // 쉬는 동안 접속이 끊겼으면 (keepalive 가 알아챔) 실행하기 전에 미리 다시 접속한다
    listen<{ id: number }>("session-lost", (e) => {
      const t = this.tabs.find((x) => x.conn?.id === e.payload.id);
      if (t && !t.running) void this.recover(t, "쉬는 동안 DB 접속이 끊겼습니다");
    });
    // 시작이 느릴 때 어디서 걸리는지 로그로 (로그 폴더의 sqlstudio.log — "시작(화면)")
    const t0 = performance.now();
    let tPrev = t0;
    const lap: string[] = [];
    const mark = (what: string) => { const now = performance.now(); lap.push(`${what} ${Math.round(now - tPrev)}ms`); tPrev = now; };
    const info = await api.appInfo();
    mark("정보");
    void loadSnippets(info.config_path);
    initSession(info.config_path);
    this.statusLeft.textContent = `SQLStudio ${info.version} · 설정: ${info.config_path}`;
    if (info.config_error) await alertBox("설정 파일 오류", `${info.config_error}\n\n기본 설정으로 시작합니다. 설정 화면에서 저장하면 파일을 다시 씁니다.`);
    await this.reloadProfiles();
    mark("접속 프로필");
    await this.ai.load();
    mark("AI 공급자");
    await this.restoreTabs();
    mark(`탭 ${this.tabs.length}개`);
    void invoke("client_log", { level: "info", message: `시작(화면): ${lap.join(" · ")} · 합계 ${Math.round(performance.now() - t0)}ms (화면을 불러온 지 ${Math.round(t0)}ms 뒤 시작)` }).catch(() => {});
    // 그 앞 — 문서·스크립트 받기, 모듈 실행, 첫 화면 구성 (어디서 몇 초가 걸리는지)
    const nav = performance.getEntriesByType("navigation")[0] as PerformanceNavigationTiming | undefined;
    const js = (performance.getEntriesByType("resource") as PerformanceResourceTiming[]).find((r) => /\/assets\/main-[^/]*\.js/.test(r.name));
    const ms = (x?: number) => (x == null ? "?" : `${Math.round(x)}ms`);
    void invoke("client_log", { level: "info", message: `시작(화면 앞): 문서 요청 시각 ${new Date(performance.timeOrigin).toISOString()} · 문서 요청 ${ms(nav?.requestStart)} · 응답 시작 ${ms(nav?.responseStart)} · 문서 받음 ${ms(nav?.responseEnd)} · main.js 받기 ${ms(js?.startTime)}~${ms(js?.responseEnd)} · 모듈 실행 끝 ${ms(tModules)} · 첫 화면 구성 ${ms(tBuilt - tModules)} · 시작 작업 시작 ${ms(t0)}` }).catch(() => {});
    // 몇 초마다 바뀐 탭이 있으면 저장 (프로그램이 죽어도 쓰던 SQL 이 남게)
    window.setInterval(() => void this.saveTabs(), 3000);
    if (localStorage.getItem("git.last")) {
      const later = (f: () => void) => ("requestIdleCallback" in window ? requestIdleCallback(f, { timeout: 4000 }) : setTimeout(f, 2000));
      later(() => void this.loadGit());
    }
    this.refresh();
    getCurrentWindow().onCloseRequested(async (e) => {
      await this.saveTabs(true).catch(() => {});
      await flushUiState();
      const pending = this.tabs.filter((t) => t.txnPending);
      // 탭을 되살리는 중이면 저장하지 않은 편집은 다음에 켤 때 그대로 나오므로 묻지 않는다
      const dirty = restoreEnabled() ? [] : this.tabs.filter((t) => t.dirty);
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
    [this.profiles, this.groups] = await Promise.all([api.listProfiles(), api.listGroups().catch(() => [] as Group[])]);
    const prev = localStorage.getItem("profile");
    this.profileSel.replaceChildren(...profileOptions(this.groups, this.profiles, (p) =>
      h("option", { value: p.name, selected: p.name === prev }, `${p.name}${p.read_only ? " (읽기 전용)" : ""}`)));
  }

  // ── 탭 ───────────────────────────────────────────

  newTab(sameConnection: boolean, text = "") {
    const prev = this.active;
    const t = new Tab(this, text);
    this.tabs.push(t);
    this.tabStrip.append(t.tabEl);
    this.enableTabDrag(t);
    this.tabBodies.append(t.root);
    this.activate(t);
    t.tabEl.scrollIntoView({ inline: "nearest", block: "nearest" });
    if (sameConnection && prev?.conn) this.connect(t, prev.conn.profile);
    return t;
  }

  /** 탭을 끌어 순서 바꾸기 (끄는 동안 놓일 자리를 선으로 보인다) */
  private enableTabDrag(t: Tab) {
    enableTabDrag(t.tabEl, this.tabStrip, {
      onReorder: () => {
        // 화면의 순서를 탭 목록에 반영한다
        const order = [...this.tabStrip.querySelectorAll<HTMLElement>(".tab")];
        this.tabs.sort((a, b) => order.indexOf(a.tabEl) - order.indexOf(b.tabEl));
      },
      setJustDragged: (v) => { this.tabJustDragged = v; },
    });
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

  /** 접속 단추 — 이미 접속한 탭에서 다른 접속을 고르면 그 탭은 그대로 두고 새 탭에 접속한다 (Toad 처럼).
   *  접속하지 않은 탭이거나 같은 접속을 다시 고르면 지금 탭에 (다시) 접속한다 */
  async connectActive() {
    const t = this.active;
    if (!t) return;
    const name = this.profileSel.value;
    if (t.conn && t.conn.profile !== name) {
      const nt = this.newTab(false);
      await this.connect(nt, name);
      // 접속을 못 했거나 취소했으면 빈 새 탭을 치우고 원래 탭으로
      if (!nt.conn && !nt.dirty && this.tabs.includes(nt)) {
        await this.closeTab(nt);
        if (this.tabs.includes(t)) this.activate(t);
      }
      return;
    }
    await this.connect(t, name);
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
        t.conn = { ...c, profile: p.name, at: Date.now() };
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

  /** 끊긴 탭을 같은 프로필로 다시 접속한다 — 묻지 않고 (비밀번호는 기억한 것). 실행은 다시 하지 않는다 */
  private async recover(t: Tab, why: string) {
    const old = t.conn;
    if (!old || t.recovering) return;
    const hadTxn = t.txnPending;
    t.recovering = true;
    api.log("warn", `탭 #${old.id} (${old.profile}) ${why}${hadTxn ? " — 커밋 안 한 변경 있었음" : ""} → 다시 접속 시도`);
    try {
      await api.abandon(old.id).catch(() => {});
      t.conn = null;
      t.txnPending = false;
      t.refreshTitle();
      this.refresh();
      t.log(`${why} — 다시 접속합니다…`, "warn");
      this.status(`${old.profile} 다시 접속 중…`);
      for (const wait of [0, 2000, 5000]) {
        if (wait) await new Promise((r) => setTimeout(r, wait));
        if (t.conn || !this.tabs.includes(t)) return; // 그새 사용자가 접속했거나 탭을 닫았다
        try {
          const c = await api.connect(old.profile);
          t.conn = { ...c, profile: old.profile, at: Date.now() };
          t.refreshTitle();
          this.refresh();
          if (t === this.active) this.loadBrowser();
          t.log(hadTxn
            ? "다시 접속했습니다. 커밋하지 않았던 변경은 서버에서 롤백되었습니다 — 확인하고 다시 실행하세요."
            : "다시 접속했습니다. 세션 설정(ALTER SESSION·패키지 변수)은 처음으로 돌아갔습니다. 필요하면 다시 실행하세요.", hadTxn ? "err" : "warn");
          toast(hadTxn ? "다시 접속함 — 커밋하지 않은 변경은 롤백됨" : "DB 에 다시 접속했습니다", hadTxn ? "error" : "info", hadTxn ? 8000 : 3000);
          api.log("info", `탭 다시 접속 #${old.id} → #${c.id} (${old.profile})`);
          api.connEvent("recover_ok", `${why}${hadTxn ? " · 커밋 안 한 변경 롤백" : ""}`);
          this.status(`${old.profile} 다시 접속됨`);
          return;
        } catch (e) {
          const err = errOf(e);
          if (err.kind === "password_required" || err.ora_code === 1017) break;
          t.log(`다시 접속 실패: ${err.message}`, "err");
          api.log("warn", `탭 #${old.id} 다시 접속 실패: ${err.message}`);
        }
      }
      this.status("접속 끊김");
      api.connEvent("recover_fail", why);
      toast("DB 에 다시 접속하지 못했습니다 — 네트워크를 확인하고 접속하세요", "error", 8000);
    } finally {
      t.recovering = false;
    }
  }

  async disconnectActive() {
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
    let cur = await this.currentStatement(t);
    if (!cur) return toast("실행할 문장이 없습니다", "error");
    // 자동 포맷: 실행하는 문장을 먼저 정리 (선택해서 실행할 때는 그대로)
    // 손으로 배치한 곳(SELECT 목록을 몇 줄로 등)이 든 문장은 그대로
    if (autoFormat().onRun && !t.editor.selection() && !hasManualLayout(t.editor.view.state, cur.from, cur.to)) {
      // 첫 글자 앞 들여쓰기도 같이 (앞에 공백만 있을 때)
      const start = fromLineStart(t.editor.view.state, cur.from);
      const src = t.editor.view.state.sliceDoc(start, cur.to);
      const r = startsWithDml(src) ? formatSql(src) : null;
      if (r?.ok && r.text !== src) {
        t.editor.view.dispatch({ changes: { from: start, to: cur.to, insert: r.text }, selection: { anchor: start + r.text.length }, userEvent: "input.format" });
        cur = await this.currentStatement(t);
        if (!cur) return toast("실행할 문장이 없습니다", "error");
      }
    }
    const { a, from, to } = cur;
    const stmt = a.statement!;
    let binds: [string, string | null][] = [];
    if (a.binds.length) {
      const b = await bindBox(a.binds);
      if (!b) return;
      binds = b;
    }
    // Toad 의 EDIT 테이블 [WHERE …] — 그 테이블을 편집 그리드로 (서버로 보내지 않는다)
    const edit = editToSelect(stmt.text);
    if (edit) {
      t.editor.flash(from, to);
      if (t.editTable) {
        if (t.grid.changeCount() && !(await confirmBox("편집 끝내기", "적용하지 않은 변경을 버릴까요?", "버리기"))) return;
        t.grid.setEditable(null);
        this.endGridEdit(t);
      }
      t.lastQuery = { sql: edit, binds };
      return this.toggleGridEdit(t);
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
        this.loadNotes(t);
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
      t.log(err.message.includes("끊겼") ? err.message : `접속이 끊겼습니다: ${err.message}`, "err");
      void this.recover(t, "실행 중 DB 접속이 끊겼습니다");
      return;
    }
    t.lastError = `${err.message}\n\n실행한 SQL:\n${sqlText}`;
    t.log(err.message, "err");
    t.show("output");
    this.status(err.ora_code ? `ORA-${String(err.ora_code).padStart(5, "0")}` : "오류");
    if (err.offset != null && err.offset > 0) t.editor.markError(stmtFrom + byteToUtf16(sqlText, err.offset));
  }

  /** 결과 컬럼의 설명 — 캐시로만 찾으니 기다리지 않고 붙인다 */
  private loadNotes(t: Tab) {
    const sql = t.lastQuery?.sql;
    if (!t.conn || !sql) return;
    api.columnNotes(t.conn.id, sql).then((n) => {
      t.grid.setNotes(n);
      t.notesBtn.disabled = !t.grid.hasNotes();
    }).catch(() => {});
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
        let lost = false;
        const s = await api.executeScript(t.conn!.id, script, false, confirmed, (ev) => {
          if (ev.event === "started") {
            this.status(`스크립트 ${ev.index + 1}/${ev.total} 실행 중 (${ev.line}행)`);
            t.log(`[${ev.index + 1}/${ev.total}] ${ev.line}행: ${ev.preview}`, "dim");
          } else if (ev.event === "finished") {
            t.log(`    ${ev.summary} (${ev.elapsed_ms} ms)`, "ok");
            for (const l of ev.output) t.log(`    ${l}`);
          } else if (ev.event === "failed") {
            t.log(`    ${ev.error.message}`, "err");
            if (ev.error.kind === "connection_lost" || ev.error.kind === "session_closed") lost = true;
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
        if (lost) void this.recover(t, "스크립트 실행 중 DB 접속이 끊겼습니다");
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
    clearInterval(t.runTimer);
    const tick = () => {
      if (!t.running) return;
      const secs = (performance.now() - t.runStarted) / 1000;
      t.showRunning(secs);
      if (t === this.active) {
        this.statusRight.textContent = `실행 중 ${secs.toFixed(1)}s (Esc: 중지)`;
        // 중지 단추에도 — 눈에 띄는 곳에서 돌고 있는 것이 보이게
        this.btn.cancel.textContent = secs >= 1 ? `■ 중지 ${Math.floor(secs)}s` : "■ 중지";
      }
    };
    t.runTimer = window.setInterval(tick, 200);
    // 탭을 바꿔 와도 바로 보이게
    tick();
    try {
      await f();
    } finally {
      t.running = false;
      clearInterval(t.runTimer);
      t.showRunning(null);
      this.btn.cancel.textContent = "■ 중지";
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
      t.running = false;
      // 실행 중이던 호출이 '세션 닫힘' 으로 끝나며 같은 복구를 부를 수 있다 — recover 는 한 번만 돈다
      await this.recover(t, "세션을 버렸습니다");
    }, 5000);
  }

  async debugActive() {
    const t = this.need();
    if (!t) return;
    const cur = await this.currentStatement(t);
    const k = cur?.a.statement?.kind;
    const text = cur && (k === "plsql" || k === "other") ? cur.a.statement!.text : "BEGIN\n  NULL;\nEND;";
    t.debugger().openBlock(text);
  }

  compareActive() {
    const t = this.need();
    if (!t) return;
    void t.compare().open();
  }

  spaceActive() {
    const t = this.need();
    if (!t) return;
    if (!t.conn) return toast("먼저 접속하세요", "error");
    void showSpace(t.conn.id, t.conn.user);
  }

  monitorActive() {
    const t = this.need();
    if (!t) return;
    if (!t.conn) return toast("먼저 접속하세요", "error");
    t.monitor().open();
  }

  /** 영역 이동 — 편집기 · 결과 그리드 · 테이블 목록 · AI 를 4방향으로 고른다 */
  areaNav() {
    const cur = this.currentArea();
    showAreaNav(cur, (a) => this.goArea(a, cur), () => {
      // 직전 영역이 없으면 편집기 ↔ 결과 그리드
      const to = this.lastArea && this.lastArea !== cur ? this.lastArea : cur === "grid" ? "editor" : "grid";
      this.goArea(to, cur);
    });
  }

  private currentArea(): Area | null {
    const a = document.activeElement;
    const t = this.active;
    if (!a || a === document.body) return null;
    if (this.ai.el.contains(a)) return "right";
    if (this.browserEl.contains(a)) return "left";
    if (t?.grid.el.contains(a)) return "grid";
    if (t?.editor.view.dom.contains(a)) return "editor";
    return null;
  }

  private goArea(a: Area, from: Area | null) {
    const t = this.active;
    if (from && from !== a) this.lastArea = from;
    if (a === "left") {
      this.layout.toggle("left", true);
      // 보이는 첫 입력칸 (테이블 목록이면 이름 필터)
      const vis = (q: string) => [...this.browserEl.querySelectorAll<HTMLElement>(q)].find((x) => x.offsetParent);
      const el = vis("input") ?? vis("select, button, [tabindex]");
      el?.focus();
      return;
    }
    if (a === "right") { this.layout.toggle("right", true); this.ai.focus(); return; }
    if (!t) return;
    // 분석·디버거 화면이 떠 있으면 편집기 화면으로
    t.showEditor();
    if (a === "grid") { t.show("result"); t.grid.focus(); }
  }

  analyzeActive(owner?: string, name?: string) {
    const t = this.active;
    if (!t) return;
    // 접속 없이도 연다 — 다른 곳에서 내보낸 결과(.sqla)를 가져와 본다
    t.analysis().open(owner, name);
  }

  async txn(kind: "commit" | "rollback") {
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
      row.addEventListener("contextmenu", (e) => this.objectMenu(o, e));
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
  openDfd(center?: string, dir?: string) {
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

  /** 객체 소스(DDL)를 새 탭에 — line 은 소스(ALL_SOURCE)의 줄 번호 */
  async openSource(id: number, owner: string, name: string, kind: string, line?: number) {
    const ddl = await api.getDdl(id, kind, `"${owner}"."${name}"`).catch((e) => `-- ${errOf(e).message}`);
    const nt = this.newTab(true, ddl);
    if (line) {
      // DDL 앞의 빈 줄을 건너 CREATE 줄이 소스의 1번째 줄
      const base = ddl.split("\n").findIndex((l) => /^\s*CREATE\b/i.test(l));
      nt.editor.gotoLine((base < 0 ? 0 : base) + line);
    }
  }

  /** 테이블 구조(컬럼) 팝업 — name: "OWNER.TABLE" 또는 TABLE */
  async openTable(id: number, name: string) {
    const q = name.split(".").map((p) => `"${p}"`).join(".");
    try { this.openColumns(await api.describe(id, q)); } catch (e) { toast(errOf(e).message, "error"); }
  }

  private openColumns(d: TableDesc, at?: { x: number; y: number }, alias?: string) {
    const t = this.active;
    // 캐시로 먼저 띄웠으면 기본값·인덱스·통계를 뒤에서 채운다
    if (d.partial && t?.conn) {
      const id = t.conn.id;
      queueMicrotask(() => api.describe(id, `"${d.owner}"."${d.name}"`).then((full) => popup.update(full)).catch(() => popup.update({ ...d, partial: false })));
    }
    const popup = showColumns(d, {
      at, alias,
      insert: (s) => { const t = this.active; if (!t) return; t.editor.insert(s); t.editor.view.focus(); },
      place: (target, cols, from, types) => this.placeColumns(target, cols, from, types),
      ddl: async () => { const t = this.active; if (t?.conn) this.newTab(true, await api.getDdl(t.conn.id, d.object_type, `"${d.owner}"."${d.name}"`)); },
      script: async () => {
        const t = this.active;
        if (!t?.conn) throw new Error("먼저 접속하세요");
        return api.getDdl(t.conn.id, d.object_type, `"${d.owner}"."${d.name}"`);
      },
      deps: async () => {
        const t = this.active;
        if (!t?.conn) throw new Error("먼저 접속하세요");
        return api.objectDeps(t.conn.id, `"${d.owner}"."${d.name}"`);
      },
      openObject: async (owner, name, kind, line) => {
        const t = this.active;
        if (!t?.conn) return toast("먼저 접속하세요", "error");
        const id = t.conn.id;
        if (kind === "TABLE" || kind === "VIEW" || kind === "MATERIALIZED VIEW") {
          // 테이블·뷰는 컬럼 팝업으로 (뷰를 줄로 열 때만 정의를)
          if (!(kind === "VIEW" && line)) {
            try { return this.openColumns(await api.describe(id, `"${owner}"."${name}"`)); } catch (e) { return toast(errOf(e).message, "error"); }
          }
        }
        await this.openSource(id, owner, name, kind, line);
      },
      usage: (column) => {
        const t = this.active;
        if (!t?.conn) return toast("먼저 접속하세요", "error");
        const id = t.conn.id;
        void showUsage(id, `"${d.owner}"."${d.name}"`, column, async (owner, name, kind) =>
          this.newTab(true, await api.getDdl(id, kind, `"${owner}"."${name}"`).catch((e) => `-- ${errOf(e).message}`)));
      },
    });
  }

  /** 편집기 위치의 낱말에서 테이블 후보 (스키마.테이블 → 별칭 → 별칭.컬럼의 앞 → 그 낱말). DB 는 묻지 않는다 */
  private tableCandidatesAt(t: Tab, pos: number): { raw: string; tries: { name: string; alias?: string }[] } {
    const view = t.editor.view;
    const line = view.state.doc.lineAt(pos);
    const s = line.text;
    const ch = /[\w$#."]/;
    let a = pos - line.from, b = a;
    while (a > 0 && ch.test(s[a - 1])) a--;
    while (b < s.length && ch.test(s[b])) b++;
    const raw = s.slice(a, b);
    const parts = raw.replace(/"/g, "").split(".").filter(Boolean);
    if (!parts.length) return { raw, tries: [] };
    const cur = Math.min(parts.length - 1, (s.slice(a, pos - line.from).match(/\./g) ?? []).length);
    const doc = view.state.doc;
    const aliases = aliasMap(doc.sliceString(Math.max(0, pos - 30000), Math.min(doc.length, pos + 30000)));
    const word = parts[cur];
    const tries: { name: string; alias?: string }[] = [];
    if (cur >= 1) tries.push({ name: parts.slice(cur - 1, cur + 1).join(".") });
    if (aliases.has(word.toUpperCase())) tries.push({ name: aliases.get(word.toUpperCase())!, alias: word });
    if (cur >= 1) {
      const p = parts[cur - 1];
      tries.push(aliases.has(p.toUpperCase()) ? { name: aliases.get(p.toUpperCase())!, alias: p } : { name: p });
    }
    tries.push({ name: word });
    if (cur === 0 && parts.length > 1) tries.push({ name: parts.slice(0, 2).join(".") });
    return { raw, tries };
  }

  /** 후보를 찾는다 — 먼저 DB 없이(기억해 둔 구조·자동완성 캐시), 없으면 차례로 사전에서 (사전 조회 세션) */
  private async resolveTable(t: Tab, tries: { name: string; alias?: string }[]): Promise<{ d: TableDesc; alias?: string } | { error: unknown }> {
    for (const x of tries) {
      const d = await api.describeFast(t.conn!.id, x.name).catch(() => null);
      if (d) return { d, alias: x.alias };
    }
    let last: unknown = null;
    for (const x of tries) {
      try {
        return { d: await api.describe(t.conn!.id, x.name), alias: x.alias };
      } catch (e) {
        last = e;
      }
    }
    return { error: last };
  }

  /** 테이블이 아닌 객체(프로시저·함수·패키지·시퀀스·트리거·타입…) 찾기 — 후보 이름을 차례로 사전에서. 패키지 본문보다 명세를 먼저 */
  private async resolveObject(t: Tab, tries: { name: string }[]): Promise<ObjectEntry | null> {
    for (const x of tries) {
      const parts = x.name.replace(/"/g, "").split(".").filter(Boolean);
      if (!parts.length || parts.length > 2) continue;
      const name = parts[parts.length - 1].toUpperCase();
      const owner = parts.length === 2 ? parts[0] : undefined;
      try {
        const list = (await api.listObjects(t.conn!.id, owner, undefined, name.replace(/[_%\\]/g, "\\$&"))).filter((o) => o.name === name);
        const best = list.find((o) => !o.object_type.endsWith("BODY")) ?? list[0];
        if (best) return best;
      } catch { /* 다음 후보 */ }
    }
    return null;
  }

  /** 컬럼 팝업에서 고른 컬럼을 커서 쪽 문장에 넣는다. "auto" 인데 절을 모르면 false (팝업이 메뉴를 띄운다) */
  private async placeColumns(target: Target | "auto", cols: string[], from: string, types: string[] = []): Promise<boolean> {
    const t = this.active;
    if (!t) return true;
    const view = t.editor.view;
    const text = t.editor.text();
    const sel = view.state.selection.main;
    const a = await api.analyze(text, utf16ToByte(text, sel.head), undefined, t.conn?.id).catch(() => null);
    const st = a?.statement ? { from: byteToUtf16(text, a.statement.start), to: byteToUtf16(text, a.statement.end) } : null;
    const inside = st && sel.from >= st.from && sel.to <= st.to;
    // 별칭 없이 연 팝업(테이블 이름에서 F4 등)이면 문장에서 그 테이블의 별칭을 찾아 붙인다 (FROM emp e → e.ENAME)
    if (st && !cols[0].includes(".")) {
      const table = from.split(" ")[0].toUpperCase();
      const hits = [...aliasMap(text.slice(st.from, st.to))].filter(([, tb]) => tb === table || table.endsWith(`.${tb}`) || tb.endsWith(`.${table.split(".").pop()}`));
      if (hits.length === 1) cols = cols.map((c) => `${hits[0][0].toLowerCase()}.${c}`);
    }
    const apply = (base: number, p: Placed) => {
      view.dispatch({
        changes: { from: base + p.from, to: base + p.to, insert: p.insert },
        selection: { anchor: base + p.cursor },
        scrollIntoView: true,
        userEvent: "input.columns",
      });
      view.focus();
    };
    if (target === "auto") {
      if (!st || !inside) return false;
      const sql = text.slice(st.from, st.to);
      const p = placeAtCursor(sql, sel.from - st.from, sel.to - st.from, cols, types);
      if (!p) return false;
      apply(st.from, p);
      return true;
    }
    // 커서 쪽에 문장이 없으면 SELECT 를 새로 만든다
    if (!st) {
      const head = sel.from > 0 && text[sel.from - 1] !== "\n" ? "\n" : "";
      const body = target === "select" ? `SELECT ${cols.join(", ")}\n  FROM ${from}`
        : target === "where" ? `SELECT *\n  FROM ${from}\n WHERE ${cols.map((c, k) => condOf(c, types[k]).text).join("\n   AND ")}`
        : `SELECT ${cols.join(", ")}\n  FROM ${from}\n ${TARGET_LABEL[target]} ${cols.join(", ")}`;
      const cursor = target === "where" ? body.indexOf(" WHERE ") + 7 + condOf(cols[0], types[0]).caret : body.length;
      apply(sel.from, { from: 0, to: sel.to - sel.from, insert: head + body, cursor: head.length + cursor });
      return true;
    }
    const p = placeInClause(text.slice(st.from, st.to), target, cols, types);
    if ("error" in p) {
      toast(p.error, "error", 5000);
      return true;
    }
    apply(st.from, p);
    return true;
  }

  /** F4: 커서 위치의 테이블(또는 별칭·스키마.테이블·별칭.컬럼)의 컬럼 팝업 */
  async describeAtCursor() {
    const t = this.active;
    if (!t?.conn) return toast("먼저 접속하세요", "error");
    const view = t.editor.view;
    const pos = view.state.selection.main.head;
    const { raw, tries } = this.tableCandidatesAt(t, pos);
    if (!tries.length) return toast("커서를 테이블·프로시저 같은 이름(또는 별칭) 위에 두고 F4", "info");
    const c = view.coordsAtPos(pos);
    const r = await this.resolveTable(t, tries);
    if ("d" in r) return this.openColumns(r.d, c ? { x: c.left, y: c.bottom } : undefined, r.alias);
    // 테이블이 아니면 다른 객체: 코드(프로시저·함수·패키지·트리거·타입)는 소스를 새 탭에, 그 밖은 구조 탭의 상세로
    const o = await this.resolveObject(t, tries);
    if (o) {
      const type = o.object_type.toUpperCase();
      if (["PACKAGE", "PACKAGE BODY", "PROCEDURE", "FUNCTION", "TRIGGER", "TYPE", "TYPE BODY"].includes(type)) {
        return api.getDdl(t.conn.id, type, `"${o.owner}"."${o.name}"`).then((x) => this.newTab(true, x)).catch((e) => toast(errOf(e).message, "error"));
      }
      return this.showObject(o);
    }
    toast(`'${raw}' 를 찾지 못했습니다${r.error ? ` — ${errOf(r.error).message}` : ""}`, "error");
  }

  // ── 우클릭 메뉴 ─────────────────────────────────

  /** 객체 메뉴 (테이블 목록·편집기 공통). 테이블·뷰는 SQL 만들기·데이터, 코드는 소스·컴파일 */
  private objectMenuItems(t: Tab, o: { owner: string; name: string; object_type: string }, at: { x: number; y: number }, alias?: string): MenuItem[] {
    const id = t.conn!.id;
    const type = o.object_type.toUpperCase();
    const q = `"${o.owner}"."${o.name}"`;
    const shown = o.owner === t.conn!.user ? o.name : `${o.owner}.${o.name}`;
    const insert = (text: string) => { t.editor.insert(text); t.editor.view.focus(); };
    // 컬럼·기본 키만 있으면 되는 것(팝업·SQL 만들기)은 캐시로 바로
    const withDesc = (f: (d: TableDesc) => void) => () => {
      api.describeFast(id, q).then((d) => d ?? api.describe(id, q)).then(f).catch((e) => toast(errOf(e).message, "error"));
    };
    const ddl = () => api.getDdl(id, type, q).then((x) => this.newTab(true, x)).catch((e) => toast(errOf(e).message, "error"));
    const copyName = { label: "이름 복사", hint: shown, run: () => navigator.clipboard.writeText(shown).then(() => toast(`복사: ${shown}`, "ok", 1500)).catch(() => {}) };
    if (["TABLE", "VIEW", "MATERIALIZED VIEW"].includes(type)) {
      // 커서 줄에 글이 있으면 문서 끝에 빈 줄을 두고 넣는다 (쓰던 문장 가운데에 끼어들지 않게)
      const block = (text: string) => {
        const v = t.editor.view;
        if (!v.state.doc.lineAt(v.state.selection.main.head).text.trim()) return insert(text);
        const doc = v.state.doc.toString();
        const pre = doc.endsWith("\n\n") ? "" : doc.endsWith("\n") ? "\n" : "\n\n";
        const from = doc.length + pre.length;
        v.dispatch({ changes: { from: doc.length, insert: pre + text }, selection: { anchor: from, head: from + text.length }, scrollIntoView: true });
        v.focus();
      };
      const sql = (kind: "select" | "insert" | "update" | "delete") => withDesc((d) => block(sqlTemplate(kind, shown, d, alias)));
      return [
        { title: `${type} ${shown}${alias ? ` (${alias})` : ""}` },
        { label: "컬럼 보기", hint: "F4", run: withDesc((d) => this.openColumns(d, at, alias)) },
        { label: "데이터 보기 (앞 200행)", run: () => this.previewData(t, q) },
        { label: "건수 세기", hint: "COUNT(*)", run: () => this.runQuery(t, `SELECT COUNT(*) AS cnt FROM ${q}`) },
        { label: "상세 보기", hint: "구조 탭", run: () => this.showObject(o) },
        { sep: true },
        { label: "SQL 만들기", sub: [
          { label: "SELECT", run: sql("select") }, { label: "INSERT", run: sql("insert") },
          { label: "UPDATE", run: sql("update") }, { label: "DELETE", run: sql("delete") },
        ] },
        { label: "DDL", hint: "새 탭", run: ddl },
        { sep: true },
        { label: "흐름도 — 어디서 오나", hint: "출력 기준", run: () => this.openDfd(`${o.owner}.${o.name}`, "up") },
        { label: "흐름도 — 어디로 가나", hint: "입력 기준", run: () => this.openDfd(`${o.owner}.${o.name}`, "down") },
        { label: "다른 접속으로 데이터 복사…", hint: "QA → DEV 등", run: () => this.copyDataFrom(t, `${o.owner}.${o.name}`) },
        ...(type === "TABLE" ? [{ label: "데이터 가져오기…", hint: "엑셀·CSV", run: () => openImport({ id, owner: o.owner, table: o.name, done: (r) => { t.txnPending = r.txn_pending; this.refresh(); } }) }] : []),
        { sep: true },
        copyName,
      ];
    }
    const code = ["PACKAGE", "PACKAGE BODY", "PROCEDURE", "FUNCTION", "TRIGGER", "TYPE", "TYPE BODY"].includes(type);
    return [
      { title: `${type} ${shown}` },
      { label: code ? "소스 열기" : "DDL", hint: "새 탭", run: ddl },
      { label: "상세 보기", hint: "구조 탭", run: () => this.showObject(o) },
      ...(code ? [
        { label: "컴파일", run: () => this.compileObject(t, o.owner, o.name, type, () => {}) },
        { label: "디버그", run: () => t.debugger().openUnit(o.owner, o.name, type) },
        ...(type !== "TYPE" ? [{ label: "분석", run: () => this.analyzeActive(o.owner, o.name) }] : []),
      ] : []),
      ...(type === "PROCEDURE" || type === "FUNCTION" ? [{ label: "흐름도", hint: "입력·출력 테이블", run: () => this.openDfd(`${o.owner}.${o.name}`, "both") }] : []),
      ...(type === "SEQUENCE" ? [{ label: "NEXTVAL 넣기", run: () => insert(`${shown}.NEXTVAL`) }] : []),
      { sep: true },
      copyName,
    ];
  }

  /** 테이블 목록에서 우클릭 */
  private objectMenu(o: ObjectEntry, e: MouseEvent) {
    e.preventDefault();
    const t = this.active;
    if (!t?.conn) return;
    showMenu(e.clientX, e.clientY, this.objectMenuItems(t, o, { x: e.clientX, y: e.clientY }));
  }

  /** 결과 그리드 우클릭: 건수 확인, 복사 */
  /** 한 번에 고를 수 있는 셀 수 (IN 목록·LIKE 묶음이 너무 길어지지 않게) */
  private static readonly PICK_LIMIT = 100;

  gridMenu(t: Tab, e: MouseEvent, value: string | null, col: number) {
    const q = t.lastQuery;
    const column = t.grid.columnAt(col);
    const canCond = !!q && !!column && !t.grid.isEditing();
    const shown = value == null ? "NULL" : value.length > 24 ? `${value.slice(0, 24)}…` : value;
    const n = t.grid.selectedRows().length;
    const sv = t.grid.selectedValues(App.PICK_LIMIT);
    const multi = sv.kind === "col" && sv.values.length > 1;
    const items: MenuItem[] = [];
    if (sv.kind === "col" && multi) {
      // 같은 열에서 여러 셀을 골랐다 — 값 목록으로
      const c = sv.column;
      const vals = sv.values;
      const distinctN = new Set(vals.filter((v) => v != null)).size;
      const nulls = vals.filter((v) => v == null).length;
      const info = `${c.name} · ${vals.length}개${distinctN !== vals.length - nulls ? ` (중복 제외 ${distinctN}개)` : ""}${nulls ? ` · NULL ${nulls}개` : ""}`;
      const add = (kind: ListCond, run: boolean) => () => void this.addListCondition(t, c, vals, kind, run);
      const copy = (form: ListText) => () => {
        const r = listText(c, vals, form);
        if ("error" in r) return toast(r.error, "error", 5000);
        navigator.clipboard.writeText(r.text).then(() => toast(`복사: ${vals.length}개 값`, "ok", 1500)).catch(() => {});
      };
      items.push(
        { title: info },
        { label: "조회 조건에 추가", disabled: !canCond, sub: [
          { label: "IN (고른 값)", hint: "맨 끝 WHERE 에 AND", run: add("in", false) },
          { label: "NOT IN (고른 값 빼고)", run: add("notin", false) },
          { label: "LIKE '%값%' 여러 개 (OR)", hint: "글자 열", run: add("like", false) },
          { sep: true },
          { label: "IN 추가하고 실행", run: add("in", true) },
          { label: "NOT IN 추가하고 실행", run: add("notin", true) },
          { label: "LIKE 추가하고 실행", run: add("like", true) },
        ] },
        { label: "고른 값 복사", sub: [
          { label: "IN 절", hint: "('A', 'B')", run: copy("in") },
          { label: "조건까지", hint: "COL IN ('A', 'B')", run: copy("cond") },
          { label: "쉼표 목록", hint: "A,B", run: copy("comma") },
          { label: "따옴표 쉼표 목록", hint: "'A','B'", run: copy("quoted") },
          { label: "줄마다 하나", run: copy("lines") },
        ] },
      );
    } else if (sv.kind === "many" || sv.kind === "multi") {
      items.push({ title: sv.kind === "many" ? `${sv.count.toLocaleString()}개 선택 — 값 목록은 ${App.PICK_LIMIT}개까지` : `${sv.count}개 선택 — 값 목록은 한 열에서만` });
    } else {
      items.push({ label: "조회 조건에 추가", hint: column ? `${column.name} · ${shown}` : "", disabled: !canCond, sub: [
        { label: "같다 (=)", hint: "맨 끝 WHERE 에 AND", run: () => void this.addCellCondition(t, column!, value, "eq", false) },
        { label: "포함 (LIKE '%값%')", hint: "글자 열", run: () => void this.addCellCondition(t, column!, value, "like", false) },
        { sep: true },
        { label: "같다 (=) 추가하고 실행", run: () => void this.addCellCondition(t, column!, value, "eq", true) },
        { label: "포함 (LIKE) 추가하고 실행", run: () => void this.addCellCondition(t, column!, value, "like", true) },
      ] });
    }
    // 값의 출처 — 이 열이 어느 테이블.열이고, 어디서 그 열에 값을 넣는지 (분석 결과로)
    items.push({ label: "값의 출처 (어디서 오나)", hint: column ? column.name : "", disabled: !q || !t.conn || !column,
      run: () => this.showValueSource(t, col, column!) });
    // 값 추적 — 이 행(키)의 이 값을 만든 문장을 다시 계산해 거슬러 오른다
    items.push({ label: "이 값은 어떻게 나왔나 (값 추적)", hint: column ? `${column.name} = ${shown}` : "", disabled: !q || !t.conn || !column || n !== 1 || t.grid.isEditing(),
      run: () => this.showValueTrace(t, col, column!) });
    // 고른 행만 보기·빼고 보기 — 화면에서만 거른다 (서버 쿼리는 그대로)
    items.push(
      { sep: true },
      { label: "행 거르기 (화면에서만)", sub: [
        { label: n > 1 ? `고른 ${n}행만 보기` : "이 행만 보기", run: () => t.grid.maskSelectedRows(true) },
        { label: n > 1 ? `고른 ${n}행 빼고 보기` : "이 행 빼고 보기", run: () => t.grid.maskSelectedRows(false) },
        { sep: true },
        { label: "거르기 해제", disabled: !t.grid.hasRowMask(), run: () => t.grid.clearRowMask() },
      ] },
      { sep: true },
      { label: "결과 비교…", hint: this.compareBase ? "기준 있음" : "기준으로 담기", run: () => this.compareMenu(t, e) },
      { label: "건수 확인", hint: "전체 행 수", disabled: !q || !t.conn || t.grid.isEditing(), run: () => void this.countResult(t) },
      { sep: true },
      { label: n > 1 ? `고른 셀 복사 (${n}행)` : "고른 셀 복사", hint: "Ctrl+C", run: () => navigator.clipboard.writeText(t.grid.selectionText()).catch(() => {}) },
      { label: "머리 포함 복사", hint: "Ctrl+Shift+C", run: () => navigator.clipboard.writeText(t.grid.selectionText(true)).catch(() => {}) },
    );
    showMenu(e.clientX, e.clientY, items);
  }

  /** 접속 간 데이터 복사 — 접속한 탭들이 원본·대상 후보 (같은 접속을 연 탭이 여럿이면 하나만) */
  copyDataFrom(t: Tab | null, table?: string) {
    if (!t?.conn) return toast("먼저 원본에 접속하세요", "error");
    const seen = new Set<number>();
    const conns = this.tabs.filter((x) => x.conn && !seen.has(x.conn.id) && seen.add(x.conn.id)).map((x) => ({
      id: x.conn!.id, label: `${x.conn!.profile} (${x.conn!.user})`, readOnly: x.conn!.read_only, production: !!x.conn!.production,
    }));
    openCopyData({
      conns, srcId: t.conn.id, srcTable: table,
      done: (dstId, pending) => {
        for (const x of this.tabs) if (x.conn?.id === dstId) { x.txnPending = pending; x.refreshTitle(); }
        this.refresh();
      },
    });
  }

  /** 결과 비교의 기준 (담아 둔 결과) — 탭을 건너 쓴다 (다른 접속의 결과와도) */
  private compareBase: Snapshot | null = null;

  compareMenu(t: Tab, e: MouseEvent) {
    const b = this.compareBase;
    showMenu(e.clientX, e.clientY, [
      { title: b ? `기준: ${b.label} · ${b.rows.length.toLocaleString()}행` : "담아 둔 기준이 없습니다" },
      { label: "이 결과를 기준으로 담기", hint: "고치기 전 · 다른 DB", run: () => void t.snapshot().then((s) => {
        if (!s) return;
        this.compareBase = s;
        toast(`기준으로 담았습니다 (${s.rows.length.toLocaleString()}행) — 쿼리를 고치거나 다른 접속에서 실행한 뒤 '기준과 비교'`, "ok", 5000);
      }) },
      { label: "기준과 비교", hint: b ? "지금 결과 ↔ 기준" : "", disabled: !b, run: () => void t.snapshot().then((s) => { if (s && b) showDiff(b, s); }) },
      { sep: true },
      { label: "기준 비우기", disabled: !b, run: () => { this.compareBase = null; } },
    ]);
  }

  private showValueSource(t: Tab, col: number, column: Column) {
    const q = t.lastQuery;
    if (!q || !t.conn) return;
    const id = t.conn.id;
    // 편집 모드면 ROWID 를 붙여 다시 조회한 결과라 열 위치가 원래 쿼리와 다르다 — 이름으로만 찾는다
    void showColumnSource(id, q.sql, t.grid.isEditing() ? null : col, column, {
      openSource: (owner, name, kind, line) => this.openSource(id, owner, name, kind, line),
      showUsage: (table, c) => void showUsage(id, table, c, async (owner, name, kind) =>
        this.newTab(true, await api.getDdl(id, kind, `"${owner}"."${name}"`).catch((e) => `-- ${errOf(e).message}`))),
      openAnalysis: () => { t.analysis(); },
    });
  }

  private showValueTrace(t: Tab, col: number, column: Column) {
    const q = t.lastQuery;
    const r = t.grid.selectedRows()[0];
    if (!q || !t.conn || !r) return;
    const id = t.conn.id;
    const row: [string, string | null][] = r.map((v, i) => [t.grid.columnAt(i)?.name ?? "", v]);
    void showValueTrace(id, q.sql, col, column, row, {
      openSource: (owner, name, kind, line) => this.openSource(id, owner, name, kind, line),
      openAnalysis: () => { t.analysis(); },
      explain: (text) => {
        this.layout.toggle("right", true);
        this.ai.askAbout("아래는 한 행의 값을 거슬러 오른 추적 결과다 (✓ 재현됨 = 그 문장을 이 키로 다시 계산한 값이 지금 값과 같음). " +
          "이 값이 어떻게 나왔는지 단계별로 짧게 설명하고, 재현되지 않거나 조건에 걸러진 곳이 있으면 그 까닭을 짚어라. 추적에 없는 것은 지어내지 마라.\n\n" + text, true);
      },
    });
  }

  /** 셀 값을 조회 조건으로 */
  private addCellCondition(t: Tab, column: Column, value: string | null, op: CondOp, run: boolean) {
    return this.applyCondition(t, column, (expr) => cellCondition(column, value, op, expr), run);
  }

  /** 고른 값 목록(IN · NOT IN · LIKE 여러 개)을 조회 조건으로 */
  private addListCondition(t: Tab, column: Column, values: (string | null)[], kind: ListCond, run: boolean) {
    return this.applyCondition(t, column, (expr) => listCondition(column, values, kind, expr), run);
  }

  /** 조건을 에디터의 그 쿼리에 넣는다 — WHERE 끝(없으면 새 WHERE, GROUP BY·ORDER BY 앞).
   *  넣은 뒤 그 쿼리가 현재 쿼리가 되므로, 여러 번 눌러 조건을 쌓을 수 있다 */
  private async applyCondition(t: Tab, column: Column, make: (expr: string | null) => { cond: string } | { error: string }, run: boolean) {
    const q = t.lastQuery;
    if (!q) return;
    const view = t.editor.view;
    const doc = view.state.doc.toString();
    // 같은 쿼리가 여러 번 있으면 커서에 가장 가까운 것 (보통 방금 실행한 것)
    const cursor = view.state.selection.main.head;
    let at = -1;
    for (let i = doc.indexOf(q.sql); i >= 0; i = doc.indexOf(q.sql, i + 1)) {
      const dist = cursor < i ? i - cursor : cursor > i + q.sql.length ? cursor - i - q.sql.length : 0;
      const best = at < 0 ? Infinity : cursor < at ? at - cursor : cursor > at + q.sql.length ? cursor - at - q.sql.length : 0;
      if (dist < best) at = i;
    }
    if (at < 0) return toast("에디터에서 이 결과를 만든 쿼리를 찾지 못했습니다 — 쿼리를 고쳤거나 지웠다면 다시 실행한 뒤 눌러 주세요", "error", 6000);
    const added = placeCondition(q.sql, column, make);
    if ("error" in added) return toast(added.error, "error", 6000);
    view.dispatch({ changes: { from: at, to: at + q.sql.length, insert: added.sql }, selection: { anchor: at }, scrollIntoView: true, userEvent: "input.cond" });
    t.lastQuery = { sql: added.sql, binds: q.binds };
    t.setDirty(true);
    t.editor.flash(at, at + added.sql.length);
    if (run) await this.runStatement();
    else { this.status(`조건 추가${added.wrapped ? " (계산한 열이라 SELECT * FROM (…) 로 감쌈)" : ""}: ${added.cond.length > 80 ? `${added.cond.slice(0, 80)}…` : added.cond}`); view.focus(); }
  }

  /** 지금 결과를 만든 조회의 전체 건수 — 읽고 있는 결과(커서)를 건드리지 않고 사전 조회 세션으로 센다 */
  private async countResult(t: Tab) {
    const q = t.lastQuery;
    if (!t.conn || !q) return;
    const started = performance.now();
    this.status("건수 세는 중…");
    try {
      const r = await api.countRows(t.conn.id, q.sql, q.binds);
      const n = Number(r.rows).toLocaleString();
      toast(`전체 ${n}행 (${(r.elapsed_ms / 1000).toFixed(1)}초)`, "ok", 6000);
      this.status(`전체 ${n}행 · 세는 데 ${((performance.now() - started) / 1000).toFixed(1)}초`);
    } catch (err) {
      toast(`건수를 세지 못했습니다: ${errOf(err).message}`, "error");
    }
  }

  /** 편집기에서 우클릭: 그 자리의 테이블 메뉴 + 편집 메뉴 */
  async editorMenu(t: Tab, e: MouseEvent) {
    e.preventDefault();
    const view = t.editor.view;
    const pos = view.posAtCoords({ x: e.clientX, y: e.clientY });
    // 선택 영역 밖을 누르면 커서를 그 자리로 (Toad 처럼)
    const sel = view.state.selection.main;
    if (pos != null && (sel.empty || pos < sel.from || pos > sel.to)) view.dispatch({ selection: { anchor: pos } });
    const at = { x: e.clientX, y: e.clientY };
    const edit: MenuItem[] = [
      { label: "실행", hint: "Ctrl+Enter", run: () => this.runStatement() },
      { label: "SQL 포맷", hint: "Ctrl+Shift+F", run: () => this.formatSql() },
      { label: "SELECT 목록을 몇 줄로…", hint: "Ctrl+Alt+L", run: () => void this.wrapSelectList() },
      { label: "대문자로 (구문만)", hint: "Ctrl+Shift+U", run: () => { changeCase("upper")(view); view.focus(); } },
      { label: "소문자로 (구문만)", hint: "Ctrl+Alt+U", run: () => { changeCase("lower")(view); view.focus(); } },
      { sep: true },
      { label: "저장", hint: "Ctrl+S", run: () => this.save() },
      { label: "다른 이름으로 저장", hint: "Ctrl+Shift+S", run: () => this.saveAs() },
      { sep: true },
      { label: "잘라내기", hint: "Ctrl+X", disabled: view.state.selection.main.empty, run: () => { view.focus(); document.execCommand("cut"); } },
      { label: "복사", hint: "Ctrl+C", disabled: view.state.selection.main.empty, run: () => { view.focus(); document.execCommand("copy"); } },
      { label: "붙여넣기", hint: "Ctrl+V", run: () => navigator.clipboard.readText().then((x) => { t.editor.insert(x); view.focus(); }).catch(() => toast("Ctrl+V 로 붙여 넣으세요", "info")) },
      { label: "모두 선택", hint: "Ctrl+A", run: () => { view.dispatch({ selection: { anchor: 0, head: view.state.doc.length } }); view.focus(); } },
    ];
    const { raw, tries } = pos != null ? this.tableCandidatesAt(t, pos) : { raw: "", tries: [] };
    if (!t.conn || !tries.length) return showMenu(at.x, at.y, edit);
    // 사전에서 찾는 동안 기다리지 않게 — 편집 메뉴를 먼저 띄우고, 찾으면 테이블 메뉴로 바꾼다
    showMenu(at.x, at.y, [{ title: `${raw} — 찾는 중…` }, { sep: true }, ...edit]);
    const r = await this.resolveTable(t, tries);
    if (!document.querySelector(".ctx-menu")) return; // 그새 닫았으면 그대로
    if ("d" in r) showMenu(at.x, at.y, [...this.objectMenuItems(t, { owner: r.d.owner, name: r.d.name, object_type: r.d.object_type }, at, r.alias), { sep: true }, ...edit]);
    else {
      // 테이블이 아니면 다른 객체(프로시저·함수·패키지…)인지
      const o = await this.resolveObject(t, tries);
      if (!document.querySelector(".ctx-menu")) return;
      if (o) showMenu(at.x, at.y, [...this.objectMenuItems(t, o, at), { sep: true }, ...edit]);
      else showMenu(at.x, at.y, [{ title: `${raw} — 찾을 수 없는 이름` }, { sep: true }, ...edit]);
    }
  }

  /** 문장 하나를 이 탭에서 실행해 결과 그리드로 (건수 세기 등) */
  private async runQuery(t: Tab, sql: string) {
    if (!t.conn) return;
    await this.running(t, async () => {
      try {
        const r = await api.execute(t.conn!.id, sql, [], false);
        this.showResult(t, r);
      } catch (e) {
        this.showError(t, errOf(e), sql, 0);
      }
    });
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
    if (!t.filePath) return this.saveAs();
    await this.writeTab(t, t.filePath);
  }

  /** 다른 이름으로 저장 — 이 탭은 새 파일을 가리키게 된다 (원래 파일은 그대로) */
  async saveAs() {
    const t = this.active;
    if (!t) return;
    const cur = t.filePath;
    const name = cur ? cur.split(/[\\/]/).pop()! : "query.sql";
    const path = await saveDialog({
      filters: [{ name: "SQL", extensions: ["sql", "pls", "pks", "pkb", "prc", "fnc", "trg", "vw", "txt"] }, { name: "모든 파일", extensions: ["*"] }],
      defaultPath: cur ?? name,
    });
    if (!path) return;
    // 다른 탭이 그 파일을 열고 있으면 — 저장하면 그 탭과 디스크가 달라진다
    const other = this.tabs.find((x) => x !== t && x.filePath && normPath(x.filePath) === normPath(path));
    if (other && !(await confirmBox("다른 탭에서 연 파일", `${path}\n\n이 파일은 다른 탭에서 열려 있습니다. 덮어쓰면 그 탭의 내용과 파일이 달라집니다. 그래도 저장할까요?`, "덮어쓰기"))) return;
    await this.writeTab(t, path);
  }

  /** 저장 단추 우클릭 — 다른 이름, 인코딩 바꿔 저장 */
  /** 인코딩 바꿔 저장 — 파일 메뉴의 하위 메뉴 */
  encodingItems(): MenuItem[] {
    const t = this.active;
    if (!t) return [];
    const enc = (v: string, label: string): MenuItem => ({
      label: `${t.encoding === v ? "✓ " : ""}${label}`,
      run: async () => {
        const before = t.encoding;
        t.encoding = v;
        if (t.filePath) await this.writeTab(t, t.filePath, before);
        else { await this.saveAs(); if (!t.filePath) t.encoding = before; }
      },
    });
    return [enc("utf-8", "UTF-8"), enc("utf-8-bom", "UTF-8 (BOM)"), enc("cp949", "CP949 (EUC-KR · 한글 Windows 메모장·Toad)")];
  }

  /** 탭 내용을 파일로. 실패하면 인코딩을 되돌린다 (CP949 로 못 쓰는 글자 등) */
  private async writeTab(t: Tab, path: string, revertEncoding?: string) {
    try {
      const text = t.eol === "\r\n" ? t.editor.text().replace(/\r?\n/g, "\r\n") : t.editor.text();
      await api.writeFile(path, text, t.encoding);
      const renamed = t.filePath !== path;
      t.filePath = path;
      t.setDirty(false);
      t.refreshTitle();
      this.status(`저장했습니다${renamed ? `: ${path}` : ""} (${t.encoding}${t.eol === "\r\n" ? " · CRLF" : ""})`);
      this.git?.refreshSoon();
    } catch (e) {
      if (revertEncoding) t.encoding = revertEncoding;
      toast(errOf(e).message, "error");
    }
  }

  // ── 상태 표시 ────────────────────────────────────

  private showCompletionStatus(s: { phase: string; objects: number; columns: number; error?: string | null }) {
    const label: Record<string, string> = {
      connecting: "자동완성 준비 중…", objects: "자동완성: 객체 읽음", columns: "자동완성: 컬럼 읽음",
      synonyms: "자동완성: 동의어 읽음", keys: "자동완성: 키 읽음", ready: "자동완성", reconnecting: "자동완성: 다시 접속 중…", error: "자동완성 꺼짐",
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
    for (const k of ["run", "script", "explain", "debug", "monitor", "compare", "space", "copy"]) this.btn[k].disabled = !c || busy;
    // 분석은 접속 없이도 — 가져온 결과를 본다
    this.btn.analyze.disabled = busy;
    this.btn.cancel.disabled = !busy;
    // 실행 중에는 실행 단추 자리에 중지
    this.runGroup.hidden = busy;
    this.btn.cancel.hidden = !busy;
    if (!busy) this.btn.cancel.textContent = "■ 중지"; // 다른 탭으로 옮겨 와도 남은 경과 시간이 보이지 않게
    this.btn.commit.disabled = !c || busy || !t?.txnPending;
    this.btn.rollback.disabled = !c || busy || !t?.txnPending;
    this.txnGroup.hidden = !t?.txnPending;
    this.connPill.disabled = busy;
    this.connName.textContent = c ? c.profile : this.profileSel.value || "접속";
    this.connSub.textContent = c ? `${c.production ? "운영 · " : ""}${c.user}@${c.connect_string}${c.read_only ? " · 읽기 전용" : ""}` : "접속 안 됨";
    this.connPill.title = c ? `세션 SID ${c.sid ?? "?"} · ${c.at ? `${new Date(c.at).toLocaleTimeString()} 에 접속` : ""} — 화면을 오가도 SID 가 같으면 같은 접속입니다 (다시 접속하면 바뀝니다)\n누르면 다른 접속·끊기` : "누르면 접속할 프로필을 고릅니다";
    this.connPill.classList.toggle("prod", !!c?.production);
    this.connPill.classList.toggle("on", !!c);
    this.connPill.style.setProperty("--conn-color", c?.color || "");
    if (!busy) this.statusRight.textContent = c ? c.server_version : "";
    for (const x of this.tabs) x.refreshTitle();
  }

  /** 명령 검색 (Ctrl+Shift+P) — menus.ts */
  palette() {
    palette(this);
  }

  /** 설정 — settings.ts */
  async settings(page = "접속") {
    await settings(this, page);
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
/** 우클릭 "SQL 만들기" — 컬럼으로 틀을 만든다. 값 자리는 :컬럼 바인드, WHERE 는 기본 키 */
function sqlTemplate(kind: "select" | "insert" | "update" | "delete", table: string, d: TableDesc, alias?: string) {
  const cols = d.columns.map((c) => c.name);
  const pk = d.primary_key.length ? d.primary_key : cols.slice(0, 1);
  const where = pk.map((c) => `${c} = :${c}`).join("\n   AND ");
  const a = alias ? `${alias}.` : "";
  switch (kind) {
    case "select": return `SELECT ${cols.map((c) => a + c).join(",\n       ")}\n  FROM ${table}${alias ? ` ${alias}` : ""}\n WHERE 1 = 1`;
    case "insert": return `INSERT INTO ${table} (\n       ${cols.join(",\n       ")})\nVALUES (\n       ${cols.map((c) => `:${c}`).join(",\n       ")})`;
    case "update": {
      const set = cols.filter((c) => !pk.includes(c));
      return `UPDATE ${table}\n   SET ${(set.length ? set : cols).map((c) => `${c} = :${c}`).join(",\n       ")}\n WHERE ${where}`;
    }
    case "delete": return `DELETE FROM ${table}\n WHERE ${where}`;
  }
}

/** 경로 비교용: 구분자·대소문자 무시 (Windows) */
function normPath(p: string) {
  return p.replace(/\\/g, "/").replace(/\/+$/, "").toLowerCase();
}

// 시작 시간 나누기 — 모듈을 다 읽고 여기 오기까지(ESM 이라 위의 import 를 모두 실행한 뒤) · 첫 화면 구성
const T_MODULES = performance.now();
const app = new App(document.getElementById("app")!);
const T_BUILT = performance.now();
app.start(T_MODULES, T_BUILT).catch((e) => alertBox("시작 오류", errOf(e).message));
