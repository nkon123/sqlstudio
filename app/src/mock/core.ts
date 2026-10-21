// 가짜 DB 모드에서만 "@tauri-apps/api/core" 대신 쓰인다(vite.config.ts 별칭).
// 앱 창의 __TAURI_INTERNALS__.invoke 는 읽기 전용이라 갈아 끼울 수 없어, 앱 코드가 부르는 invoke 를 여기서 가로챈다.
import { invoke as realInvoke } from "../../node_modules/@tauri-apps/api/core.js";
import type { InvokeArgs, InvokeOptions } from "../../node_modules/@tauri-apps/api/core.js";

export * from "../../node_modules/@tauri-apps/api/core.js";

type Handler = (args: Record<string, unknown>) => unknown;

export function invoke<T>(cmd: string, args: InvokeArgs = {}, options?: InvokeOptions): Promise<T> {
  const db = (window as unknown as { __SQLS_MOCK_DB__?: Record<string, Handler> }).__SQLS_MOCK_DB__;
  const f = db?.[cmd];
  return f ? Promise.resolve(f(args as Record<string, unknown>) as T) : realInvoke<T>(cmd, args, options);
}
