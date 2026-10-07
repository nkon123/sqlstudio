// Rust 명령의 타입 있는 껍데기. 명령 이름·인자는 src-tauri/src/*.rs 와 같아야 한다.
import { Channel, invoke } from "@tauri-apps/api/core";

export interface ErrView { kind: string; message: string; ora_code?: number; offset?: number }

export function errOf(e: unknown): ErrView {
  if (e && typeof e === "object" && "message" in e) return e as ErrView;
  return { kind: "unknown", message: String(e) };
}

export interface Profile {
  name: string; user: string; connect_string: string;
  read_only?: boolean; as_sysdba?: boolean; call_timeout_secs?: number | null; color?: string | null;
  has_env_password?: boolean;
  has_saved_password?: boolean;
}
export interface Connected {
  id: number; user: string; connect_string: string; server_version: string; read_only: boolean; color?: string | null;
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
export interface Analyzed { statement: Statement | null; binds: string[]; confirm: string | null }
export interface ObjectEntry { owner: string; name: string; object_type: string; status: string; last_ddl_time: string }
export interface ColumnDesc { name: string; data_type: string; nullable: boolean; default?: string | null; comment?: string | null }
export interface TableDesc {
  owner: string; name: string; object_type: string; comment?: string | null; num_rows?: string | null;
  last_analyzed?: string | null; columns: ColumnDesc[]; primary_key: string[];
  indexes: { name: string; unique: boolean; columns: string[] }[];
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
  effort?: string | null; fallbacks?: boolean; connect_timeout_secs?: number; idle_timeout_secs?: number;
  remote?: boolean; has_key?: boolean;
}
export type Task = "generate" | "explain" | "optimize" | "fix" | "chat";
export interface ChatMsg { role: "user" | "assistant"; content: string }
export type AiEvent =
  | { type: "status"; text: string }
  | { type: "context"; tables: string[]; plan: boolean; remote: boolean }
  | { type: "delta"; text: string };
export interface AiAnswer {
  text: string; sql?: string | null; model?: string | null; stop_reason?: string | null;
  refused: boolean; truncated: boolean; input_tokens?: number | null; output_tokens?: number | null; user_message: string;
}
export interface CompletionItem {
  label: string; kind: string; detail?: string | null; info?: string | null; apply?: string | null; boost: number;
}
export interface CompletionResultView { from: number; items: CompletionItem[]; context: string }
export interface CompletionStatus { profile: string; phase: string; objects: number; columns: number; error?: string | null }
export interface McpSettings { allowed_connections: string[]; call_timeout_secs: number; allow_explain: boolean }

export const api = {
  appInfo: () => invoke<{ version: string; config_path: string; config_error?: string | null }>("app_info"),
  listProfiles: () => invoke<Profile[]>("list_profiles"),
  saveProfile: (profile: Profile, originalName?: string) => invoke<void>("save_profile", { profile, originalName }),
  deleteProfile: (name: string) => invoke<void>("delete_profile", { name }),
  connect: (profile: string, password?: string, remember?: boolean) => invoke<Connected>("connect", { profile, password, remember }),
  forgetPassword: (profile: string) => invoke<void>("forget_password", { profile }),
  disconnect: (id: number) => invoke<void>("disconnect", { id }),
  gridEditable: (id: number, sql: string) => invoke<{ sql: string; table: string; columns: string[] }>("grid_editable", { id, sql }),
  gridApply: (id: number, table: string, changes: { edits: unknown[]; deletes: string[] }, dryRun: boolean) =>
    invoke<{ done: [string, number][]; txn_pending: boolean }>("grid_apply", { id, table, edits: changes.edits, deletes: changes.deletes, dryRun }),
  execute: (id: number, sql: string, binds: [string, string | null][], confirmed: boolean, pageSize?: number) =>
    invoke<ExecView>("execute", { args: { id, sql, binds, confirmed, page_size: pageSize } }),
  executeScript: (id: number, script: string, stopOnError: boolean, confirmed: boolean, onEvent: (e: ScriptEvent) => void) => {
    const ch = new Channel<ScriptEvent>();
    ch.onmessage = onEvent;
    return invoke<ScriptSummary>("execute_script", { id, script, stopOnError, confirmed, onEvent: ch });
  },
  fetchMore: (id: number, rows: number) => invoke<RowPage>("fetch_more", { id, rows }),
  cancel: (id: number) => invoke<void>("cancel", { id }),
  abandon: (id: number) => invoke<void>("abandon", { id }),
  commit: (id: number) => invoke<void>("commit", { id }),
  rollback: (id: number) => invoke<void>("rollback", { id }),
  explain: (id: number, sql: string) => invoke<string[]>("explain", { id, sql }),
  analyze: (text: string, cursor: number, selection?: string) => invoke<Analyzed>("analyze_sql", { text, cursor, selection }),
  listSchemas: (id: number) => invoke<string[]>("list_schemas", { id }),
  listObjects: (id: number, owner?: string, objectType?: string, nameLike?: string) =>
    invoke<ObjectEntry[]>("list_objects", { id, owner, objectType, nameLike }),
  describe: (id: number, name: string) => invoke<TableDesc>("describe", { id, name }),
  getDdl: (id: number, objectType: string, name: string) => invoke<string>("get_ddl", { id, objectType, name }),
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
  aiAsk: (args: {
    request_id: number; provider: string; task: Task; session_id?: number; sql?: string; question?: string;
    error?: string; tables?: string[]; history?: ChatMsg[];
  }, onEvent: (e: AiEvent) => void) => {
    const ch = new Channel<AiEvent>();
    ch.onmessage = onEvent;
    return invoke<AiAnswer>("ai_ask", { args, onEvent: ch });
  },
  aiCancel: (requestId: number) => invoke<void>("ai_cancel", { requestId }),
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
