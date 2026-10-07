// AI 패널 — 공급자 선택, 작업 버튼, 스트리밍 답, "에디터에 넣기".

import { api, errOf, type ChatMsg, type Provider, type Task } from "./api";
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
  private input = h("textarea", { class: "ai-input", rows: 3, placeholder: "무엇을 할까요? 예) 최근 7일 주문을 고객별로 합계 (Ctrl+Enter)" });
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
    // 외부 전송은 공급자마다 처음 한 번 확인한다
    const ackKey = `ai.ack.${p.name}`;
    if (p.remote && !localStorage.getItem(ackKey)) {
      const ok = await confirmBox("외부 AI 로 전송",
        `${p.name} (${p.base_url ?? p.kind}) 은 이 PC 밖의 서버입니다.\n\n` +
        "전송되는 것: 에디터의 SQL, 관련 테이블 구조(컬럼·주석·인덱스), 실행계획, 오류 메시지\n" +
        "전송되지 않는 것: 조회 결과 데이터, 비밀번호\n\n회사 보안 정책에 맞는지 확인하세요.", "계속", false);
      if (!ok) return;
      localStorage.setItem(ackKey, "1");
    }

    const ctx = this.host.context();
    const question = this.input.value.trim();
    if (task === "generate" && !question) return toast("무엇을 만들지 입력하세요", "error");
    if ((task === "explain" || task === "optimize") && !ctx.sql) return toast("에디터에 SQL 이 없습니다", "error");
    if (task === "fix" && !ctx.error) return toast("고칠 오류가 없습니다 — 먼저 실행해 보세요", "error");

    const id = reqSeq++;
    this.running = id;
    this.stopBtn.disabled = false;
    const label = { generate: "SQL 생성", explain: "설명", optimize: "튜닝", fix: "오류 수정", chat: "질문" }[task];
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

    try {
      const ans = await api.aiAsk({
        request_id: id, provider: p.name, task, session_id: ctx.sessionId,
        sql: task === "generate" ? undefined : ctx.sql, question: question || undefined,
        error: task === "fix" ? ctx.error : undefined, tables: ctx.tables,
        history: this.history,
      }, (ev) => {
        if (ev.type === "status") this.statusEl.textContent = ev.text;
        else if (ev.type === "context") {
          meta.textContent = `문맥: ${ev.tables.length ? ev.tables.join(", ") : "테이블 없음"}${ev.plan ? " + 실행계획" : ""}${ev.remote ? " · 외부" : ""}`;
        } else if (ev.type === "delta") {
          text += ev.text;
          this.statusEl.textContent = "답하는 중…";
          // 글자마다 다시 그리지 않고 프레임마다 한 번
          if (!pending) {
            pending = true;
            requestAnimationFrame(() => { pending = false; render(body, text, this.host); this.log.scrollTop = this.log.scrollHeight; });
          }
        }
      });
      render(body, ans.text, this.host);
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
      out.classList.remove("streaming");
      out.classList.add("error");
      body.textContent = (text ? text + "\n\n" : "") + err.message;
      this.statusEl.textContent = err.kind === "cancelled" ? "취소했습니다" : "실패";
    } finally {
      clearInterval(tick);
      delete this.statusEl.dataset.elapsed;
      this.running = null;
      this.stopBtn.disabled = true;
      this.log.scrollTop = this.log.scrollHeight;
    }
  }
}

/** 아주 작은 마크다운: 코드 블록만 따로 그리고 나머지는 글자 그대로 */
function render(el: HTMLElement, text: string, host: AiHost) {
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
        isSql ? h("button", { onclick: () => host.insertSql(code) }, "커서에 넣기") : null,
        isSql ? h("button", { onclick: () => host.replaceSql(code) }, "문장 바꾸기") : null,
        h("button", { onclick: () => navigator.clipboard.writeText(code).then(() => toast("복사했습니다", "ok")) }, "복사"),
      ) : null,
    ));
    last = m.index + m[0].length;
  }
  if (last < text.length) parts.push(h("p", {}, text.slice(last).trim()));
  el.replaceChildren(...parts.filter((p) => !(p instanceof HTMLParagraphElement && !p.textContent)));
}
