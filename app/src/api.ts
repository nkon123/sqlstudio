// Rust 명령의 타입 있는 껍데기. 명령 이름·인자는 src-tauri/src/*.rs 와 같아야 한다.
import { Channel, invoke } from "@tauri-apps/api/core";
import type { Step } from "./steps";

export interface ErrView { kind: string; message: string; ora_code?: number; offset?: number }

export function errOf(e: unknown): ErrView {
  if (e && typeof e === "object" && "message" in e) return e as ErrView;
  return { kind: "unknown", message: String(e) };
}

export interface Profile {
  name: string; user: string; connect_string: string;
  read_only?: boolean; as_sysdba?: boolean; call_timeout_secs?: number | null; color?: string | null;
  production?: boolean;
  /** 속한 그룹 — ERP 또는 ERP/운영 (두 단계까지) */
  group?: string | null;
  has_env_password?: boolean;
  has_saved_password?: boolean;
}
/** 접속 그룹 — 목록 순서가 화면 순서 */
export interface Group { path: string; color?: string | null }
export interface Connected {
  id: number; user: string; connect_string: string; server_version: string; read_only: boolean; color?: string | null; production?: boolean;
  /** 서버 세션 번호 — 같으면 같은 접속 */
  sid?: number;
}
export interface Column { name: string; type_name: string; nullable: boolean }
export interface RowPage { columns: Column[]; rows: (string | null)[][]; has_more: boolean; fetched_total: number }
export type StmtKind = "query" | "dml" | "ddl" | "plsql" | "plsql_unit" | "tcl" | "session" | "explain" | "sql_plus" | "other";
export type Outcome =
  | ({ type: "rows" } & RowPage)
  | { type: "affected"; rows: number }
  | { type: "done" }
  | { type: "skipped"; reason: string };
export interface ExecView { kind: StmtKind; outcome: Outcome; elapsed_ms: number; output: string[]; txn_pending: boolean }
export interface Statement { text: string; kind: StmtKind; start: number; end: number; line: number }
export interface TnsEntry { alias: string; host?: string | null; port?: string | null; service?: string | null }
export interface TnsView { dir?: string | null; configured?: string | null; entries: TnsEntry[]; error?: string | null }
export interface Analyzed { statement: Statement | null; binds: string[]; confirm: string | null }
export interface ObjectEntry { owner: string; name: string; object_type: string; status: string; last_ddl_time: string }
export interface ColumnDesc { name: string; data_type: string; nullable: boolean; default?: string | null; comment?: string | null }
/** 객체 의존의 상대 하나 — lines 는 Used By 만 (그 소스·뷰 정의에서 이 객체 이름이 나오는 줄) */
export interface DepObj { owner: string; name: string; kind: string; status: string; how: string; lines: { line: number; text: string; in_comment: boolean }[] }
export interface ObjectDeps { object: string; used_by: DepObj[]; uses: DepObj[] }

export interface TableDesc {
  owner: string; name: string; object_type: string; comment?: string | null; num_rows?: string | null;
  last_analyzed?: string | null; columns: ColumnDesc[]; primary_key: string[];
  indexes: { name: string; unique: boolean; columns: string[] }[];
  /** 자동완성 캐시로 바로 만든 것 — 기본값·인덱스·통계 없음 */
  partial?: boolean;
}
export type ScriptEvent =
  | { event: "started"; index: number; total: number; line: number; kind: StmtKind; preview: string }
  | { event: "finished"; index: number; elapsed_ms: number; summary: string; output: string[] }
  | { event: "failed"; index: number; line: number; error: ErrView }
  | { event: "rows"; index: number; page: RowPage };
export interface ScriptSummary { total: number; ok: number; failed: number; stopped: boolean; txn_pending: boolean }

export type ProviderKind = "ollama" | "openai_compatible" | "anthropic";
export interface Provider {
  name: string; kind: ProviderKind; base_url?: string | null; model: string; api_key_env?: string | null;
  max_tokens?: number; temperature?: number | null; num_ctx?: number | null; keep_alive?: string | null;
  /** 한 번에 넣을 문맥의 상한(토큰). 비우면 로컬은 min(num_ctx×0.6, 8000) */
  context_budget?: number | null;
  effort?: string | null; fallbacks?: boolean; connect_timeout_secs?: number; idle_timeout_secs?: number;
  /** 생각(thinking) 조절 — Ollama·OpenAI 호환: off | on | low | medium | high. 비우면 모델 기본 */
  think?: string | null;
  remote?: boolean; remote_ok?: boolean; has_key?: boolean;
  /** "auto"(기본) | "none" | "system" */
  proxy?: string | null;
  /** 모델 측정 결과 (모델을 바꾸면 하네스가 쓰지 않는다) */
  profile?: ModelProfile | null;
}
/** 모델 측정 — 첫 글자 이후 초당 토큰, 형식(JSON) 지정, 생각 모드, 모델 문맥 크기(Ollama) */
export interface ModelProfile { model: string; tok_per_sec: number; json: boolean; think: boolean; context?: number | null }
export interface HarnessOpts { table_pick: "auto" | "off"; examples: boolean; examples_count: number; examples_path: string }
export type Task = "generate" | "explain" | "optimize" | "fix" | "chat" | "trace";
export interface ChatMsg { role: "user" | "assistant"; content: string }
export type AiEvent =
  | { type: "status"; text: string }
  /** 하네스 단계 (답 위 칩) — 같은 id 는 덮어쓴다 */
  | ({ type: "step" } & Step)
  | { type: "delta"; text: string }
  | { type: "reset" };
/** 답의 SQL 확인 — ok: 서버 파싱까지 · static: 앱 안의 검사만 · failed: 고치지 못함 · none: SQL 없음 */
export interface AiVerify { state: "ok" | "static" | "failed" | "none"; note?: string | null; repairs: number; first_problem?: string | null }
export interface AiAnswer {
  text: string; sql?: string | null; model?: string | null; stop_reason?: string | null;
  refused: boolean; truncated: boolean; input_tokens?: number | null; output_tokens?: number | null; user_message: string;
  verify?: AiVerify; warnings?: string[];
}
export interface CompletionItem {
  label: string; kind: string; detail?: string | null; info?: string | null; apply?: string | null; boost: number;
  /** 이름이 아니라 설명(COMMENT)으로 맞은 후보 */
  note?: boolean;
}
export interface CompletionResultView { from: number; items: CompletionItem[]; context: string }
export interface CompletionStatus { profile: string; phase: string; objects: number; columns: number; error?: string | null }
export interface McpSettings { allowed_connections: string[]; call_timeout_secs: number; allow_explain: boolean }
/** ACP 에이전트 (opencode 등) — 설정의 [[acp]] */
export interface AcpAgent {
  name: string; command: string; args: string[]; cwd?: string | null; mcp: boolean; pure?: boolean; mode?: string | null; model?: string | null; remote_ack?: boolean;
  /** 실행 파일을 찾았는지 · 지금 띄워져 있는지 (읽을 때만) */
  available?: boolean; running?: boolean;
}
/** 에이전트가 고르게 하는 것 (opencode: model · mode) */
export interface AcpConfigOption { id: string; name: string; currentValue: string; options: { value: string; name: string; description?: string }[] }
export interface AcpSession { session_id: string; options: AcpConfigOption[]; mcp_profiles: string[]; agent: { name?: string; version?: string } | null }
export interface AcpPermOption { id: string; name: string; kind: "allow_once" | "allow_always" | "reject_once" | "reject_always" | string }
export type AcpEvent =
  | { type: "text"; text: string }
  | { type: "thought"; text: string }
  | { type: "tool"; id: string; title?: string | null; kind?: string | null; status?: string | null }
  | { type: "plan"; entries: { content: string; status: string }[] }
  | { type: "permission"; key: number; title: string; detail: string; options: AcpPermOption[] }
  | { type: "usage"; used: number; size: number };

export const api = {
  /** 조회 결과 컬럼 → 설명 (자동완성 캐시로만) */
  columnNotes: (id: number, sql: string) => invoke<Record<string, string>>("column_notes", { id, sql }),
  appInfo: () => invoke<{ version: string; config_path: string; config_error?: string | null; log_dir?: string }>("app_info"),
  /** 화면에서 일어난 일을 앱 로그 파일에 (접속 끊김·다시 접속 등) */
  log: (level: "info" | "warn", message: string) => { invoke("client_log", { level, message }).catch(() => {}); },
  /** 탭 다시 접속 결과를 접속 기록에 */
  connEvent: (kind: "recover_ok" | "recover_fail", message: string) => { invoke("conn_event", { kind, message }).catch(() => {}); },
  listProfiles: () => invoke<Profile[]>("list_profiles"),
  saveProfile: (profile: Profile, originalName?: string) => invoke<void>("save_profile", { profile, originalName }),
  deleteProfile: (name: string) => invoke<void>("delete_profile", { name }),
  moveProfile: (name: string, up: boolean) => invoke<void>("move_profile", { name, up }),
  listGroups: () => invoke<Group[]>("list_groups"),
  /** 그룹 만들기·고치기 — original 이 있으면 그 그룹의 이름·위치를 바꾼다 (안의 접속도 따라간다) */
  saveGroup: (path: string, color: string | null, original?: string) => invoke<void>("save_group", { path, color, original }),
  /** 그룹 지우기 — withConnections 가 아니면 안의 접속은 상위 그룹(맨 위면 그룹 없음)으로 */
  deleteGroup: (path: string, withConnections: boolean) => invoke<void>("delete_group", { path, withConnections }),
  moveGroup: (path: string, up: boolean) => invoke<void>("move_group", { path, up }),
  connect: (profile: string, password?: string, remember?: boolean) => invoke<Connected>("connect", { profile, password, remember }),
  forgetPassword: (profile: string) => invoke<void>("forget_password", { profile }),
  tnsList: () => invoke<TnsView>("tns_list"),
  setTnsAdmin: (dir: string | null) => invoke<void>("set_tns_admin", { dir }),
  disconnect: (id: number) => invoke<void>("disconnect", { id }),
  gridEditable: (id: number, sql: string) => invoke<{ sql: string; table: string; columns: string[] }>("grid_editable", { id, sql }),
  gridApply: (id: number, table: string, changes: { edits: unknown[]; deletes: string[]; inserts: unknown[] }, dryRun: boolean) =>
    invoke<{ done: [string, number][]; txn_pending: boolean }>("grid_apply", { id, table, edits: changes.edits, deletes: changes.deletes, inserts: changes.inserts, dryRun }),
  execute: (id: number, sql: string, binds: [string, string | null][], confirmed: boolean, pageSize?: number) =>
    invoke<ExecView>("execute", { args: { id, sql, binds, confirmed, page_size: pageSize } }),
  executeScript: (id: number, script: string, stopOnError: boolean, confirmed: boolean, onEvent: (e: ScriptEvent) => void) => {
    const ch = new Channel<ScriptEvent>();
    ch.onmessage = onEvent;
    return invoke<ScriptSummary>("execute_script", { id, script, stopOnError, confirmed, onEvent: ch });
  },
  fetchMore: (id: number, rows: number) => invoke<RowPage>("fetch_more", { id, rows }),
  exportXlsx: (path: string, columns: { name: string; type_name: string }[], rows: (string | null)[][], sql: string | null) =>
    invoke<number>("export_xlsx", { args: { path, columns, rows, sql } }),
  cancel: (id: number) => invoke<void>("cancel", { id }),
  abandon: (id: number) => invoke<void>("abandon", { id }),
  commit: (id: number, confirmed = false) => invoke<void>("commit", { id, confirmed }),
  rollback: (id: number) => invoke<void>("rollback", { id }),
  /** 같은 조회를 다른 접속(프로필)에서 읽기 전용으로 — 결과 비교. schema 는 탭의 기준 스키마 */
  queryOther: (profile: string, sql: string, binds: [string, string | null][], schema: string | null, password: string | null, maxRows: number) =>
    invoke<{ page: RowPage; ms: number; label: string; note: string | null }>("query_other", { profile, sql, binds, schema, password, maxRows }),
  explain: (id: number, sql: string) => invoke<string[]>("explain", { id, sql }),
  analyze: (text: string, cursor: number, selection?: string, id?: number) => invoke<Analyzed>("analyze_sql", { text, cursor, selection, id }),
  /** 계정을 적지 않은 이름을 찾을 스키마 — null 이면 접속 계정으로. 건 스키마 이름을 돌려준다 */
  setSchema: (id: number, schema: string | null) => invoke<string>("set_schema", { id, schema }),
  listSchemas: (id: number) => invoke<string[]>("list_schemas", { id }),
  listObjects: (id: number, owner?: string, objectType?: string, nameLike?: string) =>
    invoke<ObjectEntry[]>("list_objects", { id, owner, objectType, nameLike }),
  /** 결과의 전체 건수 (SELECT COUNT(*) FROM (…)) — 읽기 전용 사전 조회 세션으로 */
  countRows: (id: number, sql: string, binds: [string, string | null][]) => invoke<{ rows: string; elapsed_ms: number }>("count_rows", { id, sql, binds }),
  describe: (id: number, name: string) => invoke<TableDesc>("describe", { id, name }),
  /** DB 에 묻지 않는 구조 (기억해 둔 것 또는 자동완성 캐시, partial) — 없으면 null */
  describeFast: (id: number, name: string) => invoke<TableDesc | null>("describe_fast", { id, name }),
  /** grants: 그 객체에 준 GRANT 를 뒤에 붙인다 (스크립트 보기·DDL 새 탭) */
  getDdl: (id: number, objectType: string, name: string, grants = false) => invoke<string>("get_ddl", { id, objectType, name, grants }),
  objectDeps: (id: number, name: string) => invoke<ObjectDeps>("object_deps", { id, name }),
  complete: (id: number, text: string, cursor: number) => invoke<CompletionResultView>("complete", { id, text, cursor }),
  completionStatus: (id: number) => invoke<CompletionStatus>("completion_status", { id }),
  refreshCompletion: (id: number) => invoke<void>("refresh_completion", { id }),
  getMcp: () => invoke<McpSettings>("get_mcp_settings"),
  setMcp: (s: McpSettings) => invoke<void>("set_mcp_settings", { allowedConnections: s.allowed_connections, callTimeoutSecs: s.call_timeout_secs, allowExplain: s.allow_explain }),
  mcpSnippet: () => invoke<string>("mcp_snippet"),
  listProviders: () => invoke<Provider[]>("list_providers"),
  saveProvider: (provider: Provider, originalName?: string) => invoke<void>("save_provider", { provider, originalName }),
  setApiKey: (provider: string, key: string, remember?: boolean) => invoke<void>("set_api_key", { provider, key, remember }),
  testProvider: (name: string) => invoke<string[]>("test_provider", { name }),
  deleteProvider: (name: string) => invoke<void>("delete_provider", { name }),
  /** 저장하지 않은 설정(주소·종류)으로 서버의 모델 목록 — Ollama 는 /api/tags */
  listModels: (provider: Provider) => invoke<string[]>("ai_list_models", { provider }),
  aiAsk: (args: {
    request_id: number; provider: string; task: Task; session_id?: number; sql?: string; question?: string;
    error?: string; tables?: string[]; history?: ChatMsg[];
  }, onEvent: (e: AiEvent) => void) => {
    const ch = new Channel<AiEvent>();
    ch.onmessage = onEvent;
    return invoke<AiAnswer>("ai_ask", { args, onEvent: ch });
  },
  aiCancel: (requestId: number) => invoke<void>("ai_cancel", { requestId }),
  acpAgents: () => invoke<AcpAgent[]>("acp_agents"),
  acpSaveAgent: (agent: AcpAgent, originalName?: string) => invoke<void>("acp_save_agent", { agent, originalName }),
  acpDeleteAgent: (name: string) => invoke<void>("acp_delete_agent", { name }),
  acpAckRemote: (name: string) => invoke<void>("acp_ack_remote", { name }),
  /** 저장하지 않은 설정으로 따로 띄워 initialize 만 — 에이전트 이름·버전, 로그인 방법 */
  acpTestAgent: (agent: AcpAgent) => invoke<{ agent: { name?: string; version?: string } | null; auth: string[]; path: string }>("acp_test_agent", { agent }),
  /** 새 대화 — 띄우기 · 초기화 · 세션 · 모델·모드 · MCP 단계를 onStep 으로 */
  acpNewSession: (agent: string, onStep: (s: Step) => void = () => {}) => {
    const ch = new Channel<Step>();
    ch.onmessage = onStep;
    return invoke<AcpSession>("acp_new_session", { agent, onStep: ch });
  },
  /** ACP 답의 SQL 확인 — 걸리면 repair 에 에이전트에게 보낼 고쳐 달라는 글 */
  acpCheckSql: (sessionId: number | undefined, task: Task, text: string, repairs: number, firstProblem: string | null) =>
    invoke<{ step: Step; repair?: string | null; problem?: string | null }>("acp_check_sql", { sessionId: sessionId ?? null, task, text, repairs, firstProblem }),
  /** 분석 조각 크기용 — 에이전트 모델의 컨텍스트 크기와 거기서 정한 줄 수 (opencode 만, 모르면 null) */
  acpModelContext: (agent: string, model: string | null = null) => invoke<{ model: string | null; context: number | null; lines: number | null }>("acp_model_context", { agent, model }),
  /** 에이전트가 내놓는 모델 — current 는 설정의 모델, 없으면 에이전트 기본. options 는 [값, 이름] */
  acpModels: (agent: string) => invoke<{ current: string | null; options: [string, string][] }>("acp_models", { agent }),
  acpSetOption: (agent: string, sessionId: string, configId: string, value: string) =>
    invoke<AcpConfigOption[]>("acp_set_option", { agent, sessionId, configId, value }),
  acpPrompt: (agent: string, sessionId: string, text: string, onEvent: (e: AcpEvent) => void) => {
    const ch = new Channel<AcpEvent>();
    ch.onmessage = onEvent;
    return invoke<{ stop_reason: string; usage: unknown }>("acp_prompt", { agent, sessionId, text, onEvent: ch });
  },
  acpCancel: (agent: string, sessionId: string) => invoke<void>("acp_cancel", { agent, sessionId }),
  acpPermission: (agent: string, key: number, optionId: string | null) => invoke<void>("acp_permission", { agent, key, optionId }),
  /** 모델 측정 — 결과를 공급자 설정에 남긴다 */
  aiProbe: (name: string) => invoke<ModelProfile>("ai_probe", { name }),
  /** 서버 파싱을 통과한 AI 의 SQL 을 에디터에 넣었을 때 — 내 쿼리 예시로 쌓는다 */
  aiRememberExample: (sessionId: number, question: string, sql: string) => invoke<void>("ai_remember_example", { sessionId, question, sql }),
  harnessOpts: () => invoke<HarnessOpts>("ai_harness_opts"),
  setHarnessOpts: (o: { tablePick?: string; examples?: boolean }) => invoke<void>("set_ai_harness_opts", o),
  clearExamples: () => invoke<void>("ai_examples_clear"),
  readFile: (path: string) => invoke<{ text: string; encoding: string }>("read_sql_file", { path }),
  writeFile: (path: string, text: string, encoding: string) => invoke<void>("write_sql_file", { path, text, encoding }),
};

// ── UTF-8 바이트 위치 ↔ JS(UTF-16) 위치 ─────────────────────
// Rust 는 바이트 위치를 쓴다. 한글은 3바이트라 그대로 쓰면 어긋난다.
const enc = new TextEncoder();
export function utf16ToByte(text: string, pos: number): number {
  return enc.encode(text.slice(0, pos)).length;
}
export function byteToUtf16(text: string, byte: number): number {
  // 앞에서부터 세되, 대부분 ASCII 라 빠르다
  let b = 0;
  for (let i = 0; i < text.length; i++) {
    if (b >= byte) return i;
    const c = text.charCodeAt(i);
    if (c < 0x80) b += 1;
    else if (c < 0x800) b += 2;
    else if (c >= 0xd800 && c <= 0xdbff) { b += 4; i++; }
    else b += 3;
  }
  return text.length;
}
