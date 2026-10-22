// npm test — 괄호 쌍 찾기 (문법 트리로 — 문자열·주석 안의 괄호는 세지 않는다)
import { sql } from "@codemirror/lang-sql";
import { EditorState } from "@codemirror/state";
import { childPairs, enclosingPair, innerRange, outerPair, siblingPairs, wider } from "./brackets";
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
// 한 겹 바깥 — 같은 쌍으로 맴돌지 않는다 (Ctrl+Shift+] 를 거듭 누를 때)
{
  const doc = "select a from (select nvl(c, 0) from t) x where (b)";
  const state = EditorState.create({ doc, extensions: [sql({ dialect: ORACLE })] });
  const inner = enclosingPair(state, doc.indexOf("0"))!;
  eq(doc.slice(inner.open, inner.close + 1), "(c, 0)", "안쪽");
  const outer = outerPair(state, inner)!;
  eq(doc.slice(outer.open, outer.close + 1), "(select nvl(c, 0) from t)", "바깥");
  eq(outerPair(state, outer), null, "더 바깥 없음");
  n += 3;
}
// 고르는 범위는 괄호 안쪽 — 괄호와 그 바로 안의 공백·줄바꿈은 뺀다
{
  const doc = "select * from (\n  select a\n    from t\n) x";
  const state = EditorState.create({ doc, extensions: [sql({ dialect: ORACLE })] });
  const r = innerRange(state, enclosingPair(state, doc.indexOf("a\n"))!);
  eq(doc.slice(r.from, r.to), "select a\n    from t", "서브쿼리 안쪽");
  n++;
}
// Ctrl+Shift+[ — 지금 고른 것보다 넓은 가장 안쪽 괄호
{
  const doc = "select a from (select nvl(c, 0) from t) x where (b)";
  const state = EditorState.create({ doc, extensions: [sql({ dialect: ORACLE })] });
  const text = (p: ReturnType<typeof wider>) => (p ? doc.slice(p.open, p.close + 1) : null);
  const c = doc.indexOf("c, 0");
  eq(text(wider(state, c, c + 1)), "(c, 0)", "글자 하나 → 감싼 괄호");
  eq(text(wider(state, c, c + 4)), "(select nvl(c, 0) from t)", "안쪽 전체 → 바깥");
  const sub = doc.indexOf("select nvl");
  eq(text(wider(state, sub, sub + "select nvl(c, 0) from t".length)), null, "가장 바깥이면 없음");
  n += 3;
}
// 안쪽으로 들어갈 곳 · 같은 높이의 괄호들
{
  const doc = "select * from (select a from t where b in (1, 2) and nvl(c, 0) = 1) x where (y)";
  const state = EditorState.create({ doc, extensions: [sql({ dialect: ORACLE })] });
  const texts = (ps: { open: number; close: number }[]) => ps.map((p) => doc.slice(p.open, p.close + 1)).join(" | ");
  const sub = enclosingPair(state, doc.indexOf("where b"))!;
  eq(texts(childPairs(state, sub)), "(1, 2) | (c, 0)", "바로 안의 괄호들 (더 깊은 것 빼고)");
  const inList = enclosingPair(state, doc.indexOf("1, 2"))!;
  eq(texts(siblingPairs(state, inList)), "(1, 2) | (c, 0)", "같은 높이");
  eq(texts(siblingPairs(state, sub)), "(select a from t where b in (1, 2) and nvl(c, 0) = 1) | (y)", "맨 바깥 높이");
  n += 3;
}
console.log(`  괄호 ${n}건`);
