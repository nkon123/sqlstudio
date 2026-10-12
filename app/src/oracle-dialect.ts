// Oracle 구문 강조 — CodeMirror 의 PLSQL 방언에 빠진 키워드(MERGE, MATCHED, PIVOT …)를 더하고
// 내장 함수(DECODE, NVL …)를 함수 색으로 칠한다. 자동완성의 함수 목록은 Rust(sqls-core::complete)에 따로 있다.

import { PLSQL, SQLDialect } from "@codemirror/lang-sql";
import { HighlightStyle } from "@codemirror/language";
import { tags as t } from "@lezer/highlight";

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

/** 내장 함수(builtin) 색 — 기본 강조에는 없다 */
export const oracleHighlight = HighlightStyle.define([
  { tag: t.standard(t.name), color: "var(--hl-fn)" },
]);
