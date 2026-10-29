// 추적 화면 왼쪽 — 추적한 문장이 든 소스 전체.
// 탐색 기록의 쿼리마다 소스의 그 자리(문장, 거쳐 온 커서 줄, 조인 단계면 그 테이블이 적힌 줄)에 번호·결과 배지를 단다.
// 배지를 누르면 오른쪽 기록의 그 쿼리로, 오른쪽 기록·카드를 누르면 왼쪽이 그 자리로 간다.

import { invoke } from "@tauri-apps/api/core";
import { errOf } from "./api";
import { h } from "./ui";

/** 탐색 기록 한 줄 — sql 이 없으면 쿼리 없이 길을 고른 것 (gid 는 창 전체의 번호, 쿼리만) */
export interface LogEntry {
  seq: number; at: string; purpose: string; sql?: string | null; binds: [string, string][]; sent: boolean;
  result?: string | null; ms: number; error?: string | null;
  unit?: string | null; line?: number | null; end_line?: number | null; marks: number[]; hint?: string | null;
  gid?: number;
}
export interface SrcLoc { unit: string; line: number; end_line?: number | null; marks?: number[]; hint?: string | null }

/** 결과의 톤 — 0건이면 끊김, 오류, 보내지 않음, 보통 */
export function tone(e: LogEntry): "zero" | "err" | "idle" | "ok" {
  if (!e.sent) return "idle";
  if (e.error) return "err";
  return /^0(건|행)$/.test(e.result ?? "") ? "zero" : "ok";
}

const unitName = (u: string) => { const p = u.split("."); return p.length >= 3 ? `${p[0]}.${p[1]}` : u; };
const unitKind = (u: string) => u.split(".").slice(2).join(".").replace(/_/g, " ");

export class SourcePane {
  readonly el: HTMLElement;
  private tabs: HTMLElement;
  private body: HTMLElement;
  private cache = new Map<string, Promise<string[]>>();
  private entries: LogEntry[] = [];
  private ranges: SrcLoc[] = [];
  private unit: string | null = null;
  private nextGid = 1;
  /** 배지를 눌렀을 때 — 오른쪽 기록의 그 쿼리로 */
  onPick: (gid: number) => void = () => {};

  constructor(private id: number) {
    this.tabs = h("div", { class: "vt-src-tabs" });
    this.body = h("div", { class: "vt-src-body" }, h("div", { class: "vt-src-empty" }, "추적한 문장의 소스가 여기 보입니다"));
    this.el = h("div", { class: "vt-src" }, this.tabs, this.body);
  }

  /** 한 단계의 기록을 더한다 — 쿼리에 창 전체 번호(gid)를 매긴다 */
  add(log: LogEntry[], ranges: SrcLoc[]) {
    for (const e of log) if (e.sql) e.gid = this.nextGid++;
    this.entries.push(...log);
    this.ranges.push(...ranges);
    this.renderTabs();
    if (this.unit) void this.render(this.unit);
    else {
      const first = log.find((e) => e.unit && e.sql) ?? log.find((e) => e.unit);
      const loc = first?.unit ? { unit: first.unit, line: first.line ?? 1, end_line: first.end_line } : ranges[0];
      if (loc) void this.show(loc);
    }
  }

  /** 그 자리로 — 소스를 읽어 그리고, 그 줄로 굴려 잠깐 빛낸다 */
  async show(loc: SrcLoc, gid?: number) {
    if (loc.unit !== this.unit) await this.render(loc.unit);
    const target = (gid != null && this.body.querySelector<HTMLElement>(`.vt-src-badge[data-gid="${gid}"]`)?.closest<HTMLElement>(".vt-src-line"))
      || this.body.querySelector<HTMLElement>(`.vt-src-line[data-n="${loc.line}"]`);
    this.body.querySelectorAll(".focus").forEach((x) => x.classList.remove("focus"));
    const end = loc.end_line ?? loc.line;
    for (let n = loc.line; n <= end; n++) this.body.querySelector(`.vt-src-line[data-n="${n}"]`)?.classList.add("focus");
    if (gid != null) this.body.querySelector(`.vt-src-badge[data-gid="${gid}"]`)?.classList.add("focus");
    target?.scrollIntoView({ block: "center", behavior: "smooth" });
    target?.classList.remove("flash");
    void target?.offsetWidth;
    target?.classList.add("flash");
  }

  private units(): string[] {
    const out: string[] = [];
    for (const u of [...this.entries.map((e) => e.unit), ...this.ranges.map((r) => r.unit)]) if (u && !out.includes(u)) out.push(u);
    return out;
  }

  private renderTabs() {
    const us = this.units();
    this.tabs.replaceChildren(...us.map((u) => {
      const n = this.entries.filter((e) => e.unit === u && e.sql).length;
      return h("button", { class: `vt-src-tab${u === this.unit ? " on" : ""}`, title: u, onclick: () => void this.show({ unit: u, line: this.firstLine(u) }) },
        h("b", {}, unitName(u)), h("span", { class: "dim" }, ` ${unitKind(u)}`), n ? h("span", { class: "vt-src-n" }, `쿼리 ${n}`) : "");
    }));
  }

  private firstLine(u: string): number {
    const ls = [...this.entries.filter((e) => e.unit === u).map((e) => e.line ?? 0), ...this.ranges.filter((r) => r.unit === u).map((r) => r.line)].filter((x) => x > 0);
    return ls.length ? Math.min(...ls) : 1;
  }

  private load(unit: string): Promise<string[]> {
    let p = this.cache.get(unit);
    if (!p) {
      p = invoke<string[]>("trace_source", { id: this.id, unit });
      this.cache.set(unit, p);
    }
    return p;
  }

  private async render(unit: string) {
    this.unit = unit;
    this.renderTabs();
    let lines: string[];
    try {
      lines = await this.load(unit);
    } catch (e) {
      this.body.replaceChildren(h("div", { class: "vt-src-empty" }, `${unitName(unit)} 의 소스를 읽지 못했습니다 — ${errOf(e).message}`));
      return;
    }
    if (this.unit !== unit) return;
    // 줄마다: 추적한 문장 안인지, 거쳐 온 줄인지, 배지
    const inStmt = new Set<number>();
    const marked = new Set<number>();
    const badges = new Map<number, LogEntry[]>();
    const mine = this.entries.filter((e) => e.unit === unit);
    for (const r of [...this.ranges.filter((r) => r.unit === unit), ...mine.filter((e) => e.line).map((e) => ({ unit, line: e.line!, end_line: e.end_line, marks: e.marks }))]) {
      for (let n = r.line; n <= (r.end_line ?? r.line); n++) inStmt.add(n);
      for (const m of r.marks ?? []) marked.add(m);
    }
    for (const e of mine) {
      if (!e.sql || !e.line) continue;
      const at = this.placeOf(e, lines);
      badges.set(at, [...(badges.get(at) ?? []), e]);
    }
    const frag = document.createDocumentFragment();
    lines.forEach((text, i) => {
      const n = i + 1;
      const bs = badges.get(n) ?? [];
      frag.append(h("div", { class: `vt-src-line${inStmt.has(n) ? " in" : ""}${marked.has(n) ? " mark" : ""}${bs.length ? " has" : ""}`, "data-n": String(n) },
        h("span", { class: "vt-src-no" }, String(n)),
        h("span", { class: "vt-src-text" }, text || " "),
        bs.length ? h("span", { class: "vt-src-badges" }, ...bs.map((e) => h("button", {
          class: `vt-src-badge t-${tone(e)}`, "data-gid": String(e.gid), title: `#${e.gid} ${e.purpose}${e.error ? ` — ${e.error}` : ""}`,
          onclick: () => this.onPick(e.gid!),
        }, h("b", {}, `#${e.gid}`), ` ${e.sent ? (e.error ? "오류" : e.result ?? "") : "안 보냄"}`))) : ""));
    });
    this.body.replaceChildren(frag);
  }

  /** 배지를 달 줄 — 짚을 낱말(조인 테이블·변수)이 적힌 줄, 커서를 거쳤으면 커서 줄, 아니면 문장 첫 줄 */
  private placeOf(e: LogEntry, lines: string[]): number {
    const line = e.line!;
    if (e.hint) {
      const word = (e.hint.split(".").pop() ?? e.hint).replace(/[$#]/g, "\\$&");
      const re = new RegExp(`(^|[^A-Za-z0-9_$#])${word}([^A-Za-z0-9_$#]|$)`, "i");
      const spans: [number, number][] = [[line, e.end_line ?? line], ...e.marks.map((m) => [m, m + 80] as [number, number])];
      for (const [a, b] of spans) for (let n = a; n <= Math.min(b, lines.length); n++) if (re.test(lines[n - 1] ?? "")) return n;
    }
    return e.marks.length ? e.marks[e.marks.length - 1] : line;
  }
}
