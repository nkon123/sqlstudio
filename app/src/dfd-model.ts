// 데이터 흐름도(DFD) 모델 — 통합 분석 결과(integrated.json)에서 그릴 노드·선을 만든다. 화면(DOM)과 무관.
//
// 1) 바탕 그래프: 프로시저·트리거·테이블과 읽기/쓰기/호출/트리거 발동/커서 흐름 선 (분석 결과 그대로)
// 2) 보기 모델: 가운데·깊이로 자르고, 보고서형으로 단순화한다
//    - 요소별 칸: 입력 테이블 | 처리 | 출력 테이블 — 모든 선이 왼쪽 → 오른쪽으로만 흐른다
//    - 읽히고 쓰이기도 하는 테이블은 입력 칸에 "읽기용 사본"을 하나 더 둔다 → 되돌아가는 선이 없다
//    - 같은 두 노드 사이 같은 종류 선은 하나로 합친다 (라벨 C,U)
//    - 처리간 호출·테이블 흐름선은 읽기·쓰기 선과 같은 내용이라 기본으로 숨긴다
//    - XML 쿼리 파일(화면이 부르는 SQL)은 처리(PL/SQL·Pro*C)와 따로 '화면 (XML)' 칸에 — 파일 상자 안에 쿼리 id 들,
//      또는 파일 하나를 상자 하나로. 칸 순서: 입력 테이블 | 화면 (XML) | 처리 | 출력 테이블 (화면 → 프로시저 호출도 왼쪽 → 오른쪽)

export interface DfdSourceNode { id: string; unit?: string; kind: string; summary?: string | null; signature?: string; trigger_on?: string | null; trigger_events?: string | null; commits?: boolean }
export interface DfdSourceEdge { from: string; to: string; resolved: boolean; lines: number[] }
/** conf: 노드 → ok (사전·소스) / src (소스만) / dict (사전만, 연산 모름) / llm (모델 추정) */
export interface DfdSourceTable { table: string; by: Record<string, string>; conf?: Record<string, string>; fed_from?: string[]; feeds_into?: string[] }
export interface DfdSource { nodes: DfdSourceNode[]; edges: DfdSourceEdge[]; tables: DfdSourceTable[] }

/** xml: XML 쿼리 파일의 쿼리 id (파일 단위면 파일) · xmlfile: 쿼리 id 들을 담는 파일 상자 (보기에만) */
export type NodeType = "proc" | "trig" | "table" | "xml" | "xmlfile";
/** XML 을 파일·id 로 (파일 상자 안에 id 들) 볼지, 파일 하나를 상자 하나로 볼지 */
export type XmlMode = "id" | "file";
/** use: 사전에는 쓴다고 나오는데 연산을 모름 — 입력 쪽으로 그린다 */
export type EdgeKind = "read" | "write" | "call" | "trig" | "flow" | "use";

/** file: XML 쿼리 id 의 파일 ("OWNER.FILE") */
export interface BaseNode { id: string; type: NodeType; label: string; summary?: string; commits?: boolean; file?: string }
/** guess: 소스에서 직접 확인하지 못한 것 (사전·모델 추정) */
export interface BaseEdge { s: string; t: string; kind: EdgeKind; label: string; lines?: number[]; guess?: boolean }
export interface Base { nodes: Map<string, BaseNode>; edges: BaseEdge[]; owner: string }

export interface ViewOptions {
  /** null 이면 전체 */
  center: string | null;
  /** 0 이면 끝까지 */
  depth: number;
  showCalls: boolean;
  showFlows: boolean;
  hidden: Set<string>;
  /**
   * both: 가운데 주변 (방향 무시)
   * up:   출력 기준 — 가운데로 데이터가 어디서 오나 (쓰는 처리 ← 그 처리가 읽는 테이블 ← …)
   * down: 입력 기준 — 가운데의 데이터가 어디로 가나 (읽는 처리 → 그 처리가 쓰는 테이블 → …)
   * up/down 의 depth 는 "테이블 단계" (처리 하나 + 테이블 하나 = 1단계)
   */
  direction?: "both" | "up" | "down";
}
/** parent: 담는 파일 상자의 id (XML 쿼리 id) */
export interface ViewNode { id: string; ref: string; type: NodeType; label: string; lane: number; copy: boolean; center: boolean; more: boolean; parent?: string }
export interface ViewEdge { id: string; source: string; target: string; kind: EdgeKind; label: string; guess?: boolean }
export interface View { nodes: ViewNode[]; edges: ViewEdge[] }

/** 칸 이름 — 번호는 저장한 보고서에 남으므로 바꾸지 않는다. 화면의 왼쪽→오른쪽 순서는 LANE_POS */
export const LANES = ["입력 테이블", "처리", "출력 테이블", "화면 (XML)"];
export const LANE_POS = [0, 2, 3, 1];
export const XML_LANE = 3;
/** 파일 상자의 보기 id */
export const fileBoxId = (file: string) => `xf:${file}`;

/** 가장 많이 나오는 스키마 — 라벨에서 뗀다 */
function mainOwner(ids: string[]): string {
  const n = new Map<string, number>();
  for (const id of ids) { const o = id.split(".")[0]; n.set(o, (n.get(o) ?? 0) + 1); }
  return [...n].sort((a, b) => b[1] - a[1])[0]?.[0] ?? "";
}

function labelOf(id: string, owner: string): string {
  const parts = id.split(".");
  const rest = parts[0] === owner && parts.length > 1 ? parts.slice(1) : parts;
  // 패키지.프로시저 → 두 줄
  return rest.length >= 2 ? `${rest.slice(0, -1).join(".")}\n${rest[rest.length - 1]}` : rest.join(".");
}

/** "OWNER.PKG.PROC"(·중첩) → "OWNER.PKG". 단독 프로시저 "OWNER.PROC" 는 그대로 */
export function packageOf(id: string): string {
  const p = id.split(".");
  return p.length >= 3 ? p.slice(0, 2).join(".") : id;
}

/** 분석 결과에 XML 쿼리 파일이 있는지 */
export const hasXml = (g: DfdSource) => g.nodes.some((n) => n.unit?.endsWith(".XML_BODY"));

/** byPackage: 처리를 패키지 하나로 묶는다 (프로시저가 많을 때 큰 그림). xml: XML 쿼리를 id 하나씩(파일 상자 안에) / 파일 하나로 */
export function buildBase(g: DfdSource, byPackage = false, xml: XmlMode = "id"): Base {
  const owner = mainOwner([...g.nodes.map((n) => n.id), ...g.tables.map((t) => t.table)]);
  const trig = new Set(g.nodes.filter((n) => n.kind === "TRIGGER").map((n) => n.id));
  // XML 파일 ("OWNER.FILE") — 노드 목록에 없는 것((전역) 등)도 파일로 알아본다
  const xmlFiles = new Set(g.nodes.filter((n) => n.unit?.endsWith(".XML_BODY")).map((n) => packageOf(n.id)));
  const isXml = (id: string) => xmlFiles.has(packageOf(id)) && id.split(".").length >= 3;
  const key = (id: string) => (isXml(id) ? (xml === "file" ? packageOf(id) : id) : byPackage && !trig.has(id) ? packageOf(id) : id);
  const nodes = new Map<string, BaseNode>();
  const add = (raw: string, type: NodeType, extra: Partial<BaseNode> = {}) => {
    const x = type !== "table" && isXml(raw);
    if (x) type = "xml";
    const id = type === "table" ? raw : key(raw);
    const grouped = id !== raw;
    const cur = nodes.get(id);
    // 파일 상자 안의 쿼리 id 는 id 만 (파일 이름은 상자에)
    const label = x && xml === "id" ? id.split(".").slice(2).join(".") : labelOf(id, owner);
    if (!cur) nodes.set(id, { id, type, label, ...(x && xml === "id" ? { file: packageOf(id) } : {}), ...(grouped ? { commits: extra.commits } : extra) });
    else if (grouped && extra.commits) cur.commits = true;
    return id;
  };
  for (const n of g.nodes) add(n.id, n.kind === "TRIGGER" ? "trig" : "proc", { summary: n.summary ?? undefined, commits: n.commits });
  const edges: BaseEdge[] = [];
  for (const t of g.tables) {
    add(t.table, "table");
    for (const [raw, ops] of Object.entries(t.by)) {
      const n = add(raw, "proc"); // (전역)·(초기화) 처럼 노드 목록에 없는 것
      const c = t.conf?.[raw];
      const guess = c === "llm" || c === "dict" ? true : undefined;
      if (ops.includes("R")) edges.push({ s: t.table, t: n, kind: "read", label: "R", guess });
      const w = ops.replace(/[^CUD]/g, "");
      if (w) edges.push({ s: n, t: t.table, kind: "write", label: w.split("").join(","), guess });
      if (!/[CRUD]/.test(ops)) edges.push({ s: t.table, t: n, kind: "use", label: "?", guess: true });
    }
  }
  for (const e of g.edges) {
    if (!e.resolved) continue;
    const s = key(e.from), t = key(e.to);
    if (s === t || !nodes.has(s) || !nodes.has(t)) continue;
    edges.push({ s, t, kind: "call", label: "", lines: e.lines });
  }
  for (const n of g.nodes) {
    if (!n.trigger_on) continue;
    add(n.trigger_on, "table");
    edges.push({ s: n.trigger_on, t: n.id, kind: "trig", label: n.trigger_events ?? "" });
  }
  const flows = new Set<string>();
  for (const t of g.tables) {
    for (const src of t.fed_from ?? []) {
      const k = `${src}>${t.table}`;
      if (flows.has(k) || src === t.table) continue;
      flows.add(k);
      add(src, "table");
      edges.push({ s: src, t: t.table, kind: "flow", label: "" });
    }
  }
  // 묶으면 같은 선이 여러 번 생긴다 — 합친다
  const merged = new Map<string, BaseEdge>();
  for (const e of edges) {
    const k = `${e.s}|${e.t}|${e.kind}`;
    const m = merged.get(k);
    if (!m) { merged.set(k, { ...e }); continue; }
    if (!e.guess) m.guess = undefined; // 하나라도 소스에서 확인했으면 확인된 선
    const labels = new Set([...m.label.split(","), ...e.label.split(",")].filter(Boolean));
    m.label = ["C", "R", "U", "D"].filter((x) => labels.has(x)).concat([...labels].filter((x) => !"CRUD".includes(x))).join(",");
    if (e.lines) m.lines = [...new Set([...(m.lines ?? []), ...e.lines])].sort((a, b) => a - b);
  }
  return { nodes, edges: [...merged.values()], owner };
}

/** 선이 가장 많이 붙는 노드 — 처음 열 때 가운데 후보 */
export function busiest(b: Base): string | null {
  const deg = new Map<string, number>();
  for (const e of b.edges) if (e.kind !== "flow") for (const x of [e.s, e.t]) deg.set(x, (deg.get(x) ?? 0) + 1);
  return [...deg].sort((a, b) => b[1] - a[1])[0]?.[0] ?? null;
}

export function buildView(b: Base, o: ViewOptions): View {
  const typeOf = (id: string) => b.nodes.get(id)?.type;
  let edges = b.edges.filter((e) => {
    if (o.hidden.has(e.s) || o.hidden.has(e.t)) return false;
    if (e.kind === "flow") return o.showFlows;
    if (e.kind === "call") return o.showCalls;
    return true;
  });
  const dir = o.direction ?? "both";
  // 방향을 따라 지나온 선만 남긴다 — 그 길에 없는 선(처리가 쓰는 다른 테이블 등)은 보이지 않게
  let traversed: Set<BaseEdge> | null = null;
  let keep: Map<string, number>;
  if (o.center && b.nodes.has(o.center) && dir !== "both") {
    keep = new Map([[o.center, 0]]);
    traversed = new Set();
    const hops = o.depth ? o.depth * 2 : Infinity;
    const q = [o.center];
    while (q.length) {
      const x = q.shift()!;
      const d = keep.get(x)!;
      if (d >= hops) continue;
      for (const e of edges) {
        const next = dir === "up" ? (e.t === x ? e.s : null) : (e.s === x ? e.t : null);
        if (!next) continue;
        traversed.add(e);
        if (!keep.has(next)) { keep.set(next, d + 1); q.push(next); }
      }
    }
  } else if (o.center && b.nodes.has(o.center)) {
    // 가운데에서 depth 단계 (방향 무시)
    keep = new Map([[o.center, 0]]);
    const adj = new Map<string, string[]>();
    for (const e of edges) {
      (adj.get(e.s) ?? adj.set(e.s, []).get(e.s)!).push(e.t);
      (adj.get(e.t) ?? adj.set(e.t, []).get(e.t)!).push(e.s);
    }
    const q = [o.center];
    while (q.length) {
      const x = q.shift()!;
      const d = keep.get(x)!;
      if (o.depth && d >= o.depth) continue;
      for (const y of adj.get(x) ?? []) if (!keep.has(y)) { keep.set(y, d + 1); q.push(y); }
    }
  } else {
    keep = new Map();
    for (const e of edges) { keep.set(e.s, 1); keep.set(e.t, 1); }
  }
  // 화면 밖으로 이어지는 선이 있으면 "더 있음" 표시 (방향 모드면 그 방향으로 더 있는 것만)
  const more = new Set<string>();
  for (const e of b.edges) {
    if (o.hidden.has(e.s) || o.hidden.has(e.t)) continue;
    if (dir === "up" && keep.has(e.t) && !keep.has(e.s) && e.kind !== "call") more.add(e.t);
    else if (dir === "down" && keep.has(e.s) && !keep.has(e.t) && e.kind !== "call") more.add(e.s);
    else if (dir === "both" && keep.has(e.s) !== keep.has(e.t)) more.add(keep.has(e.s) ? e.s : e.t);
  }
  edges = traversed ? edges.filter((e) => traversed!.has(e)) : edges.filter((e) => keep.has(e.s) && keep.has(e.t));

  // 쓰이는 테이블 → 출력 칸. 그 테이블에서 나가는 선은 입력 칸의 사본에서 나가게 한다
  const written = new Set(edges.filter((e) => typeOf(e.t) === "table" && typeOf(e.s) !== "table").map((e) => e.t));
  for (const e of edges) if (e.kind === "flow" && !written.has(e.t)) written.add(e.t);
  const copies = new Set<string>();
  const out: ViewEdge[] = [];
  const merged = new Map<string, ViewEdge>();
  for (const e of edges) {
    let s = e.s;
    if (written.has(s)) { s = `${s}@in`; copies.add(e.s); }
    const k = `${s}|${e.t}|${e.kind}`;
    const m = merged.get(k);
    if (m) {
      for (const l of e.label.split(",")) if (l && !m.label.split(",").includes(l)) m.label = m.label ? `${m.label},${l}` : l;
      if (!e.guess) m.guess = undefined;
      continue;
    }
    const v: ViewEdge = { id: `e${out.length}`, source: s, target: e.t, kind: e.kind, label: e.label, guess: e.guess };
    merged.set(k, v);
    out.push(v);
  }
  const nodes: ViewNode[] = [];
  const boxes = new Map<string, ViewNode>();
  const mk = (id: string, ref: string, copy: boolean) => {
    const n = b.nodes.get(ref)!;
    const lane = n.type === "table" ? (copy || !written.has(ref) ? 0 : 2) : n.type === "xml" ? XML_LANE : 1;
    const parent = n.file ? fileBoxId(n.file) : undefined;
    if (n.file && !boxes.has(n.file)) {
      const box: ViewNode = { id: parent!, ref: n.file, type: "xmlfile", label: labelOf(n.file, b.owner), lane: XML_LANE, copy: false, center: false, more: false };
      boxes.set(n.file, box);
      nodes.push(box);
    }
    nodes.push({ id, ref, type: n.type, label: n.label, lane, copy, center: ref === o.center, more: more.has(ref), ...(parent ? { parent } : {}) });
  };
  for (const id of keep.keys()) {
    if (!b.nodes.has(id)) continue;
    mk(id, id, false);
    if (copies.has(id)) mk(`${id}@in`, id, true);
  }
  return { nodes, edges: out };
}

/** 노드 하나에 붙은 바탕 선 (상세 패널용) */
export function edgesOf(b: Base, id: string) {
  return { ins: b.edges.filter((e) => e.t === id), outs: b.edges.filter((e) => e.s === id) };
}

export interface LaneSpan { i: number; x1: number; x2: number }

/** 칸 경계 — 칸마다 노드가 차지한 가로 범위를 받아, 빈틈·겹침 없이 맞붙은 칸으로.
 *  이웃 칸 사이 경계는 두 칸 노드 사이 한가운데, 바깥은 pad. 노드가 칸을 넘어가 있어도 칸은 최소 폭을 지킨다. */
export function fitLanes(spans: LaneSpan[], pad = 24, minW = 60): LaneSpan[] {
  const pos = (i: number) => LANE_POS[i] ?? i;
  const s = [...spans].sort((a, b) => pos(a.i) - pos(b.i));
  if (!s.length) return [];
  const b: number[] = [s[0].x1 - pad];
  for (let k = 1; k < s.length; k++) b.push((s[k - 1].x2 + s[k].x1) / 2);
  b.push(s[s.length - 1].x2 + pad);
  for (let k = 1; k < b.length; k++) b[k] = Math.max(b[k], b[k - 1] + minW);
  return s.map((x, k) => ({ i: x.i, x1: Math.round(b[k]), x2: Math.round(b[k + 1]) }));
}
