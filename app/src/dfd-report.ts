// 흐름도 보고서 모드 — 지금 흐름도를 사본으로 떼어 손본다 (자동 흐름도는 그대로).
//
// 노드 끌기·여러 개 골라 정렬·같은 간격 · 격자 붙기 · 선 설명·모양·숨기기·손으로 선 추가 · 메모 · 색 · 표시 이름 · 되돌리기
// 결과 폴더의 integrated/reports/<id>.json 에 저장 → .sqla 내보내기에 함께 들어간다.
// 분석을 다시 돌려 관계가 바뀌면 열 때 알려 주고, 새 것 넣기·없어진 것 표시를 고르게 한다.
// 내보내기: PNG, HTML(그림 + 메모 + 선 설명 + 테이블 CRUD).

import type { Core, EdgeSingular, NodeSingular } from "cytoscape";
import { invoke } from "@tauri-apps/api/core";
import { save as saveDialog } from "@tauri-apps/plugin-dialog";
import { errOf } from "./api";
import { COLORS, KIND_KO, type Libs, boxSize, dfdStyle, loadLibs } from "./dfd";
import { type DfdSource, type EdgeKind, LANES, type LaneSpan, buildBase, buildView, fitLanes } from "./dfd-model";
import { confirmBox, h, toast } from "./ui";

export interface ReportBasis {
  center: string | null; dir: string; depth: number; unit: "proc" | "pkg"; calls: boolean; flows: boolean; hidden: string[];
}
export interface ReportNode {
  id: string; ref: string; type: "proc" | "trig" | "table" | "note"; label: string; x: number; y: number; w: number; h: number; lane: number;
  /** 표시 이름 (비우면 label) — 메모면 내용 */
  text?: string; copy?: boolean; center?: boolean; color?: string; hidden?: boolean;
  /** 메모가 붙은 노드·선 */
  attach?: string; gone?: boolean;
}
export interface ReportEdge {
  id: string; source: string; target: string; kind: EdgeKind | "manual"; label: string; guess?: boolean;
  /** 선 설명 (가운데 글) */
  text?: string; route?: "taxi" | "straight" | "bezier"; color?: string; hidden?: boolean; gone?: boolean;
}
export interface Report {
  format: 1; id: string; name: string; title: string; description: string; created_at: number; updated_at: number;
  profile: string; basis: ReportBasis; nodes: ReportNode[]; edges: ReportEdge[];
  /** 입력·처리·출력 칸 경계 (맞붙어 있다). 없으면 노드에 맞춘다 */
  lanes?: LaneSpan[];
}

/** 그림의 장식(칸 배경·칸 경계 손잡이)이 아닌 것 */
const isItem = (n: NodeSingular) => n.data("type") !== "lane" && n.data("type") !== "divider";
/** 칸에 드는 상자 (메모 제외) */
const isBox = (n: NodeSingular) => isItem(n) && n.data("type") !== "note";

const GRID = 10;
const NODE_COLORS = ["", "#fde2e1", "#fff1c2", "#dff5e1", "#dbeafe", "#efe1fb", "#e5e7eb"];
const NOTE_COLORS = ["#fff8c5", "#dbeafe", "#dff5e1", "#fde2e1"];
const EDGE_COLORS = ["", "#c62f3c", "#1a7f37", "#7a4fd0", "#111827"];
const MANUAL_COLOR = "#7a4fd0";
const ROUTE_KO = { taxi: "직각", straight: "직선", bezier: "곡선" };
const now = () => Math.floor(Date.now() / 1000);
const esc = (s: string) => s.replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" })[c]!);
const edgeKey = (s: string, t: string, k: string) => `${s}|${t}|${k}`;

/** 지금 흐름도(그려진 것)를 보고서로 떼어 낸다 */
export function snapshot(cy: Core, basis: ReportBasis, profile: string, name: string): Report {
  const nodes: ReportNode[] = cy.nodes().filter((n) => isItem(n as NodeSingular)).map((x) => {
    const n = x as NodeSingular;
    const p = n.position();
    return { id: n.id(), ref: n.data("ref"), type: n.data("type"), label: n.data("label"), x: p.x, y: p.y, w: n.data("w"), h: n.data("h"),
      lane: n.data("lane"), copy: n.data("copy") || undefined, center: n.data("center") || undefined };
  });
  const edges: ReportEdge[] = cy.edges().map((e) => ({ id: e.id(), source: e.data("source"), target: e.data("target"), kind: e.data("kind"),
    label: e.data("label") ?? "", guess: e.data("guess") || undefined }));
  const t = now();
  const title = basis.center ? `${basis.center} 데이터 흐름` : "데이터 흐름";
  return { format: 1, id: `r${Date.now().toString(36)}`, name, title, description: "", created_at: t, updated_at: t, profile, basis, nodes, edges };
}

export class DfdReport {
  readonly el: HTMLElement;
  private canvas = h("div", { class: "dfd-canvas" });
  private side = h("div", { class: "dfd-side an-pad" });
  private nameIn = h("input", { class: "rp-name", title: "보고서 이름 (목록에 보이는 것)" });
  private status = h("span", { class: "dim" });
  private snapBox = h("input", { type: "checkbox", checked: true });
  private cy: Core | null = null;
  private elk: InstanceType<Libs["ELK"]> | null = null;
  private r: Report | null = null;
  private history: string[] = [];
  private at = -1;
  private dirty = false;
  private linking: string | null | undefined = undefined; // undefined: 선 추가 아님, null: 시작 노드 기다림
  private diff: { added: ReportEdge[]; addedNodes: ReportNode[]; gone: string[] } | null = null;

  constructor(private opts: { src: () => DfdSource | null; onClose: () => void }) {
    const b = (label: string, title: string, f: () => void) => h("button", { class: "small", title, onclick: f }, label);
    const align = (k: string, label: string, title: string) => b(label, title, () => this.align(k));
    const tb = h("div", { class: "dfd-toolbar rp-toolbar" },
      h("strong", {}, "보고서"), this.nameIn,
      h("span", { class: "sep" }),
      align("left", "⇤", "왼쪽 맞춤 (여러 개 고른 뒤 — Shift+클릭 또는 Ctrl+끌기)"), align("cx", "↔", "가로 가운데 맞춤"), align("right", "⇥", "오른쪽 맞춤"),
      align("top", "⤒", "위 맞춤"), align("cy", "↕", "세로 가운데 맞춤"), align("bottom", "⤓", "아래 맞춤"),
      align("hdist", "⇹", "가로 같은 간격"), align("vdist", "⇳", "세로 같은 간격"),
      h("label", { class: "check", title: "끌어 놓을 때 10px 격자에 붙인다" }, this.snapBox, "격자"),
      b("자동 배치", "지금 보이는 노드·선으로 다시 배치한다 (메모는 그대로)", () => this.autoLayout()),
      b("칸 맞추기", "입력·처리·출력 칸을 지금 노드 자리에 딱 맞춘다 (칸 경계는 끌어서도 옮긴다)", () => this.fitLanesNow()),
      h("span", { class: "sep" }),
      b("메모", "메모 상자 — 노드·선을 고른 채 누르면 거기에 붙인다", () => this.addNote()),
      b("선 추가", "처음 노드 → 끝 노드를 차례로 누른다 (손으로 넣은 선은 보라색 점선)", () => this.startLink()),
      h("span", { class: "sep" }),
      b("↶", "되돌리기 (Ctrl+Z)", () => this.undo()), b("↷", "다시 (Ctrl+Y)", () => this.redo()),
      h("span", { class: "spacer" }), this.status,
      b("맞추기", "화면에 맞추기", () => this.cy?.fit(undefined, 30)),
      b("저장", "Ctrl+S", () => void this.save()),
      b("PNG", "그림으로 저장", () => void this.exportPng()),
      b("HTML", "그림·메모·선 설명·테이블 CRUD 를 문서 하나로 (브라우저로 열어 인쇄하면 PDF)", () => void this.exportHtml()),
      b("지우기", "이 보고서를 지운다", () => void this.remove()),
      b("닫기", "보고서 모드를 닫고 흐름도로", () => void this.close()));
    this.nameIn.addEventListener("change", () => { if (this.r) { this.r.name = this.nameIn.value.trim() || this.r.name; this.commit(); } });
    this.el = h("div", { class: "dfd rp", tabindex: "-1" }, tb, h("div", { class: "dfd-body" }, this.canvas, this.side));
    this.el.addEventListener("keydown", (e) => this.onKey(e));
  }

  // ── 열기·저장 ─────────────────────────────────

  async open(r: Report) {
    const L = await loadLibs();
    this.ensureCy(L);
    this.r = r;
    this.nameIn.value = r.name;
    this.load(r);
    this.history = [JSON.stringify(r)];
    this.at = 0;
    this.dirty = false;
    this.updateStatus();
    this.diff = this.compare();
    this.showReport();
    this.cy!.fit(undefined, 30);
    this.el.focus();
  }

  isDirty() {
    return this.dirty;
  }

  private async save(): Promise<boolean> {
    const r = this.serialize();
    if (!r) return false;
    r.updated_at = now();
    try {
      await invoke("dfd_report_save", { profile: r.profile, report: r });
    } catch (e) {
      toast(errOf(e).message, "error");
      return false;
    }
    this.r = r;
    this.dirty = false;
    this.updateStatus();
    toast(`저장했습니다: ${r.name}`, "ok", 1800);
    return true;
  }

  private async close() {
    if (this.dirty) {
      const ok = await confirmBox("저장하지 않은 변경", "보고서에 저장하지 않은 변경이 있습니다. 저장할까요?", "저장하고 닫기", false);
      if (ok && !(await this.save())) return;
      if (!ok && !(await confirmBox("변경 버리기", "저장하지 않고 닫습니다. 고친 것은 사라집니다.", "버리고 닫기"))) return;
    }
    this.opts.onClose();
  }

  private async remove() {
    const r = this.r;
    if (!r) return;
    if (!(await confirmBox("보고서 지우기", `'${r.name}' 보고서를 지웁니다.`, "지우기"))) return;
    try {
      await invoke("dfd_report_delete", { profile: r.profile, id: r.id });
    } catch (e) {
      // 저장한 적 없는 새 보고서면 파일이 없다 — 그냥 닫는다
      if (!/os error 2|찾을 수 없|not found/i.test(errOf(e).message)) return toast(errOf(e).message, "error");
    }
    this.dirty = false;
    this.opts.onClose();
  }

  // ── 그리기 ─────────────────────────────────

  private ensureCy(L: Libs) {
    if (this.cy) return;
    this.elk = new L.ELK();
    const v = (n: string) => getComputedStyle(document.documentElement).getPropertyValue(n).trim();
    const cy = L.cytoscape({
      container: this.canvas,
      wheelSensitivity: 0.25, minZoom: 0.05, maxZoom: 3,
      boxSelectionEnabled: true, selectionType: "additive",
      style: [
        ...dfdStyle(),
        { selector: "node", style: { label: (n: NodeSingular) => n.data("text") || n.data("label") } },
        { selector: "node[color]", style: { "background-color": "data(color)" } },
        { selector: "node[type='note']", style: { shape: "round-rectangle", "background-color": (n: NodeSingular) => n.data("color") || NOTE_COLORS[0],
          "border-width": 1, "border-color": "#c9a227", "text-wrap": "wrap", "text-max-width": (n: NodeSingular) => `${n.data("w") - 16}px`,
          "text-valign": "center", "text-halign": "center", "font-size": 11, "z-index": 20 } },
        { selector: "node.gone", style: { "border-color": "#c62f3c", "border-style": "dashed", opacity: 0.6 } },
        { selector: "edge", style: { "curve-style": "taxi", "taxi-direction": "horizontal", "taxi-turn": "50%", "taxi-turn-min-distance": 12,
          label: "data(text)", "font-size": 10, "text-wrap": "wrap", "text-max-width": "160px", "text-background-color": v("--bg"),
          "text-background-opacity": 0.95, "text-background-padding": "2px", "text-border-width": 1, "text-border-opacity": 0.4, "text-border-color": v("--line") } },
        { selector: "edge[route='straight']", style: { "curve-style": "straight" } },
        { selector: "edge[route='bezier']", style: { "curve-style": "unbundled-bezier", "control-point-distances": 40, "control-point-weights": 0.5 } },
        { selector: "edge[kind='manual']", style: { "line-color": MANUAL_COLOR, "target-arrow-color": MANUAL_COLOR, "line-style": "dashed", "line-dash-pattern": [8, 3, 2, 3] } },
        { selector: "edge[color]", style: { "line-color": "data(color)", "target-arrow-color": "data(color)" } },
        { selector: "edge[kind='attach']", style: { "line-color": "#c9a227", "line-style": "dashed", "target-arrow-shape": "none", width: 1, "curve-style": "straight", label: "" } },
        { selector: "edge.gone", style: { "line-color": "#c62f3c", "target-arrow-color": "#c62f3c", "line-style": "dashed", opacity: 0.7 } },
        { selector: ".linking", style: { "overlay-color": MANUAL_COLOR, "overlay-opacity": 0.25 } },
        { selector: "node[type='divider']", style: { shape: "rectangle", width: "data(w)", height: "data(h)", label: "", "border-width": 0,
          "background-color": v("--accent"), "background-opacity": 0.12, "z-index": 8, "overlay-opacity": 0 } },
        { selector: "node[type='divider'].hover, node[type='divider']:grabbed", style: { "background-opacity": 0.45 } },
      ] as never,
    });
    cy.on("tap", "node", (ev) => this.onTapNode(ev.target as NodeSingular));
    cy.on("tap", "edge", (ev) => { if (this.linking === undefined) this.showEdge(ev.target as EdgeSingular); });
    cy.on("tap", (ev) => { if (ev.target === cy) { this.cancelLink(); this.showReport(); } });
    cy.on("select unselect", () => setTimeout(() => this.onSelect(), 0));
    cy.on("drag", "node[type='divider']", (ev) => this.dragDivider(ev.target as NodeSingular));
    cy.on("mouseover", "node[type='divider']", (ev) => ev.target.addClass("hover"));
    cy.on("mouseout", "node[type='divider']", (ev) => ev.target.removeClass("hover"));
    cy.on("dragfree", "node", (ev) => {
      if ((ev.target as NodeSingular).data("type") === "divider") return this.dropDivider();
      const moved = cy.$("node:selected").union(ev.target).toArray() as NodeSingular[];
      if (this.snapBox.checked) {
        for (const n of moved) {
          const p = n.position();
          n.position({ x: Math.round(p.x / GRID) * GRID, y: Math.round(p.y / GRID) * GRID });
        }
      }
      // 옮긴 상자는 놓인 칸 소속이 된다. 칸 밖(양 끝)으로 나가면 바깥 칸을 넓힌다
      this.assignLanes(moved.filter(isBox));
      this.relane();
      this.commit();
    });
    new ResizeObserver(() => cy.resize()).observe(this.canvas);
    this.cy = cy;
  }

  private load(r: Report) {
    const cy = this.cy!;
    this.lanes = r.lanes ? r.lanes.map((l) => ({ ...l })) : null;
    cy.batch(() => {
      cy.elements().remove();
      cy.add(r.nodes.filter((n) => !n.hidden).map((n) => ({ group: "nodes" as const, data: { ...n }, position: { x: n.x, y: n.y }, classes: n.gone ? "gone" : "" })));
      const has = (id: string) => cy.getElementById(id).length > 0;
      cy.add(r.edges.filter((e) => !e.hidden && has(e.source) && has(e.target)).map((e) => ({ group: "edges" as const,
        data: { ...e, tag: e.kind === "read" || e.kind === "call" || e.kind === "manual" ? "" : e.label }, classes: e.gone ? "gone" : "" })));
      for (const n of r.nodes) if (n.type === "note" && n.attach && !n.hidden && has(n.attach)) this.addAttach(n.id, n.attach);
      // 숨긴 것은 그래프 밖에 둔다 (저장할 때 다시 합친다)
      this.hiddenNodes = r.nodes.filter((n) => n.hidden);
      this.hiddenEdges = r.edges.filter((e) => e.hidden || !has(e.source) || !has(e.target));
    });
    this.relane();
  }

  private hiddenNodes: ReportNode[] = [];
  /** 칸 경계 (null 이면 다음 relane 에서 노드에 맞춘다) */
  private lanes: LaneSpan[] | null = null;
  private hiddenEdges: ReportEdge[] = [];

  private addAttach(noteId: string, target: string) {
    const cy = this.cy!;
    const t = cy.getElementById(target);
    if (!t.length) return;
    // 선에 붙은 메모는 선의 가운데 대신 끝 노드로 잇는다 (Cytoscape 선은 노드끼리만)
    const to = t.isEdge() ? (t as EdgeSingular).target().id() : target;
    cy.add({ group: "edges", data: { id: `att:${noteId}`, source: noteId, target: to, kind: "attach", label: "" }, selectable: false });
  }

  /** 칸마다 상자가 차지한 가로 범위 */
  private spans(): LaneSpan[] {
    const ns = this.cy!.nodes().filter((n) => isBox(n as NodeSingular));
    const out: LaneSpan[] = [];
    for (let i = 0; i < LANES.length; i++) {
      const ln = ns.filter((n) => n.data("lane") === i);
      if (!ln.length) continue;
      const bb = ln.boundingBox({});
      out.push({ i, x1: bb.x1, x2: bb.x2 });
    }
    return out;
  }

  private fitLanesNow() {
    this.lanes = fitLanes(this.spans());
    this.relane();
    this.commit();
  }

  /** 상자를 놓인 칸 소속으로. 양 끝 칸 밖이면 그 칸을 넓힌다 */
  private assignLanes(ns: NodeSingular[]) {
    const L = this.lanes;
    if (!L || !L.length) return;
    for (const n of ns) {
      const x = n.position().x, bb = n.boundingBox({});
      let k = L.findIndex((l) => x >= l.x1 && x < l.x2);
      if (k < 0) k = x < L[0].x1 ? 0 : L.length - 1;
      n.data("lane", L[k].i);
      if (k === 0) L[0].x1 = Math.min(L[0].x1, Math.round(bb.x1 - 24));
      if (k === L.length - 1) L[k].x2 = Math.max(L[k].x2, Math.round(bb.x2 + 24));
    }
  }

  /** 칸 경계 손잡이를 끄는 중 — 이웃 칸 경계를 함께 옮긴다 (손잡이는 가로로만) */
  private dragDivider(d: NodeSingular) {
    const L = this.lanes;
    if (!L) return;
    const k = d.data("k") as number;
    const lo = k > 0 ? L[k - 1].x1 + 60 : -Infinity;
    const hi = k < L.length ? L[k].x2 - 60 : Infinity;
    const x = Math.max(lo, Math.min(hi, Math.round(d.position().x)));
    d.position({ x, y: d.data("y") });
    if (k > 0) L[k - 1].x2 = x;
    if (k < L.length) L[k].x1 = x;
    this.paintLanes();
  }

  private dropDivider() {
    // 경계가 바뀌었으니 상자의 칸 소속을 다시 (가운데가 든 칸)
    const L = this.lanes;
    if (L) for (const n of this.cy!.nodes().filter((x) => isBox(x as NodeSingular)).toArray() as NodeSingular[]) {
      const x = n.position().x;
      const l = L.find((s) => x >= s.x1 && x < s.x2);
      if (l) n.data("lane", l.i);
    }
    this.relane();
    this.commit();
  }

  /** 칸 배경 상자만 경계에 맞춰 다시 (손잡이를 끄는 중에) */
  private paintLanes() {
    const cy = this.cy!;
    for (const l of this.lanes ?? []) {
      const n = cy.getElementById(`lane:${l.i}`);
      if (!n.length) continue;
      n.data("w", l.x2 - l.x1);
      n.position({ x: (l.x1 + l.x2) / 2, y: n.position().y });
    }
  }

  /** 칸 배경과 경계 손잡이를 다시 그린다. 경계가 없거나 칸 수가 바뀌었으면 노드에 맞춘다 */
  private relane() {
    const cy = this.cy!;
    cy.batch(() => {
      cy.nodes("[type='lane'], [type='divider']").remove();
      const ns = cy.nodes().filter((n) => isBox(n as NodeSingular));
      if (!ns.length) return;
      const spans = this.spans();
      if (!this.lanes || spans.some((s) => !this.lanes!.some((l) => l.i === s.i))) {
        // 처음이거나 새 칸이 생겼으면 (새 것 넣기 등) 노드에 다시 맞춘다
        this.lanes = fitLanes(spans);
      }
      const L = this.lanes;
      const bb = ns.boundingBox({});
      const y1 = bb.y1 - 46, y2 = bb.y2 + 16, cyy = (y1 + y2) / 2;
      for (const l of L) {
        cy.add({ group: "nodes", data: { id: `lane:${l.i}`, type: "lane", label: LANES[l.i], alt: l.i % 2 === 1, w: l.x2 - l.x1, h: y2 - y1 },
          position: { x: (l.x1 + l.x2) / 2, y: cyy }, selectable: false, grabbable: false });
      }
      for (let k = 0; k <= L.length; k++) {
        const x = k < L.length ? L[k].x1 : L[L.length - 1].x2;
        cy.add({ group: "nodes", data: { id: `div:${k}`, type: "divider", k, y: cyy, w: 8, h: y2 - y1 }, position: { x, y: cyy }, selectable: false });
      }
    });
  }

  /** 그래프 → 보고서 */
  private serialize(): Report | null {
    const r = this.r;
    const cy = this.cy;
    if (!r || !cy) return null;
    const nodes: ReportNode[] = cy.nodes().filter((n) => isItem(n as NodeSingular)).map((x) => {
      const n = x as NodeSingular;
      const d = { ...n.data() } as ReportNode;
      const p = n.position();
      d.x = Math.round(p.x);
      d.y = Math.round(p.y);
      for (const k of Object.keys(d) as (keyof ReportNode)[]) if (d[k] === undefined || d[k] === "" || d[k] === false) delete d[k];
      return d;
    });
    const edges: ReportEdge[] = cy.edges().filter((e) => e.data("kind") !== "attach").map((e) => {
      const d = { ...e.data() } as ReportEdge & { tag?: string };
      delete d.tag;
      for (const k of Object.keys(d) as (keyof ReportEdge)[]) if (d[k] === undefined || d[k] === "" || d[k] === false) delete d[k];
      return d;
    });
    return { ...r, name: this.nameIn.value.trim() || r.name, nodes: [...nodes, ...this.hiddenNodes], edges: [...edges, ...this.hiddenEdges],
      lanes: this.lanes ? this.lanes.map((l) => ({ ...l })) : undefined };
  }

  /** 바뀐 것을 기억한다 (되돌리기) */
  private commit() {
    const r = this.serialize();
    if (!r) return;
    const s = JSON.stringify(r);
    if (s === this.history[this.at]) return;
    this.history = this.history.slice(0, this.at + 1);
    this.history.push(s);
    if (this.history.length > 80) this.history.shift();
    this.at = this.history.length - 1;
    this.r = r;
    this.dirty = true;
    this.updateStatus();
  }

  private restore(i: number) {
    if (i < 0 || i >= this.history.length) return;
    this.at = i;
    const r = JSON.parse(this.history[i]) as Report;
    this.r = r;
    this.nameIn.value = r.name;
    this.load(r);
    this.dirty = true;
    this.updateStatus();
    this.showReport();
  }

  private undo() { this.restore(this.at - 1); }
  private redo() { this.restore(this.at + 1); }

  private updateStatus() {
    const hid = this.hiddenNodes.length + this.hiddenEdges.length;
    this.status.textContent = `${this.dirty ? "● 저장 안 됨" : "저장됨"}${hid ? ` · 숨김 ${hid}` : ""}`;
  }

  private onKey(e: KeyboardEvent) {
    const t = e.target as HTMLElement;
    if (t.tagName === "INPUT" || t.tagName === "TEXTAREA" || t.tagName === "SELECT") return;
    const k = e.key.toLowerCase();
    if ((e.ctrlKey || e.metaKey) && k === "z") { e.preventDefault(); this.undo(); }
    else if ((e.ctrlKey || e.metaKey) && k === "y") { e.preventDefault(); this.redo(); }
    else if ((e.ctrlKey || e.metaKey) && k === "s") { e.preventDefault(); void this.save(); }
    else if ((e.ctrlKey || e.metaKey) && k === "a") { e.preventDefault(); this.cy?.nodes().filter((n) => isItem(n as NodeSingular)).select(); }
    else if (e.key === "Delete") { e.preventDefault(); this.removeSelected(); }
    else if (e.key === "Escape") { this.cancelLink(); this.cy?.elements().unselect(); this.showReport(); }
  }

  // ── 고치기 ─────────────────────────────────

  private selectedNodes(): NodeSingular[] {
    return (this.cy?.$("node:selected").toArray() ?? []) as NodeSingular[];
  }

  private align(k: string) {
    const ns = this.selectedNodes();
    if (ns.length < 2) return toast("노드를 둘 이상 고르세요 (Shift+클릭, 또는 Ctrl/Shift+끌기로 상자 선택)", "info");
    const box = (n: NodeSingular) => { const p = n.position(), w = n.width(), hh = n.height(); return { n, x1: p.x - w / 2, x2: p.x + w / 2, y1: p.y - hh / 2, y2: p.y + hh / 2, cx: p.x, cy: p.y, w, h: hh }; };
    const bs = ns.map(box);
    const set = (b: (typeof bs)[number], x: number, y: number) => b.n.position({ x, y });
    const minX = Math.min(...bs.map((b) => b.x1)), maxX = Math.max(...bs.map((b) => b.x2));
    const minY = Math.min(...bs.map((b) => b.y1)), maxY = Math.max(...bs.map((b) => b.y2));
    if (k === "left") bs.forEach((b) => set(b, minX + b.w / 2, b.cy));
    if (k === "right") bs.forEach((b) => set(b, maxX - b.w / 2, b.cy));
    if (k === "cx") { const c = bs.reduce((a, b) => a + b.cx, 0) / bs.length; bs.forEach((b) => set(b, c, b.cy)); }
    if (k === "top") bs.forEach((b) => set(b, b.cx, minY + b.h / 2));
    if (k === "bottom") bs.forEach((b) => set(b, b.cx, maxY - b.h / 2));
    if (k === "cy") { const c = bs.reduce((a, b) => a + b.cy, 0) / bs.length; bs.forEach((b) => set(b, b.cx, c)); }
    if (k === "hdist" || k === "vdist") {
      const horiz = k === "hdist";
      const sorted = [...bs].sort((a, b) => (horiz ? a.cx - b.cx : a.cy - b.cy));
      const total = sorted.reduce((a, b) => a + (horiz ? b.w : b.h), 0);
      const span = horiz ? maxX - minX : maxY - minY;
      const gap = (span - total) / Math.max(1, sorted.length - 1);
      let pos = horiz ? minX : minY;
      for (const b of sorted) {
        if (horiz) set(b, pos + b.w / 2, b.cy); else set(b, b.cx, pos + b.h / 2);
        pos += (horiz ? b.w : b.h) + gap;
      }
    }
    this.relane();
    this.commit();
  }

  private async autoLayout() {
    const cy = this.cy!;
    const ns = cy.nodes().filter((n) => isBox(n as NodeSingular));
    const es = cy.edges().filter((e) => e.data("kind") !== "attach" && ns.contains(e.source()) && ns.contains(e.target()));
    const graph = {
      id: "root",
      layoutOptions: { "elk.algorithm": "layered", "elk.direction": "RIGHT", "elk.partitioning.activate": "true", "elk.separateConnectedComponents": "false",
        "elk.spacing.nodeNode": "26", "elk.layered.spacing.nodeNodeBetweenLayers": "80", "elk.layered.crossingMinimization.strategy": "LAYER_SWEEP",
        "elk.layered.nodePlacement.strategy": "BRANDES_KOEPF", "elk.layered.nodePlacement.favorStraightEdges": "true" },
      children: ns.map((n) => ({ id: n.id(), width: n.width(), height: n.height(), layoutOptions: { "elk.partitioning.partition": String(n.data("lane") ?? 1) } })),
      edges: es.map((x) => { const e = x as EdgeSingular; return { id: e.id(), sources: [e.source().id()], targets: [e.target().id()] }; }),
    };
    try {
      const laid = await this.elk!.layout(graph);
      cy.batch(() => { for (const c of laid.children ?? []) cy.getElementById(c.id).position({ x: (c.x ?? 0) + (c.width ?? 0) / 2, y: (c.y ?? 0) + (c.height ?? 0) / 2 }); });
      this.lanes = null; // 새 자리에 칸을 다시 맞춘다
    } catch (e) {
      return toast(`배치 실패: ${errOf(e).message}`, "error");
    }
    this.relane();
    this.commit();
    cy.fit(undefined, 30);
  }

  private addNote(attachTo?: string) {
    const cy = this.cy!;
    const sel = cy.$(":selected").filter((x) => x.data("type") !== "note");
    const target = attachTo ?? (sel.length === 1 ? sel[0].id() : undefined);
    let x: number, y: number;
    if (target) {
      const t = cy.getElementById(target);
      const bb = t.boundingBox({});
      x = bb.x2 + 110;
      y = bb.y1 - 50;
    } else {
      const ext = cy.extent();
      x = (ext.x1 + ext.x2) / 2;
      y = ext.y1 + 80;
    }
    const id = `note:${Date.now().toString(36)}`;
    cy.add({ group: "nodes", data: { id, ref: id, type: "note", label: "", text: "메모", w: 180, h: 60, lane: -1, attach: target }, position: { x, y } });
    if (target) this.addAttach(id, target);
    this.commit();
    cy.elements().unselect();
    cy.getElementById(id).select();
  }

  private startLink() {
    this.linking = null;
    this.side.replaceChildren(h("h4", {}, "선 추가"), h("div", { class: "hint" }, "처음 노드를 누르세요. Esc: 그만"));
  }

  private cancelLink() {
    this.linking = undefined;
    this.cy?.nodes().removeClass("linking");
  }

  private onTapNode(n: NodeSingular) {
    if (!isItem(n)) return;
    if (this.linking === undefined) return;
    if (this.linking === null) {
      this.linking = n.id();
      n.addClass("linking");
      this.side.replaceChildren(h("h4", {}, "선 추가"), h("div", { class: "hint" }, `${n.data("text") || n.data("label")} → 끝 노드를 누르세요`));
      return;
    }
    const from = this.linking;
    this.cancelLink();
    if (from === n.id()) return;
    const id = `m:${Date.now().toString(36)}`;
    this.cy!.add({ group: "edges", data: { id, source: from, target: n.id(), kind: "manual", label: "", text: "" } });
    this.commit();
    this.cy!.elements().unselect();
    this.cy!.getElementById(id).select();
  }

  /** 손으로 넣은 것(메모·선)은 지우고, 분석에서 온 것은 숨긴다 */
  private removeSelected() {
    const cy = this.cy!;
    const sel = cy.$(":selected");
    if (!sel.length) return;
    cy.batch(() => {
      for (const x of sel.toArray()) {
        if (x.removed()) continue;
        const manual = x.isEdge() ? x.data("kind") === "manual" : x.data("type") === "note";
        if (x.isNode()) {
          // 붙임선·이 노드에 닿는 선도 같이 (분석 선은 숨김 목록으로)
          for (const e of (x as NodeSingular).connectedEdges().toArray()) {
            if (e.data("kind") !== "attach" && e.data("kind") !== "manual") this.hiddenEdges.push({ ...(e.data() as ReportEdge), hidden: true });
            e.remove();
          }
          if (!manual) this.hiddenNodes.push({ ...(x.data() as ReportNode), x: x.position().x, y: x.position().y, hidden: true });
          x.remove();
        } else {
          if (!manual) this.hiddenEdges.push({ ...(x.data() as ReportEdge), hidden: true });
          x.remove();
        }
      }
    });
    this.relane();
    this.commit();
    this.showReport();
  }

  private showAllHidden() {
    const r = this.serialize();
    if (!r) return;
    for (const n of r.nodes) delete n.hidden;
    for (const e of r.edges) delete e.hidden;
    this.load(r);
    this.commit();
    this.showReport();
  }

  // ── 옆 칸 ─────────────────────────────────

  private onSelect() {
    if (this.linking !== undefined) return;
    const sel = this.cy?.$(":selected").filter((x) => x.data("kind") !== "attach") ?? null;
    if (!sel || !sel.length) return;
    if (sel.length > 1) {
      this.side.replaceChildren(h("h4", {}, `${sel.length}개 고름`),
        h("div", { class: "hint" }, "위의 ⇤ ↔ ⇥ ⤒ ↕ ⤓ 로 맞추고 ⇹ ⇳ 로 같은 간격. Delete: 숨기기(손으로 넣은 것은 지우기)."));
      return;
    }
    const x = sel[0];
    if (x.isEdge()) return this.showEdge(x as EdgeSingular);
    const n = x as NodeSingular;
    if (n.data("type") === "note") this.showNote(n);
    else this.showNode(n);
  }

  private palette(colors: string[], cur: string | undefined, pick: (c: string) => void) {
    return h("div", { class: "rp-palette" }, ...colors.map((c) => h("button", {
      class: `rp-sw${(cur ?? "") === c ? " on" : ""}`, title: c ? c : "기본", style: c ? `background:${c}` : "", onclick: () => pick(c),
    }, c ? "" : "×")));
  }

  private setData(x: NodeSingular | EdgeSingular, k: string, v: unknown) {
    if (v === "" || v == null) x.removeData(k); else x.data(k, v);
    this.commit();
  }

  private showNode(n: NodeSingular) {
    const nameIn = h("input", { value: n.data("text") ?? "", placeholder: n.data("label"), style: "width:100%" });
    nameIn.onchange = () => this.setData(n, "text", nameIn.value.trim());
    this.side.replaceChildren(
      h("h4", {}, { proc: "처리", trig: "트리거", table: "테이블" }[n.data("type") as string] ?? ""),
      h("div", { class: "dfd-title" }, n.data("ref")),
      n.hasClass("gone") ? h("div", { class: "warn" }, "지금 분석 결과에는 없는 노드입니다") : "",
      h("label", { class: "rp-lbl" }, "표시 이름 (비우면 원래 이름)"), nameIn,
      h("label", { class: "rp-lbl" }, "색"), this.palette(NODE_COLORS, n.data("color"), (c) => { this.setData(n, "color", c); this.showNode(n); }),
      h("div", { class: "row", style: "margin-top:8px" },
        h("button", { class: "small", onclick: () => this.addNote(n.id()) }, "메모 달기"),
        h("button", { class: "small", title: "보고서에서 뺀다 (Delete) — 위 '숨김' 에서 다시 보인다", onclick: () => this.removeSelected() }, "숨기기")));
  }

  private showNote(n: NodeSingular) {
    const ta = h("textarea", { rows: 6, style: "width:100%" }) as HTMLTextAreaElement;
    ta.value = n.data("text") ?? "";
    ta.oninput = () => n.data("text", ta.value);
    ta.onchange = () => this.commit();
    const wIn = h("input", { type: "number", value: String(n.data("w")), min: "80", max: "600", step: "10", style: "width:70px" }) as HTMLInputElement;
    wIn.onchange = () => { const w = Math.max(80, Math.min(600, Number(wIn.value) || 180)); n.data("w", w); n.data("h", Math.max(40, Math.ceil((ta.value.length * 7.5) / (w - 16)) * 16 + 20)); this.commit(); };
    this.side.replaceChildren(h("h4", {}, "메모"), ta,
      h("label", { class: "rp-lbl" }, "너비"), wIn,
      h("label", { class: "rp-lbl" }, "색"), this.palette(NOTE_COLORS, n.data("color") ?? NOTE_COLORS[0], (c) => { this.setData(n, "color", c === NOTE_COLORS[0] ? "" : c); this.showNote(n); }),
      h("div", { class: "row", style: "margin-top:8px" },
        n.data("attach") ? h("button", { class: "small", onclick: () => { this.cy!.getElementById(`att:${n.id()}`).remove(); this.setData(n, "attach", ""); this.showNote(n); } }, "연결 끊기") : "",
        h("button", { class: "small", onclick: () => this.removeSelected() }, "지우기")));
    setTimeout(() => ta.focus(), 0);
  }

  private showEdge(e: EdgeSingular) {
    const kind = e.data("kind") as EdgeKind | "manual";
    const textIn = h("textarea", { rows: 3, style: "width:100%", placeholder: "선 가운데에 보일 설명" }) as HTMLTextAreaElement;
    textIn.value = e.data("text") ?? "";
    textIn.onchange = () => this.setData(e, "text", textIn.value.trim());
    const route = h("select", {}, ...Object.entries(ROUTE_KO).map(([k, v]) => h("option", { value: k, selected: (e.data("route") ?? "taxi") === k }, v)));
    route.onchange = () => this.setData(e, "route", route.value === "taxi" ? "" : route.value);
    const name = (id: string) => { const n = this.cy!.getElementById(id); return n.data("text") || n.data("label") || id; };
    this.side.replaceChildren(
      h("h4", {}, kind === "manual" ? "손으로 넣은 선" : KIND_KO[kind as EdgeKind] ?? kind),
      h("div", { class: "dfd-title" }, name(e.data("source"))), h("div", { class: "dim" }, "↓"), h("div", { class: "dfd-title" }, name(e.data("target"))),
      e.data("label") && kind !== "manual" ? h("p", {}, `연산 ${e.data("label")}`) : "",
      e.hasClass("gone") ? h("div", { class: "warn" }, "지금 분석 결과에는 없는 선입니다") : "",
      e.data("guess") ? h("p", { class: "warn" }, "추정 — 소스에서 직접 확인하지 못한 선") : "",
      h("label", { class: "rp-lbl" }, "설명"), textIn,
      h("label", { class: "rp-lbl" }, "모양"), route,
      h("label", { class: "rp-lbl" }, "색"), this.palette(EDGE_COLORS, e.data("color"), (c) => { this.setData(e, "color", c); this.showEdge(e); }),
      h("div", { class: "row", style: "margin-top:8px" },
        h("button", { class: "small", onclick: () => this.addNote(e.id()) }, "메모 달기"),
        h("button", { class: "small", onclick: () => this.removeSelected() }, kind === "manual" ? "지우기" : "숨기기")));
  }

  /** 아무것도 고르지 않았을 때: 보고서 정보·바뀐 것·숨긴 것 */
  private showReport() {
    const r = this.r;
    if (!r) return;
    const title = h("input", { value: r.title, style: "width:100%" });
    title.onchange = () => { r.title = title.value; this.commit(); };
    const desc = h("textarea", { rows: 5, style: "width:100%", placeholder: "보고서 설명 (HTML 머리에 들어간다)" }) as HTMLTextAreaElement;
    desc.value = r.description;
    desc.onchange = () => { r.description = desc.value; this.commit(); };
    const b = r.basis;
    const box = h("div", {},
      h("h4", {}, "보고서"),
      h("label", { class: "rp-lbl" }, "제목"), title,
      h("label", { class: "rp-lbl" }, "설명"), desc,
      h("div", { class: "hint", style: "margin-top:6px" },
        `기준: ${b.center ?? "전체"} · ${{ up: "출력 기준", down: "입력 기준", both: "주변 전체" }[b.dir] ?? b.dir} · ${b.depth || "끝까지"}단계 · ${b.unit === "pkg" ? "패키지" : "프로시저"} 단위`));
    const d = this.diff;
    if (d && (d.added.length || d.gone.length)) {
      box.append(h("div", { class: "rp-diff" },
        h("strong", {}, "분석 결과가 바뀌었습니다"),
        h("div", {}, `새로 생긴 선 ${d.added.length}${d.addedNodes.length ? ` (노드 ${d.addedNodes.length})` : ""} · 없어진 선 ${d.gone.length}`),
        h("div", { class: "row" },
          d.added.length ? h("button", { class: "small", onclick: () => this.applyAdded() }, "새 것 넣기") : "",
          d.gone.length ? h("button", { class: "small", onclick: () => this.markGone() }, "없어진 것 표시") : "",
          h("button", { class: "small", onclick: () => { this.diff = null; this.showReport(); } }, "그대로 두기"))));
    }
    const hid = this.hiddenNodes.length + this.hiddenEdges.length;
    if (hid) box.append(h("div", { class: "row", style: "margin-top:8px" }, h("button", { class: "small", onclick: () => this.showAllHidden() }, `숨긴 것 ${hid}개 다시 보이기`)));
    box.append(h("h4", {}, "고치는 법"), h("ul", { class: "an-ul" },
      h("li", {}, "끌기: 옮기기 · Shift+클릭 / Ctrl+끌기: 여러 개 고르기 · Ctrl+A: 모두"),
      h("li", {}, "노드·선을 누르면 오른쪽에서 이름·설명·색·모양"),
      h("li", {}, "Delete: 분석에서 온 것은 숨기고, 손으로 넣은 메모·선은 지운다"),
      h("li", {}, "Ctrl+Z / Ctrl+Y · Ctrl+S 저장")));
    this.side.replaceChildren(box);
  }

  // ── 분석 결과와 비교 ─────────────────────────────────

  private compare(): { added: ReportEdge[]; addedNodes: ReportNode[]; gone: string[] } | null {
    const r = this.r, src = this.opts.src();
    if (!r || !src) return null;
    const base = buildBase(src, r.basis.unit === "pkg");
    if (r.basis.center && !base.nodes.has(r.basis.center)) return null;
    const view = buildView(base, { center: r.basis.center, depth: r.basis.depth, showCalls: r.basis.calls, showFlows: r.basis.flows,
      hidden: new Set(r.basis.hidden), direction: r.basis.center ? (r.basis.dir as "up" | "down" | "both") : "both" });
    const mine = new Set(r.edges.filter((e) => e.kind !== "manual").map((e) => edgeKey(e.source, e.target, e.kind)));
    const now = new Set(view.edges.map((e) => edgeKey(e.source, e.target, e.kind)));
    const nodeIds = new Set(r.nodes.map((n) => n.id));
    const added: ReportEdge[] = view.edges.filter((e) => !mine.has(edgeKey(e.source, e.target, e.kind)))
      .map((e) => ({ id: `n:${e.id}:${Date.now().toString(36)}`, source: e.source, target: e.target, kind: e.kind, label: e.label, guess: e.guess }));
    const addedNodes: ReportNode[] = view.nodes.filter((n) => !nodeIds.has(n.id)).map((n) => {
      const [w, hh] = boxSize(n.label, n.type, n.center);
      return { id: n.id, ref: n.ref, type: n.type, label: n.label, x: 0, y: 0, w, h: hh, lane: n.lane, copy: n.copy || undefined };
    });
    const gone = r.edges.filter((e) => e.kind !== "manual" && !now.has(edgeKey(e.source, e.target, e.kind))).map((e) => e.id);
    return { added, addedNodes, gone };
  }

  private applyAdded() {
    const d = this.diff;
    const cy = this.cy!;
    if (!d) return;
    // 새 노드는 자기 칸의 맨 아래에 줄 세운다
    const bottom = new Map<number, { x: number; y: number }>();
    for (const n of cy.nodes().toArray() as NodeSingular[]) {
      const lane = n.data("lane");
      if (!isBox(n)) continue;
      const p = n.position(), cur = bottom.get(lane);
      if (!cur || p.y + n.height() / 2 > cur.y) bottom.set(lane, { x: p.x, y: p.y + n.height() / 2 });
    }
    for (const n of d.addedNodes) {
      const b = bottom.get(n.lane) ?? { x: n.lane * 300, y: 0 };
      const y = b.y + 30 + n.h / 2;
      cy.add({ group: "nodes", data: { ...n }, position: { x: b.x, y } });
      bottom.set(n.lane, { x: b.x, y: y + n.h / 2 });
    }
    const has = (id: string) => cy.getElementById(id).length > 0;
    cy.add(d.added.filter((e) => has(e.source) && has(e.target)).map((e) => ({ group: "edges" as const, data: { ...e, tag: e.kind === "read" || e.kind === "call" ? "" : e.label } })));
    this.diff = { ...d, added: [], addedNodes: [] };
    this.relane();
    this.commit();
    this.showReport();
    toast("새로 생긴 노드·선을 넣었습니다 — 자리를 손보거나 '자동 배치'", "info", 4000);
  }

  private markGone() {
    const d = this.diff;
    if (!d) return;
    for (const id of d.gone) { const e = this.cy!.getElementById(id); e.addClass("gone"); e.data("gone", true); }
    this.diff = { ...d, gone: [] };
    this.commit();
    this.showReport();
  }

  // ── 내보내기 ─────────────────────────────────

  private png(): string {
    const divs = this.cy!.nodes("[type='divider']");
    divs.style("display", "none");
    try {
      return this.cy!.png({ output: "base64uri", full: true, scale: 2, bg: "#ffffff", maxWidth: 12000, maxHeight: 12000 });
    } finally {
      divs.removeStyle("display");
    }
  }

  private fileBase() {
    return (this.r?.name || "보고서").replace(/[\\/:*?"<>|]/g, "_");
  }

  private async writeFile(path: string, bytes: Uint8Array) {
    await invoke("write_binary_file", bytes, { headers: { path: encodeURIComponent(path) } });
  }

  private async exportPng() {
    const path = await saveDialog({ filters: [{ name: "PNG", extensions: ["png"] }], defaultPath: `${this.fileBase()}.png` });
    if (!path) return;
    try {
      const bin = atob(this.png().split(",")[1]);
      await this.writeFile(path, Uint8Array.from(bin, (c) => c.charCodeAt(0)));
      toast(`저장했습니다: ${path}`, "info");
    } catch (e) { toast(errOf(e).message, "error"); }
  }

  /** 그림 + 메모 + 선 설명 + 테이블 CRUD — 혼자 열리는 HTML (인쇄하면 PDF) */
  html(): string {
    const r = this.serialize()!;
    const cy = this.cy!;
    const label = (id: string) => { const n = cy.getElementById(id); return n.length ? (n.data("text") || n.data("label")).replace(/\n/g, " ") : id; };
    const ref = (id: string) => cy.getElementById(id).data("ref") ?? id;
    const notes = r.nodes.filter((n) => n.type === "note" && !n.hidden && (n.text ?? "").trim());
    const edgesWithText = r.edges.filter((e) => !e.hidden && (e.text ?? "").trim());
    // 테이블 CRUD: 보이는 읽기·쓰기 선
    const crud = new Map<string, Map<string, Set<string>>>();
    for (const e of r.edges) {
      if (e.hidden || !(e.kind === "read" || e.kind === "write" || e.kind === "use")) continue;
      const [table, proc] = e.kind === "write" ? [ref(e.target), ref(e.source)] : [ref(e.source), ref(e.target)];
      const ops = e.kind === "read" ? ["R"] : e.kind === "use" ? ["?"] : e.label.split(",").filter(Boolean);
      const m = crud.get(table) ?? new Map<string, Set<string>>();
      const s = m.get(proc) ?? new Set<string>();
      for (const o of ops) s.add(o);
      m.set(proc, s);
      crud.set(table, m);
    }
    const ord = (s: Set<string>) => ["C", "R", "U", "D", "?"].filter((x) => s.has(x)).join("");
    const b = r.basis;
    const when = (t: number) => new Date(t * 1000).toLocaleString();
    const legend = (Object.entries(COLORS) as [EdgeKind, string][]).map(([k, c]) => `<span class="lg"><i style="background:${c}"></i>${esc(KIND_KO[k])}</span>`).join("")
      + `<span class="lg"><i style="background:${MANUAL_COLOR}"></i>손으로 넣은 선</span>`;
    return `<!doctype html>
<html lang="ko"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">
<title>${esc(r.title || r.name)}</title>
<style>
body{font:14px/1.6 "Malgun Gothic",sans-serif;color:#1d232b;max-width:1200px;margin:24px auto;padding:0 20px}
h1{font-size:22px;margin:0 0 4px}h2{font-size:16px;margin:28px 0 8px;border-bottom:1px solid #d0d7de;padding-bottom:4px}
.meta{color:#57606a;font-size:12px}.desc{white-space:pre-wrap;margin:12px 0}
img{max-width:100%;border:1px solid #d0d7de;border-radius:6px}
table{border-collapse:collapse;width:100%;font-size:13px}th,td{border:1px solid #d0d7de;padding:4px 8px;text-align:left;vertical-align:top}
th{background:#f6f8fa}code{font-family:Consolas,monospace}.lg{margin-right:14px;font-size:12px}.lg i{display:inline-block;width:18px;height:3px;margin-right:4px;vertical-align:middle}
ol li{white-space:pre-wrap;margin-bottom:4px}
@media print{body{margin:0}h2{break-after:avoid}img{break-inside:avoid}}
</style></head><body>
<h1>${esc(r.title || r.name)}</h1>
<div class="meta">분석 결과: ${esc(r.profile)} · 기준: ${esc(b.center ?? "전체")} (${esc({ up: "출력 기준", down: "입력 기준", both: "주변 전체" }[b.dir] ?? b.dir)}, ${b.depth || "끝까지"}단계) · 고친 때: ${esc(when(r.updated_at || now()))} · SQLStudio</div>
${r.description.trim() ? `<div class="desc">${esc(r.description)}</div>` : ""}
<p><img src="${this.png()}" alt="${esc(r.title || r.name)}"></p>
<div>${legend}</div>
${notes.length ? `<h2>메모</h2><ol>${notes.map((n) => `<li>${n.attach ? `<b>${esc(label(n.attach))}</b> — ` : ""}${esc(n.text!.trim())}</li>`).join("")}</ol>` : ""}
${edgesWithText.length ? `<h2>선 설명</h2><table><tr><th>보내는 쪽</th><th>받는 쪽</th><th>종류</th><th>설명</th></tr>${edgesWithText.map((e) =>
  `<tr><td><code>${esc(label(e.source))}</code></td><td><code>${esc(label(e.target))}</code></td><td>${esc(e.kind === "manual" ? "손으로 넣은 선" : `${KIND_KO[e.kind as EdgeKind] ?? e.kind}${e.label && e.kind !== "read" ? ` ${e.label}` : ""}`)}</td><td>${esc(e.text!.trim())}</td></tr>`).join("")}</table>` : ""}
${crud.size ? `<h2>테이블 CRUD</h2><table><tr><th>테이블</th><th>처리</th><th>연산</th></tr>${[...crud.entries()].sort().map(([t, m]) =>
  [...m.entries()].sort().map(([p, s], i) => `<tr>${i === 0 ? `<td rowspan="${m.size}"><code>${esc(t)}</code></td>` : ""}<td><code>${esc(p)}</code></td><td>${ord(s)}</td></tr>`).join("")).join("")}</table>
<p class="meta">C 넣기 · R 읽기 · U 바꾸기 · D 지우기 · ? 사전에는 있으나 연산 모름</p>` : ""}
</body></html>`;
  }

  private async exportHtml() {
    const path = await saveDialog({ filters: [{ name: "HTML", extensions: ["html"] }], defaultPath: `${this.fileBase()}.html` });
    if (!path) return;
    try {
      await this.writeFile(path, new TextEncoder().encode(this.html()));
      toast(`저장했습니다: ${path} — 브라우저로 열어 인쇄하면 PDF`, "info", 5000);
    } catch (e) { toast(errOf(e).message, "error"); }
  }
}
