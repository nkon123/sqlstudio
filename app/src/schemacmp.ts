// 스키마 비교 — 두 스키마(다른 DB 여도 된다)의 구조를 맞대고, 대상을 원본에 맞추는 스크립트를 만든다.
//
// 비교는 이 화면만을 위한 읽기 전용 세션 두 개로 한다 (작업 탭·자동완성을 막지 않는다).
// 스크립트는 실행하지 않는다 — 접속하지 않은 새 탭으로 연다 (원본 DB 에 잘못 돌리지 않게).

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { api, errOf, type Group, type Profile } from "./api";
import { profileOptions } from "./conngroups";
import { ResultGrid } from "./grid";
import { showMenu } from "./ctxmenu";
import { h, passwordBox, toast } from "./ui";

type Status = "only_source" | "only_target" | "changed";
interface Item { kind: string; name: string; status: Status; details: string[] }
type Progress =
  | { type: "phase"; run: number; side: string; phase: string }
  | { type: "done"; run: number; items: Item[]; src: string; tgt: string; ms: number; counts: [number, number, number] }
  | { type: "failed"; run: number; error: string };

const KIND: Record<string, string> = {
  TABLE: "테이블", VIEW: "뷰", SEQUENCE: "시퀀스", PACKAGE: "패키지", "PACKAGE BODY": "패키지 본문", PROCEDURE: "프로시저",
  FUNCTION: "함수", TRIGGER: "트리거", TYPE: "타입", "TYPE BODY": "타입 본문",
};
const STATUS: Record<Status, string> = { only_source: "대상에 없음", changed: "다름", only_target: "대상에만" };
const COLS = ["✓", "종류", "이름", "차이", "내용", "키"].map((name) => ({ name, type_name: "VARCHAR2", nullable: true }));
const KEY = COLS.length - 1;
const keyOf = (i: Item) => `${i.kind} ${i.name}`;

export class SchemaCompareView {
  readonly el: HTMLElement;
  private srcSchema = h("input", { placeholder: "원본 스키마 (비우면 접속 계정)", style: "width:150px" });
  private tgtSel = h("select", { title: "대상 접속 프로필 — 비교만 하는 읽기 전용 세션을 따로 연다" });
  private tgtSchema = h("input", { placeholder: "대상 스키마 (비우면 접속 계정)", style: "width:150px" });
  private nameIn = h("input", { placeholder: "이름 (LIKE, 예: ORD%)", style: "width:130px" });
  private kinds = { tables: h("input", { type: "checkbox", checked: true }), views: h("input", { type: "checkbox", checked: true }),
    seqs: h("input", { type: "checkbox", checked: true }), code: h("input", { type: "checkbox", checked: true }) };
  private startBtn = h("button", { class: "primary" }, "비교");
  private stopBtn = h("button", { disabled: true }, "멈춤");
  private statusEl = h("span", { class: "dbg-status" });
  private srcLabel = h("strong", {});
  private viewSel = h("select", {});
  private detail = h("div", { class: "sc-detail an-pad" });
  private grid: ResultGrid;
  private run: number | null = null;
  private items: Item[] = [];
  private checked = new Set<string>();
  private labels = { src: "", tgt: "" };
  private unlisten: (() => void) | null = null;
  private profiles: Profile[] = [];
  private phase: Record<string, string> = {};

  constructor(private sessionId: () => number | undefined, private profileOf: () => string | undefined,
    private onClose: () => void, private openScript: (text: string) => void) {
    this.grid = new ResultGrid({
      hiddenCols: [KEY],
      onCellClick: (r, c) => { const k = r[KEY] ?? ""; if (c === 0) this.toggle([k]); else void this.show(k); },
      onDoubleClick: () => true,
      onContextMenu: (r, _c, e) => this.menu(r[KEY] ?? "", e),
    });
    this.startBtn.onclick = () => void this.start();
    this.stopBtn.onclick = () => { if (this.run != null) void invoke("schema_compare_cancel", { run: this.run }); };
    this.viewSel.onchange = () => this.renderList();
    for (const i of [this.srcSchema, this.tgtSchema, this.nameIn]) i.addEventListener("keydown", (e) => { if (e.key === "Enter") void this.start(); });
    const k = (box: HTMLElement, label: string, title: string) => h("label", { class: "check", title }, box, label);
    const toolbar = h("div", { class: "dbg-toolbar an-toolbar" },
      h("span", { class: "dim" }, "원본"), this.srcLabel, this.srcSchema,
      h("span", {}, "→"), h("span", { class: "dim" }, "대상"), this.tgtSel, this.tgtSchema,
      h("span", { class: "sep" }),
      k(this.kinds.tables, "테이블", "컬럼·제약조건·인덱스"), k(this.kinds.views, "뷰", ""), k(this.kinds.seqs, "시퀀스", ""),
      k(this.kinds.code, "소스", "패키지·본문·프로시저·함수·트리거·타입"),
      this.nameIn, this.startBtn, this.stopBtn,
      h("span", { class: "spacer" }), this.statusEl,
      h("button", { onclick: () => this.close() }, "닫기"));
    const listBar = h("div", { class: "an-listbar" }, this.viewSel,
      h("button", { class: "small", title: "보이는 것 모두 스크립트에 넣기", onclick: () => this.toggle(this.visible().map(keyOf), true) }, "✓ 모두"),
      h("button", { class: "small", onclick: () => this.toggle(this.visible().map(keyOf), false) }, "✓ 풀기"),
      h("span", { class: "spacer" }),
      h("button", { class: "small primary", title: "✓ 한 항목으로 대상을 원본에 맞추는 스크립트 — 실행하지 않고 접속하지 않은 새 탭으로 연다", onclick: () => void this.script() }, "스크립트 만들기"));
    const left = h("div", { class: "an-left" }, listBar, this.grid.el);
    const main = h("div", { class: "an-main sc-main" }, left, h("div", { class: "an-gutter-static" }), this.detail);
    this.el = h("div", { class: "debugger analysis", hidden: true }, toolbar, main);
    this.detail.append(h("div", { class: "hint" },
      "원본과 대상을 고르고 '비교'. 테이블(컬럼·제약·인덱스)·뷰·시퀀스·소스를 맞댑니다. ",
      "비교는 따로 연 읽기 전용 세션으로 하고, 스크립트는 실행하지 않고 새 탭으로 엽니다."));
  }

  async open() {
    this.el.hidden = false;
    this.srcLabel.textContent = this.profileOf() ?? "(접속 안 됨)";
    this.unlisten ??= await listen<Progress>("schema-compare", (e) => this.onProgress(e.payload));
    let groups: Group[] = [];
    try {
      [this.profiles, groups] = await Promise.all([api.listProfiles(), api.listGroups().catch(() => [] as Group[])]);
    } catch { this.profiles = []; }
    const prev = this.tgtSel.value;
    this.tgtSel.replaceChildren(...profileOptions(groups, this.profiles, (p) => h("option", { value: p.name, selected: p.name === prev }, `${p.name}${p.production ? " (운영)" : ""}`)));
  }

  close() {
    this.el.hidden = true;
    this.onClose();
  }

  private async start(password?: string): Promise<void> {
    const id = this.sessionId();
    if (id == null) return toast("원본 탭을 먼저 접속하세요", "error");
    if (!this.tgtSel.value) return toast("대상 접속을 고르세요", "error");
    const kinds = { tables: this.kinds.tables.checked, views: this.kinds.views.checked, seqs: this.kinds.seqs.checked, code: this.kinds.code.checked };
    if (!Object.values(kinds).some(Boolean)) return toast("비교할 종류를 고르세요", "error");
    if (this.run != null) void invoke("schema_compare_close", { run: this.run });
    this.run = null;
    this.items = [];
    this.renderList();
    this.phase = {};
    this.statusEl.textContent = "접속하는 중…";
    this.startBtn.disabled = true;
    try {
      this.run = await invoke<number>("schema_compare_start", {
        id, srcSchema: this.srcSchema.value.trim() || null, target: this.tgtSel.value, tgtSchema: this.tgtSchema.value.trim() || null,
        kinds, nameLike: this.nameIn.value.trim() || null, password: password ?? null,
      });
      this.stopBtn.disabled = false;
    } catch (e) {
      this.startBtn.disabled = false;
      this.statusEl.textContent = "";
      const err = errOf(e);
      if (err.kind === "password_required") {
        const p = this.profiles.find((x) => x.name === this.tgtSel.value);
        const pw = await passwordBox(this.tgtSel.value, p?.user ?? "");
        if (pw) return this.start(pw.password);
        return;
      }
      toast(err.message, "error");
    }
  }

  private onProgress(p: Progress) {
    if (p.run !== this.run) return;
    if (p.type === "phase") {
      this.phase[p.side] = p.phase;
      this.statusEl.textContent = p.side === "both" ? p.phase : `원본: ${this.phase.src ?? "…"} · 대상: ${this.phase.tgt ?? "…"}`;
    } else if (p.type === "failed") {
      this.startBtn.disabled = false;
      this.stopBtn.disabled = true;
      this.statusEl.textContent = p.error;
      if (p.error !== "멈췄습니다") toast(p.error, "error");
    } else {
      this.startBtn.disabled = false;
      this.stopBtn.disabled = true;
      this.items = p.items;
      this.labels = { src: p.src, tgt: p.tgt };
      // 처음엔 대상에 없는 것·다른 것을 스크립트에 넣는다 (대상에만 있는 것은 지우기라 뺀다)
      this.checked = new Set(p.items.filter((i) => i.status !== "only_target").map(keyOf));
      const [a, c, b] = p.counts;
      this.statusEl.textContent = `${p.src} → ${p.tgt} · ${(p.ms / 1000).toFixed(1)}초 · 대상에 없음 ${a} · 다름 ${c} · 대상에만 ${b}`;
      this.viewSel.replaceChildren(
        h("option", { value: "" }, `전체 ${p.items.length}`),
        h("option", { value: "only_source" }, `대상에 없음 ${a}`),
        h("option", { value: "changed" }, `다름 ${c}`),
        h("option", { value: "only_target" }, `대상에만 ${b}`));
      this.renderList(true);
      this.detail.replaceChildren(h("div", { class: "hint" },
        p.items.length ? "왼쪽에서 항목을 누르면 다른 점을 봅니다. ✓ 칸을 눌러 스크립트에 넣고 뺍니다." : "두 스키마가 같습니다 (고른 종류·이름 안에서)."));
    }
  }

  private visible(): Item[] {
    const v = this.viewSel.value;
    return this.items.filter((i) => !v || i.status === v);
  }

  private gridOf: Item[] | null = null;

  private renderList(fresh = false) {
    const rows = this.visible().map((i) => [this.checked.has(keyOf(i)) ? "✓" : "", KIND[i.kind] ?? i.kind, i.name, STATUS[i.status],
      i.details.length > 1 ? `${i.details[0]} 외 ${i.details.length - 1}` : i.details[0] ?? "", keyOf(i)]);
    if (fresh || this.gridOf !== this.items) {
      this.gridOf = this.items;
      this.grid.setPage({ columns: COLS, rows, has_more: false, fetched_total: rows.length });
    } else this.grid.replaceRows(rows);
  }

  private toggle(keys: string[], on?: boolean) {
    for (const k of keys) (on ?? !this.checked.has(k)) ? this.checked.add(k) : this.checked.delete(k);
    this.renderList();
  }

  private menu(key: string, e: MouseEvent) {
    const picked = this.grid.selectedRows().map((r) => r[KEY] ?? "").filter(Boolean);
    const keys = picked.includes(key) ? picked : [key];
    showMenu(e.clientX, e.clientY, [
      { label: keys.length > 1 ? `스크립트에 넣기 (${keys.length}개)` : "스크립트에 넣기", run: () => this.toggle(keys, true) },
      { label: keys.length > 1 ? `스크립트에서 빼기 (${keys.length}개)` : "스크립트에서 빼기", run: () => this.toggle(keys, false) },
      { sep: true },
      { label: "이 항목만 스크립트로", run: () => void this.script(keys) },
      { label: "이름 복사", run: () => navigator.clipboard.writeText(keys.map((k) => k.split(" ").pop()).join("\n")).catch(() => {}) },
    ]);
  }

  private async show(key: string) {
    if (this.run == null) return;
    const it = this.items.find((i) => keyOf(i) === key);
    if (!it) return;
    let d: { details: string[]; src?: string | null; tgt?: string | null };
    try {
      d = await invoke("schema_compare_detail", { run: this.run, key });
    } catch (e) {
      return toast(errOf(e).message, "error");
    }
    const box = h("div", {},
      h("div", { class: "an-head" }, h("strong", {}, `${KIND[it.kind] ?? it.kind} ${it.name}`), h("span", { class: `badge ${it.status === "changed" ? "warn" : ""}` }, STATUS[it.status])),
      h("div", { class: "dim", style: "font-size:12px" }, `원본 ${this.labels.src} → 대상 ${this.labels.tgt}`));
    if (d.details.length) box.append(h("ul", { class: "an-ul sc-details" }, ...d.details.map((x) => h("li", {}, x))));
    if (d.src != null || d.tgt != null) box.append(sideBySide(d.src ?? "", d.tgt ?? "", it.status));
    this.detail.replaceChildren(box);
  }

  private async script(only?: string[]) {
    if (this.run == null) return toast("먼저 비교하세요", "error");
    const keys = only ?? this.items.map(keyOf).filter((k) => this.checked.has(k));
    if (!keys.length) return toast("✓ 한 항목이 없습니다", "error");
    try {
      const text = await invoke<string>("schema_compare_script", { run: this.run, keys });
      this.openScript(`-- 대상(${this.labels.tgt}) 에 접속해서 실행하세요. 이 탭은 접속하지 않은 채로 열었습니다.\n${text}`);
      toast(`스크립트를 새 탭으로 열었습니다 (${keys.length}개 항목) — 대상에 접속해 검토 후 실행`, "info", 5000);
    } catch (e) {
      toast(errOf(e).message, "error");
    }
  }
}

/** 줄 단위 차이 (Myers) — 같음 / 원본에만 / 대상에만 */
type Op = { k: "=" | "-" | "+"; a?: string; b?: string; ai?: number; bi?: number };
export function diffLines(a: string[], b: string[]): Op[] {
  const n = a.length, m = b.length, max = n + m;
  if (max === 0) return [];
  // 너무 크면 간단히 (앞뒤 같은 줄만 맞추고 가운데는 통째로)
  if (n * m > 25_000_000) {
    let s = 0;
    while (s < n && s < m && a[s] === b[s]) s++;
    let e = 0;
    while (e < n - s && e < m - s && a[n - 1 - e] === b[m - 1 - e]) e++;
    const ops: Op[] = [];
    for (let i = 0; i < s; i++) ops.push({ k: "=", a: a[i], b: b[i], ai: i, bi: i });
    for (let i = s; i < n - e; i++) ops.push({ k: "-", a: a[i], ai: i });
    for (let j = s; j < m - e; j++) ops.push({ k: "+", b: b[j], bi: j });
    for (let i = 0; i < e; i++) ops.push({ k: "=", a: a[n - e + i], b: b[m - e + i], ai: n - e + i, bi: m - e + i });
    return ops;
  }
  const v = new Map<number, number>([[1, 0]]);
  const trace: Map<number, number>[] = [];
  let found = false;
  for (let d = 0; d <= max && !found; d++) {
    trace.push(new Map(v));
    for (let k = -d; k <= d; k += 2) {
      let x = k === -d || (k !== d && (v.get(k - 1) ?? -1) < (v.get(k + 1) ?? -1)) ? v.get(k + 1) ?? 0 : (v.get(k - 1) ?? 0) + 1;
      let y = x - k;
      while (x < n && y < m && a[x] === b[y]) { x++; y++; }
      v.set(k, x);
      if (x >= n && y >= m) { found = true; break; }
    }
  }
  // 거꾸로 따라가며 연산을 모은다
  const ops: Op[] = [];
  let x = n, y = m;
  for (let d = trace.length - 1; d >= 0; d--) {
    const vd = trace[d];
    const k = x - y;
    const prevK = k === -d || (k !== d && (vd.get(k - 1) ?? -1) < (vd.get(k + 1) ?? -1)) ? k + 1 : k - 1;
    const px = vd.get(prevK) ?? 0, py = px - prevK;
    while (x > px && y > py) { x--; y--; ops.push({ k: "=", a: a[x], b: b[y], ai: x, bi: y }); }
    if (d > 0) {
      if (x === px) { y--; ops.push({ k: "+", b: b[y], bi: y }); } else { x--; ops.push({ k: "-", a: a[x], ai: x }); }
    }
  }
  return ops.reverse();
}

/** 양쪽 나란히 — 다른 줄만 색칠, 같은 줄이 길게 이어지면 접는다 */
function sideBySide(src: string, tgt: string, status: Status): HTMLElement {
  const norm = (s: string) => s.replace(/\r/g, "").split("\n").map((l) => l.replace(/\s+$/, ""));
  const a = norm(src), b = norm(tgt);
  const ops: Op[] = status === "only_source" ? a.map((x, i): Op => ({ k: "-", a: x, ai: i })) : status === "only_target" ? b.map((x, i): Op => ({ k: "+", b: x, bi: i })) : diffLines(a, b);
  const rows: HTMLElement[] = [];
  const cell = (n: number | undefined, text: string | undefined, cls: string) => [h("td", { class: "sc-ln" }, n != null ? String(n + 1) : ""), h("td", { class: `sc-code ${cls}` }, text ?? "")];
  let i = 0;
  let changed = 0;
  while (i < ops.length) {
    const o = ops[i];
    if (o.k === "=") {
      // 같은 줄이 8줄 넘게 이어지면 앞뒤 3줄만
      let j = i;
      while (j < ops.length && ops[j].k === "=") j++;
      const run = ops.slice(i, j);
      const show = run.length > 8 && i !== 0 && j !== ops.length ? [...run.slice(0, 3), null, ...run.slice(-3)]
        : run.length > 8 && i === 0 ? [null, ...run.slice(-3)] : run.length > 8 ? [...run.slice(0, 3), null] : run;
      for (const r of show) {
        rows.push(r ? h("tr", {}, ...cell(r.ai, r.a, ""), ...cell(r.bi, r.b, ""))
          : h("tr", { class: "sc-fold" }, h("td", { colspan: "4" }, `… 같은 줄 ${run.length - (show.length - 1)}줄 …`)));
      }
      i = j;
      continue;
    }
    // 연달은 - 와 + 를 짝지어 한 줄에
    const dels: Op[] = [], adds: Op[] = [];
    while (i < ops.length && ops[i].k !== "=") { (ops[i].k === "-" ? dels : adds).push(ops[i]); i++; }
    changed += Math.max(dels.length, adds.length);
    for (let r = 0; r < Math.max(dels.length, adds.length); r++) {
      const d = dels[r], ad = adds[r];
      rows.push(h("tr", {}, ...cell(d?.ai, d?.a, d ? "sc-del" : "sc-empty"), ...cell(ad?.bi, ad?.b, ad ? "sc-add" : "sc-empty")));
    }
  }
  return h("div", { class: "sc-diff" },
    h("div", { class: "dim", style: "font-size:12px;margin:6px 0" }, `다른 줄 ${changed} · 원본 ${a.length}줄 · 대상 ${b.length}줄`),
    h("table", { class: "sc-table" }, h("thead", {}, h("tr", {}, h("th", { colspan: "2" }, "원본"), h("th", { colspan: "2" }, "대상"))), h("tbody", {}, ...rows)));
}
