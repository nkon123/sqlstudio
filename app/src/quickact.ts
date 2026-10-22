// 자동완성 퀵 액션 — 컬럼 후보에서 → 를 누르면 그 구절·타입에 맞는 꼴(`= ''`, `IS NULL` …)을 고른다.
// 여기는 무엇을 보일지만 정한다 (화면·키 처리는 quickmenu.ts). 브라우저에 기대지 않아 단위 테스트한다.

/** 퀵 액션이 쓰는 구절 갈래 */
export type Clause = "cond" | "set" | "select" | "order";
/** 컬럼 타입 갈래 */
export type TypeClass = "text" | "num" | "date" | "other";

export interface QuickAction {
  /** 메뉴에 보일 글 (자리 표시 없이) */
  label: string;
  /** 넣을 코드 조각 — `${1}` 이 커서 자리, Tab 으로 다음 자리 */
  template: string;
}

/** 백엔드 구절 이름(`expr(where)` 등) → 갈래. 모르면 null */
function clauseOfName(name: string): Clause | null {
  switch (name.toUpperCase()) {
    case "WHERE": case "AND": case "OR": case "ON": case "HAVING": case "WHEN":
    case "CONNECT BY": case "START WITH": case "JOIN_ON":
      return "cond";
    case "SET": return "set";
    case "SELECT": return "select";
    case "ORDER BY": return "order";
    default: return null;
  }
}

const CLAUSE_WORD = /\b(SELECT|FROM|JOIN|WHERE|AND|OR|ON|HAVING|SET|VALUES|WHEN|THEN|ELSE|ORDER\s+BY|GROUP\s+BY|CONNECT\s+BY|START\s+WITH|PARTITION\s+BY|INTO)\b/gi;

/** 커서 앞 글에서 마지막 구절 키워드 — 백엔드가 구절을 못 준 경우(`e.` 뒤 등)에 쓴다. 문자열·주석은 대강 걷어 낸다 */
export function clauseBefore(head: string): Clause | null {
  const text = head.slice(-4000).replace(/'[^']*'|--[^\n]*|\/\*[\s\S]*?\*\//g, " ");
  let last: string | null = null;
  for (const m of text.matchAll(CLAUSE_WORD)) last = m[1];
  if (!last) return null;
  const w = last.toUpperCase().replace(/\s+/g, " ");
  // CASE 의 THEN·ELSE 는 값 자리 — 꼴을 권하지 않는다
  return clauseOfName(w);
}

/** 자동완성 응답의 context(`expr(where)`·`join_on` …)로, 없으면 커서 앞 글로 구절 갈래를 정한다 */
export function clauseOf(context: string, head: string): Clause | null {
  const m = /^expr\((.*)\)$/.exec(context);
  if (m) return m[1] ? clauseOfName(m[1]) : clauseBefore(head);
  if (context === "join_on") return "cond";
  return clauseBefore(head);
}

const TYPE_WORD = /^(N?VARCHAR2?|N?CHAR|N?CLOB|LONG|ROWID|NUMBER|INTEGER|INT|SMALLINT|FLOAT|DECIMAL|BINARY_(?:FLOAT|DOUBLE)|DATE|TIMESTAMP)\b/i;

/** 후보 설명(`NUMBER(4)`·`EMP · NUMBER(4)`·`NUMBER(4) · E`)에서 타입 갈래 */
export function typeClass(detail: string | undefined): TypeClass {
  for (const part of (detail ?? "").split("·")) {
    const t = TYPE_WORD.exec(part.trim())?.[1].toUpperCase();
    if (!t) continue;
    if (t === "DATE" || t === "TIMESTAMP") return "date";
    if (/NUMBER|INT|FLOAT|DECIMAL|BINARY/.test(t)) return "num";
    return "text";
  }
  return "other";
}

/** 그 컬럼(넣을 글자 `col`)에 권할 꼴. 없으면 빈 목록 */
export function quickActions(clause: Clause | null, type: TypeClass, col: string): QuickAction[] {
  const a = (template: string): QuickAction => ({
    label: template.replace(/\$\{\d+\}/g, ""),
    template,
  });
  const quoted = type === "text" || type === "other";
  const day = (n: number) => `TO_DATE('\${${n}}', 'YYYY-MM-DD')`;
  switch (clause) {
    case "cond": {
      const list =
        type === "num" ? [`${col} = \${1}`, `${col} IN (\${1})`, `${col} BETWEEN \${1} AND \${2}`, `${col} <> \${1}`]
        : type === "date" ? [`${col} = ${day(1)}`, `${col} >= ${day(1)}`, `${col} BETWEEN ${day(1)} AND ${day(2)}`, `${col} >= TRUNC(SYSDATE)`]
        : [`${col} = '\${1}'`, `${col} LIKE '%\${1}%'`, `${col} IN ('\${1}')`, `${col} <> '\${1}'`];
      return [...list, `${col} IS NULL`, `${col} IS NOT NULL`].map(a);
    }
    case "set": {
      const list =
        type === "num" ? [`${col} = \${1}`]
        : type === "date" ? [`${col} = SYSDATE`, `${col} = ${day(1)}`]
        : [`${col} = '\${1}'`];
      return [...list, `${col} = NULL`].map(a);
    }
    case "select": {
      const list = [`NVL(${col}, ${quoted ? "'${1}'" : "${1}"})`];
      if (type === "num") list.push(`SUM(${col})`);
      if (type === "date") list.push(`TO_CHAR(${col}, 'YYYY-MM-DD')`, `TO_CHAR(${col}, 'YYYY-MM-DD HH24:MI:SS')`);
      list.push(`COUNT(${col})`, `MAX(${col})`, `${col} AS \${1}`);
      return list.map(a);
    }
    case "order":
      return [`${col} DESC`, `${col} NULLS LAST`, `${col} DESC NULLS LAST`].map(a);
    default:
      return [];
  }
}
