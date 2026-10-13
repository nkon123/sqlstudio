// 결과 → 글자 내보내기 (CSV, INSERT 문). 화면과 무관한 순수 함수 — 엑셀은 Rust(export.rs)가 쓴다.
//
// 셀은 화면과 같은 문자열로 온다: DATE "2026-10-12 09:00:00", TIMESTAMP "2026-10-12 09:00:00.123456", TZ 는 뒤에 "+09:00".

export interface Col { name: string; type_name: string }

export interface CsvOpts { delimiter: string; header: boolean }

export function toCsv(cols: Col[], rows: (string | null)[][], o: CsvOpts): string {
  const d = o.delimiter;
  const esc = (v: string | null) => (v == null ? "" : v.includes(d) || /[\r\n"]/.test(v) || /^\s|\s$/.test(v) ? `"${v.replace(/"/g, '""')}"` : v);
  const out: string[] = [];
  if (o.header) out.push(cols.map((c) => esc(c.name)).join(d));
  for (const r of rows) out.push(r.map(esc).join(d));
  return out.join("\r\n") + "\r\n";
}

const quote = (s: string) => `'${s.replace(/'/g, "''")}'`;

/** 4000 바이트가 넘는 문자열은 CLOB 조각을 이어 붙인다 (SQL 문자열 상수 한도) */
function bigString(s: string) {
  const bytes = new TextEncoder().encode(s).length;
  if (bytes <= 4000) return quote(s);
  const parts: string[] = [];
  for (let i = 0; i < s.length; i += 1000) parts.push(`TO_CLOB(${quote(s.slice(i, i + 1000))})`);
  return parts.join(" || ");
}

/** 셀 하나를 SQL 값으로 */
export function sqlValue(v: string | null, typeName: string): string {
  if (v == null) return "NULL";
  const t = typeName.toUpperCase();
  if (/^(NUMBER|FLOAT|BINARY_FLOAT|BINARY_DOUBLE|INTEGER|DECIMAL)/.test(t)) {
    return /^-?(\d+\.?\d*|\.\d+)(E[+-]?\d+)?$/i.test(v.trim()) ? v.trim() : quote(v);
  }
  if (t === "DATE") {
    if (/^\d{4}-\d{2}-\d{2}$/.test(v)) return `TO_DATE(${quote(v)}, 'YYYY-MM-DD')`;
    if (/^\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2}$/.test(v)) return `TO_DATE(${quote(v)}, 'YYYY-MM-DD HH24:MI:SS')`;
    return quote(v);
  }
  if (t.startsWith("TIMESTAMP")) {
    const tz = /WITH (LOCAL )?TIME ZONE/.test(t);
    const m = /^(\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2})(\.\d+)?(\s*[+-]\d{2}:\d{2})?$/.exec(v);
    if (m && tz && m[3]) return `TO_TIMESTAMP_TZ(${quote(v)}, 'YYYY-MM-DD HH24:MI:SS${m[2] ? ".FF" : ""} TZH:TZM')`;
    if (m && !m[3]) return `TO_TIMESTAMP(${quote(v)}, 'YYYY-MM-DD HH24:MI:SS${m[2] ? ".FF" : ""}')`;
    return quote(v);
  }
  if (/^(RAW|LONG RAW|BLOB|BFILE)/.test(t)) return "NULL";
  return bigString(v);
}

/** 바이너리처럼 INSERT 로 옮길 수 없는 열 (값 대신 NULL) */
export function unsupported(cols: Col[]) {
  return cols.filter((c) => /^(RAW|LONG RAW|BLOB|BFILE)/.test(c.type_name.toUpperCase())).map((c) => c.name);
}

/** 이름에 소문자·특수문자가 있으면 따옴표 */
export function ident(name: string) {
  return /^[A-Z][A-Z0-9_$#]*$/.test(name) ? name : `"${name.replace(/"/g, "")}"`;
}

export function toInserts(table: string, cols: Col[], rows: (string | null)[][], o: { perLine?: boolean } = {}): string {
  const use = cols.map((c, i) => ({ c, i })).filter(({ c }) => c.name.toUpperCase() !== "ROWID");
  const head = `INSERT INTO ${table} (${use.map(({ c }) => ident(c.name)).join(", ")})`;
  const out: string[] = [];
  for (const r of rows) {
    const vals = use.map(({ c, i }) => sqlValue(r[i], c.type_name)).join(", ");
    out.push(o.perLine === false ? `${head}\nVALUES (${vals});` : `${head} VALUES (${vals});`);
  }
  return out.join("\n") + "\n";
}

/** SELECT 문에서 테이블 하나를 짐작 ("FROM 스키마.테이블" 의 첫 테이블) */
export function guessTable(sql: string | undefined): string {
  if (!sql) return "";
  const clean = sql.replace(/--[^\n]*|\/\*[\s\S]*?\*\//g, " ").replace(/'(?:[^']|'')*'/g, "''");
  const m = /\bFROM\s+((?:"[^"]+"|[A-Za-z_][\w$#]*)(?:\.(?:"[^"]+"|[A-Za-z_][\w$#]*))?)/i.exec(clean);
  if (!m) return "";
  // 따옴표 이름은 대소문자를 그대로, 아니면 대문자로
  return m[1].includes('"') ? m[1] : m[1].toUpperCase();
}
