// 테이블 컬럼 팝업 — 편집기 위에 떠 있는 작은 창. 여러 개 띄울 수 있고, 띄운 채로 SQL 을 계속 쓴다.
//
// 편집기에서 F4 (커서의 테이블·별칭), 왼쪽 테이블 목록에서 우클릭.
// 결과 그리드와 같다: 셀 단위 선택(끌기·Shift) · Ctrl+C 고른 셀 복사(컬럼 칸만 고르면 이름만) · 머리 눌러 정렬 · 열 너비
// 더블클릭: 컬럼 이름 넣기 · 우클릭: 이름 복사·넣기 · Esc: 닫기 · 머리 끌기: 옮기기 · 오른쪽 아래 모서리: 크기

import type { Column, TableDesc } from "./api";
import { showMenu } from "./ctxmenu";
import { ResultGrid } from "./grid";
import { h, toast } from "./ui";

const col = (name: string, type_name = "VARCHAR2"): Column => ({ name, type_name, nullable: true });
/** 컬럼 그리드의 열 — 순서는 그리드 왼쪽 번호 (머리를 세 번 누르면 원래 순서) */
const COLS = [col("컬럼"), col("형식"), col("NULL"), col("기본값"), col("설명"), col("키·인덱스")];
const NAME = 0;
const IDX_COLS = [col("인덱스"), col("유일"), col("컬럼")];

export interface ColPopupOpts {
  /** 화면 좌표 — 이 근처에 띄운다 */
  at?: { x: number; y: number };
  /** 편집기에 글을 넣는다 */
  insert: (text: string) => void;
  /** 별칭으로 열었으면 컬럼 앞에 붙인다 (o.ID) */
  alias?: string;
  ddl?: () => void;
}

const open: HTMLElement[] = [];
let zTop = 60;
/** 떠 있는 창 → 내용 바꾸기 (캐시로 먼저 띄운 뒤 전체 구조가 오면) */
const updaters = new WeakMap<HTMLElement, (d: TableDesc) => void>();

export interface ColPopup { update: (d: TableDesc) => void }

export function showColumns(d: TableDesc, o: ColPopupOpts): ColPopup {
  // 같은 테이블이 이미 떠 있으면 그 창을 앞으로
  const key = `${d.owner}.${d.name}`;
  const same = open.find((w) => w.dataset.key === key);
  if (same) {
    same.style.zIndex = String(++zTop);
    same.querySelector<HTMLInputElement>("input")?.focus();
    return { update: (x) => updaters.get(same)?.(x) };
  }

  let pk = new Set(d.primary_key);
  let idxCols = new Map<string, string[]>();
  let rows = d.columns.map((c, i) => ({ no: i + 1, ...c }));
  const setData = (x: TableDesc) => {
    d = x;
    pk = new Set(x.primary_key);
    idxCols = new Map();
    for (const i of x.indexes) for (const c of i.columns) idxCols.set(c, [...(idxCols.get(c) ?? []), i.name]);
    rows = x.columns.map((c, i) => ({ no: i + 1, ...c }));
  };
  setData(d);
  let view = "cols" as "cols" | "idx";

  const filter = h("input", { placeholder: "컬럼·형식·설명 거르기", class: "cp-filter" });
  const body = h("div", { class: "cp-body" });
  const count = h("span", { class: "cp-count" });
  const tabCols = h("button", { class: "cp-tab on" }, `컬럼 ${d.columns.length}`);
  const tabIdx = h("button", { class: "cp-tab" }, d.partial ? "인덱스 …" : `인덱스 ${d.indexes.length}`);
  const sub = h("span", { class: "cp-dim" });
  const subText = () => ` ${d.object_type}${d.comment ? ` — ${d.comment}` : ""}${d.num_rows ? ` · ${Number(d.num_rows).toLocaleString()}행` : ""}${d.partial ? " · 나머지 읽는 중…" : ""}`;
  sub.textContent = subText();
  const prefix = o.alias ? `${o.alias}.` : "";
  /** 고른 행의 컬럼 이름 (고른 게 없으면 보이는 전체) */
  const names = () => {
    const sel = grid.selectedRows();
    return (sel.length ? sel : grid.viewRows()).map((r) => prefix + (r[NAME] ?? ""));
  };
  const copyNames = () => {
    const v = names();
    navigator.clipboard.writeText(v.join("\n")).then(() => toast(`컬럼 이름 ${v.length}개 복사`, "info", 1500)).catch(() => {});
  };
  const grid: ResultGrid = new ResultGrid({
    onStatus: () => { count.textContent = view === "cols" && grid.viewRows().length !== rows.length ? `${grid.viewRows().length}/${rows.length}` : ""; },
    onDoubleClick: (r) => { if (view === "cols" && r[NAME]) o.insert(prefix + r[NAME]); return true; },
    onContextMenu: (_r, _c, e) => {
      if (view !== "cols") return;
      const n = grid.selectedRows().length;
      showMenu(e.clientX, e.clientY, [
        { label: n > 1 ? `컬럼 이름 복사 (${n}개, 줄마다)` : "컬럼 이름 복사", hint: "", run: copyNames },
        { label: "쉼표로 이어 복사", run: () => navigator.clipboard.writeText(names().join(", ")).catch(() => {}) },
        { label: "고른 셀 복사", hint: "Ctrl+C", run: () => navigator.clipboard.writeText(grid.selectionText()).catch(() => {}) },
        { label: "머리 포함 복사", hint: "Ctrl+Shift+C", run: () => navigator.clipboard.writeText(grid.selectionText(true)).catch(() => {}) },
        { sep: true },
        { label: "컬럼 넣기", run: () => o.insert(names().join(", ")) },
      ]);
    },
  });
  const win = h("div", { class: "cp-win", tabindex: "-1", "data-key": key },
    h("div", { class: "cp-head", title: "끌어서 옮기기" },
      h("strong", {}, `${d.owner}.${d.name}`),
      sub,
      h("span", { class: "spacer" }),
      h("button", { class: "cp-x", title: "닫기 (Esc)", onclick: () => close() }, "×")),
    h("div", { class: "cp-tools" }, tabCols, tabIdx, filter, count, h("span", { class: "spacer" }),
      h("button", { class: "small", title: "고른 행(없으면 보이는 전체)의 컬럼 이름을 줄마다 복사", onclick: copyNames }, "이름 복사"),
      h("button", { class: "small", title: "고른 컬럼(없으면 보이는 전체)을 쉼표로 넣는다", onclick: () => o.insert(names().join(", ")) }, "컬럼 넣기"),
      h("button", { class: "small", title: "SELECT 문을 넣는다", onclick: () => o.insert(`SELECT ${names().join(", ")}\n  FROM ${d.owner}.${d.name}${o.alias ? ` ${o.alias}` : ""}\n WHERE ROWNUM <= 100`) }, "SELECT"),
      o.ddl ? h("button", { class: "small", onclick: () => o.ddl!() }, "DDL") : ""),
    body);

  function render() {
    if (view === "idx") {
      count.textContent = "";
      grid.setPage({ columns: IDX_COLS, rows: d.indexes.map((i) => [i.name, i.unique ? "UNIQUE" : "", i.columns.join(", ")]), has_more: false, fetched_total: d.indexes.length });
      return;
    }
    const data = rows.map((r) => [r.name, r.data_type, r.nullable ? "" : "NOT NULL", r.default ?? "", r.comment ?? "",
      [pk.has(r.name) ? "PK" : "", ...(idxCols.get(r.name) ?? [])].filter(Boolean).join(", ")]);
    grid.setPage({ columns: COLS, rows: data, has_more: false, fetched_total: data.length });
  }

  function close() {
    win.remove();
    open.splice(open.indexOf(win), 1);
  }

  tabCols.onclick = () => { view = "cols"; tabCols.classList.add("on"); tabIdx.classList.remove("on"); render(); };
  tabIdx.onclick = () => { view = "idx"; tabIdx.classList.add("on"); tabCols.classList.remove("on"); render(); };
  // 거르기: 아무 칸에 든 글자 (그리드 필터식도 된다: 형식~DATE)
  filter.addEventListener("input", () => grid.setFilter(filter.value));
  filter.addEventListener("keydown", (e) => { if (e.key === "ArrowDown") { e.preventDefault(); grid.el.focus(); } });
  body.append(grid.el);
  win.addEventListener("keydown", (e) => {
    if (e.key === "Escape") { e.stopPropagation(); close(); }
  });
  win.addEventListener("mousedown", () => { win.style.zIndex = String(++zTop); });

  // 머리를 끌어 옮기기
  const head = win.querySelector<HTMLElement>(".cp-head")!;
  head.addEventListener("mousedown", (e) => {
    if ((e.target as HTMLElement).tagName === "BUTTON") return;
    e.preventDefault();
    const r = win.getBoundingClientRect();
    const dx = e.clientX - r.left, dy = e.clientY - r.top;
    const move = (ev: MouseEvent) => {
      win.style.left = `${Math.max(0, Math.min(window.innerWidth - 80, ev.clientX - dx))}px`;
      win.style.top = `${Math.max(0, Math.min(window.innerHeight - 30, ev.clientY - dy))}px`;
    };
    const up = () => { window.removeEventListener("mousemove", move); window.removeEventListener("mouseup", up); };
    window.addEventListener("mousemove", move);
    window.addEventListener("mouseup", up);
  });

  // 자리: 요청한 곳 아래, 화면 밖으로 나가지 않게. 여러 개면 조금씩 비켜서
  const w = Math.min(720, window.innerWidth - 40), hh = Math.min(380, window.innerHeight - 80);
  const off = open.length * 24;
  const x = (o.at?.x ?? window.innerWidth / 2 - w / 2) + off, y = (o.at ? o.at.y + 8 : 120) + off;
  win.style.width = `${w}px`;
  win.style.height = `${hh}px`;
  win.style.left = `${Math.max(10, Math.min(window.innerWidth - w - 10, x))}px`;
  win.style.top = `${Math.max(10, Math.min(window.innerHeight - hh - 10, y))}px`;
  win.style.zIndex = String(++zTop);
  document.body.append(win);
  open.push(win);
  render();
  filter.focus();
  const update = (x: TableDesc) => {
    if (!win.isConnected) return;
    setData(x);
    sub.textContent = subText();
    tabCols.textContent = `컬럼 ${x.columns.length}`;
    tabIdx.textContent = x.partial ? "인덱스 …" : `인덱스 ${x.indexes.length}`;
    render();
  };
  updaters.set(win, update);
  return { update };
}

/** 문서에서 별칭 → 테이블 ("FROM orders o", "JOIN app.customers AS c", ", items i") */
export function aliasMap(sql: string): Map<string, string> {
  const m = new Map<string, string>();
  const kw = new Set(["FROM", "WHERE", "ON", "JOIN", "LEFT", "RIGHT", "INNER", "OUTER", "FULL", "CROSS", "GROUP", "ORDER", "UNION", "MINUS", "INTERSECT", "SET", "CONNECT", "START", "HAVING", "FETCH", "PARTITION", "USING", "AND", "OR", "WITH", "VALUES", "SELECT", "INTO", "NATURAL", "AS"]);
  const name = String.raw`([A-Za-z_][\w$#]*(?:\.[A-Za-z_][\w$#]*)?)`;
  // 따로 훑는다 — 한쪽이 글자를 먹어 다른 쪽을 놓치지 않게 ("SELECT o.id, c.name FROM orders o")
  for (const re of [new RegExp(String.raw`\b(?:FROM|JOIN|UPDATE|INTO)\s+${name}(?:\s+(?:AS\s+)?([A-Za-z_][\w$#]*))?`, "gi"),
    new RegExp(String.raw`,\s*${name}\s+(?:AS\s+)?([A-Za-z_][\w$#]*)`, "gi")]) {
    for (const x of sql.matchAll(re)) {
      const alias = x[2]?.toUpperCase();
      if (alias && !kw.has(alias) && !kw.has(x[1].toUpperCase())) m.set(alias, x[1].toUpperCase());
    }
  }
  return m;
}
