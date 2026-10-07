// 세션·락 모니터.
//
// 위: 락 대기 (누가 누구를 막는지, 막힌 객체, 대기 시간).  왼쪽: 세션 목록.  오른쪽: 고른 세션의 SQL·트랜잭션·락.
// 조회는 프로필의 읽기 전용 메타 세션으로 한다. 세션 종료는 확인을 받은 뒤 잠깐 쓰는 별도 세션으로.

import { invoke } from "@tauri-apps/api/core";
import { errOf } from "./api";
import { confirmBox, h, toast } from "./ui";

interface SessionRow {
  sid: number; serial: number; username?: string | null; status: string; kind: string; osuser?: string | null;
  machine?: string | null; program?: string | null; module?: string | null; action?: string | null;
  sql_id?: string | null; prev_sql_id?: string | null; event?: string | null; wait_class?: string | null;
  seconds_in_wait?: number | null; last_call_et?: number | null; blocking_session?: number | null;
  logon_time: string; blocks: number; in_transaction: boolean;
}
interface Wait {
  sid: number; serial: number; username?: string | null; blocker: number; blocker_serial?: number | null;
  blocker_username?: string | null; blocker_status?: string | null; event?: string | null; seconds?: number | null;
  object?: string | null; rowid?: string | null; sql_id?: string | null;
}
interface Detail {
  sid: number; sql_text?: string | null; prev_sql_text?: string | null; tx_start?: string | null;
  tx_undo_blocks?: number | null; locked: { object: string; object_type: string; mode: string }[]; open_cursors?: number | null;
}
interface Snapshot { sessions: SessionRow[]; waits: Wait[]; me?: number | null }

const secs = (n?: number | null) => {
  if (n == null) return "";
  if (n < 60) return `${n}초`;
  if (n < 3600) return `${Math.floor(n / 60)}분 ${n % 60}초`;
  return `${Math.floor(n / 3600)}시간 ${Math.floor((n % 3600) / 60)}분`;
};

export class MonitorView {
  readonly el: HTMLElement;
  private activeBox = h("input", { type: "checkbox", checked: true });
  private bgBox = h("input", { type: "checkbox" });
  private filterIn = h("input", { placeholder: "사용자·프로그램·머신·SQL_ID 찾기", style: "width:220px" });
  private autoSel = h("select", {}, ...[["0", "자동 새로고침 끔"], ["5", "5초마다"], ["10", "10초마다"], ["30", "30초마다"]].map(([v, l]) => h("option", { value: v }, l)));
  private statusEl = h("span", { class: "dbg-status" });
  private waitsEl = h("div", { class: "mon-waits" });
  private listEl = h("tbody", {});
  private detailEl = h("div", { class: "mon-detail an-pad" });
  private snap: Snapshot | null = null;
  private selected: number | null = null;
  private timer: number | null = null;
  private loading = false;

  constructor(private sessionId: () => number | undefined, private onClose: () => void) {
    for (const b of [this.activeBox, this.bgBox]) b.onchange = () => this.refresh();
    this.filterIn.oninput = () => this.render();
    this.autoSel.onchange = () => this.schedule();
    const toolbar = h("div", { class: "dbg-toolbar an-toolbar" },
      h("label", { class: "check", title: "ACTIVE 이거나 다른 세션을 막고 있는 세션만" }, this.activeBox, "활성만"),
      h("label", { class: "check" }, this.bgBox, "백그라운드 포함"),
      this.filterIn, this.autoSel,
      h("button", { class: "primary", onclick: () => this.refresh() }, "새로고침"),
      h("span", { class: "spacer" }), this.statusEl,
      h("button", { onclick: () => this.close() }, "닫기"));
    const table = h("table", { class: "an-table mon-list" },
      h("thead", {}, h("tr", {}, ...["SID", "사용자", "상태", "대기", "경과", "막음", "프로그램 / 머신", "SQL_ID"].map((x) => h("th", {}, x)))),
      this.listEl);
    this.el = h("div", { class: "debugger analysis", hidden: true }, toolbar, this.waitsEl,
      h("div", { class: "an-main mon-main" }, h("div", { class: "an-left" }, table), h("div", { class: "an-right" }, this.detailEl)));
  }

  async open() {
    this.el.hidden = false;
    await this.refresh();
    this.schedule();
  }

  close() {
    this.el.hidden = true;
    if (this.timer != null) clearInterval(this.timer);
    this.timer = null;
    this.onClose();
  }

  private schedule() {
    if (this.timer != null) clearInterval(this.timer);
    this.timer = null;
    const n = Number(this.autoSel.value);
    if (n > 0) this.timer = window.setInterval(() => { if (!this.el.hidden) this.refresh(); }, n * 1000);
  }

  async refresh() {
    const id = this.sessionId();
    if (id == null) return toast("먼저 접속하세요", "error");
    if (this.loading) return;
    this.loading = true;
    const t0 = performance.now();
    try {
      this.snap = await invoke<Snapshot>("monitor_snapshot", { id, activeOnly: this.activeBox.checked, background: this.bgBox.checked });
      this.statusEl.textContent = `세션 ${this.snap.sessions.length} · 락 대기 ${this.snap.waits.length} · ${new Date().toLocaleTimeString()} (${Math.round(performance.now() - t0)}ms)`;
      this.render();
      if (this.selected != null) this.showDetail(this.selected, false);
    } catch (e) {
      this.statusEl.textContent = "";
      this.waitsEl.replaceChildren(h("div", { class: "hint warn" }, errOf(e).message));
    } finally {
      this.loading = false;
    }
  }

  private render() {
    const s = this.snap;
    if (!s) return;
    // 락 대기: 막는 쪽(뿌리)부터 트리로
    if (s.waits.length) {
      const byBlocker = new Map<number, Wait[]>();
      for (const w of s.waits) byBlocker.set(w.blocker, [...(byBlocker.get(w.blocker) ?? []), w]);
      const waiting = new Set(s.waits.map((w) => w.sid));
      const roots = [...byBlocker.keys()].filter((b) => !waiting.has(b));
      const node = (sid: number, depth: number): HTMLElement[] => {
        const kids = byBlocker.get(sid) ?? [];
        return kids.flatMap((w) => [
          h("div", { class: "mon-wait", style: `padding-left:${depth * 18 + 8}px` },
            "↳ ", this.sidLink(w.sid), ` ${w.username ?? ""} 이(가) ${secs(w.seconds)} 기다림`,
            w.object ? h("code", {}, ` ${w.object}`) : "", w.event ? h("small", { class: "dim" }, ` · ${w.event}`) : ""),
          ...node(w.sid, depth + 1),
        ]);
      };
      this.waitsEl.replaceChildren(h("div", { class: "mon-wait-head" }, `락 대기 ${s.waits.length}`),
        ...roots.flatMap((r) => {
          const w = s.waits.find((x) => x.blocker === r)!;
          return [h("div", { class: "mon-wait root" }, "⛔ ", this.sidLink(r), ` ${w.blocker_username ?? ""} (${w.blocker_status ?? ""}) 이(가) 막고 있음 — ${byBlocker.get(r)!.length}개 세션`,
            h("button", { class: "small danger", onclick: () => this.kill(r, w.blocker_serial ?? 0) }, "이 세션 종료")),
            ...node(r, 1)];
        }));
    } else {
      this.waitsEl.replaceChildren();
    }
    const q = this.filterIn.value.trim().toUpperCase();
    const rows = s.sessions.filter((r) => !q || [r.username, r.program, r.machine, r.osuser, r.module, r.sql_id, String(r.sid)].some((x) => (x ?? "").toUpperCase().includes(q)));
    this.listEl.replaceChildren(...rows.map((r) => {
      const tr = h("tr", { class: [r.sid === this.selected ? "active" : "", r.blocking_session ? "mon-blocked" : "", r.blocks ? "mon-blocker" : ""].join(" ") },
        h("td", { class: "num" }, String(r.sid), r.sid === s.me ? h("span", { class: "badge ok", title: "이 탭의 세션" }, "나") : ""),
        h("td", {}, r.username ?? h("span", { class: "dim" }, r.kind)),
        h("td", { class: r.status === "ACTIVE" ? "ok" : "dim" }, r.status, r.in_transaction ? h("span", { class: "badge warn", title: "커밋하지 않은 트랜잭션" }, "TX") : ""),
        h("td", { title: r.wait_class ?? "" }, r.status === "ACTIVE" ? r.event ?? "" : ""),
        h("td", { class: "num" }, secs(r.last_call_et)),
        h("td", { class: "num" }, r.blocks ? String(r.blocks) : r.blocking_session ? `← ${r.blocking_session}` : ""),
        h("td", { class: "dim" }, [r.program, r.machine].filter(Boolean).join(" / ")),
        h("td", {}, r.sql_id ?? ""));
      tr.onclick = () => this.showDetail(r.sid, true);
      return tr;
    }));
  }

  private sidLink(sid: number) {
    const a = h("a", { href: "#", class: "an-link" }, `SID ${sid}`);
    a.onclick = (e) => { e.preventDefault(); this.showDetail(sid, true); };
    return a;
  }

  private async showDetail(sid: number, scroll: boolean) {
    const id = this.sessionId();
    if (id == null) return;
    this.selected = sid;
    if (scroll) this.render();
    const r = this.snap?.sessions.find((x) => x.sid === sid);
    let d: Detail;
    try {
      d = await invoke<Detail>("monitor_detail", { id, sid });
    } catch (e) {
      this.detailEl.replaceChildren(h("div", { class: "hint" }, errOf(e).message));
      return;
    }
    const kv = (k: string, v?: string | number | null) => (v == null || v === "" ? "" : h("tr", {}, h("td", { class: "dim" }, k), h("td", {}, String(v))));
    const sqlBox = (title: string, text?: string | null) => text
      ? h("div", {}, h("h4", {}, title, h("button", { class: "small", style: "margin-left:8px", onclick: () => navigator.clipboard.writeText(text).then(() => toast("복사했습니다", "ok")) }, "복사")),
          h("pre", { class: "an-code" }, text))
      : "";
    this.detailEl.replaceChildren(
      h("div", { class: "an-head" }, h("strong", {}, `SID ${sid}${r ? `, ${r.serial}` : ""}`), r?.username ? h("span", {}, r.username) : "",
        h("span", { class: "spacer" }),
        r && r.sid !== this.snap?.me && r.kind === "USER" ? h("button", { class: "danger", onclick: () => this.kill(r.sid, r.serial) }, "세션 종료") : ""),
      h("table", { class: "dbg-vars" }, h("tbody", {},
        kv("상태", r?.status), kv("OS 사용자", r?.osuser), kv("머신", r?.machine), kv("프로그램", r?.program), kv("모듈 / 액션", [r?.module, r?.action].filter(Boolean).join(" / ")),
        kv("접속 시각", r?.logon_time), kv("마지막 호출 뒤", secs(r?.last_call_et)), kv("대기", r?.event ? `${r.event} (${secs(r.seconds_in_wait)})` : ""),
        kv("막는 세션", r?.blocking_session ?? undefined), kv("트랜잭션 시작", d.tx_start), kv("undo 블록", d.tx_undo_blocks), kv("열린 커서", d.open_cursors))),
      d.locked.length ? h("div", {}, h("h4", {}, "잡고 있는 객체 락"),
        h("ul", { class: "an-ul" }, ...d.locked.map((l) => h("li", {}, h("code", {}, l.object), ` ${l.object_type} · ${l.mode}`)))) : "",
      sqlBox("지금 SQL", d.sql_text), sqlBox("이전 SQL", d.prev_sql_text));
  }

  private async kill(sid: number, serial: number) {
    const id = this.sessionId();
    if (id == null) return;
    const r = this.snap?.sessions.find((x) => x.sid === sid);
    const who = r ? `${r.username ?? ""} · ${r.program ?? ""} · ${r.machine ?? ""}` : "";
    const ok = await confirmBox("세션 종료",
      `SID ${sid}, SERIAL# ${serial} (${who}) 를 종료합니다.\n\n` +
      "진행 중이던 작업과 커밋하지 않은 변경은 롤백됩니다. 운영 DB 라면 담당자와 먼저 확인하세요.\n\n" +
      `실행할 문장: ALTER SYSTEM KILL SESSION '${sid},${serial}' IMMEDIATE`, "종료");
    if (!ok) return;
    try {
      const msg = await invoke<string>("monitor_kill", { id, sid, serial, immediate: true });
      toast(msg, "ok");
    } catch (e) {
      toast(errOf(e).message, "error");
    }
    this.refresh();
  }
}
