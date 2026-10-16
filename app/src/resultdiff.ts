// 결과 비교 — 담아 둔 결과(기준)와 지금 결과를 행·열 단위로 맞춰 본다.
// 고치기 전·후 쿼리, 개발 DB·운영 DB 의 같은 쿼리 결과를 비교할 때.
// 비교를 누를 때만 메모리에서 계산한다 (DB 왕복 없음). 열은 이름으로 맞추고, 행은 키 열로 맞춘다 —
// 키가 없으면 행 전체를 값으로 맞춰 보고 추가·삭제만 가린다.

import type { Column } from "./api";
import { showMenu } from "./ctxmenu";
import { ResultGrid } from "./grid";
import { h, modal, toast } from "./ui";

type Row = (string | null)[];

export interface Snapshot {
  label: string;
  columns: { name: string; type_name: string }[];
  rows: Row[];
  /** 서버에 행이 더 남아 있었는지 (가져온 행만 담았는지) */
  partial: boolean;
}

export type Status = "chg" | "add" | "del" | "same";

export interface DiffRow {
  status: Status;
  a: Row | null;
  b: Row | null;
  /** 바뀐 열 (공통 열 번호) */
  changed: number[];
}

export interface Diff {
  /** 두 결과에 다 있는 열 이름 */
  cols: string[];
  onlyA: string[];
  onlyB: string[];
  key: string[];
  rows: DiffRow[];
  counts: Record<Status, number>;
  /** 키가 겹치는 행 수 (순서대로 짝지었다) */
  dupKeys: number;
}

const NUL = "\u0000";
const SEP = "\u0001";

function pick(row: Row, idx: number[]): string {
  let s = "";
  for (let i = 0; i < idx.length; i++) {
    const v = row[idx[i]];
    s += (i ? SEP : "") + (v == null ? NUL : v);
  }
  return s;
}

/** 이름으로 열 맞추기 (대소문자 무시, 앞쪽 것) */
function colIndex(cols: { name: string }[]): Map<string, number> {
  const m = new Map<string, number>();
  cols.forEach((c, i) => { const k = c.name.toUpperCase(); if (!m.has(k)) m.set(k, i); });
  return m;
}

/** 키로 쓸 만한 열 — 두 결과 모두에서 값이 비지 않고 겹치지 않는 첫 열(또는 앞의 두 열) */
export function guessKey(a: Snapshot, b: Snapshot): string[] {
  const ia = colIndex(a.columns), ib = colIndex(b.columns);
  const common = a.columns.map((c) => c.name.toUpperCase()).filter((n, i, xs) => ib.has(n) && xs.indexOf(n) === i);
  const unique = (s: Snapshot, idx: number[]) => {
    const seen = new Set<string>();
    for (const r of s.rows) {
      if (idx.some((i) => r[i] == null)) return false;
      const k = pick(r, idx);
      if (seen.has(k)) return false;
      seen.add(k);
    }
    return true;
  };
  const tries = [common.slice(0, 1), common.slice(0, 2), common.slice(0, 3)].filter((t, i) => t.length === i + 1);
  for (const t of tries) {
    if (unique(a, t.map((n) => ia.get(n)!)) && unique(b, t.map((n) => ib.get(n)!))) return t;
  }
  return [];
}

export function diffResults(a: Snapshot, b: Snapshot, key: string[]): Diff {
  const ia = colIndex(a.columns), ib = colIndex(b.columns);
  const cols = a.columns.map((c) => c.name.toUpperCase()).filter((n, i, xs) => ib.has(n) && xs.indexOf(n) === i);
  const onlyA = a.columns.map((c) => c.name).filter((n) => !ib.has(n.toUpperCase()));
  const onlyB = b.columns.map((c) => c.name).filter((n) => !ia.has(n.toUpperCase()));
  const ka = cols.map((n) => ia.get(n)!), kb = cols.map((n) => ib.get(n)!);
  key = key.map((k) => k.toUpperCase()).filter((k) => cols.includes(k));
  const rows: DiffRow[] = [];
  const counts: Record<Status, number> = { chg: 0, add: 0, del: 0, same: 0 };
  let dupKeys = 0;
  // 키가 없으면 공통 열 전부가 키 — 같은 행이 몇 번 나오는지로 맞춘다
  const keyA = key.length ? key.map((n) => ia.get(n)!) : ka;
  const keyB = key.length ? key.map((n) => ib.get(n)!) : kb;
  const byKey = new Map<string, number[]>();
  a.rows.forEach((r, i) => {
    const k = pick(r, keyA);
    const xs = byKey.get(k);
    if (xs) { xs.push(i); if (key.length) dupKeys++; } else byKey.set(k, [i]);
  });
  const used = new Uint8Array(a.rows.length);
  const seenB = new Map<string, number>();
  for (const r of b.rows) {
    const k = pick(r, keyB);
    const n = seenB.get(k) ?? 0;
    seenB.set(k, n + 1);
    if (n && key.length) dupKeys++;
    const ai = byKey.get(k)?.[n];
    if (ai == null) {
      rows.push({ status: "add", a: null, b: r, changed: [] });
      counts.add++;
      continue;
    }
    used[ai] = 1;
    const ar = a.rows[ai];
    const changed: number[] = [];
    if (key.length) for (let c = 0; c < cols.length; c++) if (ar[ka[c]] !== r[kb[c]]) changed.push(c);
    const status: Status = changed.length ? "chg" : "same";
    rows.push({ status, a: ar, b: r, changed });
    counts[status]++;
  }
  a.rows.forEach((r, i) => {
    if (!used[i]) { rows.push({ status: "del", a: r, b: null, changed: [] }); counts.del++; }
  });
  return { cols, onlyA, onlyB, key, rows, counts, dupKeys };
}

const LABEL: Record<Status, string> = { chg: "변경", add: "추가 (지금만)", del: "삭제 (기준만)", same: "같음" };

/** 비교 화면. a = 기준(담아 둔 것), b = 지금 결과 */
export function showDiff(a: Snapshot, b: Snapshot) {
  const body = h("div", { class: "usage rdiff" });
  void modal(`결과 비교 — ${a.label} ↔ ${b.label}`, body, [{ label: "닫기", value: true }], { wide: true });
  const ib = colIndex(b.columns);
  const common = a.columns.map((c) => c.name.toUpperCase()).filter((n, i, xs) => ib.has(n) && xs.indexOf(n) === i);
  if (!common.length) {
    body.replaceChildren(h("p", {}, "두 결과에 이름이 같은 열이 없어 비교할 수 없습니다."));
    return;
  }
  let key = guessKey(a, b);
  type Show = "diff" | Status | "all";
  let show = "diff" as Show;
  const meta = new Map<Row, DiffRow>();
  const grid: ResultGrid = new ResultGrid({
    rowClass: (r) => { const d = meta.get(r); return d ? `rdiff-${d.status}` : null; },
    cellClass: (r, c) => { const d = meta.get(r); return d && d.status === "chg" && c > 0 && d.changed.includes(c - 1) ? "rdiff-cell" : null; },
    onContextMenu: (_r, _c, e) => showMenu(e.clientX, e.clientY, [
      { label: "고른 셀 복사", hint: "Ctrl+C", run: () => navigator.clipboard.writeText(grid.selectionText()).catch(() => {}) },
      { label: "머리 포함 복사", hint: "Ctrl+Shift+C", run: () => navigator.clipboard.writeText(grid.selectionText(true)).catch(() => {}) },
    ]),
  });
  const summary = h("div", { class: "row" });
  const note = h("div", { class: "dim", style: "font-size:12px;margin:4px 0 6px" });
  const keyBox = h("div", { class: "rdiff-keys" });
  const filters = h("span", { class: "rdiff-filters" });
  let d: Diff;
  const render = () => {
    meta.clear();
    const columns: Column[] = [{ name: "비교", type_name: "VARCHAR2", nullable: true },
      ...d.cols.map((n) => ({ name: d.key.includes(n) ? `${n} 🔑` : n, type_name: "VARCHAR2", nullable: true }))];
    const ia = colIndex(a.columns);
    const idxA = d.cols.map((n) => ia.get(n)!), idxB = d.cols.map((n) => ib.get(n)!);
    const shown = (v: string | null) => v ?? "(null)";
    const rows: Row[] = [];
    for (const x of d.rows) {
      if (show === "diff" ? x.status === "same" : show !== "all" && x.status !== show) continue;
      const src = x.b ?? x.a!;
      const idx = x.b ? idxB : idxA;
      const r: Row = [LABEL[x.status], ...d.cols.map((_, c) =>
        x.status === "chg" && x.changed.includes(c) ? `${shown(x.a![idxA[c]])} → ${shown(x.b![idxB[c]])}` : src[idx[c]])];
      meta.set(r, x);
      rows.push(r);
    }
    grid.setPage({ columns, rows, has_more: false, fetched_total: rows.length });
  };
  const recompute = () => {
    const t0 = performance.now();
    d = diffResults(a, b, key);
    const ms = Math.round(performance.now() - t0);
    const c = d.counts;
    summary.replaceChildren(
      h("span", {}, `기준 ${a.rows.length.toLocaleString()}행 · 지금 ${b.rows.length.toLocaleString()}행 — `,
        h("b", {}, `변경 ${c.chg.toLocaleString()}`), ` · 추가 ${c.add.toLocaleString()} · 삭제 ${c.del.toLocaleString()} · 같음 ${c.same.toLocaleString()}`),
      h("span", { class: "spacer" }),
      h("button", { class: "small", onclick: () => copyDiff(d, a, b) }, "다른 것 복사"));
    const warn: string[] = [];
    if (!d.key.length) warn.push("키 열이 없어 행 전체를 값으로 맞췄습니다 — 바뀐 행은 '삭제 + 추가' 로 나옵니다. 키 열을 고르면 '변경' 으로 맞춰 봅니다.");
    if (d.dupKeys) warn.push(`키가 겹치는 행 ${d.dupKeys.toLocaleString()}개 — 나온 순서대로 짝지었습니다. 키 열을 더 고르세요.`);
    if (d.onlyA.length) warn.push(`기준에만 있는 열: ${d.onlyA.join(", ")}`);
    if (d.onlyB.length) warn.push(`지금에만 있는 열: ${d.onlyB.join(", ")}`);
    if (a.partial || b.partial) warn.push(`${a.partial ? "기준" : ""}${a.partial && b.partial ? "·" : ""}${b.partial ? "지금 결과" : ""}는 가져온 행까지만 비교했습니다.`);
    note.replaceChildren(`${ms}ms · 열은 이름으로 맞춥니다. 값은 화면에 보이는 글자 그대로 비교합니다 (거르기·정렬을 반영해 담김).`, ...warn.map((w) => h("div", { class: "warn-text" }, w)));
    render();
  };
  // 키 열 고르기
  keyBox.append(h("span", { class: "dim" }, "키 열: "), ...common.map((n) => {
    const cb = h("input", { type: "checkbox" }) as HTMLInputElement;
    cb.checked = key.includes(n);
    cb.onchange = () => { key = common.filter((x) => (x === n ? cb.checked : key.includes(x))); recompute(); };
    return h("label", { class: "rdiff-key" }, cb, n);
  }));
  const opts: [Show, string][] = [["diff", "다른 것만"], ["chg", "변경"], ["add", "추가"], ["del", "삭제"], ["all", "전체"]];
  filters.append(...opts.map(([v, label]) => {
    const b = h("button", { class: `small${v === show ? " on" : ""}` }, label);
    b.onclick = () => { show = v; filters.querySelectorAll("button").forEach((x) => x.classList.toggle("on", x === b)); render(); };
    return b;
  }));
  body.append(summary, note, h("div", { class: "row", style: "margin-bottom:6px;flex-wrap:wrap" }, filters, keyBox), h("div", { class: "usage-grid" }, grid.el));
  recompute();
}

/** 다른 행들을 TSV 로 (엑셀에 바로) — 변경은 기준·지금 두 줄 */
function copyDiff(d: Diff, a: Snapshot, b: Snapshot) {
  const ia = colIndex(a.columns), ib = colIndex(b.columns);
  const esc = (v: string | null) => (v == null ? "" : /[\t\n"]/.test(v) ? `"${v.replace(/"/g, '""')}"` : v);
  const line = (tag: string, r: Row, m: Map<string, number>) => [tag, ...d.cols.map((n) => esc(r[m.get(n)!]))].join("\t");
  const out = [["비교", ...d.cols].join("\t")];
  for (const x of d.rows) {
    if (x.status === "same") continue;
    if (x.status === "add") out.push(line("추가", x.b!, ib));
    else if (x.status === "del") out.push(line("삭제", x.a!, ia));
    else { out.push(line("변경 전", x.a!, ia)); out.push(line("변경 후", x.b!, ib)); }
  }
  navigator.clipboard.writeText(out.join("\n")).then(() => toast(`${(out.length - 1).toLocaleString()}줄 복사`, "ok", 1500)).catch(() => {});
}
