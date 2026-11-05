// Oracle 구문 강조 — CodeMirror 의 PLSQL 방언에 빠진 키워드(MERGE, MATCHED, PIVOT …)를 더하고
// 내장 함수(DECODE, NVL …)를 함수 색으로 칠한다. 자동완성의 함수 목록은 Rust(sqls-core::complete)에 따로 있다.

import { PLSQL, SQLDialect, sql, type SQLConfig } from "@codemirror/lang-sql";
import { foldNodeProp, HighlightStyle, LanguageSupport, LRLanguage } from "@codemirror/language";
import { tags as t } from "@lezer/highlight";
import { ExternalTokenizer, type InputStream, type LRParser, type Stack } from "@lezer/lr";

/** PLSQL 방언에 없는 Oracle 키워드 */
const EXTRA_KEYWORDS = [
  // DML·질의
  "merge matched pivot unpivot model rollup cube grouping sets fetch offset first next rows only percent ties lateral apply",
  "cross outer inner join natural left right full within over partition range unbounded preceding following current",
  "nocycle siblings connect_by_root connect_by_isleaf connect_by_iscycle sample seed skip locked wait nowait",
  "nulls last listagg overflow without count purge flashback recyclebin keep dense_rank",
  "interval timestamp year month day hour minute second zone local time",
  // DDL
  "materialized log refresh enable disable novalidate cascade constraints editionable noneditionable sharing",
  "invisible visible compute statistics online temporary global preserve definition",
  // PL/SQL
  "bulk collect forall exceptions limit pipelined pipe deterministic result_cache authid definer current_user",
  "autonomous_transaction exception_init serially_reusable restrict_references inline nocopy elsif continue exit goto",
  "open close fetch notfound found rowcount isopen bulk_rowcount bulk_exceptions instead each compound before after",
  "referencing old new parent nested immediate execute returning sqlerrm sqlcode raise_application_error",
].join(" ");

/** 함수 색으로 칠할 내장 함수 */
const FUNCTIONS = [
  "nvl nvl2 decode coalesce nullif lnnvl nanvl greatest least",
  "to_char to_date to_number to_timestamp to_timestamp_tz to_clob to_nchar to_nclob to_blob to_dsinterval to_yminterval",
  "to_multi_byte to_single_byte convert hextoraw rawtohex rowidtochar chartorowid numtodsinterval numtoyminterval asciistr unistr",
  "substr substrb instr instrb length lengthb upper lower initcap trim ltrim rtrim lpad rpad replace translate concat chr ascii",
  "soundex reverse dump vsize regexp_like regexp_substr regexp_replace regexp_instr regexp_count",
  "round trunc ceil floor abs mod remainder power sqrt sign exp ln log bitand width_bucket",
  "add_months months_between last_day next_day extract new_time from_tz sys_extract_utc",
  "sum avg min max median stddev variance xmlagg collect grouping_id corr covar_pop regr_slope approx_count_distinct",
  "row_number rank ntile lag lead first_value last_value nth_value cume_dist percent_rank ratio_to_report percentile_cont percentile_disc",
  "sys_connect_by_path sys_guid sys_context userenv ora_hash standard_hash empty_clob empty_blob bfilename",
  "xmlelement xmltype extractvalue json_value json_query json_object json_arrayagg json_table treat nls_upper nls_lower nlssort",
].join(" ");

export const ORACLE = SQLDialect.define({
  ...PLSQL.spec,
  keywords: `${PLSQL.spec.keywords ?? ""} ${EXTRA_KEYWORDS}`,
  builtin: `${PLSQL.spec.builtin ?? ""} ${FUNCTIONS}`,
});

// 블록 주석은 Oracle 처럼 처음 나오는 "*/" 에서 끝낸다.
// CodeMirror 는 주석 안의 "/*" 를 중첩으로 세서, "/*-- 머리말 /* 메모 */" 뒤 코드를 모두 주석으로 칠한다.
// 주석만 가로채고 나머지 낱말은 원래 토크나이저에 맡긴다
function oracleLanguage(base: LRLanguage): LRLanguage {
  const parser = base.parser as LRParser & { tokenizers?: readonly unknown[] };
  // 내부 필드(tokenizers, token)에 기댄다 — 바뀌면 원래 언어를 그대로 쓴다
  const orig = parser.tokenizers?.find((x) => x instanceof ExternalTokenizer) as { token?: (input: InputStream, stack: Stack) => void } | undefined;
  const comment = parser.nodeSet.types.find((x) => x.name === "BlockComment")?.id;
  const token = orig?.token;
  if (!orig || typeof token !== "function" || comment == null) return base;
  const tokens = new ExternalTokenizer((input, stack) => {
    if (input.peek(0) !== 47 /* / */ || input.peek(1) !== 42 /* * */) return token(input, stack);
    input.advance(2);
    while (input.peek(0) >= 0 && !(input.peek(0) === 42 && input.peek(1) === 47)) input.advance();
    if (input.peek(0) >= 0) input.advance(2);
    input.acceptToken(comment);
  });
  return base.configure({
    tokenizers: [{ from: orig as ExternalTokenizer, to: tokens }],
    // ; 로 끊은 "문장" 접기는 PL/SQL 에서 엉뚱하다 — 블록 접기는 codenav 가 한다 (주석 접기는 둔다)
    props: [foldNodeProp.add({ Statement: () => null })],
  });
}

const ORACLE_LANG = oracleLanguage(ORACLE.language);

/** 편집기·디버거·비교 화면이 함께 쓰는 Oracle SQL 언어 */
export function oracleSql(config: Omit<SQLConfig, "dialect"> = {}): LanguageSupport {
  return new LanguageSupport(ORACLE_LANG, sql({ ...config, dialect: ORACLE }).support);
}

/** 내장 함수(builtin) 색 — 기본 강조에는 없다 */
export const oracleHighlight = HighlightStyle.define([
  { tag: t.standard(t.name), color: "var(--hl-fn)" },
  // 화면 테마(밝게·어둡게)에 따라 바뀌도록 색은 변수로
  { tag: [t.keyword, t.operatorKeyword, t.controlKeyword, t.definitionKeyword, t.modifier], color: "var(--hl-kw)" },
  { tag: [t.string, t.special(t.string)], color: "var(--hl-str)" },
  { tag: [t.number, t.bool, t.null], color: "var(--hl-num)" },
  { tag: [t.comment, t.lineComment, t.blockComment], color: "var(--hl-cm)", fontStyle: "italic" },
  { tag: [t.typeName, t.className], color: "var(--hl-type)" },
  { tag: [t.operator, t.compareOperator, t.arithmeticOperator], color: "var(--hl-op)" },
]);
