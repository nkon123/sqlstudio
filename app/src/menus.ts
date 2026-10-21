// 상단 바 메뉴(접속·실행·파일·도구·자동 포맷)와 명령 검색 (main.ts 에서 나눔)

import { autoFormat, setAutoFormat, type AutoFormat } from "./format";
import { type MenuItem } from "./ctxmenu";
import { showShortcuts } from "./shortcuts";
import { openPalette, type Command } from "./palette";
import { toast } from "./ui";
import type { App } from "./main";

/** 접속 메뉴 — 프로필을 고르면 접속 (이미 접속한 탭에서 다른 프로필이면 새 탭), 다시 접속, 끊기, 접속 관리 */
export function connMenu(app: App): MenuItem[] {
  const c = app.active?.conn;
  const pick = (name: string) => { app.profileSel.value = name; void app.connectActive(); };
  return [
    { title: c ? "다른 접속은 새 탭에서 엽니다" : "접속할 프로필" },
    ...(app.profiles.length ? app.profiles.map((p): MenuItem => ({
      label: `${c?.profile === p.name ? "● " : ""}${p.name}`, hint: p.read_only ? "읽기 전용" : "",
      disabled: app.btn.connect.disabled, run: () => pick(p.name),
    })) : [{ label: "프로필 없음 — 설정에서 추가", disabled: true } as MenuItem]),
    { sep: true },
    { label: "다시 접속", disabled: !c || app.btn.connect.disabled, run: () => { if (c) pick(c.profile); } },
    { label: "끊기", disabled: app.btn.disconnect.disabled, run: () => app.disconnectActive() },
    { sep: true },
    { label: "접속 관리…", run: () => void app.settings() },
  ];
}

export function runMenu(app: App): MenuItem[] {
  const off = (k: string) => app.btn[k].disabled;
  return [
    { label: "문장 실행", hint: "Ctrl+Enter", disabled: off("run"), run: () => app.runStatement() },
    { label: "스크립트 전체 실행", hint: "F5", disabled: off("script"), run: () => app.runScript() },
    { label: "실행계획", hint: "Ctrl+E", disabled: off("explain"), run: () => app.explain() },
    { sep: true },
    { label: "디버그", disabled: off("debug"), run: () => app.debugActive() },
  ];
}

export function fileMenu(app: App): MenuItem[] {
  const t = app.active;
  return [
    { label: "새 탭", run: () => app.newTab(true) },
    { label: "열기…", run: () => void app.openFile() },
    { sep: true },
    { label: "저장", hint: "Ctrl+S", run: () => void app.save() },
    { label: "다른 이름으로 저장…", hint: "Ctrl+Shift+S", run: () => void app.saveAs() },
    { label: `인코딩 바꿔 저장${t ? ` (지금: ${t.encoding})` : ""}`, disabled: !t, sub: app.encodingItems() },
    { sep: true },
    { label: "실행 이력", hint: "F8", run: () => app.showHistory() },
    { label: "SQL 포맷", hint: "Ctrl+Shift+F", run: () => void app.formatSql() },
    { label: `자동 포맷${Object.values(autoFormat()).some(Boolean) ? " (켜짐)" : ""}`, sub: autoFormatItems(app) },
  ];
}

/** 자동 포맷 켜기/끄기 — 파일 메뉴의 하위 메뉴와 명령 검색이 같이 쓴다 */
export function autoFormatItems(app: App): MenuItem[] {
  const a = autoFormat();
  const item = (k: keyof AutoFormat, label: string): MenuItem =>
    ({ label: `${a[k] ? "✓ " : "　 "}${label}`, run: () => { setAutoFormat({ [k]: !a[k] }); toast(`${label}: ${a[k] ? "끔" : "켬"}`, "info", 1500); } });
  return [
    item("caseOnType", "키워드 대소문자 바로 맞추기"),
    item("onSemicolon", "; 를 치면 그 문장 정리"),
    item("onPaste", "SQL 을 붙여 넣으면 정리"),
    item("onRun", "실행할 때 그 문장 정리"),
    { sep: true },
    { label: "포맷 스타일…", run: () => void app.settings("편집기") },
  ];
}

export function toolsMenu(app: App): MenuItem[] {
  const off = (k: string) => app.btn[k].disabled;
  return [
    { title: "코드" },
    { label: "PL/SQL 분석", disabled: off("analyze"), run: () => app.analyzeActive() },
    { label: "데이터 흐름도", run: () => app.openDfd() },
    { label: "PL/SQL 디버그", disabled: off("debug"), run: () => app.debugActive() },
    { title: "DB 사이" },
    { label: "스키마 비교", disabled: off("compare"), run: () => app.compareActive() },
    { label: "데이터 복사", disabled: off("copy"), run: () => app.copyDataFrom(app.active) },
    { title: "운영" },
    { label: "용량 (테이블·테이블스페이스)", disabled: off("space"), run: () => app.spaceActive() },
    { label: "세션·락 모니터", disabled: off("monitor"), run: () => app.monitorActive() },
  ];
}

/** 명령 검색 (Ctrl+Shift+P) — 메뉴와 같은 명령, 접속 프로필도 */
export function palette(app: App) {
  const off = (k: string) => app.btn[k].disabled;
  const c = app.active?.conn;
  const connectTo = (name: string) => { app.profileSel.value = name; void app.connectActive(); };
  const cmds: Command[] = [
    ...(c ? [{ group: "접속", label: `${c.profile} 다시 접속`, alias: "reconnect", disabled: off("connect"), run: () => connectTo(c.profile) }] : []),
    ...app.profiles.map((p): Command => ({ group: "접속", label: `${p.name} 에 접속`, alias: "connect", disabled: off("connect"), run: () => connectTo(p.name) })),
    { group: "접속", label: "접속 끊기", alias: "disconnect", disabled: off("disconnect"), run: () => app.disconnectActive() },
    { group: "접속", label: "접속 관리", alias: "profile 프로필 설정", run: () => void app.settings() },
    { group: "실행", label: "문장 실행", keys: "Ctrl+Enter", alias: "run", disabled: off("run"), run: () => app.runStatement() },
    { group: "실행", label: "스크립트 전체 실행", keys: "F5", alias: "script", disabled: off("script"), run: () => app.runScript() },
    { group: "실행", label: "실행계획", keys: "Ctrl+E", alias: "explain plan", disabled: off("explain"), run: () => app.explain() },
    { group: "실행", label: "실행 중지", keys: "Esc", alias: "cancel", disabled: off("cancel"), run: () => app.cancel() },
    { group: "트랜잭션", label: "커밋", alias: "commit", disabled: off("commit"), run: () => app.txn("commit") },
    { group: "트랜잭션", label: "롤백", alias: "rollback", disabled: off("rollback"), run: () => app.txn("rollback") },
    { group: "파일", label: "새 탭", alias: "new tab", run: () => app.newTab(true) },
    { group: "파일", label: "파일 열기", alias: "open", run: () => void app.openFile() },
    { group: "파일", label: "저장", keys: "Ctrl+S", alias: "save", run: () => void app.save() },
    { group: "파일", label: "다른 이름으로 저장", keys: "Ctrl+Shift+S", alias: "save as", run: () => void app.saveAs() },
    { group: "편집", label: "SQL 포맷", keys: "Ctrl+Shift+F", alias: "format 정리", run: () => void app.formatSql() },
    ...autoFormatItems(app).flatMap((it): Command[] => ("run" in it && it.run && !it.label.endsWith("…")
      ? [{ group: "자동 포맷", label: `${it.label.slice(2)} ${it.label.startsWith("✓") ? "끄기" : "켜기"}`, alias: "auto format 자동 정리", run: it.run }] : [])),
    { group: "편집", label: "포맷 스타일 설정", alias: "format style 정렬 쉼표", run: () => void app.settings("편집기") },
    { group: "편집", label: "실행 이력", keys: "F8", alias: "history", run: () => app.showHistory() },
    { group: "도구", label: "PL/SQL 분석", alias: "analyze", disabled: off("analyze"), run: () => app.analyzeActive() },
    { group: "도구", label: "데이터 흐름도", alias: "dfd flow", run: () => app.openDfd() },
    { group: "도구", label: "PL/SQL 디버그", alias: "debug", disabled: off("debug"), run: () => app.debugActive() },
    { group: "도구", label: "스키마 비교", alias: "compare diff", disabled: off("compare"), run: () => app.compareActive() },
    { group: "도구", label: "데이터 복사", alias: "copy", disabled: off("copy"), run: () => app.copyDataFrom(app.active) },
    { group: "도구", label: "용량", alias: "space tablespace 테이블스페이스", disabled: off("space"), run: () => app.spaceActive() },
    { group: "도구", label: "세션·락 모니터", alias: "session lock monitor kill", disabled: off("monitor"), run: () => app.monitorActive() },
    { group: "보기", label: "왼쪽 영역 접기/펼치기", keys: "Ctrl+Shift+1", alias: "테이블 프로젝트", run: () => app.layout.toggle("left") },
    { group: "보기", label: "AI 영역 접기/펼치기", keys: "Ctrl+Shift+3", run: () => app.layout.toggle("right") },
    { group: "보기", label: "AI 입력칸으로", keys: "Ctrl+K", run: () => { app.layout.toggle("right", true); app.ai.focus(); } },
    { group: "도움말", label: "단축키 목록", keys: "F1", alias: "shortcut", run: () => showShortcuts() },
    { group: "설정", label: "설정 열기", alias: "settings", run: () => void app.settings() },
  ];
  openPalette(cmds);
}

// ── 설정 ─────────────────────────────────────────
