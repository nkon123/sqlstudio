// 접속 그룹 — 그룹 경로(ERP · ERP/운영, 두 단계)로 접속 목록을 나무로 묶는다.
// 설정의 접속 목록, 툴바 접속 선택, 접속 메뉴, 명령 검색이 같이 쓴다.

import type { Group, Profile } from "./api";

export const SEP = "/";

/** 그룹 경로의 마지막 이름 (ERP/운영 → 운영) */
export const groupLeaf = (path: string) => path.slice(path.lastIndexOf(SEP) + 1);
/** 상위 그룹 경로 (ERP/운영 → ERP). 맨 위면 null */
export const groupParent = (path: string) => (path.includes(SEP) ? path.slice(0, path.lastIndexOf(SEP)) : null);
/** 화면에 보이는 그룹 이름 (ERP / 운영) */
export const groupLabel = (path: string) => path.split(SEP).join(" / ");

const same = (a: string | null | undefined, b: string | null | undefined) => (a ?? "").toUpperCase() === (b ?? "").toUpperCase();

export interface GroupNode {
  group: Group;
  children: GroupNode[];
  profiles: Profile[];
}

/** 그룹 나무와 그룹 없는 접속. 순서는 목록 순서 그대로 */
export function groupTree(groups: Group[], profiles: Profile[]): { roots: GroupNode[]; loose: Profile[] } {
  const nodes = new Map<string, GroupNode>();
  for (const g of groups) nodes.set(g.path.toUpperCase(), { group: g, children: [], profiles: [] });
  const roots: GroupNode[] = [];
  for (const g of groups) {
    const n = nodes.get(g.path.toUpperCase())!;
    const parent = groupParent(g.path);
    const up = parent ? nodes.get(parent.toUpperCase()) : undefined;
    (up ? up.children : roots).push(n);
  }
  const loose: Profile[] = [];
  for (const p of profiles) {
    const n = p.group ? nodes.get(p.group.toUpperCase()) : undefined;
    (n ? n.profiles : loose).push(p);
  }
  return { roots, loose };
}

/** 그룹과 하위 그룹의 접속 수 */
export const countIn = (n: GroupNode): number => n.profiles.length + n.children.reduce((s, c) => s + countIn(c), 0);

/** 접속의 표시 색 — 접속에 색이 없으면 그룹, 그 위 그룹 순으로 */
export function colorOf(p: Profile, groups: Group[]): string | null {
  if (p.color) return p.color;
  for (let path = p.group ?? null; path; path = groupParent(path)) {
    const g = groups.find((x) => same(x.path, path));
    if (g?.color) return g.color;
  }
  return null;
}

/** 그룹 이름을 붙인 접속 이름 — 명령 검색에서 그룹으로도 찾게 */
export const withGroup = (p: Profile) => (p.group ? `${groupLabel(p.group)} › ${p.name}` : p.name);

// ── 접힌 그룹 (이 PC 화면 설정) ───────────────────

const KEY = "connGroupsCollapsed";

function collapsedSet(): Set<string> {
  try { return new Set(JSON.parse(localStorage.getItem(KEY) ?? "[]") as string[]); } catch { return new Set(); }
}

export const isCollapsed = (path: string) => collapsedSet().has(path.toUpperCase());

export function setCollapsed(path: string, on: boolean) {
  const s = collapsedSet();
  if (on) s.add(path.toUpperCase()); else s.delete(path.toUpperCase());
  try { localStorage.setItem(KEY, JSON.stringify([...s])); } catch { /* 저장 못 해도 이번 화면에서는 접힌다 */ }
}

/** <select> 안의 항목 — 그룹마다 <optgroup> (하위 그룹은 'ERP / 운영'), 그룹 없는 접속은 맨 위 */
export function profileOptions(groups: Group[], profiles: Profile[], option: (p: Profile) => HTMLOptionElement): HTMLElement[] {
  const { roots, loose } = groupTree(groups, profiles);
  const out: HTMLElement[] = loose.map(option);
  const walk = (n: GroupNode) => {
    if (n.profiles.length) {
      const og = document.createElement("optgroup");
      og.label = groupLabel(n.group.path);
      og.append(...n.profiles.map(option));
      out.push(og);
    }
    n.children.forEach(walk);
  };
  roots.forEach(walk);
  return out;
}
