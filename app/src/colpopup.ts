// 테이블 컬럼 팝업 — 편집기 위에 떠 있는 작은 창. 여러 개 띄울 수 있고, 띄운 채로 SQL 을 계속 쓴다.
//
// 편집기에서 F4 (커서의 테이블·별칭), 왼쪽 테이블 목록에서 우클릭.
// 클릭: 행 선택 (Ctrl·Shift 로 여러 개) · 더블클릭: 컬럼 이름 넣기 · Ctrl+C: 고른 행 복사 · Esc: 닫기 · 머리 끌기: 옮기기 · 오른쪽 아래 모서리: 크기

import type { TableDesc } from "./api";
import { h, toast } from "./ui";

export interface ColPopupOpts {
  /** 화면 좌표 — 이 근처에 띄운다 */
  at?: { x: number; y: number };
  /** 편집기에 글을 넣는다 */
  insert: (text: string) => void;
  /** 별칭으로 열었으면 컬럼 앞에 붙인다 (o.ID) */
  alias?: string;
  ddl?: () => void;
}

type SortKey = "no" | "name" | "type" | "null" | "default" | "comment";
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
  let sort: { k: SortKey; desc: boolean } = { k: "no", desc: false };
  const picked = new Set<string>();
  let anchor = -1;
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
  const names = () => {
    const sel = shown().filter((r) => picked.has(r.name));
    return (sel.length ? sel : shown()).map((r) => prefix + r.name);
  };
  const win = h("div", { class: "cp-win", tabindex: "-1", "data-key": key },
    h("div", { class: "cp-head", title: "끌어서 옮기기" },
      h("strong", {}, `${d.owner}.${d.name}`),
      sub,
      h("span", { class: "spacer" }),
      h("button", { class: "cp-x", title: "닫기 (Esc)", onclick: () => close() }, "×")),
    h("div", { class: "cp-tools" }, tabCols, tabIdx, filter, count, h("span", { class: "spacer" }),
      h("button", { class: "small", title: "고른 컬럼(없으면 보이는 전체)을 쉼표로 넣는다", onclick: () => o.insert(names().join(", ")) }, "컬럼 넣기"),
      h("button", { class: "small", title: "SELECT 문을 넣는다", onclick: () => o.insert(`SELECT ${names().join(", ")}\n  FROM ${d.owner}.${d.name}${o.alias ? ` ${o.alias}` : ""}\n WHERE ROWNUM <= 100`) }, "SELECT"),
      o.ddl ? h("button", { class: "small", onclick: () => o.ddl!() }, "DDL") : ""),
    body);

  function shown() {
    const q = filter.value.trim().toUpperCase();
    const v = q ? rows.filter((r) => `${r.name} ${r.data_type} ${r.comment ?? ""} ${r.default ?? ""}`.toUpperCase().includes(q)) : rows.slice();
    const val = (r: (typeof rows)[number]): string | number =>
      sort.k === "no" ? r.no : sort.k === "name" ? r.name : sort.k === "type" ? r.data_type : sort.k === "null" ? (r.nullable ? 1 : 0) : sort.k === "default" ? r.default ?? "" : r.comment ?? "";
    v.sort((a, b) => { const x = val(a), y = val(b); const c = typeof x === "number" ? x - (y as number) : String(x).localeCompare(String(y)); return sort.desc ? -c : c; });
    return v;
  }

  function render() {
    if (view === "idx") {
      count.textContent = "";
      body.replaceChildren(d.indexes.length ? h("table", { class: "cp-grid" },
        h("thead", {}, h("tr", {}, ...["인덱스", "유일", "컬럼"].map((x) => h("th", {}, x)))),
        h("tbody", {}, ...d.indexes.map((i) => h("tr", {}, h("td", {}, i.name), h("td", {}, i.unique ? "UNIQUE" : ""), h("td", {}, i.columns.join(", "))))))
        : h("div", { class: "cp-empty" }, "인덱스가 없습니다"));
      return;
    }
    const v = shown();
    count.textContent = v.length === rows.length ? "" : `${v.length}/${rows.length}`;
    const th = (k: SortKey, label: string) => {
      const el = h("th", { class: "sortable", title: "눌러서 정렬" }, label, sort.k === k ? (sort.desc ? " ▼" : " ▲") : "");
      el.onclick = () => { sort = { k, desc: sort.k === k ? !sort.desc : false }; render(); };
      return el;
    };
    const tbody = h("tbody", {});
    v.forEach((r, i) => {
      const tr = h("tr", { class: picked.has(r.name) ? "sel" : "" },
        h("td", { class: "num" }, String(r.no)),
        h("td", { class: "cp-name" }, pk.has(r.name) ? h("span", { class: "cp-pk", title: "기본 키" }, "PK") : "", r.name),
        h("td", {}, r.data_type),
        h("td", { class: "cp-nn" }, r.nullable ? "" : "NOT NULL"),
        h("td", { class: "cp-def", title: r.default ?? "" }, r.default ?? ""),
        h("td", { title: [r.comment, idxCols.has(r.name) ? `인덱스: ${idxCols.get(r.name)!.join(", ")}` : ""].filter(Boolean).join("\n") }, r.comment ?? ""));
      tr.onmousedown = (e) => {
        if (e.shiftKey && anchor >= 0) {
          const [a, b] = [Math.min(anchor, i), Math.max(anchor, i)];
          if (!e.ctrlKey) picked.clear();
          for (let k = a; k <= b; k++) picked.add(v[k].name);
        } else if (e.ctrlKey || e.metaKey) {
          picked.has(r.name) ? picked.delete(r.name) : picked.add(r.name);
          anchor = i;
        } else {
          picked.clear(); picked.add(r.name); anchor = i;
        }
        e.preventDefault();
        win.focus();
        render();
      };
      tr.ondblclick = () => o.insert(prefix + r.name);
      tbody.append(tr);
    });
    body.replaceChildren(h("table", { class: "cp-grid" },
      h("thead", {}, h("tr", {}, th("no", "#"), th("name", "컬럼"), th("type", "형식"), th("null", "NULL"), th("default", "기본값"), th("comment", "설명"))),
      tbody));
  }

  function close() {
    win.remove();
    open.splice(open.indexOf(win), 1);
  }

  tabCols.onclick = () => { view = "cols"; tabCols.classList.add("on"); tabIdx.classList.remove("on"); render(); };
  tabIdx.onclick = () => { view = "idx"; tabIdx.classList.add("on"); tabCols.classList.remove("on"); render(); };
  filter.addEventListener("input", render);
  win.addEventListener("keydown", (e) => {
    if (e.key === "Escape") { e.stopPropagation(); close(); }
    else if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "c" && e.target !== filter && view === "cols") {
      const v = shown().filter((r) => picked.has(r.name));
      if (!v.length) return;
      e.preventDefault();
      const tsv = v.map((r) => [r.name, r.data_type, r.nullable ? "" : "NOT NULL", r.default ?? "", r.comment ?? ""].join("\t")).join("\n");
      navigator.clipboard.writeText(tsv).then(() => toast(`${v.length}행 복사`, "info", 1500)).catch(() => {});
    } else if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "a" && e.target !== filter) {
      e.preventDefault();
      for (const r of shown()) picked.add(r.name);
      render();
    }
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
