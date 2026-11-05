// npm test — Oracle 구문 강조의 주석·문자열 구분
import { syntaxTree } from "@codemirror/language";
import { EditorState } from "@codemirror/state";
import { oracleSql } from "./oracle-dialect";

let n = 0;
/** 주석·문자열 마디의 글 */
function marks(doc: string): string[] {
  const state = EditorState.create({ doc, extensions: [oracleSql()] });
  const out: string[] = [];
  syntaxTree(state).iterate({ enter: (x) => { if (/Comment|String/.test(x.name)) out.push(`${x.name}:${doc.slice(x.from, x.to)}`); } });
  return out;
}
function eq(doc: string, want: string[]) {
  const got = marks(doc);
  if (JSON.stringify(got) !== JSON.stringify(want)) throw new Error(`${JSON.stringify(doc)}\n  got  ${JSON.stringify(got)}\n  want ${JSON.stringify(want)}`);
  n++;
}

// 주석 안의 /* 는 중첩이 아니다 — 처음 나오는 */ 에서 끝난다
eq("/*-- 머리말 /* 메모 */\nBEGIN x := 'a'; END;", ["BlockComment:/*-- 머리말 /* 메모 */", "String:'a'"]);
eq("/* a /* b */ y := 1; /* c */", ["BlockComment:/* a /* b */", "BlockComment:/* c */"]);
// 상자 꼴 머리말
eq("/*****\n * box\n *****/\nBEGIN NULL; END;", ["BlockComment:/*****\n * box\n *****/"]);
eq("/**/ x := 1;", ["BlockComment:/**/"]);
eq("/*/ x */ y := 1;", ["BlockComment:/*/ x */"]);
// 닫히지 않으면 끝까지
eq("x := 1; /* open", ["BlockComment:/* open"]);
// 문자열·한 줄 주석·나누기는 그대로
eq("x := q'[it's /* no]'; y := a/b*c; -- end", ["String:q'[it's /* no]'", "LineComment:-- end"]);
eq("x := '/*'; y := '*/';", ["String:'/*'", "String:'*/'"]);

console.log(`  Oracle 주석·문자열 ${n}건`);
