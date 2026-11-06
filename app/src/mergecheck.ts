// MERGE 점검 — 소스 데이터 때문에 나는 오류를 일으키는 행을 찾는 쿼리를 만든다.
//   ORA-30926: 대상 한 행에 소스 여러 행이 ON 으로 붙으면, 어느 값으로 고칠지 몰라 실패한다
//   ORA-00001: 대상에 없는 키가 소스에 두 번 이상 있으면, INSERT 갈래가 같은 키를 두 번 넣는다
//   ORA-38104: ON 에 쓴 열을 UPDATE SET 으로 고칠 수 없다 (문장만 보고 알 수 있어 주석으로 알린다)
// 문장만 읽는다 (브라우저·DB 에 기대지 않아 단위 테스트한다).

export interface MergeParts {
  /** 대상 이름 (스키마·DB 링크 포함, 적힌 그대로) */
  target: string;
  /** 대상 별칭 — 없으면 null */
  targetAlias: string | null;
  /** 소스 — 테이블 이름 또는 괄호째 서브쿼리 */
  source: string;
  sourceAlias: string | null;
  /** ON 괄호 안 조건 */
  on: string;
  /** UPDATE SET 으로 고치는 열 (대문자, 별칭 뗀 이름) */
  setCols: string[];
  hasUpdate: boolean;
  hasInsert: boolean;
}

export interface MergeCheck {
  /** 바로 실행할 쿼리 — 대상 한 행에 붙는 소스 행이 둘 이상인 것 (ORA-30926) */
  dupMatch: string;
  /** 편집기에 넣을 점검 스크립트 (주석 + 쿼리들, 문장마다 ;) */
  script: string;
}

interface Tok { t: "w" | "q" | "s" | "(" | ")" | "," | "=" | "op" | "."; v: string; at: number; end: number }

/** 낱말·문자열·괄호 — 주석은 건너뛴다 */
function tokens(sql: string): Tok[] {
  const out: Tok[] = [];
  let i = 0;
  while (i < sql.length) {
    const c = sql[i];
    if (/\s/.test(c)) { i++; continue; }
    if (sql.startsWith("--", i)) { const n = sql.indexOf("\n", i); i = n < 0 ? sql.length : n + 1; continue; }
    if (sql.startsWith("/*", i)) { const n = sql.indexOf("*/", i + 2); i = n < 0 ? sql.length : n + 2; continue; }
    const at = i;
    if (c === "'") {
      i++;
      while (i < sql.length && !(sql[i] === "'" && sql[i + 1] !== "'")) i += sql[i] === "'" ? 2 : 1;
      i++;
      out.push({ t: "s", v: sql.slice(at, i), at, end: i });
    } else if (c === '"') {
      const n = sql.indexOf('"', i + 1);
      i = n < 0 ? sql.length : n + 1;
      out.push({ t: "q", v: sql.slice(at, i), at, end: i });
    } else if (/[\p{L}\p{N}_$#@]/u.test(c)) {
      while (i < sql.length && /[\p{L}\p{N}_$#@]/u.test(sql[i])) i++;
      out.push({ t: "w", v: sql.slice(at, i), at, end: i });
    } else if (c === "(" || c === ")" || c === "," || c === ".") {
      i++;
      out.push({ t: c, v: c, at, end: i });
    } else if (c === "=") {
      i++;
      out.push({ t: "=", v: c, at, end: i });
    } else {
      // <> != <= >= 등은 = 로 보지 않는다
      while (i < sql.length && /[<>!^~|*+\-/:;%&]/.test(sql[i])) i++;
      if (sql[i] === "=" && at < i) i++;
      if (i === at) i++;
      out.push({ t: "op", v: sql.slice(at, i), at, end: i });
    }
  }
  return out;
}

const up = (k: Tok | undefined) => (k && k.t === "w" ? k.v.toUpperCase() : "");
const isName = (k: Tok | undefined) => !!k && (k.t === "w" || k.t === "q");

/** 짝 괄호 — ks[i] 가 ( 일 때 맞는 ) 의 번호 */
function close(ks: Tok[], i: number): number {
  let d = 0;
  for (let j = i; j < ks.length; j++) {
    if (ks[j].t === "(") d++;
    else if (ks[j].t === ")" && --d === 0) return j;
  }
  return -1;
}

/** 이름(점·@ 링크 포함) 하나를 읽는다 — 끝 번호(다음 토큰) */
function nameEnd(ks: Tok[], i: number): number {
  if (!isName(ks[i])) return i;
  let j = i + 1;
  while (ks[j]?.t === "." && isName(ks[j + 1])) j += 2;
  return j;
}

const RESERVED = new Set(["USING", "ON", "WHEN", "WHERE", "SET", "LOG"]);

/** MERGE 문을 대상·소스·ON 으로 나눈다. MERGE 가 아니거나 못 읽으면 null */
export function parseMerge(sql: string): MergeParts | null {
  const ks = tokens(sql);
  let i = 0;
  if (up(ks[i]) !== "MERGE") return null;
  i++;
  if (up(ks[i]) !== "INTO") return null;
  i++;
  let e = nameEnd(ks, i);
  if (e === i) return null;
  const target = sql.slice(ks[i].at, ks[e - 1].end);
  i = e;
  let targetAlias: string | null = null;
  if (up(ks[i]) === "AS") i++;
  if (isName(ks[i]) && !RESERVED.has(up(ks[i]))) targetAlias = ks[i++].v;
  if (up(ks[i]) !== "USING") return null;
  i++;
  let source: string;
  if (ks[i]?.t === "(") {
    const j = close(ks, i);
    if (j < 0) return null;
    source = sql.slice(ks[i].at, ks[j].end);
    i = j + 1;
  } else {
    e = nameEnd(ks, i);
    if (e === i) return null;
    source = sql.slice(ks[i].at, ks[e - 1].end);
    i = e;
  }
  let sourceAlias: string | null = null;
  if (up(ks[i]) === "AS") i++;
  if (isName(ks[i]) && !RESERVED.has(up(ks[i]))) sourceAlias = ks[i++].v;
  if (up(ks[i]) !== "ON" || ks[i + 1]?.t !== "(") return null;
  const j = close(ks, i + 1);
  if (j < 0) return null;
  const on = sql.slice(ks[i + 1].end, ks[j].at).trim();
  i = j + 1;
  // 갈래: WHEN [NOT] MATCHED THEN UPDATE SET … / INSERT …
  let hasUpdate = false;
  let hasInsert = false;
  const setCols: string[] = [];
  for (; i < ks.length; i++) {
    if (ks[i].t === "(") { const k = close(ks, i); if (k < 0) break; i = k; continue; }
    if (up(ks[i]) !== "WHEN") continue;
    const not = up(ks[i + 1]) === "NOT";
    const at = i + (not ? 2 : 1);
    if (up(ks[at]) !== "MATCHED" || up(ks[at + 1]) !== "THEN") continue;
    if (not) { hasInsert = true; continue; }
    hasUpdate = true;
    if (up(ks[at + 2]) !== "UPDATE" || up(ks[at + 3]) !== "SET") continue;
    // SET a = …, b = … — 깊이 0 의 쉼표마다 = 앞의 열 이름
    let k = at + 4;
    let lhs = true;
    for (; k < ks.length; k++) {
      const w = up(ks[k]);
      if (w === "WHERE" || w === "DELETE" || w === "WHEN" || w === "LOG") break;
      if (ks[k].t === "(") { const c = close(ks, k); if (c < 0) break; k = c; continue; }
      if (ks[k].t === ",") { lhs = true; continue; }
      if (lhs && isName(ks[k])) {
        const n = nameEnd(ks, k);
        setCols.push(unq(ks[n - 1].v));
        lhs = false;
        k = n - 1;
      }
    }
    i = k - 1;
  }
  return { target, targetAlias, source, sourceAlias, on, setCols, hasUpdate, hasInsert };
}

/** 이름 비교용 — 따옴표는 떼고 그대로, 없으면 대문자 */
const unq = (s: string) => (s.startsWith('"') ? s.slice(1, -1) : s.toUpperCase());

/** 맨 끝 이름 조각 (스키마·@링크 뗀 것) — 별칭이 없을 때 그 이름으로 가리킨다 */
function lastPart(name: string): string {
  const noLink = name.replace(/@[\w$#.]+$/, "");
  const parts = noLink.split(".");
  return parts[parts.length - 1];
}

/** 조건 조각 하나가 어느 쪽 별칭을 쓰는지 */
function refs(text: string, q: string): boolean {
  const ks = tokens(text);
  const want = unq(q);
  return ks.some((k, i) => isName(k) && ks[i + 1]?.t === "." && unq(k.v) === want);
}

/** 깊이 0 에서 낱말 AND 로 자른다. OR 가 깊이 0 에 있으면 null (키로 볼 수 없다) */
function splitAnd(cond: string): string[] | null {
  const ks = tokens(cond);
  const parts: string[] = [];
  let d = 0;
  let from = 0;
  for (let i = 0; i < ks.length; i++) {
    const k = ks[i];
    if (k.t === "(") d++;
    else if (k.t === ")") d--;
    else if (d === 0 && up(k) === "OR") return null;
    else if (d === 0 && up(k) === "AND") {
      // BETWEEN a AND b 의 AND 는 자르지 않는다
      const between = ks.slice(0, i).reverse().find((x) => ["AND", "BETWEEN"].includes(up(x)));
      if (up(between) === "BETWEEN") continue;
      parts.push(cond.slice(from, k.at).trim());
      from = k.end;
    }
  }
  parts.push(cond.slice(from).trim());
  return parts.filter(Boolean);
}

/** a = b 이면 [a, b], 아니면 null (깊이 0 의 = 가 하나뿐일 때만) */
function equality(cond: string): [string, string] | null {
  const ks = tokens(cond);
  let d = 0;
  let eq = -1;
  for (let i = 0; i < ks.length; i++) {
    if (ks[i].t === "(") d++;
    else if (ks[i].t === ")") d--;
    else if (d === 0 && ks[i].t === "=") { if (eq >= 0) return null; eq = i; }
    else if (d === 0 && ks[i].t === "op" && /[<>!]/.test(ks[i].v)) return null;
  }
  if (eq < 0) return null;
  return [cond.slice(0, ks[eq].at).trim(), cond.slice(ks[eq].end).trim()];
}

/** 들여쓴 줄 — 여러 줄 조건·서브쿼리를 SELECT 안에 넣을 때 둘째 줄부터 */
const indent = (s: string, pad: string) => s.replace(/\r?\n/g, `\n${pad}`);

/** MERGE 점검 쿼리. MERGE 가 아니면 null */
export function mergeCheck(sql: string): MergeCheck | null {
  const m = parseMerge(sql);
  if (!m) return null;
  const tq = m.targetAlias ?? lastPart(m.target);
  const sq = m.sourceAlias ?? (m.source.startsWith("(") ? null : lastPart(m.source));
  const tgt = m.targetAlias ? `${m.target} ${m.targetAlias}` : m.target;
  const src = m.sourceAlias ? `${m.source} ${m.sourceAlias}` : m.source;

  const dupMatch =
    `SELECT *\n` +
    `  FROM (SELECT COUNT(*) OVER (PARTITION BY ${tq}.ROWID) AS MATCH_CNT__,\n` +
    `               ${tq}.ROWID AS TARGET_ROWID__,\n` +
    `               ${sq ? `${sq}.*` : "*"}\n` +
    `          FROM ${indent(tgt, "               ")},\n` +
    `               ${indent(src, "               ")}\n` +
    `         WHERE ${indent(m.on, "           ")})\n` +
    ` WHERE MATCH_CNT__ > 1\n` +
    ` ORDER BY TARGET_ROWID__`;

  const out: string[] = [
    `-- MERGE 점검: ${m.target} ← ${m.source.startsWith("(") ? "서브쿼리" : m.source}`,
    `-- 1) 대상 한 행에 소스 여러 행이 붙는 경우 — WHEN MATCHED 에서 ORA-30926 (어느 행으로 고칠지 모름)`,
    `--    같은 TARGET_ROWID__ 끼리 묶어 보면, 어느 소스 값이 겹치는지 보인다`,
    `${dupMatch};`,
  ];

  // 2) INSERT 갈래: 대상에 없는 같은 키가 소스에 두 번 — ON 이 '대상 = 소스' 들의 AND 일 때만 키를 안다
  const parts = splitAnd(m.on);
  const keys: string[] = [];
  const filters: string[] = [];
  const targetCols = new Set<string>();
  let keyed = !!parts && !!sq;
  for (const p of parts ?? []) {
    const eq = equality(p);
    const inT = refs(p, tq);
    const inS = sq ? refs(p, sq) : false;
    if (eq && inT && inS) {
      const [a, b] = eq;
      const sSide = refs(a, sq!) && !refs(a, tq) ? a : refs(b, sq!) && !refs(b, tq) ? b : null;
      const tSide = sSide === a ? b : a;
      if (!sSide) { keyed = false; continue; }
      keys.push(sSide);
      // 대상 쪽이 그냥 열이면 그 이름 (ORA-38104 확인용)
      const c = /^(?:[\w$#]+|"[^"]+")\s*\.\s*([\w$#]+|"[^"]+")$/.exec(tSide);
      if (c) targetCols.add(unq(c[1]));
    } else if (inS && !inT) filters.push(p);
    else if (inT && !inS) continue; // 대상만 거르는 조건 — 소스 중복과 관계없다
    else keyed = false;
  }

  if (m.hasInsert) {
    if (keyed && keys.length) {
      out.push(
        "",
        `-- 2) 소스 안에서 ON 키가 겹치는 행 — 대상에 없는 키면 WHEN NOT MATCHED 가 두 번 넣는다 (유일 키가 있으면 ORA-00001)`,
        `SELECT ${keys.join(", ")}, COUNT(*) AS CNT__\n` +
        `  FROM ${indent(src, "       ")}\n` +
        (filters.length ? ` WHERE ${filters.join("\n   AND ")}\n` : "") +
        ` GROUP BY ${keys.join(", ")}\n` +
        `HAVING COUNT(*) > 1\n` +
        ` ORDER BY CNT__ DESC;`,
      );
    } else {
      out.push("", `-- 2) ON 이 '대상.열 = 소스.열' 의 AND 만이 아니어서 소스 키 중복 쿼리는 만들지 않았습니다`);
    }
  }

  const bad = m.setCols.filter((c) => targetCols.has(c));
  if (bad.length) out.push("", `-- 주의: ON 에 쓴 열(${bad.join(", ")})을 UPDATE SET 으로 고치면 ORA-38104 — 데이터와 관계없이 문장을 고쳐야 합니다`);
  return { dupMatch, script: out.join("\n") };
}
