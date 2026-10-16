// 데이터 흐름도 배치 — 보기 모델(View)을 받아 상자 자리·선 경로·배경(칸·단계·고리)을 낸다. 화면(DOM)과 무관.
//
//   lanes  요소별 칸: 입력 테이블 | 화면 (XML) | 처리 | 출력 테이블 — 칸이 고정, 한 처리의 입출력 요약에 좋다
//   right  가로 흐름: 데이터가 흐르는 순서대로 왼쪽 → 오른쪽 (T1 → P1 → T2 → P2 …), 테이블은 한 번만
//   down   세로 흐름: 같은 것을 위 → 아래로 (단계가 많고 폭이 좁은 그림)
//   auto   가로·세로를 둘 다 배치해 보고 화면에 더 크게 보이는 쪽
//   radial 중심형: 가운데 노드를 한가운데, 단계마다 고리. 들어오는 쪽(어디서 오나)은 왼쪽 반, 나가는 쪽은 오른쪽 반
//
// 흐름 배치는 ELK layered (층 나누기 → 교차 줄이기 → 자리 잡기 → 직각 선). 중심형은 직접 계산한다 (방사형 나무 배치).

import { LANE_POS, LANES, type LaneSpan, type View, type ViewNode, fitLanes } from "./dfd-model";

export type LayoutKind = "auto" | "right" | "down" | "radial" | "lanes";
export const LAYOUTS: [LayoutKind, string, string][] = [
  ["auto", "배치: 자동", "가로 흐름과 세로 흐름 중 화면에 더 크게 보이는 쪽"],
  ["right", "배치: 가로 흐름", "데이터가 흐르는 순서대로 왼쪽 → 오른쪽. 테이블은 한 번만 나온다"],
  ["down", "배치: 세로 흐름", "데이터가 흐르는 순서대로 위 → 아래"],
  ["radial", "배치: 중심형", "가운데를 한가운데에, 단계마다 고리. 들어오는 쪽은 왼쪽, 나가는 쪽은 오른쪽"],
  ["lanes", "배치: 요소별 칸", "입력 테이블 | 화면 | 처리 | 출력 테이블 칸에 나눠 놓는다 (읽고 쓰는 테이블은 입력 칸에 사본)"],
];

export interface Rect { x: number; y: number; w: number; h: number }
export interface Pt { x: number; y: number }
/** 배경: 칸·단계 띠(rect) 또는 고리(ring: 가운데 cx,cy 반지름 r) */
export interface Band { id: string; label: string; alt: boolean; vert?: boolean; ring?: boolean; tag?: boolean; x: number; y: number; w: number; h: number }
export interface Placed {
  /** 왼쪽 위 기준 (상자 안의 것도 전체 좌표) */
  pos: Map<string, Rect>;
  /** ELK 가 낸 선 경로. 없으면 곧은 선 (가운데 → 가운데) */
  routes: Map<string, { S: Pt; T: Pt; B: Pt[] }>;
  bands: Band[];
  /** 실제로 쓴 배치 (auto → right/down) */
  used: Exclude<LayoutKind, "auto">;
}

type Elk = { layout: (g: never) => Promise<ElkNode> };
interface ElkNode { id: string; x?: number; y?: number; width?: number; height?: number; children?: ElkNode[]; edges?: ElkEdge[] }
interface ElkEdge { id: string; sections?: { startPoint: Pt; endPoint: Pt; bendPoints?: Pt[] }[] }

/** 파일 상자 위쪽 여백 (파일 이름 자리) */
export const BOX_TOP = 24;

/** 상자 크기 — 선이 많이 붙는 쪽(흐름 방향의 양옆)을 키워 붙는 점을 벌린다 */
export function sizesFor(view: View, base: (n: ViewNode) => [number, number], dir: "right" | "down" | "radial" | "lanes"): Map<string, [number, number]> {
  const inc = new Map<string, number>(), out = new Map<string, number>();
  for (const e of view.edges) { inc.set(e.target, (inc.get(e.target) ?? 0) + 1); out.set(e.source, (out.get(e.source) ?? 0) + 1); }
  const m = new Map<string, [number, number]>();
  for (const n of view.nodes) {
    if (n.type === "xmlfile") continue;
    let [w, hh] = base(n);
    const k = Math.max(inc.get(n.id) ?? 0, out.get(n.id) ?? 0);
    if (n.type !== "trig" && dir !== "radial") {
      if (dir === "down") w = Math.max(w, k * 16 + 16);
      else hh = Math.max(hh, k * 14 + 12);
    }
    m.set(n.id, [w, hh]);
  }
  return m;
}

const LAYERED: Record<string, string> = {
  "elk.algorithm": "layered", "elk.edgeRouting": "ORTHOGONAL",
  "elk.spacing.edgeNode": "16", "elk.spacing.edgeEdge": "12", "elk.layered.spacing.edgeEdgeBetweenLayers": "12",
  "elk.layered.spacing.edgeNodeBetweenLayers": "16",
  "elk.layered.crossingMinimization.strategy": "LAYER_SWEEP",
  "elk.layered.mergeEdges": "false", "elk.layered.unnecessaryBendpoints": "false",
};

/** 흐름 배치의 ELK 옵션 (요소별 칸 말고). 시험(scratch)에서 바꿔 가며 잰다 */
export const FLOW: Record<string, string> = {
  // 잰 것 (scratch, 처리 12~80·테이블 8~50): BK 가 네트워크 심플렉스와 교차는 같고 꺾임이 적고 3배 빠르다.
  // 층 나누기는 기본(네트워크 심플렉스)이 LONGEST_PATH·COFFMAN_GRAHAM 보다 교차가 적다. 후처리 압축(EDGE_LENGTH)은 세로에서 ELK 가 죽는다
  "elk.layered.nodePlacement.strategy": "BRANDES_KOEPF", "elk.layered.nodePlacement.bk.fixedAlignment": "BALANCED",
  "elk.layered.nodePlacement.favorStraightEdges": "true",
  "elk.layered.cycleBreaking.strategy": "GREEDY",
};

function elkGraph(view: View, sizes: Map<string, [number, number]>, kind: "right" | "down" | "lanes") {
  // 세로 흐름은 선이 상자 위로 들어온다 — 파일 이름은 아래에
  const pad = kind === "down" ? `[top=10,left=10,bottom=${BOX_TOP},right=10]` : `[top=${BOX_TOP},left=10,bottom=10,right=10]`;
  const boxes = view.nodes.filter((n) => n.type === "xmlfile");
  const lanes = kind === "lanes";
  const part = (n: ViewNode) => (lanes ? { "elk.partitioning.partition": String(LANE_POS[n.lane]) } : {});
  const big = view.nodes.length > 150;
  return {
    id: "root",
    layoutOptions: {
      ...LAYERED,
      ...(boxes.length ? { "elk.hierarchyHandling": "INCLUDE_CHILDREN" } : {}),
      "elk.direction": kind === "down" ? "DOWN" : "RIGHT",
      "elk.layered.thoroughness": big ? "7" : "40",
      // 떨어진 덩어리도 같은 층에 — 단계 띠가 그림 전체에서 맞는다
      "elk.separateConnectedComponents": "false",
      ...(lanes
        ? { "elk.partitioning.activate": "true", "elk.spacing.nodeNode": "26", "elk.layered.spacing.nodeNodeBetweenLayers": "80",
          "elk.layered.nodePlacement.strategy": "BRANDES_KOEPF", "elk.layered.nodePlacement.bk.fixedAlignment": "BALANCED",
          "elk.layered.nodePlacement.favorStraightEdges": "true" }
        : { "elk.spacing.nodeNode": kind === "down" ? "34" : "24", "elk.layered.spacing.nodeNodeBetweenLayers": kind === "down" ? "56" : "70",
          ...FLOW }),
    },
    children: [
      ...view.nodes.filter((n) => !n.parent && n.type !== "xmlfile").map((n) => ({ id: n.id, width: sizes.get(n.id)![0], height: sizes.get(n.id)![1], layoutOptions: part(n) })),
      ...boxes.map((b) => ({ id: b.id, layoutOptions: { ...part(b), "elk.padding": pad, "elk.spacing.nodeNode": "10" },
        children: view.nodes.filter((n) => n.parent === b.id).map((n) => ({ id: n.id, width: sizes.get(n.id)![0], height: sizes.get(n.id)![1] })) })),
    ],
    edges: view.edges.map((e) => ({ id: e.id, sources: [e.source], targets: [e.target] })),
  };
}

function readElk(laid: ElkNode): Pick<Placed, "pos" | "routes"> {
  const pos = new Map<string, Rect>();
  for (const c of laid.children ?? []) {
    pos.set(c.id, { x: c.x ?? 0, y: c.y ?? 0, w: c.width ?? 0, h: c.height ?? 0 });
    for (const k of c.children ?? []) pos.set(k.id, { x: (c.x ?? 0) + (k.x ?? 0), y: (c.y ?? 0) + (k.y ?? 0), w: k.width ?? 0, h: k.height ?? 0 });
  }
  const routes = new Map<string, { S: Pt; T: Pt; B: Pt[] }>();
  for (const e of laid.edges ?? []) {
    const s = e.sections?.[0];
    if (s) routes.set(e.id, { S: s.startPoint, T: s.endPoint, B: s.bendPoints ?? [] });
  }
  return { pos, routes };
}

function extent(rs: Iterable<Rect>) {
  let x1 = Infinity, y1 = Infinity, x2 = -Infinity, y2 = -Infinity;
  for (const r of rs) { x1 = Math.min(x1, r.x); y1 = Math.min(y1, r.y); x2 = Math.max(x2, r.x + r.w); y2 = Math.max(y2, r.y + r.h); }
  return { x1, y1, x2, y2, w: x2 - x1, h: y2 - y1 };
}

const KIND_OF: Record<string, string> = { table: "테이블", proc: "처리", trig: "트리거", xml: "화면 (XML)", xmlfile: "화면 (XML)" };

/** 세로 흐름의 띠 이름 자리 (그림 왼쪽) */
export const VERT_LABEL = 130;

/** 흐름 배치의 단계 띠 — 층(같은 단계에 놓인 것)마다 하나. 이름은 그 층에 있는 것의 종류 */
function layerBands(view: View, pos: Map<string, Rect>, vert: boolean): Band[] {
  const tops = view.nodes.filter((n) => !n.parent);
  const items = tops.map((n) => ({ n, r: pos.get(n.id)! })).filter((x) => x.r);
  const a1 = (r: Rect) => (vert ? r.y : r.x), a2 = (r: Rect) => (vert ? r.y + r.h : r.x + r.w);
  items.sort((p, q) => a1(p.r) - a1(q.r));
  const layers: { s: number; e: number; kinds: Set<string> }[] = [];
  for (const it of items) {
    const cur = layers[layers.length - 1];
    if (cur && a1(it.r) < cur.e - 2) { cur.e = Math.max(cur.e, a2(it.r)); cur.kinds.add(KIND_OF[it.n.type] ?? it.n.type); }
    else layers.push({ s: a1(it.r), e: a2(it.r), kinds: new Set([KIND_OF[it.n.type] ?? it.n.type]) });
  }
  if (layers.length < 2) return [];
  const all = extent(pos.values());
  // 이웃 층 사이 한가운데를 경계로 (빈틈 없이)
  const cut: number[] = [layers[0].s - 20];
  for (let k = 1; k < layers.length; k++) cut.push((layers[k - 1].e + layers[k].s) / 2);
  cut.push(layers[layers.length - 1].e + 20);
  const order = ["테이블", "화면 (XML)", "처리", "트리거"];
  return layers.map((l, k) => {
    const label = [...l.kinds].sort((p, q) => order.indexOf(p) - order.indexOf(q)).join(" · ");
    const s = cut[k], e = cut[k + 1];
    return vert
      ? { id: `band:${k}`, label, alt: k % 2 === 1, vert: true, x: all.x1 - VERT_LABEL, y: s, w: all.w + VERT_LABEL + 20, h: e - s }
      : { id: `band:${k}`, label, alt: k % 2 === 1, x: s, y: all.y1 - 46, w: e - s, h: all.h + 62 };
  });
}

function laneBands(view: View, pos: Map<string, Rect>): Band[] {
  const all = extent(pos.values());
  const y1 = all.y1 - 46, y2 = all.y2 + 16;
  const spans: LaneSpan[] = [];
  for (let i = 0; i < LANES.length; i++) {
    const ps = view.nodes.filter((n) => n.lane === i && !n.parent).map((n) => pos.get(n.id)!).filter(Boolean);
    if (ps.length) spans.push({ i, x1: Math.min(...ps.map((p) => p.x)), x2: Math.max(...ps.map((p) => p.x + p.w)) });
  }
  return fitLanes(spans).map((l) => ({ id: `lane:${l.i}`, label: LANES[l.i], alt: l.i === 1, x: l.x1, y: y1, w: l.x2 - l.x1, h: y2 - y1 }));
}

async function layered(elk: Elk, view: View, sizes: Map<string, [number, number]>, kind: "right" | "down" | "lanes"): Promise<Placed> {
  const g = elkGraph(view, sizes, kind);
  let laid: ElkNode;
  try {
    laid = await elk.layout(g as never);
  } catch {
    // ELK 가 드물게 옵션 조합에서 죽는다 — 기본 옵션으로 한 번 더
    const keep = ["elk.algorithm", "elk.direction", "elk.edgeRouting", "elk.hierarchyHandling", "elk.partitioning.activate", "elk.separateConnectedComponents"];
    g.layoutOptions = Object.fromEntries(Object.entries(g.layoutOptions).filter(([k]) => keep.includes(k))) as typeof g.layoutOptions;
    laid = await elk.layout(g as never);
  }
  const { pos, routes } = readElk(laid);
  const bands = kind === "lanes" ? laneBands(view, pos) : layerBands(view, pos, kind === "down");
  return { pos, routes, bands, used: kind };
}

/** 그림이 화면(vw×vh)에 맞춰 들어갈 때의 배율 — 클수록 글씨가 크게 보인다 */
export function fitScale(p: Pick<Placed, "pos" | "bands">, vw: number, vh: number): number {
  const e = extent([...p.pos.values(), ...p.bands]);
  return Math.min((vw - 60) / Math.max(e.w, 1), (vh - 60) / Math.max(e.h, 1));
}

// ── 중심형 (방사형 나무) ─────────────────────────────

/**
 * 가운데에서 너비 우선으로 나무를 만들고, 단계(거리)마다 고리에 놓는다.
 *  - 가운데로 들어오는 쪽(그 노드 → 가운데 방향으로 이어진 것)은 왼쪽 반원, 나가는 쪽은 오른쪽 반원 — 데이터가 왼쪽 → 가운데 → 오른쪽
 *  - 각 노드는 자기 아래 잎 수만큼 각을 받고, 자식은 부모의 각 안에 — 가지끼리 겹치지 않는다
 *  - 고리 반지름은 그 고리의 상자들이 자기 각 안에 들어갈 만큼 (상자 크기에서 계산)
 */
export function radial(view: View, sizes: Map<string, [number, number]>, center: string | null): Placed {
  const ids = view.nodes.filter((n) => n.type !== "xmlfile").map((n) => n.id);
  const pos = new Map<string, Rect>();
  if (!ids.length) return { pos, routes: new Map(), bands: [], used: "radial" };
  const adj = new Map<string, { to: string; out: boolean }[]>();
  for (const id of ids) adj.set(id, []);
  for (const e of view.edges) {
    adj.get(e.source)?.push({ to: e.target, out: true });
    adj.get(e.target)?.push({ to: e.source, out: false });
  }
  const deg = (id: string) => adj.get(id)?.length ?? 0;
  const root = center && adj.has(center) ? center : [...ids].sort((a, b) => deg(b) - deg(a))[0];
  // 너비 우선 나무. 같은 단계에서는 선이 많은 것부터 (큰 가지가 먼저 각을 잡는다)
  const depth = new Map<string, number>([[root, 0]]);
  const kids = new Map<string, string[]>();
  const side = new Map<string, "in" | "out">();
  const q = [root];
  while (q.length) {
    const x = q.shift()!;
    const next = (adj.get(x) ?? []).filter((a) => !depth.has(a.to));
    next.sort((a, b) => deg(b.to) - deg(a.to));
    for (const a of next) {
      if (depth.has(a.to)) continue;
      depth.set(a.to, depth.get(x)! + 1);
      (kids.get(x) ?? kids.set(x, []).get(x)!).push(a.to);
      side.set(a.to, x === root ? (a.out ? "out" : "in") : side.get(x)!);
      q.push(a.to);
    }
  }
  // 닿지 않은 것(떨어진 덩어리)은 바깥 고리에 — 가운데의 자식처럼 다룬다
  const far = ids.filter((id) => !depth.has(id));
  const maxD = Math.max(0, ...depth.values());
  for (const id of far) {
    depth.set(id, maxD + 1);
    (kids.get(root) ?? kids.set(root, []).get(root)!).push(id);
    side.set(id, "out");
  }
  const leaves = new Map<string, number>();
  const count = (x: string): number => {
    const ks = kids.get(x) ?? [];
    const n = ks.length ? ks.reduce((s, k) => s + count(k), 0) : 1;
    leaves.set(x, n);
    return n;
  };
  count(root);
  // 각 배정 (라디안, 0 = 오른쪽, 시계 방향 = y 아래)
  const ang = new Map<string, number>();
  const span = new Map<string, number>();
  // 나무 밖의 선(같은 고리·다른 가지로 가는 것)이 덜 엇갈리게: 형제 순서를 이웃들의 평균 각으로 몇 번 다시 정한다
  const neighbors = new Map<string, string[]>();
  for (const [id, as] of adj) neighbors.set(id, as.map((a) => a.to));
  const refine = (layout: () => void) => {
    for (let pass = 0; pass < 4; pass++) {
      layout();
      let moved = false;
      for (const [p, ks] of kids) {
        if (ks.length < 2) continue;
        const bary = new Map(ks.map((k) => {
          // 자기 가지 밖 이웃의 각 (원 위 평균)
          let sx = 0, sy = 0;
          for (const n of neighbors.get(k) ?? []) {
            if (n === p || !ang.has(n)) continue;
            sx += Math.cos(ang.get(n)!); sy += Math.sin(ang.get(n)!);
          }
          const own = ang.get(k) ?? 0;
          if (!sx && !sy) return [k, own] as const;
          // 부모 각에서 가장 가까운 쪽으로 펼친 값 (−π..π 경계에서 뒤집히지 않게)
          let a = Math.atan2(sy, sx);
          while (a - own > Math.PI) a -= 2 * Math.PI;
          while (own - a > Math.PI) a += 2 * Math.PI;
          return [k, (a + own) / 2] as const;
        }));
        const next = [...ks].sort((x, y) => bary.get(x)! - bary.get(y)!);
        if (next.some((k, i) => k !== ks[i])) { kids.set(p, next); moved = true; }
      }
      if (!moved) break;
    }
    layout();
  };
  const assign = (x: string, a0: number, a1: number) => {
    const ks = kids.get(x) ?? [];
    const tot = ks.reduce((s, k) => s + leaves.get(k)!, 0) || 1;
    let a = a0;
    for (const k of ks) {
      const da = (a1 - a0) * leaves.get(k)! / tot;
      ang.set(k, a + da / 2);
      span.set(k, da);
      assign(k, a, a + da);
      a += da;
    }
  };
  const PI = Math.PI;
  const layoutAll = () => {
    const rootKids = kids.get(root) ?? [];
    const ins = rootKids.filter((k) => side.get(k) === "in"), outs = rootKids.filter((k) => side.get(k) !== "in");
    const sideAssign = (list: string[], a0: number, a1: number) => {
      kids.set(root, list);
      assign(root, a0, a1);
    };
    // 한쪽만 있으면 그쪽에 원의 3/4 를 (반원에 몰리면 고리가 커진다), 둘 다면 잎 수에 따라 나눈다
    if (ins.length && outs.length) {
      const share = Math.min(0.75, Math.max(0.25, ins.reduce((s, k) => s + leaves.get(k)!, 0) / (leaves.get(root) || 1)));
      const wIn = 2 * PI * share;
      sideAssign(ins, PI - wIn / 2, PI + wIn / 2);
      sideAssign(outs, PI + wIn / 2, 3 * PI - wIn / 2);
    } else if (ins.length) sideAssign(ins, PI / 4, 7 * PI / 4);
    else sideAssign(outs, -3 * PI / 4, 3 * PI / 4);
    kids.set(root, rootKids);
  };
  refine(layoutAll);
  // 고리 반지름: 그 고리의 상자가 자기 각에 들어갈 만큼, 앞 고리와는 상자 크기만큼 떨어지게
  const size = (id: string) => sizes.get(id) ?? [80, 30];
  const diag = (id: string) => { const [w, hh] = size(id); return Math.sqrt(w * w + hh * hh); };
  const byD = new Map<number, string[]>();
  for (const [id, d] of depth) (byD.get(d) ?? byD.set(d, []).get(d)!).push(id);
  const radius: number[] = [0];
  const D = Math.max(...byD.keys());
  for (let d = 1; d <= D; d++) {
    const here = byD.get(d) ?? [];
    const prev = byD.get(d - 1) ?? [];
    const gap = Math.max(...prev.map(diag)) / 2 + Math.max(...here.map(diag)) / 2 + 50;
    // 같은 고리 이웃이 겹치지 않게: 호 길이(r·각) ≥ 상자 폭 + 여백
    const need = Math.max(...here.map((id) => (diag(id) + 16) / Math.max(span.get(id) ?? 0.01, 0.01)));
    radius.push(Math.max(radius[d - 1] + gap, need));
  }
  for (const [id, d] of depth) {
    const [w, hh] = size(id);
    const a = ang.get(id) ?? 0, r = radius[d];
    const cx = d ? r * Math.cos(a) : 0, cy = d ? r * Math.sin(a) : 0;
    pos.set(id, { x: cx - w / 2, y: cy - hh / 2, w, h: hh });
  }
  const bands: Band[] = radius.slice(1).map((r, k) => ({ id: `ring:${k + 1}`, label: "", alt: false, ring: true, x: -r, y: -r, w: 2 * r, h: 2 * r }));
  // 고리 이름은 그 고리에서 상자가 없는 가장 넓은 틈에 (위쪽에 두면 상자에 가린다)
  for (let d = 1; d <= D; d++) {
    const as = (byD.get(d) ?? []).map((id) => ((ang.get(id) ?? 0) % (2 * PI) + 2 * PI) % (2 * PI)).sort((a, b) => a - b);
    let best = -PI / 2;
    if (as.length) {
      let gap = -1;
      for (let i = 0; i < as.length; i++) {
        const a = as[i], b = i + 1 < as.length ? as[i + 1] : as[0] + 2 * PI;
        if (b - a > gap) { gap = b - a; best = (a + b) / 2; }
      }
    }
    const r = radius[d];
    bands.push({ id: `ringtag:${d}`, label: `${d}단계`, alt: false, tag: true, x: r * Math.cos(best) - 1, y: r * Math.sin(best) - 1, w: 2, h: 2 });
  }
  return { pos, routes: new Map(), bands, used: "radial" };
}

/**
 * 배치. auto 는 가로·세로를 다 해 보고 화면(vw×vh)에 더 크게 들어가는 쪽 — 비슷하면 가로 (읽는 방향).
 * radial 은 파일 상자를 쓰지 않는다 (부르는 쪽에서 상자를 풀어 넘긴다).
 */
export async function place(elk: Elk, view: View, base: (n: ViewNode) => [number, number], kind: LayoutKind, center: string | null, vw: number, vh: number): Promise<Placed> {
  if (kind === "radial") return radial(view, sizesFor(view, base, "radial"), center);
  if (kind !== "auto") return layered(elk, view, sizesFor(view, base, kind), kind);
  const [r, d] = await Promise.all([layered(elk, view, sizesFor(view, base, "right"), "right"), layered(elk, view, sizesFor(view, base, "down"), "down")]);
  return fitScale(d, vw, vh) > fitScale(r, vw, vh) * 1.15 ? d : r;
}
