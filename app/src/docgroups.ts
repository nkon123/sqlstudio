// 왼쪽 '문서' 묶기 — 날짜(마지막으로 고친 날) · 접속 · 태그. 고른 묶기는 이 PC 에 기억한다.

export type DocGroup = "none" | "date" | "conn" | "tag";

export const GROUP_LABEL: Record<DocGroup, string> = { none: "없음", date: "날짜", conn: "접속", tag: "태그" };

export function docGroupPref(): DocGroup {
  try {
    const v = localStorage.getItem("docs.group");
    if (v && v in GROUP_LABEL) return v as DocGroup;
  } catch { /* 기본 */ }
  return "none";
}

export function setDocGroupPref(g: DocGroup) {
  try { localStorage.setItem("docs.group", g); } catch { /* 이번만 */ }
}

const DATE_ORDER = ["오늘", "어제", "지난 7일", "지난 30일", "이전"];
const NO_CONN = "접속 안 함";
const NO_TAG = "태그 없음";

/** 마지막으로 고친 때 → 오늘 · 어제 · 지난 7일 · 지난 30일 · 이전 (달력 날짜로) */
export function dateBucket(used: number, now = Date.now()): string {
  const day = (t: number) => { const d = new Date(t); return new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime(); };
  const days = Math.round((day(now) - day(used)) / 86_400_000);
  if (days <= 0) return "오늘";
  if (days === 1) return "어제";
  if (days < 7) return "지난 7일";
  if (days < 30) return "지난 30일";
  return "이전";
}

export interface Groupable { used: number; tag: string | null; conn: { profile: string } | null; lastProfile: string | null }

export function docGroupKey(t: Groupable, mode: DocGroup): string {
  if (mode === "date") return dateBucket(t.used);
  // 끊긴 문서도 마지막 접속의 묶음에 (끊김은 문서 줄의 접속 이름에 보인다)
  if (mode === "conn") return t.conn?.profile ?? t.lastProfile ?? NO_CONN;
  if (mode === "tag") return t.tag ?? NO_TAG;
  return "";
}

/** 묶음 순서 — 날짜는 가까운 것부터, 접속·태그는 이름순이고 '없음' 은 맨 뒤 */
export function sortGroupKeys(keys: string[], mode: DocGroup): string[] {
  if (mode === "date") return DATE_ORDER.filter((k) => keys.includes(k));
  const last = mode === "conn" ? NO_CONN : NO_TAG;
  return keys.filter((k) => k !== last).sort((a, b) => a.localeCompare(b)).concat(keys.includes(last) ? [last] : []);
}
