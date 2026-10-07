// PL/SQL 디버거 화면.
//
// 왼쪽: 소스 (줄 번호 칸을 눌러 중단점), 지금 줄 강조.  오른쪽: 변수·감시·호출 스택.
// 아래: 실행 블록(호출 템플릿) · 출력.  키: F5 시작/계속, F10 넘기기, F11 들어가기, Shift+F11 나오기.
//
// 디버그는 블록을 실제로 실행한다. 대상 세션의 변경은 끝날 때 기본으로 롤백한다.

import { invoke } from "@tauri-apps/api/core";
import { PLSQL, sql } from "@codemirror/lang-sql";
import { syntaxHighlighting, defaultHighlightStyle } from "@codemirror/language";
import { EditorState, RangeSet, StateEffect, StateField } from "@codemirror/state";
import { Decoration, EditorView, GutterMarker, gutter, lineNumbers, type DecorationSet } from "@codemirror/view";
import { api, errOf } from "./api";
import { bindBox, confirmBox, h, modal, toast } from "./ui";

interface Stop {
  terminated: boolean; line: number; owner: string; name: string; unit_type: string;
  reason: string; depth: number; breakpoint?: number | null; ora_code?: number | null;
}
interface Frame { depth: number; owner: string; name: string; unit_type: string; line: number }
interface VarValue { name: string; value?: string | null; error?: string | null }
interface Snapshot { stop: Stop; stack: Frame[]; vars: VarValue[]; finished?: { output: string[]; error?: string | null } }
interface Entry { label: string; template: string }
interface Prepared { source: string[]; source_type: string; debug_info?: boolean | null; compile_sql?: string | null; entries: Entry[] }

type Step = "into" | "over" | "out" | "run" | "abort";
const ANON = "ANONYMOUS BLOCK";

interface Unit { owner: string; name: string; type: string; source: string[] }
const keyOf = (o: string, n: string, t: string) => `${o}.${n}.${t}`;

// ── 소스 보기: 중단점 칸 + 지금 줄 ────────────────────────
class BpMarker extends GutterMarker {
  toDOM() {
    const d = document.createElement("span");
    d.className = "bp-dot";
    d.textContent = "●";
    return d;
  }
}
const bpMarker = new BpMarker();
const setBps = StateEffect.define<number[]>();
const bpField = StateField.define<RangeSet<GutterMarker>>({
  create: () => RangeSet.empty,
  update(set, tr) {
    for (const e of tr.effects) {
      if (e.is(setBps)) {
        const doc = tr.state.doc;
        set = RangeSet.of(
          e.value.filter((l) => l >= 1 && l <= doc.lines).sort((a, b) => a - b).map((l) => bpMarker.range(doc.line(l).from)),
        );
      }
    }
    return set;
  },
});
const setCurrent = StateEffect.define<number | null>();
const currentField = StateField.define<DecorationSet>({
  create: () => Decoration.none,
  update(deco, tr) {
    for (const e of tr.effects) {
      if (e.is(setCurrent)) {
        const doc = tr.state.doc;
        deco = e.value && e.value >= 1 && e.value <= doc.lines
          ? Decoration.set([Decoration.line({ class: "dbg-current" }).range(doc.line(e.value).from)])
          : Decoration.none;
      }
    }
    return deco;
  },
  provide: (f) => EditorView.decorations.from(f),
});

export class DebugView {
  readonly el: HTMLElement;
  private sourceView: EditorView;
  private blockView: EditorView;
  private unitSel = h("select", { class: "dbg-unit" });
  private entrySel = h("select", { class: "dbg-entry" });
  private statusEl = h("span", { class: "dbg-status" }, "준비");
  private varsEl = h("tbody", {});
  private watchEl = h("tbody", {});
  private stackEl = h("ol", { class: "dbg-stack" });
  private outputEl = h("pre", { class: "dbg-output" });
  private watchInput = h("input", { placeholder: "감시할 이름 (예: v_total, rec.id)" });
  private excBox = h("input", { type: "checkbox", checked: true });
  private commitBox = h("input", { type: "checkbox" });
  private btn: Record<string, HTMLButtonElement> = {};
  private units = new Map<string, Unit>();
  private shownKey = "";
  /** 단위 → 중단점 줄 → (디버그 중이면) 서버 중단점 번호 */
  private bps = new Map<string, Map<number, number | null>>();
  private watches: string[] = [];
  private did: number | null = null;
  private busy = false;
  private stop: Stop | null = null;
  private keyHandler = (e: KeyboardEvent) => this.onKey(e);

  constructor(private sessionId: () => number | undefined, private onClose: () => void) {
    const b = (k: string, label: string, title: string, f: () => void, cls = "") =>
      (this.btn[k] = h("button", { class: cls, title, onclick: f }, label));
    const toolbar = h("div", { class: "dbg-toolbar" },
      b("start", "▶ 시작", "F5 — 실행 블록을 디버그로 시작 / 계속", () => this.startOrRun(), "primary"),
      b("over", "↷ 넘기기", "F10 — 다음 줄 (호출은 넘김)", () => this.step("over")),
      b("into", "↓ 들어가기", "F11 — 호출 안으로", () => this.step("into")),
      b("out", "↑ 나오기", "Shift+F11 — 지금 서브프로그램을 끝내고 부른 곳으로", () => this.step("out")),
      b("stop", "■ 중지", "실행을 멈추고 디버그를 끝낸다 (변경은 롤백)", () => this.abort(), "danger"),
      h("label", { class: "check", title: "예외가 나는 줄에서 멈춘다" }, this.excBox, "예외에서 멈춤"),
      h("label", { class: "check", title: "끝날 때 대상 세션의 변경을 커밋한다 (기본: 롤백)" }, this.commitBox, "끝나면 커밋"),
      h("span", { class: "spacer" }),
      this.statusEl,
      h("button", { title: "에디터로 돌아간다", onclick: () => this.close() }, "닫기"),
    );

    this.sourceView = new EditorView({
      state: EditorState.create({
        doc: "",
        extensions: [
          EditorState.readOnly.of(true),
          bpField,
          currentField,
          gutter({
            class: "dbg-bp-gutter",
            markers: (v) => v.state.field(bpField),
            initialSpacer: () => bpMarker,
            domEventHandlers: {
              mousedown: (view, line) => {
                this.toggleBreakpoint(view.state.doc.lineAt(line.from).number);
                return true;
              },
            },
          }),
          lineNumbers({
            domEventHandlers: {
              mousedown: (view, line) => {
                this.toggleBreakpoint(view.state.doc.lineAt(line.from).number);
                return true;
              },
            },
          }),
          sql({ dialect: PLSQL }),
          syntaxHighlighting(defaultHighlightStyle, { fallback: true }),
          EditorView.theme({ "&": { height: "100%" }, ".cm-scroller": { fontFamily: "var(--mono)" } }),
        ],
      }),
    });
    this.blockView = new EditorView({
      state: EditorState.create({
        doc: "BEGIN\n  NULL;\nEND;",
        extensions: [lineNumbers(), sql({ dialect: PLSQL }), syntaxHighlighting(defaultHighlightStyle, { fallback: true }),
          EditorView.theme({ "&": { height: "100%" }, ".cm-scroller": { fontFamily: "var(--mono)" } })],
      }),
    });

    this.unitSel.addEventListener("change", () => this.showUnit(this.unitSel.value));
    this.entrySel.addEventListener("change", () => {
      const t = this.entrySel.selectedOptions[0]?.dataset.template;
      if (t) this.setBlock(t);
    });
    this.watchInput.addEventListener("keydown", (e) => {
      if (e.key === "Enter" && this.watchInput.value.trim()) {
        this.watches.push(this.watchInput.value.trim());
        this.watchInput.value = "";
        this.refreshWatches();
      }
    });

    const side = h("div", { class: "dbg-side" },
      h("h4", {}, "변수"), h("table", { class: "dbg-vars" }, this.varsEl),
      h("h4", {}, "감시"), this.watchInput, h("table", { class: "dbg-vars" }, this.watchEl),
      h("h4", {}, "호출 스택"), this.stackEl);
    const sourcePane = h("div", { class: "dbg-source" }, h("div", { class: "dbg-source-head" }, "소스", this.unitSel,
      h("small", {}, "줄 번호를 눌러 중단점")), this.sourceView.dom);
    const bottom = h("div", { class: "dbg-bottom" },
      h("div", { class: "dbg-block" }, h("div", { class: "dbg-source-head" }, "실행 블록", this.entrySel,
        h("small", {}, "바인드(:이름)는 시작할 때 묻는다")), this.blockView.dom),
      h("div", { class: "dbg-out" }, h("div", { class: "dbg-source-head" }, "출력"), this.outputEl));
    this.el = h("div", { class: "debugger", hidden: true }, toolbar,
      h("div", { class: "dbg-main" }, sourcePane, side), bottom);
    this.refresh();
  }

  // ── 열기 ─────────────────────────────────────────

  /** 단위(패키지/프로시저/함수/트리거)를 연다 */
  async openUnit(owner: string, name: string, type: string) {
    const id = this.sessionId();
    if (id == null) return toast("먼저 접속하세요", "error");
    let p: Prepared;
    try {
      p = await invoke<Prepared>("debug_prepare", { id, owner, name, unitType: type });
    } catch (e) {
      return toast(errOf(e).message, "error");
    }
    const key = keyOf(owner, name, p.source_type);
    this.units.set(key, { owner, name, type: p.source_type, source: p.source });
    this.entrySel.replaceChildren(h("option", { value: "" }, "— 호출할 서브프로그램 —"),
      ...p.entries.map((e) => { const o = h("option", { value: e.label }, e.label); o.dataset.template = e.template; return o; }));
    if (p.entries.length === 1) this.setBlock(p.entries[0].template);
    this.renderUnitList();
    this.showUnit(key);
    this.show();
    if (p.debug_info === false && p.compile_sql) {
      const ok = await confirmBox("디버그 정보 없음",
        `${owner}.${name} 은(는) 디버그 정보 없이 컴파일되어 있습니다. 중단점이 걸리지 않습니다.\n\n` +
        `${p.compile_sql}\n\n을 실행할까요? 다시 컴파일하므로 운영 DB 에서는 주의하세요 (이 단위를 쓰는 세션이 잠깐 기다릴 수 있음).`,
        "디버그로 컴파일");
      if (ok) {
        try {
          await invoke("debug_compile", { id, owner, name, unitType: p.source_type });
          toast("디버그 정보로 컴파일했습니다", "ok");
        } catch (e) {
          toast(errOf(e).message, "error", 8000);
        }
      }
    }
  }

  /** 에디터의 익명 블록을 연다 */
  openBlock(text: string) {
    this.setBlock(text);
    this.show();
  }

  private setBlock(t: string) {
    this.blockView.dispatch({ changes: { from: 0, to: this.blockView.state.doc.length, insert: t } });
  }

  show() {
    this.el.hidden = false;
    window.addEventListener("keydown", this.keyHandler, true);
    this.sourceView.requestMeasure();
    this.blockView.requestMeasure();
  }

  async close() {
    if (this.did != null && !(await confirmBox("디버그 중", "디버그를 끝낼까요? 대상 세션의 변경은 롤백됩니다.", "끝내기"))) return;
    await this.finish(false);
    this.el.hidden = true;
    window.removeEventListener("keydown", this.keyHandler, true);
    this.onClose();
  }

  private onKey(e: KeyboardEvent) {
    if (this.el.hidden) return;
    const k = e.key;
    const handled =
      k === "F5" ? (this.startOrRun(), true) :
      k === "F10" ? (this.step("over"), true) :
      k === "F11" && e.shiftKey ? (this.step("out"), true) :
      k === "F11" ? (this.step("into"), true) : false;
    if (handled) {
      e.preventDefault();
      e.stopPropagation();
    }
  }

  // ── 소스·중단점 ───────────────────────────────────

  private renderUnitList() {
    this.unitSel.replaceChildren(...[...this.units.entries()].map(([k, u]) =>
      h("option", { value: k, selected: k === this.shownKey }, u.name ? `${u.owner}.${u.name} (${u.type})` : "실행 블록")));
  }

  private showUnit(key: string) {
    const u = this.units.get(key);
    if (!u) return;
    this.shownKey = key;
    this.unitSel.value = key;
    this.sourceView.dispatch({
      changes: { from: 0, to: this.sourceView.state.doc.length, insert: u.source.join("\n") },
      effects: [setBps.of([...(this.bps.get(key)?.keys() ?? [])]), setCurrent.of(null)],
    });
    if (this.stop && !this.stop.terminated && keyOf(this.stop.owner, this.stop.name, this.stop.unit_type) === key) {
      this.markLine(this.stop.line);
    }
  }

  private markLine(line: number) {
    const doc = this.sourceView.state.doc;
    if (line < 1 || line > doc.lines) return;
    this.sourceView.dispatch({
      effects: [setCurrent.of(line), EditorView.scrollIntoView(doc.line(line).from, { y: "center" })],
    });
  }

  private async toggleBreakpoint(line: number) {
    const u = this.units.get(this.shownKey);
    if (!u || !u.name) return toast("중단점은 저장된 단위(패키지·프로시저 등)에만 둘 수 있습니다", "info");
    const map = this.bps.get(this.shownKey) ?? new Map<number, number | null>();
    this.bps.set(this.shownKey, map);
    if (map.has(line)) {
      const id = map.get(line);
      map.delete(line);
      if (this.did != null && id != null) invoke("debug_clear_breakpoint", { did: this.did, bp: id }).catch(() => {});
    } else {
      map.set(line, null);
      if (this.did != null && !this.busy) {
        try {
          map.set(line, await invoke<number>("debug_breakpoint", { did: this.did, owner: u.owner, name: u.name, unitType: u.type, line }));
        } catch (e) {
          map.delete(line);
          toast(errOf(e).message, "error");
        }
      }
    }
    this.sourceView.dispatch({ effects: setBps.of([...map.keys()]) });
  }

  // ── 실행 ─────────────────────────────────────────

  private async startOrRun() {
    if (this.busy) return;
    if (this.did != null) return this.step("run");
    const id = this.sessionId();
    if (id == null) return toast("먼저 접속하세요", "error");
    const block = this.blockView.state.doc.toString();
    if (!block.trim()) return toast("실행 블록이 비어 있습니다", "error");
    // 바인드
    let binds: [string, string | null][] = [];
    try {
      const a = await api.analyze(block, 0, block);
      if (a.binds.length) {
        const b = await bindBox(a.binds);
        if (!b) return;
        binds = b;
      }
    } catch { /* 바인드 없이 */ }
    const breakpoints: { owner: string; name: string; unit_type: string; line: number }[] = [];
    for (const [k, lines] of this.bps) {
      const u = this.units.get(k);
      if (u?.name) for (const l of lines.keys()) breakpoints.push({ owner: u.owner, name: u.name, unit_type: u.type, line: l });
    }
    this.outputEl.textContent = "";
    this.setBusy(true, "디버그 세션을 여는 중…");
    try {
      const r = await invoke<{ did: number; snapshot: Snapshot; breakpoints: { owner: string; name: string; line: number; id?: number | null; error?: string | null }[] }>(
        "debug_start", { id, block, binds, breakpoints, breakOnException: this.excBox.checked });
      this.did = r.did;
      this.units.set(keyOf("", "", ANON), { owner: "", name: "", type: ANON, source: block.split("\n") });
      for (const b of r.breakpoints) {
        const k = [...this.units.entries()].find(([, u]) => u.owner === b.owner && u.name === b.name)?.[0];
        const map = k ? this.bps.get(k) : undefined;
        if (map && b.id != null) map.set(b.line, b.id);
        if (b.error) {
          map?.delete(b.line);
          toast(b.error, "error", 8000);
        }
      }
      this.renderUnitList();
      await this.apply(r.snapshot);
    } catch (e) {
      this.setBusy(false, "시작 실패");
      toast(errOf(e).message, "error", 10000);
    }
  }

  private async step(s: Step) {
    if (this.did == null || this.busy) return;
    this.setBusy(true, s === "run" ? "실행 중… (오래 걸리면 중지)" : "한 걸음…");
    try {
      const snap = await invoke<Snapshot>("debug_step", { did: this.did, step: s, breakOnException: this.excBox.checked });
      await this.apply(snap);
    } catch (e) {
      this.setBusy(false, "오류");
      toast(errOf(e).message, "error", 8000);
    }
  }

  private async abort() {
    if (this.did == null) return;
    if (this.busy) {
      // 돌고 있는 중 — 대상을 끊는다
      invoke("debug_interrupt", { did: this.did }).catch(() => {});
      return;
    }
    await this.step("abort");
  }

  private async apply(snap: Snapshot) {
    this.stop = snap.stop;
    if (snap.stop.terminated) {
      const f = snap.finished;
      if (f?.output.length) this.outputEl.textContent = f.output.join("\n");
      if (f?.error) this.outputEl.textContent += (this.outputEl.textContent ? "\n\n" : "") + f.error;
      this.sourceView.dispatch({ effects: setCurrent.of(null) });
      await this.finish(this.commitBox.checked);
      this.setBusy(false, snap.stop.reason === "abort" ? "중지했습니다" : f?.error ? "오류로 끝났습니다" : "끝났습니다");
      return;
    }
    const st = snap.stop;
    const key = keyOf(st.owner, st.name, st.unit_type);
    if (!this.units.has(key) && st.name) {
      try {
        const src = await invoke<string[]>("debug_source", { did: this.did, owner: st.owner, name: st.name, unitType: st.unit_type });
        this.units.set(key, { owner: st.owner, name: st.name, type: st.unit_type, source: src });
        this.renderUnitList();
      } catch { /* 소스를 못 읽어도 계속 */ }
    }
    if (this.shownKey !== key) this.showUnit(key);
    this.markLine(st.line);
    this.renderVars(this.varsEl, snap.vars, 0);
    this.renderStack(snap.stack);
    await this.refreshWatches();
    const why = { breakpoint: "중단점", step: "한 걸음", exception: `예외 ORA-${String(st.ora_code ?? 0).padStart(5, "0")}`, start: "시작" }[st.reason] ?? st.reason;
    this.setBusy(false, `${st.name || "실행 블록"} ${st.line}행 — ${why}`);
  }

  private renderVars(el: HTMLElement, vars: VarValue[], frame: number) {
    el.replaceChildren(...vars.map((v) => {
      const val = h("td", { class: v.error ? "dim" : v.value == null ? "null" : "", title: v.error ?? "더블클릭해서 바꾸기" },
        v.error ?? v.value ?? "NULL");
      val.addEventListener("dblclick", () => this.editVar(v.name, frame));
      return h("tr", {}, h("td", {}, v.name), val);
    }));
    if (!vars.length) el.append(h("tr", {}, h("td", { class: "dim", colSpan: 2 }, "없음")));
  }

  private renderStack(stack: Frame[]) {
    this.stackEl.replaceChildren(...stack.map((f, i) => {
      const li = h("li", { class: i === 0 ? "active" : "" }, `${f.name ? `${f.owner}.${f.name}` : "실행 블록"} : ${f.line}`);
      li.addEventListener("click", async () => {
        for (const x of this.stackEl.children) x.classList.remove("active");
        li.classList.add("active");
        const key = keyOf(f.owner, f.name, f.unit_type);
        if (this.units.has(key)) {
          this.showUnit(key);
          this.markLine(f.line);
        }
        try {
          const vars = await invoke<VarValue[]>("debug_frame_vars", { did: this.did, frame: f });
          this.renderVars(this.varsEl, vars, i === 0 ? 0 : f.depth);
        } catch (e) {
          toast(errOf(e).message, "error");
        }
      });
      return li;
    }));
  }

  private async refreshWatches() {
    if (this.did == null || !this.watches.length) {
      this.watchEl.replaceChildren();
      return;
    }
    const vals: VarValue[] = [];
    for (const w of this.watches) {
      try {
        vals.push(await invoke<VarValue>("debug_eval", { did: this.did, name: w, frame: 0 }));
      } catch (e) {
        vals.push({ name: w, error: errOf(e).message });
      }
    }
    this.renderVars(this.watchEl, vals, 0);
    // 감시 지우기
    [...this.watchEl.children].forEach((tr, i) => {
      tr.append(h("td", {}, h("button", { class: "small", title: "감시 지우기", onclick: () => { this.watches.splice(i, 1); this.refreshWatches(); } }, "×")));
    });
  }

  private async editVar(name: string, frame: number) {
    if (this.did == null || this.busy) return;
    const input = h("input", { placeholder: "새 값 (문자열은 '따옴표', 날짜는 DATE '2026-01-01')" });
    const ok = await modal(`${name} 바꾸기`, h("label", { class: "field" }, `${name} :=`, input), [
      { label: "취소", value: false }, { label: "바꾸기", value: true, kind: "primary" },
    ]);
    if (!ok || !input.value.trim()) return;
    try {
      await invoke("debug_set", { did: this.did, frame, assignment: `${name} := ${input.value.trim().replace(/;$/, "")};` });
      const v = await invoke<VarValue>("debug_eval", { did: this.did, name, frame });
      toast(`${name} = ${v.value ?? "NULL"}`, "ok");
      // 화면 갱신
      const row = [...this.varsEl.children].find((tr) => tr.firstElementChild?.textContent === name);
      if (row?.lastElementChild) row.lastElementChild.textContent = v.value ?? "NULL";
    } catch (e) {
      toast(errOf(e).message, "error");
    }
  }

  private async finish(commit: boolean) {
    const did = this.did;
    this.did = null;
    this.stop = null;
    for (const m of this.bps.values()) for (const k of m.keys()) m.set(k, null);
    if (did == null) return;
    try {
      const r = await invoke<{ output: string[]; error?: string | null } | null>("debug_finish", { did, commit });
      if (r && !this.outputEl.textContent) {
        this.outputEl.textContent = [...r.output, ...(r.error ? ["", r.error] : [])].join("\n");
      }
      if (commit) toast("대상 세션의 변경을 커밋했습니다", "ok");
    } catch { /* 이미 끝남 */ }
    this.varsEl.replaceChildren();
    this.stackEl.replaceChildren();
    this.refresh();
  }

  private setBusy(b: boolean, status: string) {
    this.busy = b;
    this.statusEl.textContent = status;
    this.refresh();
  }

  private refresh() {
    const running = this.did != null;
    this.btn.start.textContent = running ? "▶ 계속" : "▶ 시작";
    this.btn.start.disabled = this.busy;
    for (const k of ["over", "into", "out"]) this.btn[k].disabled = !running || this.busy;
    this.btn.stop.disabled = !running;
    this.btn.stop.textContent = this.busy && running ? "■ 끊기" : "■ 중지";
  }
}
