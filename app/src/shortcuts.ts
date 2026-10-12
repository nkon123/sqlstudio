// 단축키 목록 (F1) — 프로그램 안의 모든 단축키를 한 화면에. 위 칸에 글자를 넣으면 거른다.

import { h, modal } from "./ui";

const GROUPS: [string, [string, string][]][] = [
  ["실행", [
    ["Ctrl+Enter · F9", "커서 위치 문장 실행 (선택했으면 선택한 것)"],
    ["F5", "스크립트 전체 실행 (선택했으면 선택한 것)"],
    ["Ctrl+E", "실행계획"],
    ["Esc", "실행 중인 쿼리 중지"],
  ]],
  ["편집", [
    ["Ctrl+S", "저장"],
    ["Ctrl+Shift+F", "SQL 포맷 (선택 영역, 없으면 커서 위치 문장)"],
    ["Ctrl+/", "주석 넣기/빼기"],
    ["Ctrl+Space", "자동완성 띄우기"],
    ["Tab · Shift+Tab", "들여쓰기 · 코드 조각의 다음/앞 칸"],
    ["Ctrl+Z · Ctrl+Y", "되돌리기 · 다시 하기"],
    ["Ctrl+F", "찾기·바꾸기"],
    ["F3 · Shift+F3", "다음 · 앞 찾기"],
    ["Ctrl+Alt+G", "줄로 이동"],
  ]],
  ["여러 곳 편집 (열 편집)", [
    ["Alt + 드래그", "사각형(열) 선택"],
    ["Alt+Shift+방향키", "키보드로 사각형 선택"],
    ["Ctrl + 클릭", "커서 추가"],
    ["Ctrl+D", "같은 낱말 하나 더 선택"],
    ["Ctrl+Shift+L", "같은 낱말 모두 선택"],
    ["Esc", "커서 하나로"],
  ]],
  ["보기·찾기", [
    ["F4", "커서의 테이블(별칭) 컬럼 팝업 — 테이블 목록에서는 우클릭"],
    ["F8", "SQL 실행 이력"],
    ["Ctrl+K", "AI 입력칸으로 (AI 입력칸에서 Esc: 편집기로, Ctrl+Enter: 보내기)"],
    ["Ctrl+Shift+1", "왼쪽 영역(테이블·프로젝트) 접기/펼치기"],
    ["Ctrl+Shift+3", "오른쪽 영역(AI) 접기/펼치기"],
    ["F1", "이 단축키 목록"],
  ]],
  ["결과 그리드", [
    ["Ctrl+C", "고른 셀 복사"],
    ["Ctrl+A", "모두 고르기"],
    ["F2 · Enter", "셀 고치기 (편집 모드에서)"],
    ["Delete", "행 삭제 표시 (편집 모드에서, 다시 누르면 풀림)"],
    ["Esc", "거르기 칸 비우기 (거르기 칸에서)"],
  ]],
  ["컬럼 팝업 (F4)", [
    ["더블클릭", "컬럼 이름 넣기"],
    ["Ctrl+클릭 · Shift+클릭", "여러 행 고르기"],
    ["Ctrl+A · Ctrl+C", "모두 고르기 · 고른 행 복사"],
    ["Esc", "닫기"],
  ]],
  ["실행 이력 (F8)", [
    ["↑ · ↓", "고르기"],
    ["Enter · 더블클릭", "편집기에 넣기"],
  ]],
  ["프로젝트 (Git)", [
    ["더블클릭", "파일을 편집기로 열기"],
    ["Ctrl+Enter", "커밋 (메시지 칸에서)"],
  ]],
];

export function showShortcuts() {
  const q = h("input", { placeholder: "찾기 (예: 실행, F4, 복사)", class: "sc-q" });
  const body = h("div", { class: "sc-body" });
  const render = () => {
    const w = q.value.trim().toLowerCase();
    body.replaceChildren(...GROUPS.map(([g, rows]) => {
      const hit = rows.filter(([k, d]) => !w || `${g} ${k} ${d}`.toLowerCase().includes(w));
      if (!hit.length) return "";
      return h("section", { class: "sc-group" }, h("h4", {}, g),
        h("table", {}, h("tbody", {}, ...hit.map(([k, d]) =>
          h("tr", {}, h("td", { class: "sc-key" }, ...k.split(" · ").flatMap((x, i) => [i ? " · " : "", h("kbd", {}, x)])), h("td", {}, d))))));
    }));
  };
  q.addEventListener("input", render);
  render();
  modal("단축키 (F1)", h("div", { class: "sc" }, q, body), [{ label: "닫기", value: true, kind: "primary" }], { wide: true });
  q.focus();
}
