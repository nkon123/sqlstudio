// AI 패널 — 공급자 선택, 작업 버튼, 스트리밍 답, "에디터에 넣기".

import { invoke } from "@tauri-apps/api/core";
import { api, errOf, type AiVerify, type ChatMsg, type Provider, type Task } from "./api";
import { glowProgress } from "./glow";
import { confirmBox, h, toast } from "./ui";

export interface AiHost {
  /** 문맥: 현재 탭의 세션, SQL(선택 영역 > 커서 문장), 마지막 오류, 탐색기에서 고른 테이블 */
  context(): { sessionId?: number; sql?: string; error?: string; tables: string[] };
  insertSql(sql: string): void;
  replaceSql(sql: string): void;
}

let reqSeq = 1;

export class AiPanel {
  readonly el: HTMLElement;
  private providers: Provider[] = [];
  private select = h("select", { class: "ai-provider" });
  private badge = h("span", { class: "badge" });
  private log = h("div", { class: "ai-log" });
  private input = h("textarea", { class: "ai-input", rows: 3, placeholder: "무엇을 할까요? 예) 최근 7일 주문을 고객별로 합계\n(Ctrl+Enter 보내기 · 어디서든 Ctrl+K 로 여기로 · Esc 편집기로)" });
  private statusEl = h("div", { class: "ai-status" });
  private stopBtn = h("button", { class: "danger", disabled: true }, "중지");
  private history: ChatMsg[] = [];
  private running: number | null = null;

  constructor(private host: AiHost) {
    const task = (t: Task, label: string, title: string) =>
      h("button", { title, onclick: () => this.ask(t) }, label);
    this.el = h("aside", { class: "ai" },
      h("div", { class: "ai-head" }, h("strong", {}, "AI"), this.select, this.badge),
      h("div", { class: "ai-tasks" },
        task("generate", "SQL 생성", "아래 입력을 SQL 로 만든다"),
        task("explain", "설명", "현재 SQL 을 설명한다"),
        task("optimize", "튜닝", "실행계획을 근거로 개선안을 낸다"),
        task("fix", "오류 수정", "마지막 ORA 오류를 고친다"),
        h("button", { title: "대화 기록을 지운다", onclick: () => this.reset() }, "새 대화"),
      ),
      this.log,
      this.statusEl,
      h("div", { class: "ai-compose" }, this.input,
        h("div", { class: "ai-compose-buttons" },
          h("button", { class: "primary", onclick: () => this.ask(this.history.length ? "chat" : "generate") }, "보내기"),
          this.stopBtn)),
    );
    this.select.addEventListener("change", () => this.updateBadge());
    this.stopBtn.addEventListener("click", () => this.stop());
    this.input.addEventListener("keydown", (e) => {
      if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) {
        e.preventDefault();
        this.ask(this.history.length ? "chat" : "generate");
      }
    });
  }

  /** 다른 화면 안에서 답을 받는다 (값 추적 설명). 조회한 값이 담겨 로컬 공급자에만 보내고, 대화 기록에는 넣지 않는다 */
  async explainInto(text: string, out: HTMLElement): Promise<void> {
    const p = this.current();
    if (!p) throw new Error("AI 공급자를 먼저 설정하세요 (설정 → AI)");
    if (p.remote) throw new Error("조회한 값이 담겨 있어 외부 AI 로는 보내지 않습니다 — AI 패널에서 로컬 공급자를 고르세요");
    if (this.running != null) throw new Error("AI 패널이 다른 질문에 답하는 중입니다 — 끝난 뒤 다시 누르세요");
    const id = reqSeq++;
    this.running = id;
    this.stopBtn.disabled = false;
    const glow = glowProgress(`${p.model} 이(가) 추적을 읽는 중…`);
    const body = h("div", { class: "md" });
    out.replaceChildren(glow.el, body);
    let acc = "";
    let pending = false;
    try {
      const ctx = this.host.context();
      const ans = await api.aiAsk({ request_id: id, provider: p.name, task: "trace", session_id: ctx.sessionId, question: text }, (ev) => {
        if (ev.type === "status") glow.label(ev.text);
        else if (ev.type === "reset") { acc = ""; body.replaceChildren(); }
        else if (ev.type === "delta") {
          acc += ev.text;
          glow.label("답하는 중…");
          if (!pending) {
            pending = true;
            requestAnimationFrame(() => { pending = false; render(body, acc, this.host); });
          }
        }
      });
      render(body, ans.text, this.host);
    } finally {
      glow.done();
      this.running = null;
      this.stopBtn.disabled = true;
    }
  }

  /** 지금 질문 멈추기 (다른 화면에서) */
  cancelCurrent() {
    this.stop();
  }

  /** Ctrl+K — 입력칸으로. 이미 쓴 글은 전체 선택해 바로 고쳐 쓰게 */
  focus() {
    this.input.focus();
    this.input.select();
  }

  async load() {
    try {
      this.providers = await api.listProviders();
    } catch (e) {
      toast(`AI 공급자를 읽을 수 없습니다: ${errOf(e).message}`, "error");
    }
    const prev = localStorage.getItem("ai.provider");
    this.select.replaceChildren(...this.providers.map((p) => h("option", { value: p.name, selected: p.name === prev }, `${p.name} · ${p.model}`)));
    this.updateBadge();
  }

  private current() {
    return this.providers.find((p) => p.name === this.select.value);
  }

  private updateBadge() {
    const p = this.current();
    localStorage.setItem("ai.provider", this.select.value);
    this.badge.textContent = p ? (p.remote ? "외부 전송" : "로컬") : "";
    this.badge.className = `badge ${p?.remote ? "warn" : "ok"}`;
    this.badge.title = p?.remote
      ? "SQL 과 테이블 구조(컬럼·주석·인덱스)가 이 PC 밖으로 전송됩니다. 조회 결과 데이터는 보내지 않습니다."
      : "이 PC(또는 지정한 사내 서버) 안에서만 처리합니다.";
  }

  reset() {
    this.history = [];
    this.log.replaceChildren();
    this.statusEl.textContent = "";
  }

  private stop() {
    if (this.running != null) api.aiCancel(this.running);
  }

  async ask(task: Task) {
    if (this.running != null) return;
    const p = this.current();
    if (!p) return toast("AI 공급자를 먼저 설정하세요", "error");
    if (p.remote && !p.has_key && p.kind !== "ollama") {
      return toast(`${p.name}: API 키가 없습니다. 설정에서 입력하세요`, "error");
    }
    // 외부 전송은 공급자·주소마다 처음 한 번 확인한다. 동의는 백엔드가 설정 파일에 남기고 보내기 직전에 다시 확인한다.
    if (p.remote && !p.remote_ok) {
      const ok = await confirmBox("외부 AI 로 전송",
        `${p.name} (${p.base_url ?? p.kind}) 은 이 PC 밖의 서버입니다.\n\n` +
        "전송되는 것: 에디터의 SQL, 관련 테이블 구조(컬럼·주석·인덱스), 실행계획, 오류 메시지\n" +
        "전송되지 않는 것: 조회 결과 데이터, 비밀번호\n\n회사 보안 정책에 맞는지 확인하세요.", "계속", false);
      if (!ok) return;
      try {
        await invoke("ai_ack_remote", { provider: p.name });
        p.remote_ok = true;
      } catch (e) {
        return toast(errOf(e).message, "error");
      }
    }

    const ctx = this.host.context();
    const question = this.input.value.trim();
    if (task === "generate" && !question) return toast("무엇을 만들지 입력하세요", "error");
    if ((task === "explain" || task === "optimize") && !ctx.sql) return toast("에디터에 SQL 이 없습니다", "error");
    if (task === "fix" && !ctx.error) return toast("고칠 오류가 없습니다 — 먼저 실행해 보세요", "error");

    const id = reqSeq++;
    this.running = id;
    this.stopBtn.disabled = false;
    const label = { generate: "SQL 생성", explain: "설명", optimize: "튜닝", fix: "오류 수정", chat: "질문", trace: "출처 설명" }[task];
    this.log.append(h("div", { class: "msg user" }, h("small", {}, label), question || (ctx.sql ?? "").slice(0, 300)));
    const out = h("div", { class: "msg assistant streaming" });
    const meta = h("small", {});
    const body = h("div", { class: "md" });
    out.append(meta, body);
    this.log.append(out);
    this.input.value = "";
    let text = "";
    let pending = false;
    const started = performance.now();
    const tick = setInterval(() => {
      this.statusEl.dataset.elapsed = `${Math.round((performance.now() - started) / 1000)}s`;
    }, 500);
    // 기다리는 동안 은은한 진행 막대 (글자가 오기 시작하면 '답하는 중')
    const glow = glowProgress("", { compact: true });
    this.statusEl.before(glow.el);

    try {
      const ans = await api.aiAsk({
        request_id: id, provider: p.name, task, session_id: ctx.sessionId,
        sql: task === "generate" ? undefined : ctx.sql, question: question || undefined,
        error: task === "fix" ? ctx.error : undefined, tables: ctx.tables,
        history: this.history,
      }, (ev) => {
        if (ev.type === "status") this.statusEl.textContent = ev.text;
        else if (ev.type === "context") {
          const by = ev.picked === "ai" ? " (AI 가 고름)" : ev.picked === "keyword" ? " (낱말로 고름)" : "";
          meta.textContent = `문맥: ${ev.tables.length ? ev.tables.join(", ") + by : "테이블 없음"}${ev.plan ? " + 실행계획" : ""}${ev.remote ? " · 외부" : ""}` +
            (ev.guides?.length ? ` · 참고 문법: ${ev.guides.join(", ")}` : "") +
            (ev.examples?.length ? ` · 내 쿼리 예시 ${ev.examples.length}개` : "") +
            (ev.tokens ? ` · 약 ${ev.tokens.toLocaleString()}/${(ev.budget ?? 0).toLocaleString()} 토큰` : "");
          meta.title = [
            ev.trimmed?.length ? `예산에 맞추느라 줄인 것: ${ev.trimmed.join(" · ")}` : "",
            ev.examples?.length ? `참고로 넣은 예시: ${ev.examples.join(" / ")}` : "",
          ].filter(Boolean).join("\n");
          if (ev.trimmed?.length) meta.textContent += " (줄임)";
        } else if (ev.type === "reset") {
          // 다시 묻는 중 — 앞의 답을 지우고 새 답을 받는다
          text = "";
          body.replaceChildren();
        } else if (ev.type === "delta") {
          text += ev.text;
          this.statusEl.textContent = "답하는 중…";
          glow.label("");
          // 글자마다 다시 그리지 않고 프레임마다 한 번
          if (!pending) {
            pending = true;
            requestAnimationFrame(() => { pending = false; render(body, text, this.host); this.log.scrollTop = this.log.scrollHeight; });
          }
        }
      });
      // 서버 파싱을 통과한 SQL 을 에디터에 넣으면 내 쿼리 예시로 쌓는다 (로컬 모델만 — 다음 비슷한 질문에 예시로).
      // 이어 묻기("조건 추가")는 질문만으로 뜻이 서지 않아 쌓지 않는다
      const remember = ans.verify?.state === "ok" && !p.remote && ctx.sessionId != null && question && task === "generate"
        ? (code: string) => { api.aiRememberExample(ctx.sessionId!, question, code).catch(() => {}); }
        : undefined;
      render(body, ans.text, this.host, remember);
      const v = verifyLine(ans.verify, ans.warnings ?? []);
      if (v) out.append(v);
      out.classList.remove("streaming");
      const secs = ((performance.now() - started) / 1000).toFixed(1);
      const notes: string[] = [`${ans.model ?? p.model} · ${secs}s`];
      if (ans.output_tokens) notes.push(`${ans.output_tokens} tok`);
      if (ans.refused) notes.push("⚠ 모델이 답을 거절했습니다");
      if (ans.truncated) notes.push("⚠ 길이 제한으로 잘렸습니다 (max_tokens 를 늘리세요)");
      this.statusEl.textContent = notes.join(" · ");
      this.history.push({ role: "user", content: ans.user_message }, { role: "assistant", content: ans.text });
      // 대화가 길어지면 앞쪽을 버린다 (로컬 모델 컨텍스트)
      if (this.history.length > 12) this.history = this.history.slice(-12);
    } catch (e) {
      const err = errOf(e);
      // 설정 파일에서 동의가 지워졌거나 주소가 바뀐 경우 — 다음 질문 때 다시 묻는다
      if (err.kind === "remote_consent") p.remote_ok = false;
      out.classList.remove("streaming");
      out.classList.add("error");
      body.textContent = (text ? text + "\n\n" : "") + err.message;
      this.statusEl.textContent = err.kind === "cancelled" ? "취소했습니다" : "실패";
    } finally {
      clearInterval(tick);
      glow.done();
      delete this.statusEl.dataset.elapsed;
      this.running = null;
      this.stopBtn.disabled = true;
      this.log.scrollTop = this.log.scrollHeight;
    }
  }
}

/** 답 아래 한 줄 — SQL 을 어디까지 확인했는지, 고쳤으면 무엇을, 하네스가 알릴 것 */
function verifyLine(v: AiVerify | undefined, warnings: string[]): HTMLElement | null {
  const parts: [string, string][] = [];
  const fixed = v?.repairs ? ` · ${v.repairs}번 고침${v.first_problem ? ` (처음 답: ${firstLine(v.first_problem)})` : ""}` : "";
  if (v?.state === "ok") parts.push(["ok", `✓ 서버에서 파싱 확인${fixed}`]);
  else if (v?.state === "static") parts.push(["dim", `버전 문법만 확인 — ${v.note ?? ""}${fixed}`]);
  else if (v?.state === "failed") parts.push(["err", `⚠ 확인에서 걸림${v.repairs ? ` (${v.repairs}번 고쳐도)` : ""}: ${v.note ?? ""}`]);
  for (const w of warnings) parts.push(["warn", `⚠ ${w}`]);
  if (!parts.length) return null;
  return h("div", { class: "ai-verify" }, ...parts.map(([k, t]) => h("div", { class: k }, t)));
}

function firstLine(s: string) {
  const l = s.split("\n")[0];
  return l.length > 80 ? l.slice(0, 80) + "…" : l;
}

/** 아주 작은 마크다운: 코드 블록만 따로 그리고 나머지는 글자 그대로 */
function render(el: HTMLElement, text: string, host: AiHost, onUse?: (code: string) => void) {
  const parts: Node[] = [];
  const re = /```([\w-]*)\n([\s\S]*?)(```|$)/g;
  let last = 0;
  let m: RegExpExecArray | null;
  while ((m = re.exec(text))) {
    if (m.index > last) parts.push(h("p", {}, text.slice(last, m.index).trim()));
    const code = m[2].replace(/\n$/, "");
    const lang = m[1].toLowerCase();
    const isSql = !lang || ["sql", "plsql", "oracle"].includes(lang);
    parts.push(h("div", { class: "code" },
      h("pre", {}, code),
      m[3] ? h("div", { class: "code-actions" },
        isSql ? h("button", { onclick: () => { host.insertSql(code); onUse?.(code); } }, "커서에 넣기") : null,
        isSql ? h("button", { onclick: () => { host.replaceSql(code); onUse?.(code); } }, "문장 바꾸기") : null,
        h("button", { onclick: () => navigator.clipboard.writeText(code).then(() => toast("복사했습니다", "ok")) }, "복사"),
      ) : null,
    ));
    last = m.index + m[0].length;
  }
  if (last < text.length) parts.push(h("p", {}, text.slice(last).trim()));
  el.replaceChildren(...parts.filter((p) => !(p instanceof HTMLParagraphElement && !p.textContent)));
}
