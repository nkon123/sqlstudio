// 문장 안 별칭 찾기 — F4·컬럼 팝업이 쓴다 (DOM 없이 테스트하려고 따로 둔다)

/** 문서에서 별칭 → 테이블 ("FROM orders o", "JOIN app.customers AS c", ", items i") */
export function aliasMap(sql: string): Map<string, string> {
  const m = new Map<string, string>();
  const kw = new Set(["FROM", "WHERE", "ON", "JOIN", "LEFT", "RIGHT", "INNER", "OUTER", "FULL", "CROSS", "GROUP", "ORDER", "UNION", "MINUS", "INTERSECT", "SET", "CONNECT", "START", "HAVING", "FETCH", "PARTITION", "USING", "AND", "OR", "WITH", "VALUES", "SELECT", "INTO", "NATURAL", "AS"]);
  const name = String.raw`([A-Za-z_][\w$#]*(?:\.[A-Za-z_][\w$#]*)?)`;
  // 테이블과 별칭 사이 공백 — 줄바꿈은 하나까지. 빈 줄 너머는 다른 문장이다
  // ("select * from tab⏎⏎sp_procedure" 에서 sp_procedure 를 tab 의 별칭으로 읽지 않게)
  const gap = String.raw`(?=\s)[ \t]*(?:\r?\n[ \t]*)?`;
  // 따로 훑는다 — 한쪽이 글자를 먹어 다른 쪽을 놓치지 않게 ("SELECT o.id, c.name FROM orders o")
  for (const re of [new RegExp(String.raw`\b(?:FROM|JOIN|UPDATE|INTO)\s+${name}(?:${gap}(?:AS\s+)?([A-Za-z_][\w$#]*))?`, "gi"),
    new RegExp(String.raw`,\s*${name}${gap}(?:AS\s+)?([A-Za-z_][\w$#]*)`, "gi")]) {
    for (const x of sql.matchAll(re)) {
      const alias = x[2]?.toUpperCase();
      if (alias && !kw.has(alias) && !kw.has(x[1].toUpperCase())) m.set(alias, x[1].toUpperCase());
    }
  }
  return m;
}
