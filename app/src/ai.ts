// AI 패널 — 공급자 선택, 작업 버튼, 스트리밍 답, "에디터에 넣기".

import { invoke } from "@tauri-apps/api/core";
import { api, errOf, type AcpAgent, type AcpConfigOption, type AcpEvent, type AiVerify, type ChatMsg, type Provider, type Task } from "./api";
import { glowProgress } from "./glow";
import { confirmBox, h, modal, toast } from "./ui";

export interface AiHost {
  /** 문맥: 현재 탭의 세션·접속 프로필, SQL(선택 영역 > 커서 문장), 마지막 오류, 탐색기에서 고른 테이블 */
  context(): { sessionId?: number; profile?: string; sql?: string; error?: string; tables: string[] };
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
  /** ACP 에이전트 (opencode 등) — 고르면 '공급자' 대신 에이전트에게 묻는다 */
  private agents: AcpAgent[] = [];
  /** 지금 ACP 대화 — 새 대화나 다른 에이전트를 고르면 버린다 (에이전트가 대화를 기억한다) */
  private acp: AcpChat | null = null;
  private acpStarting: Promise<AcpChat> | null = null;
  /** 답하는 중인 ACP 대화 — 중지 단추가 session/cancel 을 보낸다 */
  private acpBusy: AcpChat | null = null;
  /** 에이전트가 고르게 하는 것 (모델·모드) */
  private acpOpts = h("div", { class: "ai-acp-opts", hidden: true });

  constructor(private host: AiHost) {
    const task = (t: Task, label: string, title: string) =>
      h("button", { title, onclick: () => this.ask(t) }, label);
    this.el = h("aside", { class: "ai" },
      h("div", { class: "ai-head" }, h("strong", {}, "AI"), this.select, this.badge),
      this.acpOpts,
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
    this.select.addEventListener("change", () => {
      this.updateBadge();
      // 에이전트를 고르면 미리 띄워 모델·모드를 고를 수 있게
      const a = this.agentName();
      if (this.acp && this.acp.agent !== a) this.dropAcp();
      if (a) this.ensureAcp(a).catch((e) => { this.statusEl.textContent = errOf(e).message; });
    });
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
    if (!p && this.agentName()) throw new Error("추적 설명은 조회한 값이 담겨 로컬 공급자에만 묻습니다 — AI 패널에서 로컬 공급자를 고르세요");
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
    this.agents = await api.acpAgents().catch(() => []);
    const prev = localStorage.getItem("ai.provider");
    this.select.replaceChildren(...this.providers.map((p) => h("option", { value: p.name, selected: p.name === prev }, `${p.name} · ${p.model}`)));
    if (this.agents.length) {
      this.select.append(h("optgroup", { label: "ACP 에이전트" },
        ...this.agents.map((a) => h("option", { value: `acp:${a.name}`, selected: `acp:${a.name}` === prev }, `${a.name} (ACP)${a.available ? "" : " — 찾을 수 없음"}`))));
    }
    // 설정에서 바꾸거나 지운 에이전트의 대화는 버린다 (다음 질문에 새로 연다)
    if (this.acp && this.acp.agent !== this.agentName()) this.dropAcp();
    this.updateBadge();
  }

  private current() {
    return this.providers.find((p) => p.name === this.select.value);
  }

  /** 고른 것이 ACP 에이전트면 그 이름 */
  private agentName() {
    return this.select.value.startsWith("acp:") ? this.select.value.slice(4) : null;
  }

  private updateBadge() {
    const p = this.current();
    localStorage.setItem("ai.provider", this.select.value);
    if (this.agentName()) {
      this.badge.textContent = "외부 전송";
      this.badge.className = "badge warn";
      this.badge.title = "에이전트가 쓰는 모델(대개 외부 서버)로 질문·SQL·오류가 전송됩니다. MCP 를 켜 두면 허용한 접속의 사전(스키마)도 에이전트가 직접 조회합니다. 조회 결과 데이터는 보내지 않습니다.";
      return;
    }
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
    // 에이전트 대화도 새로 — 모델·모드 고르기는 남겨 두려 바로 다시 연다
    const a = this.agentName();
    this.dropAcp();
    if (a) this.ensureAcp(a).catch((e) => { this.statusEl.textContent = errOf(e).message; });
  }

  private stop() {
    if (this.acpBusy) api.acpCancel(this.acpBusy.agent, this.acpBusy.session).catch(() => {});
    else if (this.running != null) api.aiCancel(this.running);
  }

  private dropAcp() {
    this.acp = null;
    this.acpStarting = null;
    this.acpOpts.hidden = true;
    this.acpOpts.replaceChildren();
  }

  /** 에이전트 대화를 연다 (처음이면 에이전트를 띄운다) — 이미 열려 있으면 그것 */
  private ensureAcp(name: string): Promise<AcpChat> {
    if (this.acp?.agent === name) return Promise.resolve(this.acp);
    if (this.acpStarting) return this.acpStarting;
    this.statusEl.textContent = `${name} 시작 중…`;
    const start: Promise<AcpChat> = api.acpNewSession(name).then((s) => {
      if (this.acpStarting !== start) throw new Error("다른 대화로 바뀌었습니다");
      const chat: AcpChat = { agent: name, session: s.session_id, mcp: s.mcp_profiles, sent: false };
      this.acp = chat;
      this.acpStarting = null;
      this.renderAcpOpts(chat, s.options);
      if (this.running == null) {
        this.statusEl.textContent = `${s.agent?.name ?? name} ${s.agent?.version ?? ""} — ` +
          (s.mcp_profiles.length ? `DB 사전 조회(MCP): ${s.mcp_profiles.join(", ")}` : "DB 사전 조회 없음 (설정 → MCP 에서 접속을 허용하면 에이전트가 스키마를 직접 봅니다)");
      }
      return chat;
    }, (e) => {
      if (this.acpStarting === start) this.acpStarting = null;
      throw e;
    });
    this.acpStarting = start;
    return start;
  }

  private renderAcpOpts(chat: AcpChat, options: AcpConfigOption[]) {
    const sels = options.filter((o) => o.options?.length).map((o) => {
      const sel = h("select", { title: o.name },
        ...o.options.map((x) => h("option", { value: x.value, selected: x.value === o.currentValue, title: x.description ?? "" }, x.name))) as HTMLSelectElement;
      sel.addEventListener("change", async () => {
        try {
          const next = await api.acpSetOption(chat.agent, chat.session, o.id, sel.value);
          if (this.acp === chat) this.renderAcpOpts(chat, next);
        } catch (e) {
          toast(errOf(e).message, "error");
          sel.value = o.currentValue;
        }
      });
      const tip = o.id === "mode" ? "plan: 파일 편집 도구를 막음 (이 앱의 기본) · build: 모든 도구 — 에이전트 설정에 따라 묻지 않고 파일을 고칠 수 있습니다" : o.name;
      return h("label", { class: "ai-acp-opt", title: tip }, h("span", {}, o.name), sel);
    });
    this.acpOpts.replaceChildren(...sels);
    this.acpOpts.hidden = !sels.length;
  }

  /** 에이전트의 권한 요청 — 고른 선택지로 답한다 (Esc 는 취소) */
  private async permission(agent: string, ev: Extract<AcpEvent, { type: "permission" }>) {
    const body = h("div", {}, h("p", {}, ev.title), ev.detail ? h("pre", { class: "modal-text" }, ev.detail) : null);
    const kind = (k: string) => (k === "allow_once" ? "primary" : k.startsWith("reject") ? "danger" : undefined);
    const choice = await modal(`${agent} 권한 요청`, body, ev.options.map((o) => ({ label: o.name, value: o.id, kind: kind(o.kind) })));
    api.acpPermission(agent, ev.key, choice).catch(() => {});
  }

  async ask(task: Task) {
    if (this.running != null) return;
    const agent = this.agentName();
    if (agent) return this.askAcp(task, agent);
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

  /** ACP 에이전트에게 묻기 — 문맥은 글로 함께 보내고, 스키마는 에이전트가 MCP 로 직접 찾는다 */
  private async askAcp(task: Task, name: string) {
    const a = this.agents.find((x) => x.name === name);
    if (!a) return toast(`ACP 에이전트가 없습니다: ${name}`, "error");
    if (!a.available) return toast(`${name}: '${a.command}' 을(를) 찾을 수 없습니다 — 설정 → AI 의 ACP 에이전트에서 실행 파일을 확인하세요`, "error");
    if (!a.remote_ack) {
      const ok = await confirmBox("ACP 에이전트로 전송",
        `${name} 은(는) 받은 내용을 에이전트에 설정된 모델(대개 이 PC 밖의 서버)로 보냅니다.\n\n` +
        "전송되는 것: 질문, 에디터의 SQL, 오류 메시지, (MCP 를 켰으면) 에이전트가 조회한 테이블 구조\n" +
        "전송되지 않는 것: 조회 결과 데이터, 비밀번호\n\n회사 보안 정책에 맞는지 확인하세요.", "계속", false);
      if (!ok) return;
      try {
        await api.acpAckRemote(name);
        a.remote_ack = true;
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
    const tools = h("div", { class: "ai-tools" });
    const body = h("div", { class: "md" });
    out.append(h("small", {}, `${name} (ACP)`), tools, body);
    this.log.append(out);
    this.input.value = "";
    let text = "";
    let pending = false;
    let usage = "";
    const started = performance.now();
    const glow = glowProgress("", { compact: true });
    this.statusEl.before(glow.el);
    const toolEls = new Map<string, { el: HTMLElement; title: string; kind: string; status: string }>();
    let planEl: HTMLElement | null = null;
    const onEvent = (ev: AcpEvent) => {
      if (ev.type === "text") {
        text += ev.text;
        this.statusEl.textContent = "답하는 중…";
        // 글자마다 다시 그리지 않고 프레임마다 한 번
        if (!pending) {
          pending = true;
          requestAnimationFrame(() => { pending = false; render(body, text, this.host); this.log.scrollTop = this.log.scrollHeight; });
        }
      } else if (ev.type === "thought") {
        this.statusEl.textContent = "생각하는 중…";
      } else if (ev.type === "tool") {
        let t = toolEls.get(ev.id);
        if (!t) {
          t = { el: h("div", { class: "ai-tool" }), title: "", kind: "", status: "pending" };
          toolEls.set(ev.id, t);
          tools.append(t.el);
        }
        // 바뀔 때(tool_call_update)는 바뀐 칸만 온다
        t.title = ev.title ?? t.title;
        t.kind = ev.kind ?? t.kind;
        t.status = ev.status ?? t.status;
        const what = `${TOOL_KIND[t.kind] ?? "도구"} ${shortTitle(t.title)}`;
        t.el.dataset.status = t.status;
        t.el.textContent = `${what} — ${TOOL_STATUS[t.status] ?? t.status}`;
        t.el.title = t.title;
        if (t.status === "in_progress") this.statusEl.textContent = `${what} …`;
      } else if (ev.type === "plan") {
        planEl ??= tools.appendChild(h("div", { class: "ai-plan" }));
        planEl.replaceChildren(...ev.entries.map((e) => h("div", {}, `${e.status === "completed" ? "✓" : e.status === "in_progress" ? "…" : "○"} ${e.content}`)));
      } else if (ev.type === "permission") {
        this.statusEl.textContent = "권한 요청을 기다리는 중…";
        void this.permission(name, ev);
      } else if (ev.type === "usage" && ev.size) {
        usage = `문맥 ${ev.used.toLocaleString()}/${ev.size.toLocaleString()} 토큰`;
      }
    };

    try {
      let done: Awaited<ReturnType<typeof api.acpPrompt>> | null = null;
      for (let attempt = 0; !done; attempt++) {
        const chat = await this.ensureAcp(name);
        this.acpBusy = chat;
        this.statusEl.textContent = "에이전트가 일하는 중…";
        try {
          done = await api.acpPrompt(name, chat.session, acpPrompt(task, ctx, question, chat.mcp, !chat.sent), onEvent);
          chat.sent = true;
        } catch (e) {
          // 에이전트가 끝났다 — 새로 띄워 새 대화로 한 번 더
          if (attempt === 0 && !text && ["acp_session_lost", "acp_closed"].includes(errOf(e).kind)) {
            this.dropAcp();
            continue;
          }
          throw e;
        }
      }
      render(body, text, this.host);
      out.classList.remove("streaming");
      const secs = ((performance.now() - started) / 1000).toFixed(1);
      const notes = [`${name} · ${secs}s`];
      if (usage) notes.push(usage);
      if (done.stop_reason === "cancelled") notes.push("취소했습니다");
      else if (done.stop_reason === "refusal") notes.push("⚠ 에이전트가 답을 거절했습니다");
      else if (done.stop_reason === "max_tokens" || done.stop_reason === "max_turn_requests") notes.push("⚠ 한도에 걸려 멈췄습니다");
      this.statusEl.textContent = notes.join(" · ");
    } catch (e) {
      const err = errOf(e);
      if (err.kind === "remote_consent") a.remote_ack = false;
      out.classList.remove("streaming");
      out.classList.add("error");
      body.textContent = (text ? text + "\n\n" : "") + err.message;
      this.statusEl.textContent = "실패";
    } finally {
      glow.done();
      this.acpBusy = null;
      this.running = null;
      this.stopBtn.disabled = true;
      this.log.scrollTop = this.log.scrollHeight;
    }
  }
}

/** 열린 ACP 대화 — mcp: sqlstudio-mcp 로 넘긴 접속, sent: 첫 질문(규칙을 붙인다)을 보냈는지 */
interface AcpChat { agent: string; session: string; mcp: string[]; sent: boolean }

const TOOL_KIND: Record<string, string> = { read: "읽기", edit: "편집", delete: "삭제", move: "이동", search: "찾기", execute: "실행", think: "생각", fetch: "가져오기", other: "도구" };
const TOOL_STATUS: Record<string, string> = { pending: "대기", in_progress: "실행 중", completed: "완료", failed: "실패" };

/** 도구 제목이 경로면 파일 이름만 */
function shortTitle(t: string) {
  const s = /[\\/]/.test(t) && !/\s/.test(t) ? t.split(/[\\/]/).pop() || t : t;
  return s.length > 80 ? s.slice(0, 80) + "…" : s;
}

/** 에이전트에게 보낼 글 — 첫 질문에만 이 앱에서 묻는다는 것과 규칙을 붙인다 (에이전트가 대화를 기억한다) */
export function acpPrompt(task: Task, ctx: ReturnType<AiHost["context"]>, question: string, mcp: string[], first: boolean): string {
  const parts: string[] = [];
  if (first) {
    parts.push("[SQLStudio — Oracle SQL 편집기의 AI 패널에서 보낸 질문]\n" +
      "규칙: 파일을 만들거나 고치지 말고 답만 한다. SQL 은 ```sql 코드 블록으로 쓴다 (사용자가 단추로 편집기에 넣는다). 한국어로 간결하게 답한다.");
    if (mcp.length) {
      parts.push(`sqlstudio MCP 도구로 Oracle 사전(테이블·컬럼·인덱스·소스)을 조회할 수 있다 — 쓸 수 있는 접속: ${mcp.join(", ")}. 테이블·컬럼 이름을 지어내지 말고 도구로 확인한다.`);
    }
  }
  if (mcp.length && ctx.profile && mcp.includes(ctx.profile)) parts.push(`지금 편집기 탭의 접속: ${ctx.profile}`);
  const ask = {
    generate: "아래 요청을 Oracle SQL 로 작성해 줘.",
    explain: "아래 SQL 이 무엇을 하는지 설명해 줘.",
    optimize: "아래 SQL 의 성능을 개선해 줘. 인덱스·조인·불필요한 연산을 살피고, 고친 SQL 과 까닭을 보여 줘.",
    fix: "아래 SQL 을 실행했더니 오류가 났다. 원인을 짚고 고친 SQL 을 보여 줘.",
    chat: "",
    trace: "",
  }[task];
  if (ask) parts.push(ask);
  if (question) parts.push(`요청: ${question}`);
  if (ctx.tables.length) parts.push(`참고할 테이블: ${ctx.tables.join(", ")}`);
  if (task !== "generate" && ctx.sql) parts.push("SQL:\n```sql\n" + ctx.sql + "\n```");
  if (task === "fix" && ctx.error) parts.push("오류:\n" + ctx.error);
  return parts.join("\n\n");
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
