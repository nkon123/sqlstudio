// PL/SQL 분석 화면.
//
// 왼쪽: 스키마의 단위 목록 (체크해서 분석).  오른쪽: 요약 / 호출 관계 / 테이블 / 확인할 것.
// 분석은 단위를 조각내어 모델에 묻고 조각마다 JSON 으로 남긴다. 끝나면 저장된 결과를 이어 통합 분석한다.
// 모델은 소스를 읽기만 한다 — 이 화면에서 DB 로 가는 것은 사전 조회(ALL_SOURCE 등)뿐이다.

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { api, errOf, type Provider } from "./api";
import { confirmBox, h, toast } from "./ui";

interface UnitRef { owner: string; name: string; unit_type: string }
interface UnitRow extends UnitRef {
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
interface Node { id: string; unit: string; path: string; kind: string; signature: string; start_line: number; end_line: number; public?: boolean | null; summary?: string | null; complexity: number; commits: boolean }
interface Edge { from: string; to: string; resolved: boolean; external?: string | null; lines: number[] }
interface TableRow { table: string; by: Record<string, string>; impacted_entries: string[]; fed_from?: string[]; feeds_into?: string[] }
interface Flow { node: string; unit: string; cursor: string; cursor_kind: string; from: string[]; to: string; ops: string; line: number; via: string; cursor_summary?: string | null }
interface Finding { node: string; unit: string; line?: number | null; source: string; kind: string; message: string }
interface Integrated {
  units: number; nodes: Node[]; edges: Edge[]; tables: TableRow[]; entries: string[]; cycles: string[][];
  unused: string[]; transactions: Record<string, string[]>; findings: Finding[]; flows?: Flow[]; overview?: Insight | null;
}
type Progress =
  | { type: "event"; run: number; event: { type: string; key?: string; id?: string; done?: number; total?: number; status?: string; elapsed_ms?: number; message?: string | null; what?: string } }
  | { type: "fetch"; run: number; index: number; total: number; key: string; error?: string | null }
  | { type: "finished"; run: number; error?: string | null; units: number; nodes: number; tables: number; findings: number; dir: string };

type View = "summary" | "calls" | "tables" | "findings" | "quality";
const keyOf = (u: UnitRef) => `${u.owner}.${u.name}.${u.unit_type.replace(/ /g, "_")}`;
const when = (t?: number | null) => (t ? new Date(t * 1000).toLocaleString() : "");
const short = (id: string) => id.split(".").slice(1).join(".");
const TYPE_LABEL: Record<string, string> = { "PACKAGE BODY": "패키지", PROCEDURE: "프로시저", FUNCTION: "함수", TRIGGER: "트리거", "TYPE BODY": "타입" };

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
  private stopBtn = h("button", { class: "danger", disabled: true }, "중지");
  private statusEl = h("span", { class: "dbg-status" }, "");
  private bar = h("div", { class: "an-bar-fill" });
  private listEl = h("tbody", {});
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

  constructor(private sessionId: () => number | undefined, private onClose: () => void) {
    this.startBtn.onclick = () => this.start();
    this.stopBtn.onclick = () => this.cancel();
    this.provSel.onchange = () => this.updateBadge();
    this.allBox.onchange = () => {
      for (const r of this.rows) this.allBox.checked ? this.checked.add(keyOf(r)) : this.checked.delete(keyOf(r));
      this.renderList();
    };
    const listBtn = h("button", { onclick: () => this.load() }, "목록");
    for (const i of [this.ownerIn, this.nameIn]) i.addEventListener("keydown", (e) => { if (e.key === "Enter") this.load(); });
    const toolbar = h("div", { class: "dbg-toolbar an-toolbar" },
      this.ownerIn, this.nameIn, listBtn, h("span", { class: "sep" }),
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
    tab("tables", "테이블");
    tab("findings", "확인할 것");
    tab("quality", "품질");
    nav.append(h("span", { class: "spacer" }), h("button", { class: "small", title: "저장된 조각 결과로 통합 분석을 다시 만든다 (모델 없이)", onclick: () => this.reload(true) }, "다시 통합"));
    const left = h("div", { class: "an-left" },
      h("table", { class: "an-list" },
        h("thead", {}, h("tr", {}, h("th", {}, this.allBox), h("th", {}, "형식"), h("th", {}, "이름"), h("th", {}, "분석"))),
        this.listEl));
    this.el = h("div", { class: "debugger analysis", hidden: true }, toolbar,
      h("div", { class: "an-progress" }, this.bar),
      h("div", { class: "an-main" }, left, h("div", { class: "an-right" }, nav, this.content)));
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
    await this.load();
    await this.reload(false);
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
    const id = this.sessionId();
    if (id == null) return toast("먼저 접속하세요", "error");
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
    this.listEl.replaceChildren(...this.rows.map((r) => {
      const k = keyOf(r);
      const box = h("input", { type: "checkbox", checked: this.checked.has(k), disabled: !!r.warning });
      box.onchange = () => { box.checked ? this.checked.add(k) : this.checked.delete(k); this.statusEl.textContent = `${this.checked.size}개 선택`; };
      const state = r.warning ? h("span", { class: "dim", title: r.warning }, "읽을 수 없음")
        : r.analyzed_at ? h("span", { class: r.failed ? "warn" : "ok", title: `${when(r.analyzed_at)}${r.model ? ` · ${r.model}` : " · 정적"}${r.summary ? `\n${r.summary}` : ""}` },
          `${r.chunks ?? 0}조각${r.failed ? ` · 실패 ${r.failed}` : ""}${r.model ? "" : " · 정적"}`)
          : h("span", { class: "dim" }, "—");
      const tr = h("tr", { class: this.selectedUnit === k ? "active" : "" },
        h("td", {}, box), h("td", { class: "dim", title: r.unit_type }, TYPE_LABEL[r.unit_type] ?? r.unit_type), h("td", {}, r.name), h("td", { class: "an-state" }, state));
      tr.addEventListener("click", (e) => {
        if ((e.target as HTMLElement).tagName === "INPUT") return;
        if (r.analyzed_at) this.showUnit(k);
      });
      return tr;
    }));
  }

  private async start() {
    const id = this.sessionId();
    if (id == null) return toast("먼저 접속하세요", "error");
    const units = this.rows.filter((r) => this.checked.has(keyOf(r))).map(({ owner, name, unit_type }) => ({ owner, name, unit_type }));
    if (!units.length) return toast("분석할 단위를 체크하세요", "error");
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
      this.run = await invoke<number>("analysis_start", { id, units, provider: p?.name ?? null, allowRemote: !!p?.remote, options });
    } catch (e) {
      return toast(errOf(e).message, "error");
    }
    this.startBtn.disabled = true;
    this.stopBtn.disabled = false;
    this.bar.style.width = "0";
    this.statusEl.textContent = `시작 — 단위 ${units.length}개`;
    this.total = units.length;
  }

  private total = 0;
  private unitIndex = 0;

  private onProgress(p: Progress) {
    if (p.run !== this.run) return;
    if (p.type === "fetch") {
      this.unitIndex = p.index;
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
        this.statusEl.textContent = `[${this.unitIndex + 1}/${this.total}] 요약: ${e.what}`;
      } else if (e.type === "unit_done") {
        const k = e.key!;
        const r = this.rows.find((x) => keyOf(x) === k);
        if (r) { r.analyzed_at = Date.now() / 1000; this.checked.delete(k); this.renderList(); }
      }
    } else if (p.type === "finished") {
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
    const id = this.sessionId();
    if (id == null) return;
    const dir = await invoke<string>("analysis_dir", { id });
    await navigator.clipboard.writeText(dir).catch(() => {});
    toast(`결과 폴더: ${dir} (복사함) — integrated/report.md, 단위·조각별 JSON`, "info", 6000);
  }

  private async reload(rebuild: boolean) {
    const id = this.sessionId();
    if (id == null) return;
    try {
      this.g = await invoke<Integrated | null>("analysis_result", { id, rebuild });
    } catch (e) {
      toast(errOf(e).message, "error");
    }
    this.render();
  }

  // ── 오른쪽 ─────────────────────────────────────

  private render() {
    for (const [v, b] of this.navBtns) b.classList.toggle("active", v === this.view);
    const g = this.g;
    if (!g) {
      this.content.replaceChildren(h("div", { class: "hint" },
        "아직 분석 결과가 없습니다. 왼쪽에서 단위를 체크하고 '분석 시작'. 모델 없이(정적 분석)도 호출 관계·테이블 CRUD 는 나옵니다."));
      return;
    }
    if (this.view === "summary") return this.renderSummary(g);
    if (this.view === "calls") return this.renderCalls(g);
    if (this.view === "tables") return this.renderTables(g);
    if (this.view === "quality") return void this.renderQuality();
    return this.renderFindings(g);
  }

  private renderSummary(g: Integrated) {
    const unresolved = g.edges.filter((e) => e.external === "unknown").length;
    const box = h("div", { class: "an-pad" },
      h("div", { class: "an-stats" },
        stat("단위", g.units), stat("서브프로그램", g.nodes.length), stat("호출", g.edges.length), stat("못 푼 호출", unresolved),
        stat("테이블", g.tables.length), stat("시작점", g.entries.length), stat("순환", g.cycles.length), stat("확인할 것", g.findings.length)));
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
    const id = this.sessionId();
    if (id == null) return;
    this.selectedUnit = key;
    this.renderList();
    this.view = "summary";
    for (const [v, b] of this.navBtns) b.classList.toggle("active", v === this.view);
    let d: { unit: UnitResult; chunks: ChunkResult[] };
    try {
      d = await invoke("analysis_unit", { id, key });
    } catch (e) {
      return toast(errOf(e).message, "error");
    }
    const u = d.unit;
    const box = h("div", { class: "an-pad" },
      h("div", { class: "an-head" }, h("button", { class: "small", onclick: () => { this.selectedUnit = null; this.renderList(); this.render(); } }, "← 전체"),
        h("strong", {}, `${u.unit_type} ${u.owner}.${u.name}`),
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
        return li;
      }));
      detail.replaceChildren(this.focusNode ? this.nodeDetail(g, this.focusNode) : h("div", { class: "hint" }, "왼쪽에서 서브프로그램을 고르세요"));
    };
    input.oninput = fill;
    fill();
    this.content.replaceChildren(h("div", { class: "an-split" }, h("div", { class: "an-col" }, input, list), detail));
  }

  private nodeDetail(g: Integrated, id: string): HTMLElement {
    const n = g.nodes.find((x) => x.id === id);
    const callers = g.edges.filter((e) => e.to === id);
    const callees = g.edges.filter((e) => e.from === id);
    const tables = g.tables.filter((t) => t.by[id]);
    const box = h("div", { class: "an-pad" },
      h("div", { class: "an-head" }, h("strong", {}, id)),
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
        const users = h("td", {}, ...Object.entries(t.by).map(([n, o]) => h("div", {}, h("code", {}, o.padEnd(4)), " ", n.endsWith("(전역)") ? short(n) : this.nodeLink(n))));
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

  /** 모델 답의 품질 — 저장된 결과만 읽는다 */
  private async renderQuality() {
    const id = this.sessionId();
    if (id == null) return;
    this.content.replaceChildren(h("div", { class: "hint" }, "평가 중…"));
    try {
      const r = await invoke<{ report: { advice: string[]; chunks: number }; markdown: string }>("analysis_eval", { id });
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
