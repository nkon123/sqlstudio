// SQLStudio 화면.
//
// 탭 하나 = 에디터 하나 + 세션 하나 + 결과 영역 하나. 탭끼리는 아무것도 공유하지 않는다
// (한 탭의 긴 쿼리가 다른 탭을 묶지 않는다 — 세션마다 서버 쪽 스레드가 따로 있다).

import { open as openDialog, save as saveDialog } from "@tauri-apps/plugin-dialog";
import { getCurrentWindow } from "@tauri-apps/api/window";
import {
  api, byteToUtf16, errOf, utf16ToByte,
  type Connected, type ErrView, type ExecView, type ObjectEntry, type Profile, type Provider, type TableDesc,
} from "./api";
import { AiPanel } from "./ai";
import { SqlEditor, type SchemaMap } from "./editor";
import { ResultGrid } from "./grid";
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
  dirty = false;
  txnPending = false;
  running = false;
  runStarted = 0;
  lastError: string | undefined;
  schema: SchemaMap = {};

  constructor(app: App, text = "") {
    const editorHost = h("div", { class: "editor-host" });
    this.grid = new ResultGrid({
      onNeedMore: async () => (this.conn ? api.fetchMore(this.conn.id, 1000) : null),
      onStatus: (t) => { this.gridStatus.textContent = t; },
    });
    const gridPane = h("div", { class: "pane result" }, this.grid.el);
    this.panes.set("result", gridPane).set("output", this.output).set("plan", this.plan).set("desc", this.desc);
    const btn = (p: Pane, label: string) => {
      const b = h("button", { class: "pane-tab", onclick: () => this.show(p) }, label);
      this.paneBtns.set(p, b);
      return b;
    };
    const bottom = h("section", { class: "bottom" },
      h("nav", { class: "pane-tabs" }, btn("result", "결과"), btn("output", "출력"), btn("plan", "실행계획"), btn("desc", "구조"),
        h("span", { class: "spacer" }), this.gridStatus,
        h("button", { class: "small", title: "가져온 행을 CSV 로 복사", onclick: () => this.copyCsv() }, "CSV 복사")),
      gridPane, this.output, this.plan, this.desc);
    const splitter = h("div", { class: "splitter" });
    this.root = h("div", { class: "tab-body" }, editorHost, splitter, bottom);
    this.editor = new SqlEditor(editorHost, {
      runStatement: () => app.runStatement(),
      runScript: () => app.runScript(),
      explain: () => app.explain(),
      save: () => app.save(),
    }, text);
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
  private btn: Record<string, HTMLButtonElement> = {};
  private browserList = h("div", { class: "obj-list" });
  private browserOwner = h("select", { title: "스키마" });
  private browserType = h("select", { title: "종류" },
    ...["TABLE", "VIEW", "PACKAGE", "PROCEDURE", "FUNCTION", "TRIGGER", "SEQUENCE", "SYNONYM", "TYPE", "MATERIALIZED VIEW", "INDEX"].map((t) => h("option", { value: t }, t)));
  private browserFilter = h("input", { placeholder: "이름 필터 (예: ORD%)" });
  private browserObjs: ObjectEntry[] = [];
  private pickedTables = new Set<string>();
  private ai: AiPanel;
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
      b("cancel", "중지", "실행 중인 쿼리를 취소", () => this.cancel(), "danger"),
      h("span", { class: "sep" }),
      b("commit", "커밋", "COMMIT", () => this.txn("commit")),
      b("rollback", "롤백", "ROLLBACK", () => this.txn("rollback")),
      h("span", { class: "sep" }),
      b("new", "새 탭", "새 에디터 (같은 접속으로)", () => this.newTab(true)),
      b("open", "열기", "SQL 파일 열기", () => this.openFile()),
      b("save", "저장", "Ctrl+S", () => this.save()),
      h("span", { class: "spacer" }),
      b("settings", "설정", "접속·AI·MCP 설정", () => this.settings()),
    );
    const browser = h("aside", { class: "browser" },
      h("div", { class: "browser-head" }, this.browserOwner, this.browserType),
      this.browserFilter,
      this.browserList,
      h("small", { class: "hint" }, "클릭: 구조 · 더블클릭: 이름 넣기 · 체크: AI 문맥에 포함"));
    this.ai = new AiPanel({
      context: () => this.aiContext(),
      insertSql: (s) => this.active?.editor.insert(s),
      replaceSql: (s) => this.replaceStatement(s),
    });
    const center = h("section", { class: "center" }, this.tabStrip, this.tabBodies);
    root.append(toolbar, h("main", { class: "layout" }, browser, center, this.ai.el),
      h("footer", { class: "statusbar" }, this.statusLeft, h("span", { class: "spacer" }), this.statusRight));

    this.browserOwner.addEventListener("change", () => this.loadObjects());
    this.browserType.addEventListener("change", () => this.loadObjects());
    let ft = 0;
    this.browserFilter.addEventListener("input", () => { clearTimeout(ft); ft = window.setTimeout(() => this.loadObjects(), 250); });
    window.addEventListener("keydown", (e) => {
      if (e.key === "Escape" && this.active?.running) this.cancel();
    });
  }

  async start() {
    const info = await api.appInfo();
    this.statusLeft.textContent = `SQLStudio ${info.version} · 설정: ${info.config_path}`;
    if (info.config_error) await alertBox("설정 파일 오류", `${info.config_error}\n\n기본 설정으로 시작합니다. 설정 화면에서 저장하면 파일을 다시 씁니다.`);
    await this.reloadProfiles();
    await this.ai.load();
    this.newTab(false);
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
    for (let attempt = 0; attempt < 3; attempt++) {
      try {
        const c = await api.connect(p.name, password);
        t.conn = { ...c, profile: p.name };
        t.txnPending = false;
        t.refreshTitle();
        t.log(`접속: ${c.user}@${c.connect_string} — ${c.server_version}${c.read_only ? " (읽기 전용)" : ""}`, "ok");
        this.status(`${p.name} 접속됨`);
        this.refresh();
        if (t === this.active) this.loadBrowser();
        this.loadCompletion(t);
        return;
      } catch (e) {
        const err = errOf(e);
        if (err.kind === "password_required" || err.ora_code === 1017) {
          if (err.ora_code === 1017) toast("비밀번호가 틀렸습니다 (ORA-01017)", "error");
          const pw = await passwordBox(p.name, p.user);
          if (pw == null) { this.status("접속 취소"); return; }
          password = pw;
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
    const a = await api.analyze(text, utf16ToByte(text, t.editor.cursor()), sel || undefined);
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

  private async txn(kind: "commit" | "rollback") {
    const t = this.need();
    if (!t || !t.conn) return;
    try {
      await (kind === "commit" ? api.commit(t.conn.id) : api.rollback(t.conn.id));
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
      const objs = await api.listObjects(t.conn.id, this.browserOwner.value || undefined, this.browserType.value,
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
        check, h("span", {}, o.name));
      row.addEventListener("click", () => this.showObject(o));
      row.addEventListener("dblclick", () => t?.editor.insert(o.owner === t.conn?.user ? o.name : q));
      frag.append(row);
    }
    if (this.browserObjs.length > 3000) frag.append(h("div", { class: "empty" }, `… ${this.browserObjs.length - 3000}개 더 (필터를 좁히세요)`));
    if (!this.browserObjs.length) frag.append(h("div", { class: "empty" }, "없음"));
    this.browserList.replaceChildren(frag);
  }

  private async showObject(o: ObjectEntry) {
    const t = this.active;
    if (!t?.conn) return;
    const q = `"${o.owner}"."${o.name}"`;
    try {
      if (["TABLE", "VIEW", "MATERIALIZED VIEW"].includes(o.object_type)) {
        const d = await api.describe(t.conn.id, q);
        this.renderDesc(t, d);
        t.schema[d.name] = d.columns.map((c) => c.name);
        t.editor.setSchema(t.schema, t.conn.user);
      } else {
        const ddl = await api.getDdl(t.conn.id, o.object_type, q);
        t.desc.replaceChildren(h("div", { class: "desc-actions" },
          h("button", { onclick: () => this.newTab(true, ddl) }, "새 탭에서 열기")), h("pre", {}, ddl));
      }
      t.show("desc");
    } catch (e) {
      toast(errOf(e).message, "error");
    }
  }

  private renderDesc(t: Tab, d: TableDesc) {
    const rows = d.columns.map((c) => h("tr", {},
      h("td", {}, d.primary_key.includes(c.name) ? "🔑" : ""), h("td", {}, c.name), h("td", {}, c.data_type),
      h("td", {}, c.nullable ? "" : "NOT NULL"), h("td", {}, c.default ?? ""), h("td", {}, c.comment ?? "")));
    const cols = d.columns.map((c) => c.name).join(", ");
    t.desc.replaceChildren(
      h("div", { class: "desc-head" },
        h("strong", {}, `${d.object_type} ${d.owner}.${d.name}`),
        d.comment ? h("span", {}, ` — ${d.comment}`) : null,
        d.num_rows ? h("small", {}, ` · 통계 ${Number(d.num_rows).toLocaleString()}행 (${d.last_analyzed ?? "?"})`) : null),
      h("div", { class: "desc-actions" },
        h("button", { onclick: () => t.editor.insert(`SELECT ${cols}\n  FROM ${d.owner}.${d.name}\n WHERE ROWNUM <= 100`) }, "SELECT 넣기"),
        h("button", { onclick: async () => { if (t.conn) this.newTab(true, await api.getDdl(t.conn.id, d.object_type, `"${d.owner}"."${d.name}"`)); } }, "DDL")),
      h("table", { class: "desc-table" }, h("thead", {}, h("tr", {}, ...["", "컬럼", "형식", "", "기본값", "설명"].map((x) => h("th", {}, x)))), h("tbody", {}, ...rows)),
      d.indexes.length ? h("div", { class: "desc-idx" }, h("strong", {}, "인덱스"),
        ...d.indexes.map((i) => h("div", {}, `${i.unique ? "UNIQUE " : ""}${i.name} (${i.columns.join(", ")})`))) : "",
    );
  }

  /** 자동완성: 접속 사용자의 테이블·뷰 이름 (컬럼은 구조를 볼 때 채운다) */
  private async loadCompletion(t: Tab) {
    if (!t.conn) return;
    try {
      const [tables, views] = await Promise.all([
        api.listObjects(t.conn.id, undefined, "TABLE"), api.listObjects(t.conn.id, undefined, "VIEW"),
      ]);
      for (const o of [...tables, ...views]) t.schema[o.name] ??= [];
      t.editor.setSchema(t.schema, t.conn.user);
    } catch { /* 자동완성은 없어도 된다 */ }
  }

  // ── 파일 ─────────────────────────────────────────

  async openFile() {
    const path = await openDialog({ multiple: false, filters: [{ name: "SQL", extensions: ["sql", "pls", "pks", "pkb", "prc", "fnc", "trg", "vw", "txt"] }] });
    if (!path || Array.isArray(path)) return;
    try {
      const f = await api.readFile(path);
      const cur = this.active;
      const t = cur && !cur.dirty && !cur.editor.text().trim() && !cur.filePath ? cur : this.newTab(true);
      t.editor.setText(f.text);
      t.filePath = path;
      t.encoding = f.encoding;
      t.setDirty(false);
      t.refreshTitle();
      this.status(`열었습니다 (${f.encoding})`);
    } catch (e) {
      toast(errOf(e).message, "error");
    }
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
      await api.writeFile(path, t.editor.text(), t.encoding);
      t.filePath = path;
      t.setDirty(false);
      t.refreshTitle();
      this.status(`저장했습니다 (${t.encoding})`);
    } catch (e) {
      toast(errOf(e).message, "error");
    }
  }

  // ── 상태 표시 ────────────────────────────────────

  status(text: string) {
    this.statusLeft.textContent = text;
  }

  refresh() {
    const t = this.active;
    const c = t?.conn;
    const busy = !!t?.running;
    this.btn.connect.disabled = busy;
    this.btn.disconnect.disabled = !c || busy;
    for (const k of ["run", "script", "explain"]) this.btn[k].disabled = !c || busy;
    this.btn.cancel.disabled = !busy;
    this.btn.commit.disabled = !c || busy || !t?.txnPending;
    this.btn.rollback.disabled = !c || busy || !t?.txnPending;
    this.txnBadge.hidden = !t?.txnPending;
    this.connBadge.textContent = c ? `${c.user}@${c.connect_string}${c.read_only ? " · 읽기 전용" : ""}` : "접속 안 됨";
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

  private async profilesPage() {
    const list = h("div", { class: "list" });
    const form = h("div", { class: "form" });
    const edit = (p?: Profile) => {
      const name = h("input", { value: p?.name ?? "" });
      const user = h("input", { value: p?.user ?? "" });
      const cs = h("input", { value: p?.connect_string ?? "", placeholder: "host:1521/SERVICE 또는 TNS 별칭" });
      const ro = h("input", { type: "checkbox", checked: !!p?.read_only });
      const color = h("input", { type: "color", value: p?.color ?? "#3a7bd5" });
      const useColor = h("input", { type: "checkbox", checked: !!p?.color });
      const timeout = h("input", { type: "number", min: "0", value: p?.call_timeout_secs ?? "" , placeholder: "없음" });
      form.replaceChildren(
        field("이름", name), field("사용자", user), field("접속 문자열", cs, "11g 는 Instant Client 가 필요합니다 (19c 권장)"),
        h("label", { class: "check" }, ro, "읽기 전용 (SELECT 만, 서버에서도 막음)"),
        h("label", { class: "check" }, useColor, "탭 색 표시 (운영 DB 는 빨강 권장)", color),
        field("호출 시간 상한(초)", timeout, "Oracle Client 18 이상에서만 동작"),
        h("small", {}, "비밀번호는 저장하지 않습니다. 접속할 때 묻거나, 환경변수 SQLSTUDIO_PW_<이름> 을 씁니다."),
        h("div", { class: "row" },
          h("button", { class: "primary", onclick: async (e: Event) => {
            e.preventDefault();
            try {
              await api.saveProfile({
                name: name.value.trim(), user: user.value.trim(), connect_string: cs.value.trim(),
                read_only: ro.checked, color: useColor.checked ? color.value : null,
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
    return h("div", { class: "split" }, list, form);
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
      const key = h("input", { type: "password", placeholder: p?.has_key ? "(설정됨)" : "이 실행 동안만 기억" });
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
        if (key.value) await api.setApiKey(np.name, key.value);
        return np;
      };
      form.replaceChildren(
        field("이름", name), field("종류", kind), field("주소", base, "Ollama: http://127.0.0.1:11434 · 사내 GPU 서버 주소도 됩니다"),
        field("모델", model, "예: qwen2.5-coder:14b, claude-opus-5-5"),
        field("API 키 환경변수", keyEnv), field("API 키", key, "파일에 저장하지 않습니다"),
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
