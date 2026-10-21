// 편집 탭 자동 저장·복원 — 프로그램이 죽거나 PC 가 꺼져도 쓰던 SQL 이 남게.
//
// 몇 초마다 바뀐 탭이 있으면 데이터 폴더의 tabs.json 에 탭 내용·파일 경로·커서를 쓰고, 다음에 켤 때 되살린다.
// 저장하지 않은 탭은 쓰던 그대로(● 표시), 파일로 저장한 탭은 디스크에서 다시 읽는다 (밖에서 고쳤을 수 있다).
// 접속은 되살리지 않는다 (운영 DB 일 수 있다) — 마지막 프로필만 골라 둔다.

import { api } from "./api";

export interface SavedTab {
  text: string;
  filePath: string | null;
  encoding: string;
  eol: "\n" | "\r\n";
  dirty: boolean;
  cursor: number;
  profile: string | null;
}

interface SavedSession { v: 1; at: number; active: number; tabs: SavedTab[] }

const KEY = "session.restore";

/** 시작할 때 지난 탭을 되살릴지 (기본 켬) */
export function restoreEnabled(): boolean {
  try { return localStorage.getItem(KEY) !== "0"; } catch { return true; }
}

export function setRestoreEnabled(on: boolean) {
  try { localStorage.setItem(KEY, on ? "1" : "0"); } catch { /* 이번 실행에서만 */ }
}

let file: string | null = null;

/** 설정 파일 경로로 저장 위치를 정한다 (데이터 폴더의 tabs.json) */
export function initSession(configPath: string) {
  file = configPath.replace(/[\\/][^\\/]*$/, "") + "/tabs.json";
}

export async function loadSession(): Promise<{ active: number; tabs: SavedTab[] } | null> {
  if (!file || !restoreEnabled()) return null;
  try {
    const s = JSON.parse((await api.readFile(file)).text) as SavedSession;
    if (s?.v !== 1 || !Array.isArray(s.tabs) || !s.tabs.length) return null;
    return { active: Math.min(Math.max(0, s.active | 0), s.tabs.length - 1), tabs: s.tabs };
  } catch {
    return null; // 없거나 깨졌으면 새로 시작
  }
}

export async function saveSession(active: number, tabs: SavedTab[]) {
  if (!file) return;
  const body: SavedSession = { v: 1, at: Date.now(), active, tabs };
  await api.writeFile(file, JSON.stringify(body), "utf-8");
}
