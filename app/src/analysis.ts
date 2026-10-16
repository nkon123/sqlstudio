// PL/SQL 분석 화면.
//
// 왼쪽: 스키마의 단위 목록 (체크해서 분석).  오른쪽: 요약 / 호출 관계 / 테이블 / 확인할 것.
// 분석은 단위를 조각내어 모델에 묻고 조각마다 JSON 으로 남긴다. 끝나면 저장된 결과를 이어 통합 분석한다.
// 모델은 소스를 읽기만 한다 — 이 화면에서 DB 로 가는 것은 사전 조회(ALL_SOURCE 등)뿐이다.

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { open as openDialog, save as saveDialog } from "@tauri-apps/plugin-dialog";
import { api, errOf, type Provider } from "./api";
import type { DfdView } from "./dfd";
import { confirmBox, field, h, modal, toast } from "./ui";
import { ResultGrid } from "./grid";
import { showMenu, type MenuItem } from "./ctxmenu";

interface UnitRef { owner: string; name: string; unit_type: string }
interface UnitRow extends UnitRef {
  /** 코드 폴더 목록일 때: 폴더 기준 경로 · 파일 경로 · 종류 · 함수/쿼리 수 */
  rel?: string; path?: string; kind?: string; count?: number;
  analyzed_at?: number | null; chunks?: number | null; failed?: number | null;
  model?: string | null; summary?: string | null; warning?: string | null;
}
interface Risk { line?: number | null; issue: string }
interface Insight { summary: string; steps: string[]; rules: string[]; risks: Risk[] }
interface TableUse { name: string; ops: string; lines: number[] }
interface CallUse { name: string; lines: number[] }
interface Mark { line: number; what: string }
interface Feed { cursor: string; via: string }
interface SqlStmt { line: number; end_line: number; kind: string; cursor?: string | null; writes: TableUse[]; reads: string[]; into?: string[]; fed_by?: Feed[]; text: string }
interface CursorFeed { table: string; ops: string; line: number; via: string }
interface CursorInfo { name: string; kind: string; line: number; reads: string[]; used_at: number[]; feeds: CursorFeed[] }
interface Facts {
  tables: TableUse[]; calls: CallUse[]; sequences: string[]; dynamic_sql: Mark[]; transactions: Mark[];
  raises: Mark[]; handles: string[]; swallowed: number[]; complexity: number; lines: number;
  statements?: SqlStmt[]; cursors?: CursorInfo[];
}
interface SubResult {
  path: string; name: string; overload: number; kind: string; signature: string; start_line: number; end_line: number;
  public?: boolean | null; facts: Facts; summary?: Insight | null; chunk_ids: string[];
}
interface UnitResult {
  key: string; owner: string; name: string; unit_type: string; lines: number; warning?: string | null;
  subprograms: SubResult[]; facts: Facts; summary?: Insight | null; stats: { chunks: number; asked: number; cached: number; failed: number };
  model?: string | null; analyzed_at: number;
  sql_summaries?: { subprogram?: string | null; kind: string; cursor?: string | null; line: number; end_line: number; chunk_ids: string[]; summary: Insight }[];
}
interface Chunk { id: string; part: number; parts: number; start_line: number; end_line: number; signature: string; context: string; code: string; facts: Facts; subprogram?: string | null }
interface ChunkResult { chunk: Chunk; insight?: Insight | null; error?: string | null; raw?: string | null; llm?: { model: string; elapsed_ms: number; attempts: number; repaired?: string[] } | null }
interface Node { id: string; unit: string; path: string; kind: string; signature: string; start_line: number; end_line: number; public?: boolean | null; summary?: string | null; complexity: number; commits: boolean; trigger_on?: string | null; trigger_events?: string | null }
interface Edge { from: string; to: string; resolved: boolean; external?: string | null; lines: number[] }
interface TableRow { table: string; by: Record<string, string>; conf?: Record<string, string>; impacted_entries: string[]; fed_from?: string[]; feeds_into?: string[] }
/** 사전(ALL_DEPENDENCIES)과 소스가 다른 곳 */
interface Gap { unit: string; kind: "dict_only" | "source_only" | "dynamic" | "no_dict"; table?: string | null; node?: string | null; line?: number | null; ops?: string | null; note: string }
interface DictStats { checked: number; unusable?: number; both: number; dict_only: number; source_only: number; dynamic: number; guessed: number }
interface Flow { node: string; unit: string; cursor: string; cursor_kind: string; from: string[]; to: string; ops: string; line: number; via: string; cursor_summary?: string | null }
interface Finding { node: string; unit: string; line?: number | null; source: string; kind: string; message: string }
interface Integrated {
  units: number; nodes: Node[]; edges: Edge[]; tables: TableRow[]; entries: string[]; cycles: string[][];
  unused: string[]; transactions: Record<string, string[]>; findings: Finding[]; flows?: Flow[]; overview?: Insight | null;
  gaps?: Gap[]; dict?: DictStats | null;
}
type Progress =
  | { type: "event"; run: number; event: { type: string; key?: string; id?: string; done?: number; total?: number; status?: string; elapsed_ms?: number; message?: string | null; what?: string } }
  | { type: "fetch"; run: number; index: number; total: number; key: string; error?: string | null }
  | { type: "finished"; run: number; error?: string | null; units: number; nodes: number; tables: number; findings: number; dir: string };

type View = "summary" | "calls" | "dfd" | "tables" | "gaps" | "findings" | "quality";
const keyOf = (u: UnitRef) => `${u.owner}.${u.name}.${u.unit_type.replace(/ /g, "_")}`;
const when = (t?: number | null) => (t ? new Date(t * 1000).toLocaleString() : "");
const short = (id: string) => id.split(".").slice(1).join(".");
/** 쓰임의 근거 — 테이블 화면의 배지 */
const CONF: Record<string, [string, string, string]> = {
  ok: ["사전✓", "ok", "소스에서 찾았고 사전(ALL_DEPENDENCIES)에도 있습니다"],
  src: ["소스만", "warn", "소스에서 찾았지만 사전에는 없습니다 — 실행 중 만드는 테이블, 동적 SQL 안의 이름, 또는 파서 오인일 수 있습니다"],
  dict: ["사전만", "warn", "사전에는 있지만 소스에서 쓰는 곳을 찾지 못했습니다 — 연산을 모릅니다"],
  llm: ["추정", "", "모델이 추정한 것 — 소스에서 직접 확인하지 못했습니다"],
};
const GAP_KIND: Record<Gap["kind"], string> = { dict_only: "사전에만", source_only: "소스에만", dynamic: "동적 SQL", no_dict: "사전 없음" };
/** 왼쪽 칸 너비를 끌어서 바꾼다 (더블클릭: 원래대로). 너비는 이 PC 에 기억한다. grid 의 둘째 자식으로 넣는다 */
function colSplitter(grid: HTMLElement, key: string, min = 90): HTMLElement {
  try {
    const saved = Number(localStorage.getItem(key));
    if (saved > 0) grid.style.setProperty("--c1", `${saved}px`);
  } catch { /* 저장소를 못 쓰면 기본 너비 */ }
  const handle = h("div", { class: "an-gutter", title: "끌어서 너비 조절 · 더블클릭: 원래대로" });
  handle.addEventListener("mousedown", (e) => {
    e.preventDefault();
    const first = grid.firstElementChild as HTMLElement;
    const x0 = e.clientX;
    const w0 = first.getBoundingClientRect().width;
    const max = grid.getBoundingClientRect().width - 160;
    document.body.classList.add("col-resizing");
    const move = (ev: MouseEvent) => grid.style.setProperty("--c1", `${Math.round(Math.max(min, Math.min(max, w0 + ev.clientX - x0)))}px`);
    const up = () => {
      window.removeEventListener("mousemove", move);
      window.removeEventListener("mouseup", up);
      document.body.classList.remove("col-resizing");
      try { localStorage.setItem(key, String(Math.round(first.getBoundingClientRect().width))); } catch { /* 기억 못 해도 된다 */ }
    };
    window.addEventListener("mousemove", move);
    window.addEventListener("mouseup", up);
  });
  handle.addEventListener("dblclick", () => {
    grid.style.removeProperty("--c1");
    try { localStorage.removeItem(key); } catch { /* 무시 */ }
  });
  return handle;
}

/** 이 PC 의 분석 결과 하나 (접속 프로필의 것, 또는 가져온 것) */
interface SetInfo {
  name: string; dir: string; units: number; generated_at?: number | null;
  imported?: { profile: string; db_user?: string | null; server?: string | null; exported_at: number; exported_by: string; app_version: string; units: number; imported_at?: number | null } | null;
}
interface ImportTarget { name: string; kind: "connection" | "imported"; units: number; same_db: boolean; already?: number | null }
interface ImportPeek { manifest: Manifest; targets: ImportTarget[]; suggested?: string | null }
interface MergeReport { added: number; updated: number; kept: number; reports: number; merged: boolean }
interface ImportDone extends MergeReport { name: string; connection: boolean }
const mergeText = (r: MergeReport) => {
  const parts = [r.added && `새 단위 ${r.added}`, r.updated && `바꾼 단위 ${r.updated}`, r.kept && `그대로 ${r.kept} (이쪽이 같거나 더 나중)`, r.reports && `흐름도 보고서 ${r.reports}`].filter(Boolean);
  return parts.length ? parts.join(" · ") : "바뀐 것 없음";
};
interface Manifest { profile: string; db_user?: string | null; server?: string | null; exported_at: number; exported_by: string; app_version: string; units: number; files: number }

/** 흐름도 가운데: 무엇을, 처리 상자를 프로시저 단위로 볼지 패키지 단위로 볼지 */
interface DfdTarget { focus: string; unit: "proc" | "pkg" }

/** 단위 목록 그리드의 열 — 마지막(키)은 숨긴다 */
const LIST_COLS = ["✓", "형식", "이름", "분석", "분석 시각", "모델", "키"].map((name) => ({ name, type_name: "VARCHAR2", nullable: true }));
const KEY_COL = LIST_COLS.length - 1;

const TYPE_LABEL: Record<string, string> = { "PACKAGE BODY": "패키지", PROCEDURE: "프로시저", FUNCTION: "함수", TRIGGER: "트리거", "TYPE BODY": "타입", "PROC BODY": "Proc", "XML BODY": "XML" };

export class AnalysisView {
  readonly el: HTMLElement;
  private ownerIn = h("input", { placeholder: "스키마 (비우면 접속 계정)", style: "width:150px" });
  private nameIn = h("input", { placeholder: "이름 (예: ORD%)", style: "width:130px" });
  private provSel = h("select", { style: "max-width:240px" });
  private badge = h("span", { class: "badge" });
  private linesIn = h("input", { type: "number", value: "120", min: "20", max: "400", style: "width:64px", title: "조각 하나의 최대 줄 수 — 작은 모델일수록 작게" });
  private jobsIn = h("input", { type: "number", value: "1", min: "1", max: "16", style: "width:48px", title: "동시에 물을 조각 수 (로컬 모델은 보통 1)" });
  private forceBox = h("input", { type: "checkbox" });
  private startBtn = h("button", { class: "primary" }, "분석 시작");
  /** 분석 대상: DB 의 PL/SQL 단위, 또는 폴더의 Pro*C·XML 쿼리 파일 */
  private mode: "db" | "code" = "db";
  private modeSel = h("select", { title: "분석할 대상" }, h("option", { value: "db" }, "DB 프로시저"), h("option", { value: "code" }, "코드 폴더 (Pro*C·XML)"));
  /** 분석할 코드 폴더들 — Pro*C 와 XML 이 다른 폴더에 있을 수 있어 여러 개를 추가한다 */
  private folders: string[] = [];
  private folderIn = h("input", { placeholder: "Pro*C(.pc)·XML 쿼리 파일이 있는 폴더 (하위 폴더 포함) — 누르면 목록 관리", readonly: true, style: "width:300px;cursor:pointer", hidden: true });
  private folderBtn = h("button", { hidden: true, title: "폴더를 추가하면 파일 목록만 읽습니다 — 분석은 목록에서 고른 뒤 '분석 시작'. 여러 폴더를 추가할 수 있습니다" }, "폴더 추가…");
  private stopBtn = h("button", { class: "danger", disabled: true }, "중지");
  private statusEl = h("span", { class: "dbg-status" }, "");
  private bar = h("div", { class: "an-bar-fill" });
  private grid = new ResultGrid({
    hiddenCols: [KEY_COL],
    onCellClick: (row, col) => this.onListClick(row[KEY_COL] ?? "", col),
    onDoubleClick: () => true,
    onContextMenu: (row, _col, e) => this.unitMenu(row[KEY_COL] ?? "", e),
  });
  /** 그리드에 마지막으로 넣은 목록 (같으면 행만 바꾸어 정렬·선택을 지킨다) */
  private gridOf: UnitRow[] | null = null;
  /** 접속 없이 보는 결과 (가져온 것 등). null 이면 탭 접속의 결과 */
  private set: string | null = null;
  private sets: SetInfo[] = [];
  private setSel = h("select", { title: "볼 분석 결과 — 접속한 DB 의 것, 또는 가져온 결과(접속 없이 본다)", style: "max-width:220px" });
  private liveOnly: HTMLElement[] = [];
  private allBox = h("input", { type: "checkbox", title: "전체 선택" });
  private content = h("div", { class: "an-content" });
  private navBtns = new Map<View, HTMLButtonElement>();
  private rows: UnitRow[] = [];
  private checked = new Set<string>();
  private providers: Provider[] = [];
  private g: Integrated | null = null;
  private view: View = "summary";
  private run: number | null = null;
  private selectedUnit: string | null = null;
  private unlisten: (() => void) | null = null;
  private main: HTMLElement;
  private dfd: DfdView | null = null;
  /** DFD 에 넘긴 결과 (같으면 다시 그리지 않는다) */
  private dfdOf: Integrated | null = null;
  private dfdFocus: DfdTarget | null = null;
  /** 흐름도에 마지막으로 가운데 둔 것 — 탭을 오갈 때 사용자가 옮긴 가운데를 덮어쓰지 않게 */
  private dfdCentered: string | null = null;

  constructor(private sessionId: () => number | undefined, private onClose: () => void) {
    this.startBtn.onclick = () => this.start();
    try {
      const saved = JSON.parse(localStorage.getItem("analysis.codeFolders") ?? "[]");
      if (Array.isArray(saved)) this.folders = saved.filter((x) => typeof x === "string");
      // 전에는 폴더 하나만 기억했다
      const one = localStorage.getItem("analysis.codeFolder");
      if (!this.folders.length && one) this.folders = [one];
    } catch { /* 기억 못 해도 된다 */ }
    this.showFolders();
    this.folderIn.onclick = () => void this.manageFolders();
    this.modeSel.onchange = () => this.setMode(this.modeSel.value === "code" ? "code" : "db");
    this.folderBtn.onclick = () => this.pickFolder();
    this.stopBtn.onclick = () => this.cancel();
    this.provSel.onchange = () => this.updateBadge();
    this.allBox.onchange = () => {
      for (const r of this.rows) this.allBox.checked ? this.checked.add(keyOf(r)) : this.checked.delete(keyOf(r));
      this.renderList();
    };
    const listBtn = h("button", { onclick: () => this.load() }, "목록");
    this.setSel.onchange = () => this.useSet(this.setSel.value || null);
    for (const i of [this.ownerIn, this.nameIn]) i.addEventListener("keydown", (e) => { if (e.key === "Enter") this.load(); });
    const toolbar = h("div", { class: "dbg-toolbar an-toolbar" },
      h("label", { class: "check", title: "볼 분석 결과" }, "결과", this.setSel), this.mergeBtn, this.removeBtn,
      h("button", { title: "보고 있는 분석 결과를 파일 하나(.sqla)로 — 분석할 수 없는 곳에서 '가져오기' 로 본다", onclick: () => this.exportSet() }, "내보내기"),
      h("button", { title: "다른 곳에서 내보낸 분석 결과(.sqla)를 가져와 접속 없이 본다", onclick: () => this.importSet() }, "가져오기"),
      h("span", { class: "sep" }),
      this.modeSel, this.ownerIn, this.nameIn, this.folderIn, this.folderBtn, listBtn, h("span", { class: "sep" }),
      this.provSel, this.badge,
      h("label", { class: "check", title: "조각 하나의 최대 줄 수" }, "조각", this.linesIn, "줄"),
      h("label", { class: "check", title: "동시에 물을 조각 수" }, "동시", this.jobsIn),
      h("label", { class: "check", title: "저장된 결과가 있어도 다시 묻는다" }, this.forceBox, "다시"),
      this.startBtn, this.stopBtn,
      h("span", { class: "spacer" }), this.statusEl,
      h("button", { title: "결과 폴더 경로 복사", onclick: () => this.copyDir() }, "폴더"),
      h("button", { title: "에디터로 돌아간다", onclick: () => this.close() }, "닫기"));
    const nav = h("nav", { class: "pane-tabs" });
    const tab = (v: View, label: string) => {
      const b = h("button", { class: "pane-tab", onclick: () => { this.view = v; this.render(); } }, label);
      this.navBtns.set(v, b);
      nav.append(b);
    };
    tab("summary", "요약");
    tab("calls", "호출 관계");
    tab("dfd", "데이터 흐름도");
    tab("tables", "테이블");
    tab("gaps", "사전 대조");
    tab("findings", "확인할 것");
    tab("quality", "품질");
    nav.append(h("span", { class: "spacer" }), h("button", { class: "small", title: "저장된 조각 결과로 통합 분석을 다시 만든다 (모델 없이)", onclick: () => this.reload(true) }, "다시 통합"));
    const left = h("div", { class: "an-left" },
      h("div", { class: "an-listbar" }, h("label", { class: "check", title: "목록 전체를 분석 대상으로" }, this.allBox, "전체"),
        h("span", { class: "dim" }, "✓ 칸을 눌러 분석 대상 · 우클릭: 흐름도·결과")),
      this.grid.el);
    this.main = h("div", { class: "an-main" }, left);
    this.main.append(colSplitter(this.main, "analysis.col.units"), h("div", { class: "an-right" }, nav, this.content));
    // 가져온 결과를 볼 때는 DB 가 필요한 것을 끈다
    this.liveOnly = [this.ownerIn, this.nameIn, listBtn, this.provSel, this.linesIn, this.jobsIn, this.forceBox, this.startBtn, this.modeSel, this.folderBtn];
    this.el = h("div", { class: "debugger analysis", hidden: true }, toolbar,
      h("div", { class: "an-progress" }, this.bar), this.main);
  }

  async open(owner?: string, name?: string) {
    this.el.hidden = false;
    if (owner) this.ownerIn.value = owner;
    if (name) this.nameIn.value = name;
    if (!this.unlisten) this.unlisten = await listen<Progress>("analysis-progress", (e) => this.onProgress(e.payload));
    try {
      this.providers = await api.listProviders();
    } catch { this.providers = []; }
    const prev = this.provSel.value;
    this.provSel.replaceChildren(h("option", { value: "" }, "모델 없이 (정적 분석만)"),
      ...this.providers.map((p) => h("option", { value: p.name, selected: p.name === prev }, `${p.name} · ${p.model}`)));
    if (!prev) {
      const local = this.providers.find((p) => !p.remote);
      if (local) this.provSel.value = local.name;
    }
    this.updateBadge();
    await this.refreshSets();
    // 접속이 없으면 이 PC 의 결과 중 하나를 연다
    if (this.sessionId() == null && !this.set && this.sets.length) this.set = this.sets[0].name;
    this.syncSetSel();
    await this.load();
    await this.reload(false);
  }

  /** 결과를 읽을 곳 — 명령 인자로 그대로 붙인다 */
  private src(): { id: number | null; set: string | null } | null {
    if (this.set) return { id: null, set: this.set };
    const id = this.sessionId();
    return id == null ? null : { id, set: null };
  }

  private async refreshSets() {
    try {
      this.sets = await invoke<SetInfo[]>("analysis_sets");
    } catch { this.sets = []; }
    this.syncSetSel();
  }

  private syncSetSel() {
    const live = this.sessionId() != null;
    const opts = [
      ...(live ? [h("option", { value: "" }, "지금 접속한 DB")] : []),
      ...this.sets.map((x) => h("option", { value: x.name, title: x.imported ? `${x.imported.profile} 에서 내보냄 · ${when(x.imported.exported_at)} · ${x.imported.exported_by}` : x.dir },
        `${x.name}${x.imported ? " · 가져옴" : ""} (${x.units})`)),
    ];
    this.setSel.replaceChildren(...(opts.length ? opts : [h("option", { value: "" }, "결과 없음")]));
    this.setSel.value = this.set ?? "";
    for (const el of this.liveOnly) (el as HTMLInputElement).disabled = !!this.set;
    this.updateRemoveBtn();
  }

  private removeBtn = h("button", { class: "small", title: "가져온 결과를 이 PC 에서 지운다", hidden: true, onclick: () => this.removeSet() }, "지우기");

  private mergeBtn = h("button", { class: "small", title: "따로 둔 이 결과를 접속의 결과에 합친다 — 접속한 DB 목록에 분석 표시가 나오게", hidden: true, onclick: () => this.mergeSet() }, "접속에 합치기");

  private updateRemoveBtn() {
    this.removeBtn.hidden = this.mergeBtn.hidden = !this.sets.find((x) => x.name === this.set)?.imported;
  }

  private async useSet(name: string | null) {
    this.set = name;
    this.g = null;
    this.dfdOf = null;
    this.selectedUnit = null;
    this.focusNode = null;
    this.checked.clear();
    this.syncSetSel();
    await this.load();
    await this.reload(false);
  }

  private async exportSet() {
    const src = this.src();
    if (!src) return toast("접속하거나 결과를 고르세요", "error");
    const ok = await confirmBox("분석 결과 내보내기",
      "파일에 PL/SQL 소스(조각 코드)와 SQL 문이 그대로 들어갑니다.\n사내 규정이 허용하는 곳으로만 옮기세요.\n\n" +
      "받는 곳에서는 분석 화면의 '가져오기' 로 열면 접속 없이 요약·호출 관계·흐름도·테이블을 봅니다.", "파일로 저장", false);
    if (!ok) return;
    const stamp = new Date().toISOString().slice(0, 10);
    const path = await saveDialog({ defaultPath: `${(this.set ?? (await this.liveName()) ?? "분석결과").replace(/[\\/:*?"<>|]/g, "_")}-${stamp}.sqla`, filters: [{ name: "SQLStudio 분석 결과", extensions: ["sqla"] }] });
    if (!path) return;
    this.statusEl.textContent = "내보내는 중…";
    try {
      const m = await invoke<Manifest>("analysis_export", { ...src, path });
      this.statusEl.textContent = "";
      toast(`내보냄: 단위 ${m.units}개 → ${path}`, "ok", 6000);
    } catch (e) {
      this.statusEl.textContent = "";
      toast(errOf(e).message, "error");
    }
  }

  /** 지금 접속한 탭의 결과 폴더 이름 (= 프로필) */
  private async liveName(): Promise<string | null> {
    const id = this.sessionId();
    if (id == null) return null;
    try {
      const dir = await invoke<string>("analysis_dir", { id, set: null });
      return dir.split(/[\\/]/).pop() ?? null;
    } catch { return null; }
  }

  private async importSet() {
    const path = await openDialog({ multiple: false, filters: [{ name: "SQLStudio 분석 결과", extensions: ["sqla"] }] });
    if (!path || Array.isArray(path)) return;
    let p: ImportPeek;
    try {
      p = await invoke<ImportPeek>("analysis_peek", { path });
    } catch (e) {
      return toast(errOf(e).message, "error");
    }
    const m = p.manifest;
    const NEW = "*new*";
    const conns = p.targets.filter((t) => t.kind === "connection");
    const sets = p.targets.filter((t) => t.kind === "imported");
    const label = (t: ImportTarget) => `${t.name}${t.same_db ? " ✓ 같은 DB" : ""}${t.units ? ` — 단위 ${t.units}개에 합침` : ""}`;
    const sel = h("select", { style: "width:100%" },
      ...(conns.length ? [h("optgroup", { label: "접속의 결과로 (목록의 분석 표시·값의 출처에 쓰인다)" }, ...conns.map((t) => h("option", { value: t.name }, label(t))))] : []),
      ...(sets.length ? [h("optgroup", { label: "따로 둔 결과 (접속 없이 본다)" }, ...sets.map((t) => h("option", { value: t.name }, label(t))))] : []),
      h("option", { value: NEW }, "새 이름으로 따로 두기…"));
    sel.value = p.suggested ?? NEW;
    const nameIn = h("input", { value: `${m.profile} (가져옴)`, style: "width:100%" });
    const nameField = field("이 PC 에서 부를 이름", nameIn, "같은 이름이 있으면 그 결과에 합칩니다");
    const note = h("div", { class: "hint" });
    const sync = () => {
      const t = p.targets.find((x) => x.name === sel.value);
      nameField.hidden = !!t;
      const lines: string[] = [];
      if (t?.already) lines.push(`이 파일은 ${when(t.already)} 에 이미 '${t.name}' 로 가져왔습니다 — 다시 가져와도 바뀌는 것이 없습니다.`);
      if (t?.units) lines.push("있는 결과에 합칩니다: 단위마다 더 나중에 분석한 것을 남깁니다 (이쪽에서 더 나중에 분석한 것은 그대로).");
      if (t?.kind === "connection" && !t.same_db) lines.push(`⚠ 이 파일은 ${m.db_user ?? m.profile} 에서 분석한 것입니다. 이 접속의 소스와 다르면 분석 표시가 실제와 맞지 않을 수 있습니다.`);
      if (!t) lines.push("접속 없이만 볼 수 있습니다. 접속한 DB 목록의 분석 표시에 쓰려면 위에서 접속을 고르세요.");
      note.replaceChildren(...lines.flatMap((l, i) => (i ? [h("br"), l] : [l])));
    };
    sel.onchange = sync;
    sync();
    const info = h("div", { class: "hint" },
      `${m.profile}${m.db_user ? ` · ${m.db_user}` : ""}${m.server ? ` · ${m.server}` : ""}`, h("br"),
      `단위 ${m.units}개 · ${when(m.exported_at)} 내보냄${m.exported_by ? ` (${m.exported_by})` : ""} · SQLStudio ${m.app_version}`);
    const go = await modal("분석 결과 가져오기", h("div", {}, info, field("가져올 곳", sel), nameField, note),
      [{ label: "취소", value: false }, { label: "가져오기", value: true, kind: "primary" }]);
    if (!go) return;
    const target = sel.value === NEW ? (nameIn.value.trim() || m.profile) : sel.value;
    let r: ImportDone;
    this.statusEl.textContent = "가져오는 중…";
    try {
      r = await invoke<ImportDone>("analysis_import", { path, target });
    } catch (e) {
      this.statusEl.textContent = "";
      return toast(errOf(e).message, "error");
    }
    this.statusEl.textContent = "";
    toast(`가져옴 → ${r.name}: ${mergeText(r)}`, "ok", 7000);
    await this.refreshSets();
    // 지금 접속한 DB 의 결과로 들어갔으면 접속 목록으로 (분석 표시가 보이게)
    const live = await this.liveName();
    await this.useSet(r.connection && live === r.name ? null : r.name);
  }

  /** 따로 둔 결과를 접속의 결과에 합친다 — 전에 다른 이름으로 가져와 접속 목록에 분석 표시가 안 보이던 것 */
  private async mergeSet() {
    const x = this.sets.find((s) => s.name === this.set);
    if (!x?.imported) return;
    let profiles: string[] = [];
    try {
      profiles = (await api.listProfiles()).map((p) => p.name);
    } catch { /* 아래에서 알린다 */ }
    if (!profiles.length) return toast("접속 프로필이 없습니다", "error");
    const live = await this.liveName();
    const sel = h("select", { style: "width:100%" }, ...profiles.map((n) => h("option", { value: n }, n)));
    sel.value = profiles.includes(x.imported.profile) ? x.imported.profile : live && profiles.includes(live) ? live : profiles[0];
    const del = h("input", { type: "checkbox", checked: true });
    const go = await modal("접속의 결과에 합치기", h("div", {},
      h("div", { class: "hint" }, `'${x.name}' (단위 ${x.units}개, ${x.imported.profile} 에서 내보냄) 을 접속의 결과에 합칩니다.`, h("br"),
        "단위마다 더 나중에 분석한 것을 남깁니다. 합치면 그 접속의 목록에 분석 표시가 나오고, 값의 출처에도 쓰입니다."),
      field("합칠 접속", sel),
      h("label", { class: "check" }, del, "합친 뒤 따로 둔 이 결과는 지우기")),
      [{ label: "취소", value: false }, { label: "합치기", value: true, kind: "primary" }]);
    if (!go) return;
    try {
      const r = await invoke<MergeReport>("analysis_merge_set", { set: x.name, into: sel.value });
      if (del.checked) await invoke("analysis_remove_set", { name: x.name });
      toast(`'${sel.value}' 에 합침: ${mergeText(r)}`, "ok", 7000);
    } catch (e) {
      return toast(errOf(e).message, "error");
    }
    await this.refreshSets();
    await this.useSet(live === sel.value ? null : sel.value);
  }

  private async removeSet() {
    const x = this.sets.find((s) => s.name === this.set);
    if (!x?.imported) return;
    if (!await confirmBox("가져온 결과 지우기", `'${x.name}' (단위 ${x.units}개) 을 이 PC 에서 지웁니다. 원래 파일(.sqla)은 그대로입니다.`, "지우기")) return;
    try {
      await invoke("analysis_remove_set", { name: x.name });
    } catch (e) {
      return toast(errOf(e).message, "error");
    }
    await this.refreshSets();
    await this.useSet(this.sessionId() != null ? null : this.sets[0]?.name ?? null);
  }

  close() {
    this.el.hidden = true;
    this.onClose();
  }

  private provider(): Provider | undefined {
    return this.providers.find((p) => p.name === this.provSel.value);
  }

  private updateBadge() {
    const p = this.provider();
    this.badge.textContent = !p ? "정적" : p.remote ? "외부 전송" : "로컬";
    this.badge.className = `badge ${p?.remote ? "warn" : "ok"}`;
    this.badge.title = p?.remote ? "소스 코드가 이 PC 밖의 서버로 갑니다" : "소스 코드가 이 PC 밖으로 나가지 않습니다";
  }

  private async load() {
    if (this.set) {
      try {
        this.rows = await invoke<UnitRow[]>("analysis_set_units", { set: this.set });
        const x = this.sets.find((s) => s.name === this.set);
        this.statusEl.textContent = x?.imported ? `가져온 결과 · ${x.imported.profile} · 단위 ${this.rows.length}개` : `단위 ${this.rows.length}개 (저장된 결과)`;
      } catch (e) {
        this.rows = [];
        toast(errOf(e).message, "error");
      }
      this.renderList();
      return;
    }
    const id = this.sessionId();
    if (id == null) {
      this.rows = [];
      this.renderList();
      this.statusEl.textContent = "";
      return toast("접속하거나, '가져오기' 로 다른 곳에서 내보낸 결과를 여세요", "info", 5000);
    }
    if (this.mode === "code") {
      if (!this.folders.length) { this.rows = []; this.renderList(); this.statusEl.textContent = "'폴더 추가…' 로 소스 폴더를 추가하세요 (Pro*C 와 XML 이 다른 폴더여도 됩니다)"; return; }
      this.statusEl.textContent = "파일을 읽는 중…";
      try {
        this.rows = await invoke<UnitRow[]>("analysis_code_list", { id, folders: this.folders });
        this.statusEl.textContent = `분석할 수 있는 파일 ${this.rows.length}개 (Pro*C·XML 쿼리) — 체크해서 '분석 시작'`;
      } catch (e) {
        this.rows = [];
        this.statusEl.textContent = "";
        this.renderList();
        return toast(errOf(e).message, "error");
      }
      if (!this.checked.size) for (const r of this.rows) if (!r.analyzed_at && !r.warning) this.checked.add(keyOf(r));
      this.renderList();
      return;
    }
    this.statusEl.textContent = "목록을 읽는 중…";
    try {
      this.rows = await invoke<UnitRow[]>("analysis_list", { id, owner: this.ownerIn.value.trim() || null, nameLike: this.nameIn.value.trim() || null });
      this.statusEl.textContent = `단위 ${this.rows.length}개`;
    } catch (e) {
      this.statusEl.textContent = "";
      return toast(errOf(e).message, "error");
    }
    // 처음 보면: 아직 분석하지 않은 것을 체크
    if (!this.checked.size) for (const r of this.rows) if (!r.analyzed_at && !r.warning) this.checked.add(keyOf(r));
    this.renderList();
  }

  private renderList() {
    const rows = this.rows.map((r) => {
      const k = keyOf(r);
      const state = r.warning ? "읽을 수 없음" : r.analyzed_at ? `${r.chunks ?? 0}조각${r.failed ? ` · 실패 ${r.failed}` : ""}` : "—";
      if (r.path) {
        const what = r.kind === "XML" ? `쿼리 ${r.count ?? 0}개` : `함수 ${r.count ?? 0}개`;
        return [this.checked.has(k) ? "✓" : "", r.kind ?? "", r.rel ?? r.name, r.analyzed_at ? `${state} · ${what}` : what,
          r.analyzed_at ? when(r.analyzed_at) : "", r.analyzed_at ? (r.model ?? "정적") : "", k];
      }
      return [this.checked.has(k) ? "✓" : "", TYPE_LABEL[r.unit_type] ?? r.unit_type, r.name, state,
        r.analyzed_at ? when(r.analyzed_at) : "", r.analyzed_at ? (r.model ?? "정적") : "", k];
    });
    if (this.gridOf !== this.rows) {
      this.gridOf = this.rows;
      this.grid.setPage({ columns: LIST_COLS, rows, has_more: false, fetched_total: rows.length });
    } else this.grid.replaceRows(rows);
  }

  private rowOf(key: string) {
    return this.rows.find((r) => keyOf(r) === key);
  }

  private toggleCheck(keys: string[], on?: boolean) {
    for (const k of keys) {
      const r = this.rowOf(k);
      if (!r || r.warning) continue;
      (on ?? !this.checked.has(k)) ? this.checked.add(k) : this.checked.delete(k);
    }
    this.statusEl.textContent = `${this.checked.size}개 선택`;
    this.renderList();
  }

  private onListClick(key: string, col: number) {
    if (col === 0) return this.toggleCheck([key]);
    if (this.rowOf(key)?.analyzed_at) this.showUnit(key);
  }

  /** 단위를 흐름도 가운데에 — 패키지는 패키지 상자로, 단독 프로시저·함수·트리거는 그 노드로 */
  private unitTarget(r: UnitRow): DfdTarget {
    const body = r.unit_type.endsWith("BODY");
    return { focus: `${r.owner}.${r.name}`, unit: body ? "pkg" : "proc" };
  }

  /** 이 단위가 지금 통합 결과의 흐름에 있는지 (분석 도중에는 통합이 아직 안 만들어져 없을 수 있다) */
  private hasFlow(t: DfdTarget): boolean {
    const g = this.g;
    if (!g) return false;
    return t.unit === "pkg" ? g.nodes.some((n) => n.id.startsWith(`${t.focus}.`)) : g.nodes.some((n) => n.id === t.focus);
  }

  /** 단위의 흐름도 — 방금 분석한 단위가 아직 통합에 없으면(분석 도중) 저장된 결과로 통합을 다시 만든 뒤 연다.
   *  없는 채로 열면 가운데를 못 찾아 전체 그림이 나온다 */
  private async openUnitDfd(t: DfdTarget, pop: boolean) {
    if (!this.hasFlow(t)) {
      this.statusEl.textContent = "통합 분석을 다시 만드는 중…";
      await this.reload(true);
      this.statusEl.textContent = "";
      if (!this.hasFlow(t)) return toast(`'${t.focus}' 는 흐름도에 올라간 것이 없습니다 — 읽고 쓰는 테이블·호출이 없거나, 분석이 아직 끝나지 않았습니다`, "info", 7000);
    }
    if (pop) this.popDfd(t);
    else this.showDfd(t);
  }

  private showDfd(t: DfdTarget) {
    this.dfdFocus = t;
    this.view = "dfd";
    this.render();
  }

  private popDfd(t: DfdTarget) {
    invoke("open_dfd_window", { ...this.src(), center: t.focus, dir: "both", unit: t.unit }).catch((e) => toast(errOf(e).message, "error"));
  }

  private unitMenu(key: string, e: MouseEvent) {
    const r = this.rowOf(key);
    if (!r) return;
    const picked = this.grid.selectedRows().map((x) => x[KEY_COL] ?? "").filter(Boolean);
    const keys = picked.includes(key) ? picked : [key];
    const done = !!r.analyzed_at && !!this.g;
    const t = this.unitTarget(r);
    const items: MenuItem[] = [
      { title: `${TYPE_LABEL[r.unit_type] ?? r.unit_type} ${r.name}` },
      { label: "흐름도 보기", hint: t.unit === "pkg" ? "패키지 단위" : "", disabled: !done, run: () => void this.openUnitDfd(t, false) },
      { label: "흐름도 새 창", disabled: !done, run: () => void this.openUnitDfd(t, true) },
      { label: "분석 결과 보기", disabled: !r.analyzed_at, run: () => this.showUnit(key) },
      ...(t.unit === "proc" ? [{ label: "호출 관계", disabled: !done, run: () => { this.focusNode = t.focus; this.view = "calls"; this.render(); } }] : []),
      { sep: true },
      { label: keys.length > 1 ? `분석 대상으로 (${keys.length}개)` : "분석 대상으로", run: () => this.toggleCheck(keys, true) },
      { label: keys.length > 1 ? `분석 대상에서 빼기 (${keys.length}개)` : "분석 대상에서 빼기", run: () => this.toggleCheck(keys, false) },
      { label: "이름 복사", run: () => navigator.clipboard.writeText(keys.map((k) => this.rowOf(k)?.name ?? "").join("\n")).catch(() => {}) },
    ];
    if (!done) items.splice(1, 0, { title: "아직 분석하지 않아 흐름도가 없습니다" });
    showMenu(e.clientX, e.clientY, items);
  }

  /** 서브프로그램 이름 우클릭 */
  private nodeMenu(id: string, e: MouseEvent) {
    e.preventDefault();
    const t: DfdTarget = { focus: id, unit: "proc" };
    showMenu(e.clientX, e.clientY, [
      { title: short(id) || id },
      { label: "흐름도 보기", run: () => this.showDfd(t) },
      { label: "흐름도 새 창", run: () => this.popDfd(t) },
      { label: "호출 관계", run: () => { this.focusNode = id; this.view = "calls"; this.render(); } },
      { sep: true },
      { label: "이름 복사", run: () => navigator.clipboard.writeText(id).catch(() => {}) },
    ]);
  }


  private async start() {
    if (this.set) return toast("가져온 결과를 보는 중입니다 — 분석하려면 '결과' 에서 '지금 접속한 DB' 를 고르세요", "error");
    const id = this.sessionId();
    if (id == null) return toast("먼저 접속하세요", "error");
    const picked = this.rows.filter((r) => this.checked.has(keyOf(r)));
    const units = picked.map(({ owner, name, unit_type }) => ({ owner, name, unit_type }));
    if (!units.length) return toast(this.mode === "code" ? "분석할 파일을 체크하세요" : "분석할 단위를 체크하세요", "error");
    const p = this.provider();
    if (p?.remote) {
      const ok = await confirmBox("외부로 소스 전송",
        `'${p.name}' 은 외부 서버입니다. 선택한 ${units.length}개 단위의 PL/SQL 소스가 이 PC 밖으로 나갑니다.\n\n` +
        "사내 규정상 허용된 경우에만 진행하세요. 로컬 모델(Ollama 등)을 쓰면 밖으로 나가지 않습니다.", "보내고 분석");
      if (!ok) return;
    }
    const options = {
      limits: { max_lines: Math.max(20, Number(this.linesIn.value) || 120), max_chars: Math.max(1000, (Number(this.linesIn.value) || 120) * 50) },
      lang: "ko", jobs: Math.max(1, Number(this.jobsIn.value) || 1), force: this.forceBox.checked, rollup: true,
    };
    try {
      this.run = this.mode === "code"
        ? await invoke<number>("analysis_code_start", { id, folders: this.folders, paths: picked.map((r) => r.path), provider: p?.name ?? null, allowRemote: !!p?.remote, options })
        : await invoke<number>("analysis_start", { id, units, provider: p?.name ?? null, allowRemote: !!p?.remote, options });
    } catch (e) {
      return toast(errOf(e).message, "error");
    }
    this.startBtn.disabled = true;
    this.stopBtn.disabled = false;
    this.bar.style.width = "0";
    this.statusEl.textContent = `시작 — 단위 ${units.length}개`;
    this.total = units.length;
  }

  private setMode(m: "db" | "code") {
    this.mode = m;
    this.modeSel.value = m;
    const code = m === "code";
    this.ownerIn.hidden = code;
    this.nameIn.hidden = code;
    this.folderIn.hidden = !code;
    this.folderBtn.hidden = !code;
    this.checked.clear();
    this.rows = [];
    this.gridOf = null;
    void this.load();
  }

  private showFolders() {
    const last = (f: string) => f.split(/[\\/]/).filter(Boolean).pop() ?? f;
    this.folderIn.value = !this.folders.length ? "" : this.folders.length === 1 ? this.folders[0] : `폴더 ${this.folders.length}개 — ${this.folders.map(last).join(", ")}`;
    this.folderIn.title = this.folders.join("\n");
    try { localStorage.setItem("analysis.codeFolders", JSON.stringify(this.folders)); } catch { /* 기억 못 해도 된다 */ }
  }

  /** 폴더 추가 — 목록만 읽는다 (분석은 고른 뒤 '분석 시작') */
  private async pickFolder() {
    const f = await openDialog({ directory: true, multiple: false, title: "Pro*C·XML 쿼리 소스 폴더 추가 (하위 폴더 포함)", defaultPath: this.folders[this.folders.length - 1] || undefined });
    if (!f || Array.isArray(f)) return;
    if (this.folders.includes(f)) return toast("이미 추가한 폴더입니다", "info");
    this.folders.push(f);
    this.showFolders();
    await this.load();
  }

  /** 추가한 폴더 목록 — 빼기 */
  private async manageFolders() {
    if (!this.folders.length) return this.pickFolder();
    const body = h("div", { class: "folder-list" });
    const draw = () => body.replaceChildren(
      ...this.folders.map((f, i) => h("div", { class: "row", style: "display:flex;gap:8px;align-items:center;margin:4px 0" },
        h("code", { style: "flex:1;word-break:break-all" }, f),
        h("button", { class: "small", onclick: () => { this.folders.splice(i, 1); this.showFolders(); draw(); } }, "빼기"))),
      h("p", { class: "hint" }, "빼도 이미 분석한 결과는 그대로 남습니다 (목록에서만 안 보입니다)."));
    draw();
    await modal("코드 폴더", body, [{ label: "닫기", value: true, kind: "primary" }]);
    this.checked.clear();
    await this.load();
  }

  private total = 0;
  private unitIndex = 0;

  private onProgress(p: Progress) {
    if (p.run !== this.run) return;
    if (p.type === "fetch") {
      this.unitIndex = p.index;
      this.total = p.total;
      if (p.error) toast(`${p.key}: ${p.error}`, "error");
      this.statusEl.textContent = `[${p.index + 1}/${p.total}] ${short(p.key)}`;
    } else if (p.type === "event") {
      const e = p.event;
      if (e.type === "chunk" && e.total) {
        const frac = (this.unitIndex + (e.done ?? 0) / e.total) / Math.max(1, this.total);
        this.bar.style.width = `${Math.round(frac * 100)}%`;
        const t = e.elapsed_ms ? ` ${(e.elapsed_ms / 1000).toFixed(1)}s` : "";
        this.statusEl.textContent = `[${this.unitIndex + 1}/${this.total}] ${short(e.key ?? "")} · 조각 ${e.done}/${e.total} ${e.status}${t}`;
        if (e.status === "failed" && e.message) this.statusEl.title = e.message;
      } else if (e.type === "rollup") {
        this.statusEl.textContent = e.key === "사전 대조" || e.key === "접속" ? e.what ?? "" : `[${this.unitIndex + 1}/${this.total}] 요약: ${e.what}`;
      } else if (e.type === "unit_done") {
        const k = e.key!;
        const r = this.rows.find((x) => keyOf(x) === k);
        if (r) { r.analyzed_at = Date.now() / 1000; this.checked.delete(k); this.renderList(); }
      }
    } else if (p.type === "finished") {
      void this.refreshSets();
      this.run = null;
      this.startBtn.disabled = false;
      this.stopBtn.disabled = true;
      this.bar.style.width = "100%";
      this.statusEl.textContent = `${p.error ? `${p.error} · ` : "끝 · "}단위 ${p.units} · 서브프로그램 ${p.nodes} · 테이블 ${p.tables} · 확인할 것 ${p.findings}`;
      if (p.error) toast(p.error, "error");
      this.load().then(() => this.reload(false));
    }
  }

  private async cancel() {
    if (this.run == null) return;
    await invoke("analysis_cancel", { run: this.run });
    this.statusEl.textContent = "멈추는 중 — 진행 중인 조각까지 저장합니다";
  }

  private async copyDir() {
    const src = this.src();
    if (!src) return;
    const dir = await invoke<string>("analysis_dir", src);
    await navigator.clipboard.writeText(dir).catch(() => {});
    toast(`결과 폴더: ${dir} (복사함) — integrated/report.md, 단위·조각별 JSON`, "info", 6000);
  }

  private async reload(rebuild: boolean) {
    const src = this.src();
    if (!src) { this.g = null; this.render(); return; }
    try {
      this.g = await invoke<Integrated | null>("analysis_result", { ...src, rebuild });
    } catch (e) {
      toast(errOf(e).message, "error");
    }
    this.render();
  }

  // ── 오른쪽 ─────────────────────────────────────

  private render() {
    for (const [v, b] of this.navBtns) b.classList.toggle("active", v === this.view);
    // 그림은 넓게 — 단위 목록을 접는다
    this.main.classList.toggle("an-wide", this.view === "dfd");
    const g = this.g;
    if (!g) {
      this.content.replaceChildren(h("div", { class: "hint" },
        "아직 분석 결과가 없습니다. 왼쪽에서 단위를 체크하고 '분석 시작'. 모델 없이(정적 분석)도 호출 관계·테이블 CRUD 는 나옵니다."));
      return;
    }
    if (this.view === "summary") return this.renderSummary(g);
    if (this.view === "calls") return this.renderCalls(g);
    if (this.view === "dfd") return void this.renderDfd(g);
    if (this.view === "tables") return this.renderTables(g);
    if (this.view === "gaps") return this.renderGaps(g);
    if (this.view === "quality") return void this.renderQuality();
    return this.renderFindings(g);
  }

  private renderSummary(g: Integrated) {
    const unresolved = g.edges.filter((e) => e.external === "unknown").length;
    const box = h("div", { class: "an-pad" },
      h("div", { class: "an-stats" },
        stat("단위", g.units), stat("서브프로그램", g.nodes.length), stat("호출", g.edges.length), stat("못 푼 호출", unresolved),
        stat("테이블", g.tables.length), stat("시작점", g.entries.length), stat("순환", g.cycles.length), stat("확인할 것", g.findings.length)));
    if (g.dict) {
      const d = g.dict;
      box.append(h("div", { class: "an-stats", title: "사전(ALL_DEPENDENCIES)과 파서가 찾은 테이블을 (단위, 테이블) 쌍으로 맞댄 결과" },
        stat("사전 본 단위", d.checked), ...(d.unusable ? [stat("사전 없음", d.unusable)] : []), stat("사전·소스 모두", d.both), stat("사전에만", d.dict_only), stat("소스에만", d.source_only),
        stat("동적 SQL", d.dynamic), stat("모델 추정", d.guessed)));
    }
    if (g.overview) box.append(h("h3", {}, "전체 요약"), insightEl(g.overview));
    if (this.selectedUnit) {
      box.append(h("div", { class: "hint" }, "단위를 불러오는 중…"));
      this.content.replaceChildren(box);
      this.showUnit(this.selectedUnit);
      return;
    }
    box.append(h("h3", {}, "시작점 (아무도 부르지 않는 공개 서브프로그램·트리거)"),
      h("ul", { class: "an-ul" }, ...g.entries.slice(0, 300).map((e) => {
        const tx = g.transactions[e];
        return h("li", {}, this.nodeLink(e), tx ? h("span", { class: "badge warn", title: tx.join("\n") }, `COMMIT ${tx.length}곳`) : "");
      })));
    box.append(h("p", { class: "hint" }, "왼쪽 목록에서 분석된 단위를 누르면 서브프로그램별 요약과 조각을 봅니다."));
    this.content.replaceChildren(box);
  }

  private async showUnit(key: string) {
    const src = this.src();
    if (!src) return;
    this.selectedUnit = key;
    this.renderList();
    this.view = "summary";
    for (const [v, b] of this.navBtns) b.classList.toggle("active", v === this.view);
    let d: { unit: UnitResult; chunks: ChunkResult[] };
    try {
      d = await invoke("analysis_unit", { ...src, key });
    } catch (e) {
      return toast(errOf(e).message, "error");
    }
    const u = d.unit;
    const box = h("div", { class: "an-pad" },
      h("div", { class: "an-head" }, h("button", { class: "small", onclick: () => { this.selectedUnit = null; this.renderList(); this.render(); } }, "← 전체"),
        h("strong", {}, `${TYPE_LABEL[u.unit_type] ?? u.unit_type} ${u.owner}.${u.name}`),
        h("small", { class: "dim" }, `${u.lines}행 · 조각 ${u.stats.chunks}${u.stats.failed ? ` · 실패 ${u.stats.failed}` : ""} · ${u.model ?? "정적 분석"} · ${when(u.analyzed_at)}`)));
    if (u.warning) box.append(h("p", { class: "warn" }, u.warning));
    if (u.summary) box.append(insightEl(u.summary));
    box.append(factsEl(u.facts));
    const sqls = u.sql_summaries ?? [];
    if (sqls.length) box.append(h("div", { class: "an-block" }, h("h4", {}, `긴 SQL ${sqls.length} (여러 조각을 모아 요약)`),
      ...sqls.map((q) => h("details", { class: "an-chunk", open: true },
        h("summary", {}, h("code", {}, q.cursor ? `${q.kind} ${q.cursor}` : q.kind), h("small", { class: "dim" }, ` ${q.line}~${q.end_line}행 · 조각 ${q.chunk_ids.length}개${q.subprogram ? ` · ${q.subprogram}` : " · 전역"}`)),
        insightEl(q.summary)))));
    for (const s of u.subprograms) {
      const chunks = d.chunks.filter((c) => s.chunk_ids.includes(c.chunk.id));
      const det = h("details", { class: "an-sub" },
        h("summary", {}, h("code", {}, s.path), " ",
          s.public === true ? h("span", { class: "badge ok" }, "공개") : s.public === false ? h("span", { class: "badge" }, "비공개") : "",
          h("small", { class: "dim" }, ` ${s.start_line}~${s.end_line}행 · 복잡도 ${s.facts.complexity}`),
          h("div", { class: "an-sub-sum" }, s.summary?.summary ?? (chunks.some((c) => c.error) ? "분석 실패 — 다시 돌리면 실패한 조각만 묻습니다" : ""))),
        h("div", { class: "an-sig" }, s.signature),
        s.summary ? insightEl(s.summary) : "",
        factsEl(s.facts),
        cursorsEl(s.facts.cursors ?? []),
        stmtsEl(s.facts.statements ?? []),
        ...chunks.map((c) => chunkEl(c)));
      box.append(det);
    }
    const globals = d.chunks.filter((c) => !c.chunk.subprogram);
    if (globals.length) box.append(h("details", { class: "an-sub" }, h("summary", {}, "전역 선언"), ...globals.map(chunkEl)));
    this.content.replaceChildren(box);
  }

  private nodeLink(id: string): HTMLElement {
    const a = h("a", { href: "#", class: "an-link", title: id }, short(id) || id);
    a.onclick = (e) => { e.preventDefault(); this.view = "calls"; this.focusNode = id; this.render(); };
    a.oncontextmenu = (e) => this.nodeMenu(id, e);
    return a;
  }

  private focusNode: string | null = null;
  private callFilter = "";

  private renderCalls(g: Integrated) {
    const input = h("input", { placeholder: "서브프로그램 찾기", value: this.callFilter, style: "width:260px" });
    const list = h("ul", { class: "an-ul an-nodes" });
    const detail = h("div", { class: "an-detail" });
    const fill = () => {
      const q = input.value.trim().toUpperCase();
      this.callFilter = input.value;
      const nodes = g.nodes.filter((n) => !q || n.id.includes(q)).slice(0, 400);
      list.replaceChildren(...nodes.map((n) => {
        const li = h("li", { class: n.id === this.focusNode ? "active" : "" }, short(n.id));
        li.onclick = () => { this.focusNode = n.id; fill(); };
        li.oncontextmenu = (e) => this.nodeMenu(n.id, e);
        return li;
      }));
      detail.replaceChildren(this.focusNode ? this.nodeDetail(g, this.focusNode) : h("div", { class: "hint" }, "왼쪽에서 서브프로그램을 고르세요"));
    };
    input.oninput = fill;
    fill();
    const split = h("div", { class: "an-split" }, h("div", { class: "an-col" }, input, list));
    split.append(colSplitter(split, "analysis.col.nodes"), detail);
    this.content.replaceChildren(split);
  }

  /** 흐름도 코드(그림 라이브러리 포함)는 이 탭을 처음 열 때 불러온다 — 시작 번들을 키우지 않게 */
  private async renderDfd(g: Integrated) {
    if (!this.dfd) {
      this.content.replaceChildren(h("div", { class: "hint an-pad" }, "흐름도를 불러오는 중…"));
      const { DfdView } = await import("./dfd");
      this.dfd ??= new DfdView((id) => { this.view = "calls"; this.focusNode = id; this.render(); }, {
        popout: (center, dir) => invoke("open_dfd_window", { ...this.src(), center, dir }).catch((e) => toast(errOf(e).message, "error")),
        profile: async () => this.set ?? (await this.liveName()),
      });
      if (this.view !== "dfd") return;
    }
    this.content.replaceChildren(this.dfd.el);
    // 가운데: 방금 고른 것 → 호출 관계에서 보던 서브프로그램 → 열어 둔 단위. 바뀌었을 때만 옮긴다 (사용자가 옮긴 가운데를 지킨다)
    const want = this.dfdFocus ?? this.defaultDfdTarget(g);
    this.dfdFocus = null;
    if (this.dfdOf !== g || (want && `${want.unit}:${want.focus}` !== this.dfdCentered)) {
      this.dfdOf = g;
      this.dfd.setData(g, want?.focus ?? null, want ? "both" : null, want?.unit ?? null);
      this.dfdCentered = want ? `${want.unit}:${want.focus}` : null;
    }
  }

  private defaultDfdTarget(g: Integrated): DfdTarget | null {
    if (this.focusNode && g.nodes.some((n) => n.id === this.focusNode)) return { focus: this.focusNode, unit: "proc" };
    const r = this.selectedUnit ? this.rowOf(this.selectedUnit) : undefined;
    return r ? this.unitTarget(r) : null;
  }

  private nodeDetail(g: Integrated, id: string): HTMLElement {
    const n = g.nodes.find((x) => x.id === id);
    const callers = g.edges.filter((e) => e.to === id);
    const callees = g.edges.filter((e) => e.from === id);
    const tables = g.tables.filter((t) => t.by[id]);
    const box = h("div", { class: "an-pad" },
      h("div", { class: "an-head" }, h("strong", {}, id),
        h("button", { class: "small", title: "이 서브프로그램을 가운데에 두고 데이터 흐름도로 본다", onclick: () => this.showDfd({ focus: id, unit: "proc" }) }, "흐름도")),
      n ? h("div", { class: "an-sig" }, `${n.signature}   (${n.start_line}~${n.end_line}행, 복잡도 ${n.complexity}${n.commits ? ", COMMIT/ROLLBACK" : ""})`) : "",
      n?.summary ? h("p", {}, n.summary) : "");
    box.append(h("h4", {}, `부르는 곳 ${callers.length}`),
      h("ul", { class: "an-ul" }, ...callers.map((e) => h("li", {}, this.nodeLink(e.from), h("small", { class: "dim" }, ` ${e.lines.join(", ")}행`)))));
    box.append(h("h4", {}, `부르는 것 ${callees.length}`),
      h("ul", { class: "an-ul" }, ...callees.map((e) => h("li", {}, e.resolved ? this.nodeLink(e.to) : h("span", { class: "dim" }, `${e.to} ${e.external === "system" ? "(시스템)" : "(밖)"}`),
        h("small", { class: "dim" }, ` ${e.lines.join(", ")}행`)))));
    box.append(h("h4", {}, `테이블 ${tables.length}`),
      h("ul", { class: "an-ul" }, ...tables.map((t) => h("li", {}, h("code", {}, t.by[id]), " ", t.table))));
    const flows = (g.flows ?? []).filter((f) => f.node === id);
    if (flows.length) box.append(h("h4", {}, `커서 → DML ${flows.length}`),
      h("ul", { class: "an-ul" }, ...flows.map((f) => h("li", { title: f.cursor_summary ?? "" }, h("code", {}, f.cursor), ` (${f.from.join(", ") || "?"}) → `, h("code", {}, f.ops), ` ${f.to} `,
        h("small", { class: "dim" }, `${f.line}행 · ${viaLabel(f.via)}`)))));
    const tx = g.transactions[id];
    if (tx) box.append(h("h4", {}, "이 시작점에서 닿는 COMMIT/ROLLBACK"), h("ul", { class: "an-ul" }, ...tx.map((x) => h("li", {}, this.nodeLink(x)))));
    return box;
  }

  private tableFilter = "";

  private renderTables(g: Integrated) {
    const input = h("input", { placeholder: "테이블 찾기", value: this.tableFilter, style: "width:260px" });
    const tbody = h("tbody", {});
    const fill = () => {
      const q = input.value.trim().toUpperCase();
      this.tableFilter = input.value;
      tbody.replaceChildren(...g.tables.filter((t) => !q || t.table.includes(q)).slice(0, 500).map((t) => {
        const ops = (o: string) => Object.entries(t.by).filter(([, v]) => v.includes(o)).length;
        const users = h("td", {}, ...Object.entries(t.by).map(([n, o]) => {
          const c = CONF[t.conf?.[n] ?? ""];
          return h("div", {}, h("code", {}, o.padEnd(4)), " ", n.endsWith(")") ? short(n) : this.nodeLink(n),
            c ? h("span", { class: `badge ${c[1]}`, title: c[2], style: "margin-left:4px" }, c[0]) : "");
        }));
        return h("tr", {}, h("td", {}, h("strong", {}, t.table)), h("td", { class: "num" }, String(ops("C") || "")), h("td", { class: "num" }, String(ops("R") || "")),
          h("td", { class: "num" }, String(ops("U") || "")), h("td", { class: "num" }, String(ops("D") || "")), users,
          h("td", { class: "an-flow" }, ...(t.fed_from ?? []).map((x) => h("div", { title: flowsTo(g, t.table, x) }, `← ${x}`)), ...(t.feeds_into ?? []).map((x) => h("div", { class: "dim" }, `→ ${x}`))),
          h("td", { title: t.impacted_entries.join("\n") }, String(t.impacted_entries.length)));
      }));
    };
    input.oninput = fill;
    fill();
    this.content.replaceChildren(h("div", { class: "an-pad" }, input,
      h("table", { class: "an-table" }, h("thead", {}, h("tr", {}, ...["테이블", "C", "R", "U", "D", "쓰는 곳", "커서 흐름", "영향 시작점"].map((x) => h("th", {}, x)))), tbody)));
  }

  private renderGaps(g: Integrated) {
    const gaps = g.gaps ?? [];
    const head = h("p", { class: "hint" },
      "사전(ALL_DEPENDENCIES)은 단위가 참조하는 테이블을 확실히 알려 주지만 연산(C/R/U/D)은 모릅니다. 파서는 연산과 줄을 알지만 동적 SQL·동의어를 놓칠 수 있습니다. ",
      "둘이 다른 곳만 모았습니다. 모델을 고르고 분석하면 이 부분만 모델에 물어 '추정'으로 채웁니다.");
    if (!g.dict) {
      this.content.replaceChildren(h("div", { class: "an-pad" }, head,
        h("div", { class: "hint" }, "사전 정보가 없습니다. 이 버전에서 단위를 다시 분석하면 사전 의존성을 함께 읽습니다 (바뀌지 않은 조각은 모델에 다시 묻지 않습니다).")));
      return;
    }
    const sel = h("select", {}, h("option", { value: "" }, `전체 ${gaps.length}`),
      ...(Object.keys(GAP_KIND) as Gap["kind"][]).map((k) => h("option", { value: k }, `${GAP_KIND[k]} ${gaps.filter((x) => x.kind === k).length}`)));
    const tbody = h("tbody", {});
    const fill = () => {
      tbody.replaceChildren(...gaps.filter((x) => !sel.value || x.kind === sel.value).slice(0, 1000).map((x) =>
        h("tr", {},
          h("td", {}, GAP_KIND[x.kind] ?? x.kind),
          h("td", {}, x.table ? h("strong", {}, x.table) : h("span", { class: "dim" }, "(모름)")),
          h("td", {}, x.node && g.nodes.some((n) => n.id === x.node) ? this.nodeLink(x.node) : short(x.node ?? x.unit)),
          h("td", { class: "num" }, x.line ? String(x.line) : ""),
          h("td", {}, x.ops ? h("span", {}, h("code", {}, x.ops), " ", h("span", { class: "badge", title: CONF.llm[2] }, "추정")) : ""),
          h("td", {}, x.note))));
    };
    sel.onchange = fill;
    fill();
    this.content.replaceChildren(h("div", { class: "an-pad" }, head, sel,
      h("table", { class: "an-table" }, h("thead", {}, h("tr", {}, ...["종류", "테이블", "어디", "줄", "연산", "내용"].map((x) => h("th", {}, x)))), tbody)));
  }

  /** 모델 답의 품질 — 저장된 결과만 읽는다 */
  private async renderQuality() {
    const src = this.src();
    if (!src) return;
    this.content.replaceChildren(h("div", { class: "hint" }, "평가 중…"));
    try {
      const r = await invoke<{ report: { advice: string[]; chunks: number }; markdown: string }>("analysis_eval", src);
      this.content.replaceChildren(h("div", { class: "an-pad" },
        h("h3", {}, "조언"), h("ul", { class: "an-ul" }, ...r.report.advice.map((a) => h("li", {}, a))),
        h("p", { class: "hint" }, "아래 표는 integrated/eval.md 로도 저장된다. 두 모델 비교: sqlstudio-analyze eval --out A --compare B"),
        h("pre", { class: "an-code", style: "max-height:none" }, r.markdown)));
    } catch (e) {
      this.content.replaceChildren(h("div", { class: "hint" }, errOf(e).message));
    }
  }

  private renderFindings(g: Integrated) {
    const kinds = [...new Set(g.findings.map((f) => f.kind))];
    const sel = h("select", {}, h("option", { value: "" }, `전체 ${g.findings.length}`), ...kinds.map((k) => h("option", { value: k }, `${k} ${g.findings.filter((f) => f.kind === k).length}`)));
    const tbody = h("tbody", {});
    const fill = () => {
      tbody.replaceChildren(...g.findings.filter((f) => !sel.value || f.kind === sel.value).slice(0, 1000).map((f) =>
        h("tr", {}, h("td", {}, g.nodes.some((n) => n.id === f.node) ? this.nodeLink(f.node) : f.node), h("td", { class: "num" }, f.line ? String(f.line) : ""),
          h("td", {}, f.kind, f.source === "llm" ? h("span", { class: "badge", title: "모델이 짚은 것 — 확인이 필요합니다" }, "모델") : ""), h("td", {}, f.message))));
    };
    sel.onchange = fill;
    fill();
    this.content.replaceChildren(h("div", { class: "an-pad" }, sel,
      h("table", { class: "an-table" }, h("thead", {}, h("tr", {}, ...["어디", "줄", "종류", "내용"].map((x) => h("th", {}, x)))), tbody)));
  }
}

const VIA: Record<string, string> = { "CURRENT OF": "WHERE CURRENT OF", "loop body": "루프 안 (변수를 직접 쓰지 않음)" };
function viaLabel(v: string) {
  if (v.startsWith("record ")) return `루프 변수 ${v.slice(7)}.컬럼`;
  if (v.startsWith("variable ")) return `받은 변수 ${v.slice(9)}`;
  return VIA[v] ?? v;
}
const CURSOR_KIND: Record<string, string> = { declared: "선언", implicit_loop: "FOR (SELECT)", ref_cursor: "OPEN FOR", select_into: "SELECT INTO" };

function flowsTo(g: Integrated, to: string, from: string) {
  return (g.flows ?? []).filter((f) => f.to === to && f.from.includes(from)).map((f) => `${f.node} · ${f.cursor} · ${f.line}행 · ${viaLabel(f.via)}`).join("\n");
}

function cursorsEl(cs: CursorInfo[]): HTMLElement | string {
  if (!cs.length) return "";
  return h("div", { class: "an-block" }, h("h4", {}, `커서 ${cs.length}`),
    h("table", { class: "an-table" },
      h("thead", {}, h("tr", {}, ...["커서", "종류", "줄", "읽는 테이블", "→ DML (테이블 연산 줄 · 근거)"].map((x) => h("th", {}, x)))),
      h("tbody", {}, ...cs.map((c) => h("tr", {},
        h("td", {}, h("code", {}, c.name)), h("td", { class: "dim" }, CURSOR_KIND[c.kind] ?? c.kind),
        h("td", { class: "num", title: c.line ? "" : "패키지 전역이나 바깥 서브프로그램에 선언됨" }, c.line ? String(c.line) : "전역"),
        h("td", {}, c.reads.join(", ") || "?"),
        h("td", {}, ...(c.feeds.length ? c.feeds.map((f) => h("div", {}, h("code", {}, f.ops), ` ${f.table} `, h("small", { class: "dim" }, `${f.line}행 · ${viaLabel(f.via)}`))) : [h("span", { class: "dim" }, "—")])))))));
}

function stmtsEl(ss: SqlStmt[]): HTMLElement | string {
  if (!ss.length) return "";
  return h("details", { class: "an-chunk" }, h("summary", {}, `SQL 문 ${ss.length}`),
    h("table", { class: "an-table" },
      h("thead", {}, h("tr", {}, ...["줄", "종류", "읽기", "쓰기", "데이터를 대는 커서", "SQL"].map((x) => h("th", {}, x)))),
      h("tbody", {}, ...ss.map((x) => h("tr", {},
        h("td", { class: "num" }, x.end_line > x.line ? `${x.line}~${x.end_line}` : String(x.line)),
        h("td", {}, x.kind, x.cursor ? h("div", {}, h("code", {}, x.cursor)) : ""),
        h("td", {}, x.reads.join(", ")),
        h("td", {}, x.writes.map((w) => `${w.name} ${w.ops}`).join(", ")),
        h("td", {}, ...(x.fed_by ?? []).map((f) => h("div", {}, h("code", {}, f.cursor), h("small", { class: "dim" }, ` ${viaLabel(f.via)}`)))),
        h("td", { class: "an-sql", title: x.text }, x.text))))));
}

function stat(label: string, n: number) {
  return h("div", { class: "an-stat" }, h("b", {}, String(n)), h("small", {}, label));
}

function insightEl(i: Insight): HTMLElement {
  return h("div", { class: "an-insight" },
    i.summary ? h("p", {}, i.summary) : "",
    i.steps.length ? h("ol", {}, ...i.steps.map((s) => h("li", {}, s))) : "",
    i.rules.length ? h("ul", { class: "an-rules" }, ...i.rules.map((s) => h("li", {}, s))) : "",
    i.risks.length ? h("ul", { class: "an-risks" }, ...i.risks.map((r) => h("li", {}, r.line ? h("code", {}, `${r.line}행 `) : "", r.issue))) : "");
}

function factsEl(f: Facts): HTMLElement {
  const parts: (HTMLElement | string)[] = [];
  if (f.tables.length) parts.push(h("div", {}, h("span", { class: "dim" }, "테이블 "), ...f.tables.map((t) => h("span", { class: "an-chip", title: `${t.lines.join(", ")}행` }, `${t.name} ${t.ops}`))));
  if (f.calls.length) parts.push(h("div", {}, h("span", { class: "dim" }, "호출 "), ...f.calls.map((c) => h("span", { class: "an-chip", title: `${c.lines.join(", ")}행` }, c.name))));
  const marks = [...f.transactions.map((m) => `${m.what}@${m.line}`), ...f.dynamic_sql.map((m) => `${m.what}@${m.line}`), ...f.swallowed.map((l) => `예외 삼킴@${l}`)];
  if (marks.length) parts.push(h("div", {}, h("span", { class: "dim" }, "표시 "), ...marks.map((m) => h("span", { class: "an-chip warn" }, m))));
  return h("div", { class: "an-facts" }, ...parts);
}

function chunkEl(c: ChunkResult): HTMLElement {
  const ch = c.chunk;
  const meta = c.llm ? `${c.llm.model} · ${(c.llm.elapsed_ms / 1000).toFixed(1)}s${c.llm.attempts > 1 ? ` · ${c.llm.attempts}번 시도` : ""}${c.llm.repaired?.length ? ` · 고침: ${c.llm.repaired.join(", ")}` : ""}` : "정적 분석";
  return h("details", { class: "an-chunk" },
    h("summary", {}, h("code", {}, ch.id), h("small", { class: "dim" }, ` ${ch.start_line}~${ch.end_line}행 · ${meta}`),
      c.error ? h("span", { class: "badge warn", title: c.raw ?? "" }, c.error) : ""),
    c.insight ? insightEl(c.insight) : "",
    ch.context ? h("pre", { class: "an-code dim" }, ch.context) : "",
    h("pre", { class: "an-code" }, ch.code));
}
