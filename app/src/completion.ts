// 구절 인식 자동완성 — 판단은 Rust(sqls-core::complete)가 하고, 여기서는 언제 부를지만 정한다.
//
// 빠르게 하는 방법:
// - 단어를 시작할 때(또는 '.' / 구절 키워드 뒤 공백) 한 번만 묻는다. 그 단어를 이어 치는 동안은
//   validFor 로 화면에서 거른다 — 키 하나마다 백엔드를 부르지 않는다.
// - 문서 전체가 아니라 커서 앞뒤 2만 자만 보낸다.
// - 백엔드는 접속 때 읽어 둔 캐시만 본다 (DB 왕복 없음).

import type { Completion, CompletionContext, CompletionResult, CompletionSource } from "@codemirror/autocomplete";
import { api, byteToUtf16, utf16ToByte, type CompletionItem } from "./api";

const WINDOW = 20000;
/** 이 키워드 뒤에 공백을 치면 바로 후보를 띄운다 */
const CLAUSE = new Set([
  "SELECT", "FROM", "JOIN", "WHERE", "AND", "OR", "ON", "BY", "SET", "INTO", "UPDATE", "TABLE",
  "HAVING", "USING", "DISTINCT", "EXEC", "EXECUTE", "CALL", "DESC", "DESCRIBE", "WITH", "NOT", "PRIOR",
]);
const IDENT_END = /[\p{L}\p{N}_$#]*$/u;
const VALID = /^[\p{L}\p{N}_$#]*$/u;

const TYPE: Record<string, string> = {
  table: "class", view: "interface", synonym: "class", column: "property", alias: "variable",
  function: "function", procedure: "method", package: "namespace", schema: "namespace",
  sequence: "constant", type: "type", keyword: "keyword", join: "join", snippet: "text",
};

function toCompletion(i: CompletionItem): Completion {
  return {
    label: i.label,
    type: TYPE[i.kind] ?? "text",
    detail: i.detail ?? undefined,
    info: i.info ?? undefined,
    apply: i.apply ?? undefined,
    boost: i.boost,
  };
}

export function sqlCompletion(sessionId: () => number | undefined): CompletionSource {
  return async (ctx: CompletionContext): Promise<CompletionResult | null> => {
    const id = sessionId();
    if (id == null) return null;
    const pos = ctx.pos;
    const line = ctx.state.doc.lineAt(pos);
    const before = line.text.slice(0, pos - line.from);
    const word = IDENT_END.exec(before)![0];
    const head = before.slice(0, before.length - word.length);
    const charBefore = head.slice(-1);
    const prevWord = /([A-Za-z_]+)\s+$/.exec(head)?.[1]?.toUpperCase();
    const trigger =
      ctx.explicit ||
      word.length > 0 ||
      charBefore === "." ||
      (word.length === 0 && (
        (prevWord !== undefined && CLAUSE.has(prevWord)) ||
        /,\s*$/.test(head) ||
        /\(\s*$/.test(head)
      ));
    // 숫자로 시작하는 단어 (1, 2.5 …) 에는 띄우지 않는다
    if (!trigger || /^\d/.test(word)) return null;

    const doc = ctx.state.doc;
    const from = Math.max(0, pos - WINDOW);
    const to = Math.min(doc.length, pos + WINDOW);
    const text = doc.sliceString(from, to);
    let r;
    try {
      r = await api.complete(id, text, utf16ToByte(text, pos - from));
    } catch {
      return null;
    }
    if (ctx.aborted || !r.items.length) return null;
    return {
      from: from + byteToUtf16(text, r.from),
      options: r.items.map(toCompletion),
      validFor: VALID,
    };
  };
}
