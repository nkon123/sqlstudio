// 코드 조각 (Toad 의 Code Templates) — 줄임말을 치면 자동완성에 뜨고, 고르면 틀이 들어간다.
//
// 틀 안의 ${이름} 은 채울 칸이다: Tab 으로 다음 칸, Shift+Tab 으로 앞 칸, Esc 로 끝. 이름이 같은 칸은 함께 바뀐다.
// 설정 폴더의 snippets.json 에 둔다 — 파일을 나눠 주면 팀이 같은 조각을 쓴다 (설정 > 편집기 > 내보내기·가져오기).

import { snippetCompletion, type CompletionContext, type CompletionResult, type CompletionSource } from "@codemirror/autocomplete";
import { api } from "./api";

export interface Snip { key: string; desc: string; body: string }

export const DEFAULT_SNIPPETS: Snip[] = [
  { key: "sel", desc: "SELECT … FROM … WHERE", body: "SELECT ${*}\n  FROM ${table}\n WHERE ${1 = 1}" },
  { key: "selc", desc: "건수 세기", body: "SELECT COUNT(*)\n  FROM ${table}\n WHERE ${1 = 1}" },
  { key: "ins", desc: "INSERT", body: "INSERT INTO ${table} (${columns})\nVALUES (${values})" },
  { key: "upd", desc: "UPDATE … WHERE", body: "UPDATE ${table}\n   SET ${column} = ${value}\n WHERE ${condition}" },
  { key: "del", desc: "DELETE … WHERE", body: "DELETE FROM ${table}\n WHERE ${condition}" },
  { key: "mer", desc: "MERGE (있으면 UPDATE, 없으면 INSERT)", body: "MERGE INTO ${target} t\nUSING ${source} s\n   ON (t.${id} = s.${id})\n WHEN MATCHED THEN\n   UPDATE SET t.${column} = s.${column}\n WHEN NOT MATCHED THEN\n   INSERT (${id}, ${column}) VALUES (s.${id}, s.${column})" },
  { key: "case", desc: "CASE WHEN", body: "CASE WHEN ${condition} THEN ${a} ELSE ${b} END" },
  { key: "dup", desc: "중복 행 찾기", body: "SELECT ${columns}, COUNT(*)\n  FROM ${table}\n GROUP BY ${columns}\nHAVING COUNT(*) > 1" },
  { key: "beg", desc: "익명 블록 (예외 처리 포함)", body: "DECLARE\n  ${v_name} ${NUMBER};\nBEGIN\n  ${NULL;}\nEXCEPTION\n  WHEN OTHERS THEN\n    DBMS_OUTPUT.PUT_LINE(SQLERRM);\n    RAISE;\nEND;\n/" },
  { key: "forr", desc: "커서 FOR 루프", body: "FOR r IN (SELECT ${*}\n            FROM ${table}\n           WHERE ${1 = 1}) LOOP\n  ${NULL;}\nEND LOOP;" },
  { key: "exc", desc: "EXCEPTION 절", body: "EXCEPTION\n  WHEN NO_DATA_FOUND THEN\n    ${NULL;}\n  WHEN OTHERS THEN\n    RAISE;" },
  { key: "put", desc: "DBMS_OUTPUT.PUT_LINE", body: "DBMS_OUTPUT.PUT_LINE(${});" },
  { key: "xplan", desc: "직전 실행의 실제 실행계획", body: "SELECT * FROM TABLE(DBMS_XPLAN.DISPLAY_CURSOR(NULL, NULL, 'ALLSTATS LAST'))" },
  { key: "tsize", desc: "테이블 크기 (MB)", body: "SELECT segment_name, segment_type, ROUND(SUM(bytes) / 1024 / 1024, 1) AS mb\n  FROM user_segments\n WHERE segment_name = UPPER('${table}')\n GROUP BY segment_name, segment_type" },
  { key: "lock", desc: "잠긴 객체와 세션", body: "SELECT o.owner, o.object_name, s.sid, s.serial#, s.username, s.program, s.status\n  FROM v$locked_object l\n  JOIN dba_objects o ON o.object_id = l.object_id\n  JOIN v$session s ON s.sid = l.session_id" },
];

let list: Snip[] = DEFAULT_SNIPPETS;
let file: string | null = null;

/** 설정 폴더 (config.toml 이 있는 곳) 의 snippets.json 을 읽는다. 없으면 기본 조각 */
export async function loadSnippets(configPath: string) {
  file = configPath.replace(/[\\/][^\\/]*$/, "") + "/snippets.json";
  try {
    const f = await api.readFile(file);
    list = parse(f.text);
  } catch {
    list = DEFAULT_SNIPPETS;
  }
}

export function parse(text: string): Snip[] {
  const v = JSON.parse(text);
  const arr = Array.isArray(v) ? v : v?.snippets;
  if (!Array.isArray(arr)) throw new Error("조각 목록이 아닙니다 (배열 또는 { snippets: [...] })");
  return arr.filter((x) => x && typeof x.key === "string" && typeof x.body === "string" && x.key.trim())
    .map((x) => ({ key: x.key.trim(), desc: String(x.desc ?? ""), body: x.body }));
}

export function snippets() {
  return list;
}

export async function saveSnippets(next: Snip[]) {
  if (!file) throw new Error("설정 폴더를 아직 모릅니다");
  await api.writeFile(file, JSON.stringify({ snippets: next }, null, 2), "utf-8");
  list = next;
}

const WORD = /[A-Za-z_][\w$#]*$/;

/** 자동완성 출처 — 접속하지 않아도 뜬다. 줄임말 앞부분이 맞는 것 */
export const snippetSource: CompletionSource = (ctx: CompletionContext): CompletionResult | null => {
  const line = ctx.state.doc.lineAt(ctx.pos);
  const before = line.text.slice(0, ctx.pos - line.from);
  const m = WORD.exec(before);
  // '.' 뒤(별칭.컬럼)에는 띄우지 않는다
  if (!m || before[before.length - m[0].length - 1] === ".") return null;
  const w = m[0].toLowerCase();
  const hits = list.filter((s) => s.key.toLowerCase().startsWith(w));
  if (!hits.length) return null;
  return {
    from: ctx.pos - m[0].length,
    options: hits.map((s) => snippetCompletion(s.body, {
      label: s.key, detail: `조각 · ${s.desc}`, type: "text",
      info: s.body.replace(/\$\{([^}]*)\}/g, "$1"),
      // 정확히 같은 줄임말이면 맨 위, 아니면 테이블·컬럼 후보 아래
      boost: s.key.toLowerCase() === w ? 50 : -20,
    })),
    validFor: /^[\w$#]*$/,
  };
};
