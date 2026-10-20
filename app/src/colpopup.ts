// 테이블 컬럼 팝업 — 편집기 위에 떠 있는 작은 창. 여러 개 띄울 수 있고, 띄운 채로 SQL 을 계속 쓴다.
//
// 편집기에서 F4 (커서의 테이블·별칭), 왼쪽 테이블 목록에서 우클릭.
// 결과 그리드와 같다: 셀 단위 선택(끌기·Shift) · Ctrl+C 고른 셀 복사(컬럼 칸만 고르면 이름만) · 머리 눌러 정렬 · 열 너비
// 더블클릭: 컬럼 이름 넣기 · 우클릭: 이름 복사·넣기 · Esc: 닫기 · 머리 끌기: 옮기기 · 오른쪽 아래 모서리: 크기
// 키보드: 방향키로 고르기 (Shift+ 넓히기 · Ctrl+ 커서만 · Space 더하기/빼기) · 글자를 치면 거르기 칸으로
//   Enter: 커서가 있던 절에 맞춰 넣기 · Shift+Enter: 넣을 절 메뉴
//   Ctrl+Shift(또는 Alt) 를 누르고 있으면 4방향 휠 — 누른 채 ↑ SELECT · ↓ WHERE · ← ORDER BY · → GROUP BY, 떼면 넣는다

import type { Column, TableDesc } from "./api";
import { TARGET_LABEL, type Target } from "./colinsert";
import { showMenu, type MenuItem } from "./ctxmenu";
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
  /** 컬럼 영향도 (이 컬럼을 쓰는 곳) */
  usage?: (column: string) => void;
  /** 고른 컬럼을 SQL 에 넣는다 — "auto" 는 커서가 있던 절에 맞춰. 절을 알 수 없으면 false (메뉴로 고르게).
   *  `from` 은 FROM 에 쓸 테이블 (커서 쪽에 문장이 없으면 새 SELECT 를 만든다) */
  place?: (target: Target | "auto", cols: string[], from: string) => Promise<boolean>;
}

/** 휠을 여는 키 — Ctrl+Shift 가 Windows 입력 언어 전환 키인 PC 는 Alt 로 */
type WheelKey = "ctrlshift" | "alt";
const WHEEL_KEY = "colpopup.wheelKey";
function wheelKey(): WheelKey {
  try { return localStorage.getItem(WHEEL_KEY) === "alt" ? "alt" : "ctrlshift"; } catch { return "ctrlshift"; }
}
const WHEEL_LABEL: Record<WheelKey, string> = { ctrlshift: "Ctrl+Shift", alt: "Alt" };
const TARGETS: Target[] = ["select", "where", "order", "group"];
/** 방향 → 절 */
const DIRS: Record<string, Target> = { ArrowUp: "select", ArrowDown: "where", ArrowLeft: "order", ArrowRight: "group" };
const ARROW: Record<Target, string> = { select: "↑", where: "↓", order: "←", group: "→" };

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
  /** 고른 행의 컬럼 (넣기용 — 고른 게 없으면 빈 목록) */
  const chosen = () => grid.selectedRows().map((r) => r[NAME]).filter((n): n is string => !!n).map((n) => prefix + n);
  const fromText = `${d.owner}.${d.name}${o.alias ? ` ${o.alias}` : ""}`;
  const NONE = "넣을 컬럼을 고르세요 (방향키 · Shift+방향키 · Space)";
  const placeCols = async (target: Target | "auto") => {
    if (view !== "cols") return;
    const cols = chosen();
    if (!cols.length) return toast(NONE, "info", 2000);
    if (!o.place) return o.insert(cols.join(", "));
    if (!(await o.place(target, cols, fromText))) placeMenu();
  };
  const placeItems = (): MenuItem[] => {
    const k = WHEEL_LABEL[wheelKey()];
    return [
      { label: "커서 자리에 (절에 맞춰)", hint: "Enter", run: () => void placeCols("auto") },
      ...TARGETS.map((t) => ({ label: `${TARGET_LABEL[t]} 에`, hint: `${k} ${ARROW[t]}`, run: () => void placeCols(t) })),
      { label: "이름만 커서 자리에", run: () => { const c = chosen(); if (c.length) o.insert(c.join(", ")); } },
    ];
  };
  /** 넣을 절 메뉴 — 커서 행 옆에 */
  const placeMenu = () => {
    const n = chosen().length;
    if (!n) return toast(NONE, "info", 2000);
    const r = grid.cursorRect() ?? body.getBoundingClientRect();
    showMenu(r.left + 60, r.bottom, [{ title: `고른 컬럼 ${n}개 넣기` }, ...placeItems()]);
  };
  const copyNames = () => {
    const v = names();
    navigator.clipboard.writeText(v.join("\n")).then(() => toast(`컬럼 이름 ${v.length}개 복사`, "info", 1500)).catch(() => {});
  };
  const grid: ResultGrid = new ResultGrid({
    onStatus: () => { count.textContent = view === "cols" && grid.viewRows().length !== rows.length ? `${grid.viewRows().length}/${rows.length}` : ""; },
    onDoubleClick: (r) => { if (view === "cols" && r[NAME]) o.insert(prefix + r[NAME]); return true; },
    onKeyDown: (e) => {
      if (view !== "cols") return false;
      if (e.key === "Enter" && !e.ctrlKey && !e.altKey && !e.metaKey) {
        e.preventDefault();
        if (e.shiftKey) placeMenu();
        else void placeCols("auto");
        return true;
      }
      // 글자를 치면 거르기 칸으로 (그 글자는 칸에 들어간다)
      if (e.key.length === 1 && e.key !== " " && !e.ctrlKey && !e.altKey && !e.metaKey) {
        filter.focus();
        return true;
      }
      return false;
    },
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
        ...(o.place ? [{ label: "절에 맞춰 넣기", sub: placeItems() }] : []),
        { label: "휠 키", sub: (["ctrlshift", "alt"] as WheelKey[]).map((k) => ({
          label: `${wheelKey() === k ? "✓ " : ""}${WHEEL_LABEL[k]} 누르고 있기`,
          run: () => { try { localStorage.setItem(WHEEL_KEY, k); } catch { /* 기억 못 해도 된다 */ } },
        })) },
        ...(o.usage ? [{ sep: true as const }, { label: "이 컬럼을 쓰는 곳 (영향도)", hint: "뷰·소스의 줄", run: () => { const c = _r[NAME]; if (c) o.usage!(c); } }] : []),
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
    closeWheel();
    window.removeEventListener("blur", closeWheel);
    win.remove();
    open.splice(open.indexOf(win), 1);
  }

  tabCols.onclick = () => { view = "cols"; tabCols.classList.add("on"); tabIdx.classList.remove("on"); render(); };
  tabIdx.onclick = () => { view = "idx"; tabIdx.classList.add("on"); tabCols.classList.remove("on"); render(); };
  // 거르기: 아무 칸에 든 글자 (그리드 필터식도 된다: 형식~DATE)
  filter.addEventListener("input", () => grid.setFilter(filter.value));
  filter.addEventListener("keydown", (e) => {
    if ((e.key === "ArrowDown" || (e.key === "Enter" && !e.isComposing)) && view === "cols") { e.preventDefault(); grid.el.focus(); grid.selectRow(0); }
    else if (e.key === "ArrowDown") { e.preventDefault(); grid.el.focus(); }
  });
  body.append(grid.el);
  win.addEventListener("keydown", (e) => {
    if (e.key === "Escape") { e.stopPropagation(); close(); }
  });

  // ── 4방향 휠 ── 모디파이어만 0.2초 누르고 있으면 뜬다 (Ctrl+Shift+C 같은 단축키는 그 전에 다른 키가 와서 안 뜬다)
  let wheelTimer = 0;
  let wheel: { el: HTMLElement; dir: Target | null } | null = null;
  const held = (e: KeyboardEvent) => (wheelKey() === "alt" ? e.altKey && !e.ctrlKey && !e.shiftKey : e.ctrlKey && e.shiftKey && !e.altKey);
  const MODS = new Set(["Control", "Shift", "Alt"]);
  const closeWheel = () => { clearTimeout(wheelTimer); wheelTimer = 0; wheel?.el.remove(); wheel = null; };
  const pickDir = (t: Target) => {
    if (!wheel) return;
    wheel.dir = t;
    for (const b of wheel.el.querySelectorAll<HTMLElement>("button")) b.classList.toggle("on", b.dataset.t === t);
  };
  const openWheel = () => {
    const n = chosen().length;
    if (!n || view !== "cols") return;
    const el = h("div", { class: "cp-wheel" },
      ...TARGETS.map((t) => h("button", { class: `w-${t}`, "data-t": t,
        onmousedown: (ev: MouseEvent) => { ev.preventDefault(); closeWheel(); void placeCols(t); } },
      h("span", { class: "k" }, ARROW[t]), TARGET_LABEL[t])),
      h("div", { class: "w-mid" }, `${n}개`));
    const r = grid.cursorRect() ?? body.getBoundingClientRect();
    const W = 3 * 92 + 8, H = 3 * 34 + 8;
    const cx = (r.left + r.right) / 2, cy = (r.top + r.bottom) / 2;
    el.style.left = `${Math.max(4, Math.min(window.innerWidth - W - 4, cx - W / 2))}px`;
    el.style.top = `${Math.max(4, Math.min(window.innerHeight - H - 4, cy - H / 2))}px`;
    document.body.append(el);
    wheel = { el, dir: null };
  };
  // 잡기 단계 — 휠이 떠 있으면 방향키는 그리드로 가지 않는다
  win.addEventListener("keydown", (e) => {
    if (wheel) {
      e.preventDefault();
      e.stopPropagation();
      if (DIRS[e.key]) pickDir(DIRS[e.key]);
      else if (e.key === "Escape") closeWheel();
      return;
    }
    if (MODS.has(e.key)) {
      if (e.key === "Alt" && wheelKey() === "alt") e.preventDefault();
      if (held(e) && !wheelTimer && view === "cols") wheelTimer = window.setTimeout(() => { wheelTimer = 0; openWheel(); }, 200);
    } else {
      clearTimeout(wheelTimer);
      wheelTimer = 0;
    }
  }, true);
  win.addEventListener("keyup", (e) => {
    if (!MODS.has(e.key) || held(e)) return;
    clearTimeout(wheelTimer);
    wheelTimer = 0;
    if (!wheel) return;
    e.preventDefault();
    const t = wheel.dir;
    closeWheel();
    if (t) void placeCols(t);
  }, true);
  window.addEventListener("blur", closeWheel);
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
