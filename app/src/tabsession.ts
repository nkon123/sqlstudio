// 편집 탭 자동 저장·복원 — 프로그램이 죽거나 PC 가 꺼져도 쓰던 SQL 이 남게.
//
// 몇 초마다 바뀐 탭이 있으면 데이터 폴더의 tabs.json 에 파일 경로·커서를, tabs/<key>.sql 에 탭 내용을 쓰고 다음에 켤 때 되살린다.
// 내용은 바뀐 탭만 다시 쓴다 (큰 소스를 여러 개 열어 둬도 가볍게). 예전 형식(v1, 내용이 tabs.json 안에)도 읽는다.
// 저장하지 않은 탭은 쓰던 그대로(● 표시), 파일로 저장한 탭은 디스크에서 다시 읽는다 (밖에서 고쳤을 수 있다).
// 접속은 되살리지 않는다 (운영 DB 일 수 있다) — 마지막 프로필만 골라 둔다.

import { invoke } from "@tauri-apps/api/core";

export interface SavedTab {
  text: string;
  filePath: string | null;
  encoding: string;
  eol: "\n" | "\r\n";
  dirty: boolean;
  cursor: number;
  profile: string | null;
  /** 마지막으로 고친 때 (왼쪽 목록의 날짜 묶음) — 예전 파일에는 없다 */
  used?: number;
  tag?: string | null;
  /** 파일 없는 탭의 이름 (DB 에서 연 소스는 객체 이름) */
  name?: string | null;
  /** 기준 스키마 (profile 로 다시 접속할 때) */
  schema?: string | null;
  /** 내용 파일 이름 (tabs/<key>.sql) — 예전 형식에는 없다 */
  key?: string;
}

/** 저장 형식 — v2 는 내용 대신 key (tabs/<key>.sql) */
type IndexTab = Omit<SavedTab, "text"> & { text?: string };
interface SavedSession { v: 1 | 2; at: number; active: number; tabs: IndexTab[] }

const KEY = "session.restore";

/** 시작할 때 지난 탭을 되살릴지 (기본 켬) */
export function restoreEnabled(): boolean {
  try { return localStorage.getItem(KEY) !== "0"; } catch { return true; }
}

export function setRestoreEnabled(on: boolean) {
  try { localStorage.setItem(KEY, on ? "1" : "0"); } catch { /* 이번 실행에서만 */ }
}

let ready = false;

/** 설정을 읽은 뒤 부른다 — 저장 위치(설정 파일 옆)는 백엔드가 안다. 설정 경로가 없으면(가짜 모드 등) 저장하지 않는다 */
export function initSession(configPath: string) {
  ready = !!configPath;
}

export async function loadSession(): Promise<{ active: number; tabs: SavedTab[] } | null> {
  if (!ready || !restoreEnabled()) return null;
  try {
    const r = await invoke<{ index: string; texts: Record<string, string> } | null>("load_tab_session");
    if (!r) return null;
    const s = JSON.parse(r.index) as SavedSession;
    if (!Array.isArray(s?.tabs) || !s.tabs.length) return null;
    // 내용 파일이 없어진 탭은 빈 문서로 두지 않고 뺀다
    const tabs = s.tabs.flatMap((t): SavedTab[] => {
      const text = t.key != null ? r.texts[t.key] : t.text;
      return text == null ? [] : [{ ...t, text }];
    });
    if (!tabs.length) return null;
    return { active: Math.min(Math.max(0, s.active | 0), tabs.length - 1), tabs };
  } catch {
    return null; // 없거나 깨졌으면 새로 시작
  }
}

/** tabs 는 내용 없이 key 로, changed 는 지난번 뒤로 바뀐 탭의 key → 내용 */
export async function saveSession(active: number, tabs: Omit<SavedTab, "text">[], changed: Map<string, string>) {
  if (!ready) return;
  const body: SavedSession = { v: 2, at: Date.now(), active, tabs };
  await invoke("save_tab_session", { index: JSON.stringify(body), texts: Object.fromEntries(changed) });
}
