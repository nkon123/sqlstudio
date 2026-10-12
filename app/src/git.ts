// 프로젝트(사내 Git) 패널 — 왼쪽 영역의 "프로젝트" 탭.
//
// 저장소 열기/복제 · 파일 트리(상태 표시) · 바뀐 파일 비교 · 고른 파일 커밋 · 풀/푸시 · 브랜치 전환.
// 실제 작업은 PC 의 git 이 한다 (src-tauri/src/git.rs). 인증은 git 의 자격 증명 관리자가 맡고 이 화면은 비밀번호를 받지 않는다.

import { invoke } from "@tauri-apps/api/core";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { errOf } from "./api";
import { alertBox, confirmBox, h, modal, toast } from "./ui";

interface RepoInfo { root: string; name: string; remote?: string | null }
interface FileState { path: string; orig?: string | null; code: string }
interface Status { branch?: string | null; upstream?: string | null; ahead: number; behind: number; files: FileState[] }
interface Branches { current?: string | null; local: string[]; remote: string[] }
interface Diff { old?: string | null; new?: string | null; encoding: string; binary: boolean }

/** 편집기 쪽 — main.ts 의 App 이 채운다 */
export interface GitHost {
  /** 파일을 편집기 탭으로 연다 (이미 열려 있으면 그 탭으로) */
  openPath(path: string): Promise<void>;
  /** 이 폴더 아래 파일을 연 탭들 */
  tabsUnder(root: string): { path: string; dirty: boolean; reload: () => Promise<void> }[];
  /** 상태줄 표시 */
  onState(text: string, title: string): void;
}

const CODE_LABEL: Record<string, string> = { M: "수정", A: "추가", D: "삭제", R: "이름 바뀜", "?": "새 파일 (아직 Git 이 모름)", U: "충돌" };
const RECENT_KEY = "git.recent";
const LAST_KEY = "git.last";
const FETCH_EVERY = 10 * 60 * 1000;

const norm = (p: string) => p.replace(/\\/g, "/").replace(/\/+$/, "").toLowerCase();
const join = (root: string, rel: string) => `${root.replace(/[\\/]+$/, "")}/${rel}`;

function loadList(key: string): string[] {
  try { const v = JSON.parse(localStorage.getItem(key) ?? "[]"); return Array.isArray(v) ? v : []; } catch { return []; }
}

/** 실패 종류별 안내 */
function explain(e: unknown): string {
  const x = errOf(e);
  const tip: Record<string, string> = {
    auth: "인증에 실패했습니다. Windows '자격 증명 관리자'에서 이 Git 서버 항목을 지우고 다시 시도하면 로그인 창이 다시 뜹니다. (토큰을 쓰는 서버면 비밀번호 자리에 토큰)",
    cert: "사내 인증서를 git 이 믿지 못합니다. 보통 이 명령 한 번으로 해결됩니다 (Windows 인증서 저장소를 쓰게 함):\n  git config --global http.sslBackend schannel",
    network: "Git 서버에 연결하지 못했습니다. 주소·사내망 연결·프록시 설정을 확인하세요.",
    git_missing: "",
  };
  return tip[x.kind] ? `${tip[x.kind]}\n\n${x.message}` : x.message;
}

export class GitPanel {
  private repo: RepoInfo | null = null;
  private st: Status | null = null;
  private files: string[] = [];
  private open = new Set<string>();
  private checked = new Map<string, boolean>();
  private filterIn = h("input", { placeholder: "파일 찾기", class: "git-filter" });
  private treeEl = h("div", { class: "git-tree" });
  private chgEl = h("div", { class: "git-changes-list" });
  private chgCount = h("b", {});
  private msgIn = h("textarea", { rows: 2, placeholder: "커밋 메시지 (Ctrl+Enter)" });
  private commitBtn = h("button", { class: "primary small" }, "커밋");
  private pushAfter = h("input", { type: "checkbox" });
  private branchSel = h("select", { class: "git-branch", title: "브랜치" });
  private abEl = h("span", { class: "git-ab" });
  private busyEl = h("span", { class: "git-busy" });
  private busy = false;
  private fetchTimer = 0;
  private refreshTimer = 0;
  /** 패널이 보일 때만 트리를 그린다. 숨어 있는 동안 바뀌면 보일 때 그린다 */
  private visible = false;
  private treeStale = true;
  /** 상태 읽기가 진행 중이면 겹쳐 부르지 않는다 */
  private reading: Promise<void> | null = null;

  constructor(private host: GitHost, readonly el: HTMLElement = h("div", { class: "git" })) {
    this.filterIn.addEventListener("input", () => this.renderTree());
    this.commitBtn.onclick = () => this.commit();
    this.msgIn.addEventListener("keydown", (e) => { if (e.key === "Enter" && e.ctrlKey) { e.preventDefault(); this.commit(); } });
    this.branchSel.onchange = () => this.switchBranch();
    // 창으로 돌아오면 상태만 가볍게 다시 본다 (파일 목록·브랜치는 바뀐 게 보일 때만)
    window.addEventListener("focus", () => this.refreshSoon());
    this.renderEmpty();
    const last = localStorage.getItem(LAST_KEY);
    if (last) this.openRepo(last, true);
  }

  // ── 열기 ──

  private renderEmpty() {
    this.repo = null;
    this.host.onState("", "");
    const recent = loadList(RECENT_KEY);
    this.el.replaceChildren(h("div", { class: "git-empty" },
      h("p", {}, "사내 Git 저장소의 SQL·PL/SQL 파일을 열고 커밋·푸시·풀 합니다."),
      h("div", { class: "row" },
        h("button", { class: "primary", onclick: () => this.pickFolder() }, "폴더 열기"),
        h("button", { onclick: () => this.cloneDialog() }, "저장소 복제")),
      recent.length ? h("h4", {}, "최근") : "",
      ...recent.map((p) => h("a", { href: "#", class: "git-recent", title: p, onclick: (e: Event) => { e.preventDefault(); this.openRepo(p); } }, p)),
      h("p", { class: "hint" }, "인증은 Git 이 맡습니다 — 처음 한 번 로그인 창이 뜨고 Windows 자격 증명 관리자에 저장됩니다. SQLStudio 는 비밀번호를 받지 않습니다.")));
  }

  private async pickFolder() {
    const p = await openDialog({ directory: true, multiple: false, title: "Git 저장소 폴더" });
    if (p && !Array.isArray(p)) this.openRepo(p);
  }

  async openRepo(path: string, quiet = false) {
    try {
      this.repo = await invoke<RepoInfo>("git_open", { path });
    } catch (e) {
      if (!quiet) toast(explain(e), "error", 8000);
      if (quiet) localStorage.removeItem(LAST_KEY);
      return this.renderEmpty();
    }
    localStorage.setItem(LAST_KEY, this.repo.root);
    localStorage.setItem(RECENT_KEY, JSON.stringify([this.repo.root, ...loadList(RECENT_KEY).filter((x) => norm(x) !== norm(this.repo!.root))].slice(0, 8)));
    this.open.clear();
    this.checked.clear();
    this.renderRepo();
    await this.reload();
    this.fetchQuiet();
    clearInterval(this.fetchTimer);
    this.fetchTimer = window.setInterval(() => this.fetchQuiet(), FETCH_EVERY);
  }

  private close() {
    clearInterval(this.fetchTimer);
    localStorage.removeItem(LAST_KEY);
    this.renderEmpty();
  }

  private async cloneDialog() {
    const url = h("input", { placeholder: "https://git.회사/그룹/저장소.git 또는 git@…", style: "width:100%" });
    const parent = h("input", { placeholder: "받을 위치 (상위 폴더)", style: "flex:1" });
    const name = h("input", { placeholder: "폴더 이름", style: "width:160px" });
    parent.value = localStorage.getItem("git.cloneParent") ?? "";
    url.addEventListener("input", () => { name.value = url.value.trim().replace(/\/+$/, "").split(/[/:]/).pop()!.replace(/\.git$/, ""); });
    const pick = h("button", { onclick: async () => { const p = await openDialog({ directory: true }); if (p && !Array.isArray(p)) parent.value = p; } }, "찾기");
    const body = h("div", { class: "git-form" },
      h("label", {}, "저장소 주소"), url,
      h("label", {}, "받을 폴더"), h("div", { class: "row" }, parent, pick, name),
      h("p", { class: "hint" }, "ID·비밀번호(또는 토큰)는 Git 의 로그인 창이 묻습니다. 주소에 비밀번호를 넣지 마세요."));
    const ok = await modal("저장소 복제", body, [{ label: "취소", value: false }, { label: "복제", value: true, kind: "primary" }]);
    if (!ok) return;
    if (!url.value.trim() || !parent.value.trim() || !name.value.trim()) return toast("주소와 받을 폴더를 채우세요", "error");
    localStorage.setItem("git.cloneParent", parent.value.trim());
    const dir = join(parent.value.trim(), name.value.trim());
    await this.task(`복제 중… ${url.value.trim()}`, async () => {
      const r = await invoke<RepoInfo>("git_clone", { url: url.value.trim(), dir });
      toast(`복제했습니다: ${r.root}`, "ok");
      await this.openRepo(r.root);
    });
  }

  // ── 화면 ──

  private renderRepo() {
    const r = this.repo!;
    const pull = h("button", { class: "small", title: "서버의 새 커밋 받기 (빨리 감기만)", onclick: () => this.pull() }, "⤓ 풀");
    const push = h("button", { class: "small", title: "내 커밋 올리기", onclick: () => this.push() }, "⤒ 푸시");
    const menu = h("select", { class: "git-menu", title: "프로젝트" },
      h("option", { value: "" }, "⋯"), h("option", { value: "refresh" }, "새로 고침 (서버 확인 포함)"), h("option", { value: "other" }, "다른 폴더 열기…"),
      h("option", { value: "clone" }, "저장소 복제…"), h("option", { value: "close" }, "프로젝트 닫기"));
    menu.onchange = () => {
      const v = menu.value; menu.value = "";
      if (v === "refresh") this.task("서버 확인 중…", async () => { await invoke("git_fetch", { root: r.root }); await this.reload(); });
      if (v === "other") this.pickFolder();
      if (v === "clone") this.cloneDialog();
      if (v === "close") this.close();
    };
    const revert = h("button", { class: "small", title: "체크한 파일의 변경 취소", onclick: () => this.discard() }, "되돌리기");
    this.el.replaceChildren(
      h("div", { class: "git-head" },
        h("div", { class: "row" }, h("strong", { title: r.root }, r.name), h("span", { class: "dim git-remote", title: r.remote ?? "" }, r.remote ?? "원격 없음"), h("span", { class: "spacer" }), menu),
        h("div", { class: "row" }, this.branchSel, this.abEl, h("span", { class: "spacer" }), pull, push),
        this.busyEl),
      this.filterIn,
      this.treeEl,
      h("div", { class: "git-changes" },
        h("div", { class: "row" }, h("span", {}, "변경 ", this.chgCount), h("span", { class: "spacer" }),
          h("button", { class: "small", title: "모두 체크/해제", onclick: () => this.toggleAll() }, "전체"), revert),
        this.chgEl,
        this.msgIn,
        h("div", { class: "row" }, this.commitBtn, h("label", { class: "check" }, this.pushAfter, "커밋 후 푸시"))));
  }

  setVisible(v: boolean) {
    this.visible = v;
    if (v && this.treeStale && this.repo) this.renderTree();
  }

  /** 상태·파일·브랜치를 다시 읽는다 */
  async reload() {
    const r = this.repo;
    if (!r) return;
    try {
      const [st, files, br] = await Promise.all([
        invoke<Status>("git_status", { root: r.root }),
        invoke<string[]>("git_files", { root: r.root }),
        invoke<Branches>("git_branches", { root: r.root }),
      ]);
      if (r !== this.repo) return;
      this.files = files;
      this.renderBranches(br);
      this.applyStatus(st);
      this.renderTree();
    } catch (e) {
      toast(explain(e), "error", 8000);
    }
  }

  private applyStatus(st: Status) {
    this.st = st;
    for (const f of st.files) if (!this.checked.has(f.path)) this.checked.set(f.path, f.code !== "?");
    for (const k of [...this.checked.keys()]) if (!st.files.some((f) => f.path === k)) this.checked.delete(k);
    this.renderState();
    this.renderChanges();
  }

  /** 가벼운 갱신: git status 하나만. 파일이 새로 생기거나 없어졌을 때만 파일 목록을 다시 읽는다 */
  private async refreshStatus() {
    const r = this.repo;
    if (!r || this.busy) return;
    if (this.reading) return this.reading;
    this.reading = (async () => {
      const st = await invoke<Status>("git_status", { root: r.root });
      if (r !== this.repo) return;
      const prev = this.st;
      const key = (s: Status | null) => (s?.files ?? []).map((f) => `${f.code}${f.path}`).join("\n");
      const branchChanged = prev?.branch !== st.branch;
      if (key(prev) === key(st) && !branchChanged && prev?.ahead === st.ahead && prev?.behind === st.behind) return;
      const known = new Set(this.files);
      const filesChanged = st.files.some((f) => (f.code === "?" || f.code === "A" || f.code === "D" || f.code === "R") && !known.has(f.path))
        || (prev?.files ?? []).some((f) => f.code === "?" && !st.files.some((x) => x.path === f.path));
      if (branchChanged || filesChanged) return this.reload();
      this.applyStatus(st);
      this.renderTree();
    })().catch(() => { /* 다음 갱신 때 다시 */ }).finally(() => { this.reading = null; });
    return this.reading;
  }

  /** 파일 저장 뒤·창 포커스 때 — 몰려도 한 번만 */
  refreshSoon() {
    if (!this.repo || this.busy) return;
    clearTimeout(this.refreshTimer);
    this.refreshTimer = window.setTimeout(() => void this.refreshStatus(), 300);
  }

  private async fetchQuiet() {
    const r = this.repo;
    // 창이 최소화·뒤에 있으면 건너뛴다
    if (!r?.remote || this.busy || document.hidden) return;
    try {
      await invoke("git_fetch", { root: r.root });
      if (r === this.repo) this.refreshStatus();
    } catch { /* 사내망 밖 등 — 조용히 넘긴다. 풀/푸시 때 알린다 */ }
  }

  private renderBranches(br: Branches) {
    const cur = br.current ?? "";
    this.branchSel.replaceChildren(
      ...(cur ? [] : [h("option", { value: "", selected: true }, "(브랜치 아님)")]),
      ...br.local.map((b) => h("option", { value: b, selected: b === cur }, b)),
      ...(br.remote.length ? [h("optgroup", { label: "서버에만 있는 브랜치" }, ...br.remote.map((b) => h("option", { value: b }, b)))] : []),
      h("option", { value: "+new" }, "＋ 새 브랜치…"));
  }

  private renderState() {
    const st = this.st!;
    const ab = st.upstream ? `↑${st.ahead} ↓${st.behind}` : "서버에 없음";
    this.abEl.textContent = ab;
    this.abEl.title = st.upstream ? `${st.upstream} 기준 · 올릴 커밋 ${st.ahead} · 받을 커밋 ${st.behind}` : "아직 푸시하지 않은 브랜치 — 푸시하면 서버에 만든다";
    this.host.onState(`⎇ ${st.branch ?? "(분리됨)"} ${st.upstream ? ab : ""}${st.files.length ? ` · 변경 ${st.files.length}` : ""}`, `${this.repo!.root}`);
  }

  private renderTree() {
    if (!this.visible) { this.treeStale = true; return; }
    this.treeStale = false;
    const st = this.st;
    const q = this.filterIn.value.trim().toLowerCase();
    const code = new Map((st?.files ?? []).map((f) => [f.path, f.code]));
    const all = [...new Set([...this.files, ...(st?.files ?? []).map((f) => f.path)])].sort((a, b) => a.localeCompare(b));
    const list = q ? all.filter((p) => p.toLowerCase().includes(q)) : all;
    if (list.length > 5000 && !q) {
      this.treeEl.replaceChildren(h("div", { class: "hint" }, `파일 ${list.length.toLocaleString()}개 — 위 칸에 이름을 넣어 찾으세요`));
      return;
    }
    // 폴더 → 자식
    type Dir = { dirs: Map<string, Dir>; files: string[]; changed: boolean };
    const top: Dir = { dirs: new Map(), files: [], changed: false };
    for (const p of list) {
      const parts = p.split("/");
      let d = top;
      for (const seg of parts.slice(0, -1)) {
        if (code.has(p)) d.changed = true;
        if (!d.dirs.has(seg)) d.dirs.set(seg, { dirs: new Map(), files: [], changed: false });
        d = d.dirs.get(seg)!;
      }
      if (code.has(p)) d.changed = true;
      d.files.push(p);
    }
    const frag = document.createDocumentFragment();
    const walk = (d: Dir, prefix: string, depth: number) => {
      for (const [name, sub] of [...d.dirs].sort((a, b) => a[0].localeCompare(b[0]))) {
        const key = prefix + name + "/";
        const isOpen = !!q || this.open.has(key);
        const row = h("div", { class: "git-row dir", style: `padding-left:${6 + depth * 12}px` },
          h("span", { class: "git-tw" }, isOpen ? "▾" : "▸"), name, sub.changed ? h("span", { class: "git-dot", title: "안에 바뀐 파일이 있음" }, "●") : "");
        row.onclick = () => { this.open.has(key) ? this.open.delete(key) : this.open.add(key); this.renderTree(); };
        frag.append(row);
        if (isOpen) walk(sub, key, depth + 1);
      }
      for (const p of d.files) {
        const c = code.get(p);
        const row = h("div", { class: `git-row file${c === "D" ? " gone" : ""}`, style: `padding-left:${18 + depth * 12}px`, title: `${p}${c ? ` — ${CODE_LABEL[c] ?? c}` : ""}\n더블클릭: 편집기로 열기` },
          p.split("/").pop()!, c ? h("span", { class: `git-code c${c === "?" ? "Q" : c}` }, c === "?" ? "A" : c) : "");
        row.ondblclick = () => { if (c !== "D") this.host.openPath(join(this.repo!.root, p)); };
        row.onclick = () => { if (c) this.showDiff(p); };
        frag.append(row);
      }
    };
    walk(top, "", 0);
    if (!list.length) frag.append(h("div", { class: "hint" }, q ? "없음" : "파일이 없습니다"));
    this.treeEl.replaceChildren(frag);
  }

  private renderChanges() {
    const files = this.st?.files ?? [];
    this.chgCount.textContent = String(files.length);
    this.chgEl.replaceChildren(...files.map((f) => {
      const box = h("input", { type: "checkbox", checked: this.checked.get(f.path) ?? false });
      box.onclick = (e) => e.stopPropagation();
      box.onchange = () => { this.checked.set(f.path, box.checked); this.updateCommitBtn(); };
      const row = h("div", { class: "git-chg", title: `${CODE_LABEL[f.code] ?? f.code}${f.orig ? ` (원래 ${f.orig})` : ""} — 눌러서 비교` },
        box, h("span", { class: "git-path" }, f.path), h("span", { class: `git-code c${f.code === "?" ? "Q" : f.code}` }, f.code));
      row.onclick = () => this.showDiff(f.path);
      return row;
    }));
    if (!files.length) this.chgEl.append(h("div", { class: "hint" }, "바뀐 파일이 없습니다"));
    this.updateCommitBtn();
  }

  private picked() {
    return (this.st?.files ?? []).filter((f) => this.checked.get(f.path));
  }

  private updateCommitBtn() {
    const n = this.picked().length;
    this.commitBtn.textContent = `커밋 (${n})`;
    this.commitBtn.disabled = !n || this.busy;
  }

  private toggleAll() {
    const files = this.st?.files ?? [];
    const on = !files.every((f) => this.checked.get(f.path));
    for (const f of files) this.checked.set(f.path, on);
    this.renderChanges();
  }

  // ── 비교 ──

  private async showDiff(path: string) {
    const r = this.repo!;
    let d: Diff;
    try {
      d = await invoke<Diff>("git_diff", { root: r.root, path });
    } catch (e) {
      return toast(explain(e), "error");
    }
    const box = h("div", { class: "git-diff" });
    const f = this.st?.files.find((x) => x.path === path);
    const head = h("div", { class: "git-diff-head" },
      h("span", {}, d.old == null ? "(새 파일)" : "마지막 커밋 (HEAD)"), h("span", {}, d.new == null ? "(지움)" : `작업 중인 파일${d.encoding !== "utf-8" ? ` · ${d.encoding}` : ""}`));
    if (d.binary) box.append(h("p", {}, "바이너리 파일이라 내용을 비교하지 않습니다."));
    else {
      const { MergeView } = await import("@codemirror/merge");
      const { EditorView, lineNumbers } = await import("@codemirror/view");
      const { EditorState } = await import("@codemirror/state");
      const { sql, PLSQL } = await import("@codemirror/lang-sql");
      const { syntaxHighlighting, defaultHighlightStyle } = await import("@codemirror/language");
      const ext = [lineNumbers(), EditorView.editable.of(false), EditorState.readOnly.of(true), sql({ dialect: PLSQL }), syntaxHighlighting(defaultHighlightStyle, { fallback: true })];
      box.append(head);
      const mv = h("div", { class: "git-merge" });
      box.append(mv);
      new MergeView({ parent: mv, a: { doc: d.old ?? "", extensions: ext }, b: { doc: d.new ?? "", extensions: ext },
        collapseUnchanged: { margin: 3, minSize: 6 }, highlightChanges: true, gutter: true });
    }
    const act = await modal(`${path}${f ? ` — ${CODE_LABEL[f.code] ?? f.code}` : ""}`, box,
      [...(d.new != null ? [{ label: "편집기로 열기", value: "open" }] : []), ...(f ? [{ label: "이 파일 되돌리기", value: "discard" }] : []), { label: "닫기", value: "close", kind: "primary" as const }],
      { wide: true });
    if (act === "open") this.host.openPath(join(r.root, path));
    if (act === "discard" && f) this.discard([f]);
  }

  // ── 작업 ──

  /** 한 번에 하나씩. 진행 표시 */
  private async task(label: string, f: () => Promise<void>) {
    if (this.busy) return toast("다른 Git 작업이 진행 중입니다", "info");
    this.busy = true;
    this.busyEl.textContent = label;
    this.el.classList.add("busy");
    this.updateCommitBtn();
    try {
      await f();
    } catch (e) {
      await alertBox("Git", explain(e));
    } finally {
      this.busy = false;
      this.busyEl.textContent = "";
      this.el.classList.remove("busy");
      this.updateCommitBtn();
    }
  }

  /** 저장하지 않은 탭이 있으면 묻는다. 계속해도 되면 true */
  private async unsavedOk(what: string) {
    const dirty = this.host.tabsUnder(this.repo!.root).filter((t) => t.dirty);
    if (!dirty.length) return true;
    return confirmBox("저장하지 않은 편집", `이 프로젝트에서 저장하지 않은 탭이 ${dirty.length}개 있습니다:\n${dirty.map((t) => `· ${t.path}`).join("\n")}\n\n${what} 하면 그 탭의 내용과 디스크 파일이 달라질 수 있습니다. 그래도 할까요?`, `${what} 계속`);
  }

  /** 풀·브랜치 전환 뒤 — 저장된(편집 중이 아닌) 탭을 디스크에서 다시 읽는다 */
  private async reloadTabs() {
    for (const t of this.host.tabsUnder(this.repo!.root)) if (!t.dirty) await t.reload().catch(() => {});
  }

  private async commit() {
    const r = this.repo!;
    const files = this.picked();
    const message = this.msgIn.value.trim();
    if (!files.length) return toast("커밋할 파일을 체크하세요", "error");
    if (!message) { this.msgIn.focus(); return toast("커밋 메시지를 쓰세요", "error"); }
    const unsaved = this.host.tabsUnder(r.root).filter((t) => t.dirty && files.some((f) => norm(join(r.root, f.path)) === norm(t.path)));
    if (unsaved.length && !(await confirmBox("저장하지 않은 편집", `커밋할 파일 중 편집기에서 저장하지 않은 것이 있습니다 — 저장된 내용만 커밋됩니다:\n${unsaved.map((t) => `· ${t.path}`).join("\n")}`, "그대로 커밋"))) return;
    await this.task("커밋 중…", async () => {
      let hash: string;
      try {
        hash = await invoke<string>("git_commit", { root: r.root, paths: files.flatMap((f) => (f.orig ? [f.path, f.orig] : [f.path])), message });
      } catch (e) {
        if (errOf(e).kind !== "identity" || !(await this.askIdentity())) throw e;
        hash = await invoke<string>("git_commit", { root: r.root, paths: files.flatMap((f) => (f.orig ? [f.path, f.orig] : [f.path])), message });
      }
      this.msgIn.value = "";
      toast(`커밋했습니다 ${hash} (파일 ${files.length}개)`, "ok");
      await this.reload();
      if (this.pushAfter.checked) await this.doPush();
    });
  }

  private async askIdentity() {
    const r = this.repo!;
    const cur = await invoke<{ name: string; email: string }>("git_identity", { root: r.root }).catch(() => ({ name: "", email: "" }));
    const name = h("input", { value: cur.name, placeholder: "홍길동", style: "width:100%" });
    const email = h("input", { value: cur.email, placeholder: "gildong@회사", style: "width:100%" });
    const global = h("input", { type: "checkbox", checked: true });
    const ok = await modal("커밋 작성자", h("div", { class: "git-form" },
      h("p", { class: "hint" }, "커밋에 남을 이름과 이메일입니다 (git config user.name / user.email)."),
      h("label", {}, "이름"), name, h("label", {}, "이메일"), email,
      h("label", { class: "check" }, global, "이 PC 의 모든 저장소에 쓰기 (끄면 이 저장소만)")),
      [{ label: "취소", value: false }, { label: "저장", value: true, kind: "primary" }]);
    if (!ok) return false;
    await invoke("git_set_identity", { root: r.root, name: name.value, email: email.value, global: global.checked });
    return true;
  }

  private async discard(files = this.picked()) {
    const r = this.repo!;
    if (!files.length) return toast("되돌릴 파일을 체크하세요", "error");
    const fresh = files.filter((f) => f.code === "?" || f.code === "A");
    const text = `${files.length}개 파일의 변경을 버리고 마지막 커밋 상태로 되돌립니다:\n${files.map((f) => `· ${f.path}`).join("\n")}` +
      (fresh.length ? `\n\n이 중 새 파일 ${fresh.length}개는 디스크에서 지워집니다 (휴지통으로 가지 않습니다).` : "") + "\n\n되돌린 내용은 복구할 수 없습니다.";
    if (!(await confirmBox("변경 되돌리기", text, "되돌리기"))) return;
    await this.task("되돌리는 중…", async () => {
      await invoke("git_discard", { root: r.root, paths: files.map((f) => f.path) });
      await this.reload();
      await this.reloadTabs();
      toast("되돌렸습니다", "ok");
    });
  }

  private async pull() {
    const r = this.repo!;
    if (!(await this.unsavedOk("풀"))) return;
    await this.task("풀 하는 중…", async () => {
      let msg: string;
      try {
        msg = await invoke<string>("git_pull", { root: r.root, rebase: false });
      } catch (e) {
        if (errOf(e).kind !== "diverged") throw e;
        const ok = await confirmBox("서버와 내 커밋이 갈라졌습니다",
          "서버에 새 커밋이 있고, 나에게도 아직 올리지 않은 커밋이 있습니다.\n\n" +
          "내 커밋을 서버 커밋 위로 다시 쌓을까요? (git pull --rebase)\n충돌이 나면 자동으로 풀 하기 전 상태로 되돌립니다.", "다시 쌓기", false);
        if (!ok) return;
        msg = await invoke<string>("git_pull", { root: r.root, rebase: true });
      }
      await this.reload();
      await this.reloadTabs();
      toast(msg, "ok");
    });
  }

  private async push() {
    await this.task("푸시 준비…", () => this.doPush());
  }

  private async doPush() {
    const r = this.repo!;
    const out = await invoke<{ hash: string; subject: string }[]>("git_outgoing", { root: r.root });
    const st = this.st;
    if (!out.length && st?.upstream) return void toast("올릴 커밋이 없습니다", "info");
    const target = st?.upstream ?? `origin/${st?.branch ?? "?"} (새로 만듦)`;
    const ok = await modal(`푸시 — ${target}`, h("div", {},
      h("p", {}, `올릴 커밋 ${out.length}개`),
      h("ul", { class: "git-commits" }, ...out.map((c) => h("li", {}, h("code", {}, c.hash), " ", c.subject))),
      st?.behind ? h("p", { class: "warn" }, `서버에 받을 커밋이 ${st.behind}개 있습니다 — 서버가 거절하면 먼저 풀 하세요.`) : ""),
      [{ label: "취소", value: false }, { label: "푸시", value: true, kind: "primary" }]);
    if (!ok) return;
    this.busyEl.textContent = "푸시 하는 중…";
    try {
      const msg = await invoke<string>("git_push", { root: r.root });
      toast(msg, "ok");
    } catch (e) {
      if (errOf(e).kind === "rejected") {
        await alertBox("푸시 거절", "서버에 내가 아직 받지 않은 커밋이 있어 거절되었습니다.\n먼저 '풀' 한 다음 다시 푸시하세요. (강제 푸시는 하지 않습니다)");
        return;
      }
      throw e;
    } finally {
      await this.reload();
    }
  }

  private async switchBranch() {
    const r = this.repo!;
    const v = this.branchSel.value;
    const back = () => this.reload();
    if (!v) return back();
    let name = v, create = false;
    if (v === "+new") {
      const inp = h("input", { value: "feature/", style: "width:100%" });
      const ok = await modal("새 브랜치", h("div", { class: "git-form" }, h("label", {}, "이름 (지금 위치에서 만든다)"), inp), [{ label: "취소", value: false }, { label: "만들기", value: true, kind: "primary" }]);
      if (!ok || !inp.value.trim()) return back();
      name = inp.value.trim(); create = true;
    }
    if (!(await this.unsavedOk("브랜치 전환"))) return back();
    await this.task(`${name} 로 옮기는 중…`, async () => {
      try {
        await invoke("git_switch", { root: r.root, name, create });
        toast(create ? `새 브랜치 ${name}` : `브랜치: ${name}`, "ok");
        await this.reloadTabs();
      } finally {
        await this.reload();
      }
    });
  }
}
