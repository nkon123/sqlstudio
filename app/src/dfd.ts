// 데이터 흐름도(DFD) 화면 — 보고서형: 요소별 칸, 직각 선, 겹침 최소화.
//
// 배치·선 경로는 ELK(layered) 가 함께 계산하고 Cytoscape 로 그린다. 둘 다 처음 열 때만 불러온다 (번들 분리).
// 노드 클릭: 상세 · 더블클릭: 그 노드를 가운데로 · 마우스 올리기: 닿는 선만 진하게

import type { Core, EdgeSingular, NodeSingular } from "cytoscape";
import { invoke } from "@tauri-apps/api/core";
import { save as saveDialog } from "@tauri-apps/plugin-dialog";
import { errOf } from "./api";
import { type Base, type DfdSource, type EdgeKind, type XmlMode, buildBase, buildView, busiest, edgesOf, fileBoxId, flattenBoxes, hasXml, packageOf } from "./dfd-model";
import { BOX_TOP, LAYOUTS, VERT_LABEL, type LayoutKind, type Placed, place } from "./dfd-layout";
import { field, h, modal, toast } from "./ui";
import { showMenu } from "./ctxmenu";
import type { DfdReport, Report } from "./dfd-report";

/** XML 쿼리(화면) 상자 색 */
export const XML_COLOR = "#2e8b57";
export const COLORS = { read: "#2f6fdd", write: "#e07a1f", call: "#8a93a0", flow: "#7a4fd0", trig: "#c62f3c", use: "#a3abb5" };
export const KIND_KO: Record<EdgeKind, string> = { read: "읽기", write: "쓰기", call: "호출", trig: "트리거 발동", flow: "커서 흐름", use: "쓰임 (연산 모름)" };
/** 이보다 크면 처음에는 가장 바쁜 노드를 가운데로 */
const WHOLE_LIMIT = 80;

export type Libs = { cytoscape: typeof import("cytoscape"); ELK: typeof import("elkjs/lib/elk.bundled.js").default };
let libs: Promise<Libs> | null = null;
export function loadLibs(): Promise<Libs> {
  libs ??= Promise.all([import("cytoscape"), import("elkjs/lib/elk.bundled.js")]).then(([c, e]) => ({ cytoscape: c.default, ELK: e.default }));
  return libs;
}

export class DfdView {
  readonly el: HTMLElement;
  private canvas = h("div", { class: "dfd-canvas" });
  private side = h("div", { class: "dfd-side an-pad" });
  private centerIn = h("input", { placeholder: "가운데 (비우면 전체)", style: "width:240px" });
  private names = h("datalist", { id: "dfd-names" });
  private dirSel = h("select", { title: "가운데를 어느 쪽으로 볼지" },
    h("option", { value: "up" }, "출력 기준 ← 어디서 오나"),
    h("option", { value: "down" }, "입력 기준 → 어디로 가나"),
    h("option", { value: "both" }, "주변 전체"));
  private depthSel = h("select", { title: "가운데에서 몇 단계까지 (출력·입력 기준이면 테이블 단계)" },
    ...[1, 2, 3, 4].map((d) => h("option", { value: String(d), selected: d === 1 }, `${d}단계`)), h("option", { value: "0" }, "끝까지"));
  private unitSel = h("select", { title: "처리 상자 하나가 프로시저 하나인지, 패키지 하나인지" },
    h("option", { value: "proc" }, "처리: 프로시저"), h("option", { value: "pkg" }, "처리: 패키지"));
  private xmlSel = h("select", { title: "XML 쿼리 파일(화면)을 파일 상자 안에 쿼리 id 들로 볼지, 파일 하나를 상자 하나로 볼지", hidden: true },
    h("option", { value: "id" }, "XML: 파일·id"), h("option", { value: "file" }, "XML: 파일"));
  private layoutSel = h("select", { title: "배치 — 흐름 순서(가로·세로·자동), 가운데 중심, 요소별 칸" },
    ...LAYOUTS.map(([v, label, title]) => h("option", { value: v, title }, label)));
  /** 마지막으로 그린 배치 (auto 면 고른 쪽) — 보고서에 남긴다 */
  private placedAs: Exclude<LayoutKind, "auto"> = "right";
  private callBox = h("input", { type: "checkbox" });
  private flowBox = h("input", { type: "checkbox" });
  private hiddenBtn = h("button", { class: "small", hidden: true, title: "숨긴 노드를 모두 다시 보인다" });
  private info = h("span", { class: "dim" });
  private cy: Core | null = null;
  private elk: InstanceType<Libs["ELK"]> | null = null;
  private base: Base | null = null;
  private src: DfdSource | null = null;
  private center: string | null = null;
  private hidden = new Set<string>();
  private runId = 0;

  private report: DfdReport | null = null;
  private main!: HTMLElement[];

  /** profile: 보고서를 둘 결과 폴더 이름 (없으면 보고서 단추를 숨긴다) */
  constructor(private onOpenNode: ((id: string) => void) | null,
    private opts: { popout?: (center: string | null, dir: string) => void; profile?: () => Promise<string | null> } = {}) {
    this.centerIn.setAttribute("list", "dfd-names");
    const tb = h("div", { class: "dfd-toolbar" },
      this.centerIn, this.names, this.dirSel, this.depthSel,
      h("button", { class: "small", title: "가운데 없이 전체", onclick: () => this.setCenter(null) }, "전체"),
      h("span", { class: "sep" }), this.layoutSel, this.unitSel, this.xmlSel,
      h("label", { class: "check", title: "처리 → 처리 호출선" }, this.callBox, "처리간 호출"),
      h("label", { class: "check", title: "커서로 읽어 다른 테이블에 넣는 흐름. 읽기·쓰기 선과 겹치는 내용이라 기본으로 숨긴다" }, this.flowBox, "테이블 흐름"),
      this.hiddenBtn,
      h("span", { class: "spacer" }), this.info,
      h("button", { class: "small", onclick: () => this.cy?.fit(undefined, 30) }, "맞추기"),
      h("button", { class: "small", title: "보고서에 넣을 그림으로 저장", onclick: () => this.savePng() }, "PNG"),
      opts.profile ? h("button", { class: "small primary", title: "지금 그림을 사본으로 떼어 손보는 보고서 모드 (메모·선·정렬) — 저장한 보고서 열기", onclick: (e: MouseEvent) => void this.reportMenu(e) }, "보고서 ▾") : "",
      opts.popout ? h("button", { class: "small", title: "지금 보는 것을 별도 창으로", onclick: () => opts.popout!(this.center, this.dirSel.value) }, "새 창") : "");
    this.centerIn.addEventListener("change", () => {
      const v = this.centerIn.value.trim();
      if (!v) return this.setCenter(null);
      const id = this.resolveName(v);
      if (!id) return toast(`'${v}' 를 찾지 못했습니다`, "error");
      this.setCenter(id);
    });
    this.depthSel.onchange = () => this.draw();
    try {
      const saved = localStorage.getItem("dfd.layout");
      if (saved && LAYOUTS.some(([v]) => v === saved)) this.layoutSel.value = saved;
    } catch { /* 기억 못 해도 된다 */ }
    this.layoutSel.onchange = () => {
      try { localStorage.setItem("dfd.layout", this.layoutSel.value); } catch { /* 무시 */ }
      this.sideHint();
      this.draw();
    };
    this.dirSel.onchange = () => this.draw();
    this.unitSel.onchange = () => this.rebase();
    this.xmlSel.onchange = () => this.rebase();
    this.callBox.onchange = () => this.draw();
    this.flowBox.onchange = () => this.draw();
    this.hiddenBtn.onclick = () => { this.hidden.clear(); this.draw(); };
    const body = h("div", { class: "dfd-body" }, this.canvas, this.side);
    this.main = [tb, body];
    this.el = h("div", { class: "dfd" }, tb, body);
    this.sideHint();
  }

  private layoutKind(): LayoutKind {
    return (this.layoutSel.value as LayoutKind) || "auto";
  }

  private xmlMode(): XmlMode {
    return this.xmlSel.value === "file" ? "file" : "id";
  }

  /** 처리·XML 단위를 바꾼 뒤 바탕 그래프를 다시 */
  private rebase(center = this.center) {
    if (!this.src) return;
    this.base = buildBase(this.src, this.unitSel.value === "pkg", this.xmlMode());
    this.fillNames();
    // 가운데가 프로시저·쿼리 id 였으면 그 패키지·파일로 (되돌릴 때는 패키지·파일 그대로 두면 없으니 전체로)
    if (center && !this.base.nodes.has(center)) {
      const p = packageOf(center);
      center = this.base.nodes.has(p) ? p : null;
    }
    this.center = center;
    this.centerIn.value = this.center ?? "";
    this.hidden.clear();
    this.draw();
  }

  // ── 보고서 모드 ─────────────────────────────

  private async reportMenu(e: MouseEvent) {
    const profile = await this.opts.profile?.();
    if (!profile) return toast("분석 결과 폴더를 알 수 없습니다", "error");
    let list: { id: string; name: string; center?: string | null; updated_at?: number | null }[] = [];
    try {
      list = await invoke("dfd_reports", { profile });
    } catch (err) {
      return toast(errOf(err).message, "error");
    }
    const r = (e.target as HTMLElement).getBoundingClientRect();
    showMenu(r.left, r.bottom + 2, [
      { label: "새 보고서 (지금 그림으로)", disabled: !this.cy?.nodes().length, run: () => void this.newReport(profile) },
      ...(list.length ? [{ sep: true as const }, { title: `저장한 보고서 ${list.length}` }] : []),
      ...list.map((x) => ({ label: x.name, hint: x.updated_at ? new Date(x.updated_at * 1000).toLocaleDateString() : "", run: () => void this.openReport(profile, x.id) })),
    ]);
  }

  private async newReport(profile: string) {
    const nameIn = h("input", { value: `${this.center ? this.center.split(".").pop() : "전체"} 흐름`, style: "width:100%" });
    const ok = await modal("새 보고서", h("div", {}, field("보고서 이름", nameIn, "같은 프로시저로 여러 개 만들 수 있습니다 (예: 운영 보고용, 개발 검토용)")),
      [{ label: "취소", value: false }, { label: "만들기", value: true, kind: "primary" }]);
    if (!ok || !this.cy) return;
    const { snapshot } = await import("./dfd-report");
    const basis = { center: this.center, dir: this.dirSel.value, depth: Number(this.depthSel.value), unit: this.unitSel.value as "proc" | "pkg", xml: this.xmlMode(), layout: this.placedAs,
      calls: this.callBox.checked, flows: this.flowBox.checked, hidden: [...this.hidden] };
    await this.showReport(snapshot(this.cy, basis, profile, nameIn.value.trim() || "보고서"));
  }

  private async openReport(profile: string, id: string) {
    try {
      await this.showReport(await invoke<Report>("dfd_report_get", { profile, id }));
    } catch (e) {
      toast(errOf(e).message, "error");
    }
  }

  private async showReport(r: Report) {
    const { DfdReport } = await import("./dfd-report");
    this.report ??= new DfdReport({ src: () => this.src, onClose: () => this.closeReport() });
    for (const x of this.main) x.hidden = true;
    if (!this.report.el.isConnected) this.el.append(this.report.el);
    this.report.el.hidden = false;
    await this.report.open(r);
  }

  private closeReport() {
    if (this.report) this.report.el.hidden = true;
    for (const x of this.main) x.hidden = false;
    this.cy?.resize();
  }

  /** 분석 결과를 받는다. focus 가 있으면 그 노드를 가운데로 */
  /** unit: 처리 상자 단위 — 패키지를 가운데 두려면 "pkg". 없으면 가운데가 있을 때 프로시저 단위 */
  setData(g: DfdSource, focus?: string | null, dir?: string | null, unit?: "proc" | "pkg" | null) {
    this.src = g;
    if (dir && ["up", "down", "both"].includes(dir)) this.dirSel.value = dir;
    if (unit) this.unitSel.value = unit;
    // 프로시저에서 넘어오면 프로시저 단위로
    else if (focus) this.unitSel.value = "proc";
    this.xmlSel.hidden = !hasXml(g);
    this.sideHint();
    this.base = buildBase(g, this.unitSel.value === "pkg", this.xmlMode());
    this.hidden.clear();
    const all = this.fillNames();
    if (focus && this.base.nodes.has(focus)) this.center = focus;
    else if (!this.center || !this.base.nodes.has(this.center)) this.center = all.length > WHOLE_LIMIT ? busiest(this.base) : null;
    this.centerIn.value = this.center ?? "";
    this.draw();
  }

  private fillNames() {
    const all = [...this.base!.nodes.values()];
    this.names.replaceChildren(...all.map((n) => h("option", { value: n.id })));
    return all;
  }

  private resolveName(v: string): string | null {
    const b = this.base!;
    const q = v.toUpperCase();
    if (b.nodes.has(q)) return q;
    const hit = [...b.nodes.keys()].filter((id) => id.endsWith(`.${q}`) || id === `${b.owner}.${q}`);
    return hit[0] ?? null;
  }

  private setCenter(id: string | null) {
    this.center = id;
    this.centerIn.value = id ?? "";
    this.draw();
  }

  private async draw() {
    const b = this.base;
    if (!b) return;
    const run = ++this.runId;
    const L = await loadLibs().catch((e) => { toast(`그림 도구를 불러오지 못했습니다: ${e}`, "error"); return null; });
    if (!L || run !== this.runId) return;
    const view = buildView(b, {
      center: this.center, depth: Number(this.depthSel.value), showCalls: this.callBox.checked, showFlows: this.flowBox.checked, hidden: this.hidden,
      direction: this.center ? (this.dirSel.value as "up" | "down" | "both") : "both", copies: this.layoutKind() === "lanes",
    });
    this.hiddenBtn.hidden = !this.hidden.size;
    this.hiddenBtn.textContent = `숨김 ${this.hidden.size} 해제`;
    this.info.textContent = `노드 ${view.nodes.filter((n) => !n.copy && n.type !== "xmlfile").length} · 선 ${view.edges.length}`;
    // 가운데로 고른 것이 흐름에 없으면 전체를 그린다 — 그렇다고 알린다 (다른 프로시저가 나와 헷갈리지 않게)
    if (this.center && !b.nodes.has(this.center)) this.info.textContent += ` · '${this.center}' 가 흐름에 없어 전체를 보여 줍니다`;
    if (!view.nodes.length) {
      this.cy?.elements().remove();
      this.info.textContent = "그릴 관계가 없습니다";
      return;
    }
    if (view.nodes.length > 400) toast(`노드 ${view.nodes.length}개 — 배치가 오래 걸릴 수 있습니다. 가운데를 고르면 빨라집니다.`, "info");

    const cy = this.ensureCy(L);
    const kind = this.layoutKind();
    // 중심형은 파일 상자를 쓰지 않는다 (가지가 상자를 가로지른다)
    const shown = kind === "radial" ? flattenBoxes(view) : view;
    this.info.textContent += " · 배치 중…";
    let placed: Placed;
    try {
      const r = this.canvas.getBoundingClientRect();
      placed = await place(this.elk! as never, shown, (n) => boxSize(n.label, n.type, n.center), kind, this.center, r.width || 1200, r.height || 700);
    } catch (e) {
      this.info.textContent = "배치 실패";
      return toast(`배치 실패: ${errOf(e).message}`, "error");
    }
    if (run !== this.runId) return;
    this.info.textContent = this.info.textContent.replace(" · 배치 중…", "");
    if (kind === "auto") this.info.textContent += placed.used === "down" ? " · 세로" : " · 가로";
    if (kind === "radial" && !this.center) this.info.textContent += " · 가운데를 고르면 중심형이 보기 좋습니다";
    this.placedAs = placed.used;
    const { pos, routes } = placed;
    const boxes = shown.nodes.filter((n) => n.type === "xmlfile");
    cy.batch(() => {
      cy.elements().remove();
      // 배경: 칸·단계 띠 또는 고리
      cy.add(placed.bands.map((b) => ({ group: "nodes" as const,
        data: { id: b.id, type: "lane", label: b.label, alt: b.alt, vert: b.vert, ring: b.ring, tag: b.tag, w: b.w, h: b.h },
        position: { x: b.x + b.w / 2, y: b.y + b.h / 2 }, selectable: false, grabbable: false })));
      // 상자 먼저 (Cytoscape 는 부모가 있어야 자식을 붙인다). 상자 크기는 자식에 맞춰 Cytoscape 가 정한다 — 여백은 ELK 와 같게
      cy.add([...boxes, ...shown.nodes.filter((n) => n.type !== "xmlfile")].map((n) => {
        const p = pos.get(n.id)!;
        return { group: "nodes" as const, data: { ...n, w: p.w, h: p.h, ...(n.type === "xmlfile" && placed.used === "down" ? { below: true } : {}) },
          position: { x: p.x + p.w / 2, y: p.y + p.h / 2 }, grabbable: false };
      }));
      cy.add(shown.edges.map((e) => ({ group: "edges" as const, data: { ...e, tag: e.kind === "read" || e.kind === "call" ? "" : e.label } })));
      // ELK 가 낸 꺾이는 점 → Cytoscape segments (시작→끝 직선 기준 비율과 수직 거리). 경로가 없으면(중심형) 곧은 선
      for (const ce of cy.edges().toArray() as EdgeSingular[]) {
        const rt = routes.get(ce.id());
        if (!rt) { ce.addClass("straight"); continue; }
        const { S, T, B } = rt;
        const sc = ce.source().position(), tc = ce.target().position();
        const st: Record<string, string> = { "source-endpoint": `${S.x - sc.x}px ${S.y - sc.y}px`, "target-endpoint": `${T.x - tc.x}px ${T.y - tc.y}px` };
        if (!B.length) { ce.addClass("straight"); ce.style(st); continue; }
        const dx = T.x - S.x, dy = T.y - S.y, L2 = dx * dx + dy * dy || 1, Ln = Math.sqrt(L2);
        ce.style({ ...st,
          "segment-weights": B.map((p) => ((p.x - S.x) * dx + (p.y - S.y) * dy) / L2).join(" "),
          "segment-distances": B.map((p) => ((p.y - S.y) * dx - (p.x - S.x) * dy) / Ln).join(" ") });
      }
    });
    cy.resize();
    cy.fit(undefined, 30);
  }

  private ensureCy(L: Libs): Core {
    if (this.cy) return this.cy;
    this.elk = new L.ELK();
    const cy = L.cytoscape({
      container: this.canvas,
      wheelSensitivity: 0.25,
      minZoom: 0.05,
      maxZoom: 3,
      boxSelectionEnabled: false,
      style: dfdStyle() as never,
    });
    const twinsOf = (n: NodeSingular) => cy.nodes().filter((x) => x.data("ref") === n.data("ref"));
    cy.on("mouseover", "node", (ev) => {
      const n = ev.target as NodeSingular;
      if (n.data("type") === "lane") return;
      const near = n.data("type") === "xmlfile" ? n.union(n.children()).closedNeighborhood() : twinsOf(n).closedNeighborhood();
      // 상자를 흐리게 하면 안의 것도 흐려진다 — 닿는 것의 상자는 남긴다
      const keep = near.union(near.parents()).union(cy.nodes("[type='lane']"));
      cy.elements().not(keep).addClass("dim");
    });
    cy.on("mouseout", "node", () => cy.elements().removeClass("dim"));
    cy.on("tap", "node", (ev) => this.showNode(ev.target.data("ref")));
    cy.on("tap", "edge", (ev) => this.showEdge(ev.target as EdgeSingular));
    cy.on("tap", (ev) => { if (ev.target === cy) this.sideHint(); });
    cy.on("dbltap", "node", (ev) => {
      const n = ev.target as NodeSingular;
      // 파일 상자: 파일 단위로 바꿔 그 파일을 가운데로
      if (n.data("type") === "xmlfile") { this.xmlSel.value = "file"; return this.rebase(n.data("ref")); }
      this.setCenter(n.data("ref"));
    });
    new ResizeObserver(() => cy.resize()).observe(this.canvas);
    this.cy = cy;
    return cy;
  }

  private sideHint() {
    this.side.replaceChildren(h("h4", {}, "보는 법"),
      h("div", { class: "hint" }, {
        lanes: "요소별 칸: 왼쪽 입력 테이블 → 처리 → 오른쪽 출력 테이블. 읽히고 쓰이기도 하는 테이블은 입력 칸에 점선 사본으로 한 번 더 나옵니다.",
        right: "가로 흐름: 데이터가 흐르는 순서대로 왼쪽 → 오른쪽. 테이블은 한 번만 나오고, 띠 위에 그 단계에 있는 것(테이블·처리·화면)을 적습니다.",
        down: "세로 흐름: 데이터가 흐르는 순서대로 위 → 아래. 띠 왼쪽에 그 단계에 있는 것을 적습니다.",
        auto: "자동: 가로·세로 흐름을 둘 다 놓아 보고 화면에 더 크게 보이는 쪽으로 그립니다 (오른쪽 위에 고른 쪽).",
        radial: "중심형: 가운데를 한가운데에, 단계마다 점선 고리. 가운데로 들어오는 쪽(어디서 오나)은 왼쪽, 나가는 쪽은 오른쪽. 가운데를 고르고 1~2단계로 볼 때 좋습니다.",
      }[this.layoutKind()]),
      this.xmlSel.hidden ? "" : h("div", { class: "hint" }, h("span", { class: "dfd-sw", style: `background:${XML_COLOR}` }),
        " 화면 (XML): XML 쿼리 파일의 SQL — 처리(PL/SQL·Pro*C)와 칸을 나눴습니다. 'XML: 파일·id' 면 파일 상자 안에 쿼리 id 들, 'XML: 파일' 이면 파일 하나가 상자 하나. 파일 상자 더블클릭: 그 파일을 가운데로."),
      h("div", { class: "dfd-legend" },
        ...(Object.entries(COLORS) as [EdgeKind, string][]).map(([k, c]) => h("div", {}, h("span", { class: "dfd-sw", style: `background:${c}` }), KIND_KO[k]))),
      h("div", { class: "hint" }, "점선(·····)은 추정 — 소스에서 직접 확인하지 못하고 사전이나 모델로 채운 선입니다."),
      h("div", { class: "hint" }, "클릭: 상세 · 더블클릭: 가운데로 · 마우스 올리기: 닿는 선만 진하게 · ＋: 화면 밖으로 더 이어짐"));
  }

  /** 파일 상자 — 안의 쿼리 id 들 */
  private showFile(file: string) {
    const b = this.base!;
    const qs = [...b.nodes.values()].filter((n) => n.file === file);
    const shown = new Set(this.cy?.getElementById(fileBoxId(file)).children().map((n) => n.data("ref") as string) ?? []);
    this.side.replaceChildren(h("h4", {}, "화면 (XML 파일)"), h("div", { class: "dfd-title" }, file),
      h("div", { class: "row" },
        h("button", { class: "small", title: "파일 단위로 바꿔 이 파일을 가운데로", onclick: () => { this.xmlSel.value = "file"; this.rebase(file); } }, "파일로 가운데")),
      h("h4", {}, `쿼리 ${qs.length}${shown.size < qs.length ? ` (그림에 ${shown.size})` : ""}`),
      h("ul", { class: "an-ul" }, ...qs.map((q) => h("li", {},
        h("a", { href: "#", class: "an-link", onclick: (ev: Event) => { ev.preventDefault(); this.showNode(q.id); } }, q.label),
        shown.has(q.id) ? "" : h("small", { class: "dim" }, " (그림 밖)"),
        q.summary ? h("small", { class: "dim" }, ` — ${q.summary}`) : ""))));
  }

  private showNode(id: string) {
    const b = this.base!;
    if (!b.nodes.has(id) && [...b.nodes.values()].some((n) => n.file === id)) return this.showFile(id);
    const n = b.nodes.get(id);
    if (!n) return;
    const { ins, outs } = edgesOf(b, id);
    const row = (e: (typeof ins)[number], other: string) => h("li", {},
      h("a", { href: "#", class: "an-link", onclick: (ev: Event) => { ev.preventDefault(); this.showNode(other); } }, other),
      h("small", { class: "dim" }, ` ${KIND_KO[e.kind]}${e.label && e.kind !== "read" ? ` ${e.label}` : ""}${e.lines?.length ? ` · ${e.lines.join(", ")}행` : ""}`));
    this.side.replaceChildren(
      h("h4", {}, { proc: this.unitSel.value === "pkg" ? "처리 (패키지)" : "처리 (프로시저)", trig: "트리거", table: "테이블",
        xml: n.file ? "화면 (XML 쿼리)" : "화면 (XML 파일)", xmlfile: "화면 (XML 파일)" }[n.type]),
      h("div", { class: "dfd-title" }, id),
      n.file ? h("div", {}, h("a", { href: "#", class: "an-link", onclick: (ev: Event) => { ev.preventDefault(); this.showFile(n.file!); } }, `파일 ${n.file}`)) : "",
      n.summary ? h("p", {}, n.summary) : "",
      n.commits ? h("div", { class: "warn" }, "COMMIT/ROLLBACK 함") : "",
      h("div", { class: "row" },
        h("button", { class: "small", onclick: () => this.setCenter(id) }, "가운데로"),
        h("button", { class: "small", onclick: () => { this.hidden.add(id); this.draw(); } }, "숨기기"),
        this.onOpenNode && n.type !== "table" && n.id.split(".").length >= 2 && (n.type === "xml" ? !!n.file : this.unitSel.value === "proc") ? h("button", { class: "small", title: "호출 관계 탭에서 본다", onclick: () => this.onOpenNode?.(id) }, "상세") : ""),
      h("h4", {}, `들어오는 것 ${ins.length}`), h("ul", { class: "an-ul" }, ...ins.map((e) => row(e, e.s))),
      h("h4", {}, `나가는 것 ${outs.length}`), h("ul", { class: "an-ul" }, ...outs.map((e) => row(e, e.t))));
  }

  private showEdge(e: EdgeSingular) {
    const kind = e.data("kind") as EdgeKind;
    this.side.replaceChildren(h("h4", {}, KIND_KO[kind]),
      h("div", { class: "dfd-title" }, e.source().data("ref")), h("div", { class: "dim" }, "↓"), h("div", { class: "dfd-title" }, e.target().data("ref")),
      e.data("label") && kind !== "use" ? h("p", {}, kind === "write" ? `연산 ${e.data("label")}` : e.data("label")) : "",
      e.data("guess") ? h("p", { class: "warn" }, kind === "use"
        ? "사전(ALL_DEPENDENCIES)에는 참조한다고 나오지만 소스에서 쓰는 곳을 찾지 못했습니다. '확인 필요' 목록을 보세요."
        : "추정 — 소스에서 직접 확인하지 못했습니다 (모델이 사전·동적 SQL 을 보고 추정).") : "");
  }

  private async savePng() {
    if (!this.cy) return;
    const stamp = new Date().toISOString().slice(0, 10);
    const name = (this.center ?? "전체").replace(/[\\/:*?"<>|]/g, "_");
    const path = await saveDialog({ filters: [{ name: "PNG", extensions: ["png"] }], defaultPath: `DFD_${name}_${stamp}.png` });
    if (!path) return;
    const bg = getComputedStyle(document.documentElement).getPropertyValue("--bg").trim() || "#ffffff";
    const blob = this.cy.png({ output: "blob", full: true, scale: 2, bg, maxWidth: 12000, maxHeight: 12000 }) as unknown as Blob;
    try {
      const bytes = new Uint8Array(await blob.arrayBuffer());
      await invoke("write_binary_file", bytes, { headers: { path: encodeURIComponent(path) } });
      toast(`저장했습니다: ${path}`, "info");
    } catch (e) {
      toast(errOf(e).message, "error");
    }
  }
}

/** 흐름도 그림 모양 — 보고서 편집기도 같은 것을 쓰고 덧붙인다 */
export function dfdStyle(): unknown[] {
  const v = (n: string) => getComputedStyle(document.documentElement).getPropertyValue(n).trim();
  return [
    { selector: "node", style: { label: "data(label)", "text-wrap": "wrap", "text-valign": "center", "text-halign": "center", "font-size": 11,
      "font-family": "Malgun Gothic, sans-serif", color: "#1d232b", width: "data(w)", height: "data(h)", shape: "round-rectangle",
      "border-width": 2, "z-index-compare": "manual", "z-index": 10 } },
    { selector: "node[type='proc']", style: { "border-color": COLORS.read, "background-color": "#e8f0fd" } },
    { selector: "node[type='table']", style: { shape: "rectangle", "border-color": COLORS.write, "border-style": "double", "border-width": 3, "background-color": "#fff4e6" } },
    { selector: "node[type='xml']", style: { "border-color": XML_COLOR, "background-color": "#e7f5ec" } },
    { selector: "node[type='xmlfile']", style: { shape: "round-rectangle", "border-color": XML_COLOR, "border-width": 2, "border-style": "solid",
      "background-color": XML_COLOR, "background-opacity": 0.06, label: "data(label)", "text-valign": "top", "text-halign": "center",
      "text-margin-y": BOX_TOP - 4, "font-weight": "bold", "font-size": 11, color: XML_COLOR, padding: "10px",
      "padding-top": `${BOX_TOP}px` as never, "z-index": 6 } },
    { selector: "node[type='xmlfile'][?below]", style: { "text-valign": "bottom", "text-margin-y": -(BOX_TOP - 4), "padding-top": "10px" as never,
      "padding-bottom": `${BOX_TOP}px` as never } },
    { selector: "node[type='trig']", style: { shape: "diamond", "border-color": COLORS.trig, "background-color": "#fdeaec" } },
    { selector: "node[?copy]", style: { "border-style": "dashed", "border-width": 2 } },
    { selector: "node[?center]", style: { "border-width": 4, "font-weight": "bold", "font-size": 13 } },
    { selector: "node[?more]", style: { label: (n: NodeSingular) => `${n.data("label")}  ＋` } },
    { selector: "node[type='lane']", style: { shape: "rectangle", "z-index": 0, events: "no", "background-color": v("--text"), "background-opacity": 0.04,
      "border-width": 0, label: "data(label)", "text-valign": "top", "text-margin-y": 26, "font-size": 13, "font-weight": "bold", color: v("--dim") } },
    // 맞붙은 칸을 가려 보이게 — 가운데(처리) 칸을 조금 진하게
    { selector: "node[type='lane'][?alt]", style: { "background-opacity": 0.09 } },
    // 세로 흐름의 단계 띠: 이름은 왼쪽에
    { selector: "node[type='lane'][?vert]", style: { "text-valign": "center", "text-halign": "left", "text-margin-x": VERT_LABEL - 14, "text-margin-y": 0,
      "text-wrap": "wrap", "text-max-width": `${VERT_LABEL - 24}px`, "text-justification": "right", "font-size": 12 } },
    // 고리 이름 (고리 위 빈 자리의 점)
    { selector: "node[type='lane'][?tag]", style: { "background-opacity": 0, "border-width": 0, "text-valign": "center", "text-halign": "center",
      "text-margin-y": 0, "font-size": 11, "font-weight": "normal", "text-background-color": v("--bg"), "text-background-opacity": 1, "text-background-padding": "2px" } },
    // 중심형의 고리
    { selector: "node[type='lane'][?ring]", style: { shape: "ellipse", "background-opacity": 0, "border-width": 1, "border-style": "dashed",
      "border-color": v("--dim"), "border-opacity": 0.5, "text-valign": "top", "text-margin-y": 16, "font-size": 11, "font-weight": "normal" } },
    { selector: "node:selected", style: { "overlay-color": v("--accent"), "overlay-opacity": 0.18 } },
    { selector: "edge", style: { width: 2, "curve-style": "segments", "edge-distances": "endpoints" as never, // @types 에 아직 없는 값 (3.30 은 지원) "segment-radius": 6,
      "target-arrow-shape": "triangle", "arrow-scale": 0.9, "z-index-compare": "manual", "z-index": 5,
      "target-label": "data(tag)", "target-text-offset": 22, "target-text-margin-y": -8, "font-size": 10, color: v("--text"),
      "text-background-color": v("--bg"), "text-background-opacity": 0.9, "text-background-padding": "1px" } },
    { selector: "edge.straight", style: { "curve-style": "straight" } },
    ...(Object.entries(COLORS) as [string, string][]).map(([k, c]) => ({ selector: `edge[kind='${k}']`, style: { "line-color": c, "target-arrow-color": c } })),
    { selector: "edge[kind='call']", style: { "line-style": "dashed" } },
    { selector: "edge[kind='flow']", style: { width: 3 } },
    { selector: "edge[?guess]", style: { "line-style": "dotted" } },
    { selector: "edge:selected", style: { width: 4 } },
    { selector: ".dim", style: { opacity: 0.12 } },
  ];
}

const measure = document.createElement("canvas").getContext("2d")!;
export function boxSize(label: string, type: string, center: boolean): [number, number] {
  measure.font = `${center ? "bold 13px" : "11px"} "Malgun Gothic", sans-serif`;
  const lines = label.split("\n");
  const w = Math.max(...lines.map((l) => measure.measureText(l).width)) + 24 + 14; // ＋ 표시 자리
  const hh = lines.length * 15 + 16;
  if (type === "trig") return [Math.round(w * 1.5), Math.round(hh * 1.7)];
  return [Math.max(70, Math.round(w)), Math.max(30, Math.round(hh))];
}
