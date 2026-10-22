// npm test — 괄호 쌍 찾기 (문법 트리로 — 문자열·주석 안의 괄호는 세지 않는다)
import { sql } from "@codemirror/lang-sql";
import { EditorState } from "@codemirror/state";
import { enclosingPair } from "./brackets";
import { ORACLE } from "./oracle-dialect";

let n = 0;
/** `|` 자리의 커서를 감싼 쌍을 "(…)" 글로 */
function at(src: string): string | null {
  const pos = src.indexOf("|");
  const doc = src.replace("|", "");
  const state = EditorState.create({ doc, extensions: [sql({ dialect: ORACLE })] });
  const p = enclosingPair(state, pos);
  n++;
  return p ? doc.slice(p.open, p.close + 1) : null;
}
function eq(got: unknown, want: unknown, what: string) {
  if (got !== want) throw new Error(`${what}: ${JSON.stringify(got)}, 기대 ${JSON.stringify(want)}`);
}

const q = "select a from (select b, nvl(c, 0) from t where d in (1, 2)) x";
eq(at("select a from (select b, nvl(c, |0) from t where d in (1, 2)) x"), "(c, 0)", "가장 안쪽");
eq(at("select a from (select b, nvl(c, 0) from |t where d in (1, 2)) x"), q.slice(14, q.length - 2), "서브쿼리 안");
eq(at("select a from |(select b from t) x"), "(select b from t)", "( 앞");
eq(at("select a from (select b from t)| x"), "(select b from t)", ") 뒤");
eq(at("select a| from t"), null, "괄호 밖");
// 문자열·주석 안의 괄호는 아니다
eq(at("select f(')', |x) from t"), "(')', x)", "문자열 안 )");
eq(at("select f(a /* ) */, |b) from t"), "(a /* ) */, b)", "주석 안 )");
console.log(`  괄호 ${n}건`);
