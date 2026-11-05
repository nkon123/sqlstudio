// 결과 비교의 계산 (화면 없이 — 테스트에서 부른다). 화면은 resultdiff.ts

export type Row = (string | null)[];

export interface Snapshot {
  label: string;
  columns: { name: string; type_name: string }[];
  rows: Row[];
  /** 서버에 행이 더 남아 있었는지 (가져온 행만 담았는지) */
  partial: boolean;
  /** 조회한 테이블의 기본 키 (알면) — 키 열 자동 선택에 */
  pk?: string[];
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
export function colIndex(cols: { name: string }[]): Map<string, number> {
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

/** 처음 키 열 — 기본 키가 두 결과에 다 있고 겹치지 않으면 그것, 아니면 guessKey */
export function initialKey(a: Snapshot, b: Snapshot): string[] {
  const ia = colIndex(a.columns), ib = colIndex(b.columns);
  const pk = (b.pk?.length ? b.pk : a.pk ?? []).map((k) => k.toUpperCase());
  if (pk.length && pk.every((k) => ia.has(k) && ib.has(k))) {
    const unique = (s: Snapshot, idx: number[]) => {
      const seen = new Set<string>();
      for (const r of s.rows) {
        const k = pick(r, idx);
        if (seen.has(k)) return false;
        seen.add(k);
      }
      return true;
    };
    if (unique(a, pk.map((k) => ia.get(k)!)) && unique(b, pk.map((k) => ib.get(k)!))) return pk;
  }
  return guessKey(a, b);
}

/** 자동 비교 (결과 위의 표시) — 같은 모양의 결과일 때만 (공통 열이 절반 이상, 행이 너무 많지 않게). 아니면 null */
export function quickDiff(a: Snapshot, b: Snapshot): Diff | null {
  const ib = colIndex(b.columns);
  const common = new Set(a.columns.map((c) => c.name.toUpperCase()).filter((n) => ib.has(n))).size;
  if (!common || common * 2 < Math.max(a.columns.length, b.columns.length)) return null;
  if (a.rows.length > 50_000 || b.rows.length > 50_000) return null;
  return diffResults(a, b, initialKey(a, b));
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

