//! 소스에서 기계적으로 뽑는 사실 — LLM 을 쓰지 않는다.
//!
//! 작은 모델은 테이블 이름을 틀리거나 지어낸다. 통합 분석(호출 관계, 테이블 CRUD)의 뼈대는
//! 여기서 뽑은 것만 쓰고, 모델 답은 의미(요약·규칙·위험)에만 쓴다.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use serde::{Deserialize, Serialize};

use crate::plsql::{Kind, Tok};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TableUse {
    /// 적힌 그대로 (대문자). "SCOTT.EMP", "EMP@REMOTE"
    pub name: String,
    /// C/R/U/D 중 쓰인 것 (MERGE 는 C+U)
    pub ops: String,
    pub lines: Vec<u32>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallUse {
    /// 적힌 그대로 (대문자). "PKG.PROC", "PROC", "SCHEMA.PKG.FN"
    pub name: String,
    pub lines: Vec<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mark {
    pub line: u32,
    pub what: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Facts {
    pub tables: Vec<TableUse>,
    pub calls: Vec<CallUse>,
    pub sequences: Vec<String>,
    /// EXECUTE IMMEDIATE / DBMS_SQL / OPEN … FOR 문자열 — 정적으로 볼 수 없는 SQL
    pub dynamic_sql: Vec<Mark>,
    /// COMMIT / ROLLBACK / SAVEPOINT / 자율 트랜잭션
    pub transactions: Vec<Mark>,
    /// RAISE_APPLICATION_ERROR 번호, RAISE 이름
    pub raises: Vec<Mark>,
    /// 잡는 예외 이름 (WHEN x THEN)
    pub handles: Vec<String>,
    /// WHEN OTHERS THEN NULL 같은, 예외를 삼키는 곳
    pub swallowed: Vec<u32>,
    pub db_links: Vec<String>,
    /// 1 + 분기 수 (IF, ELSIF, CASE WHEN, LOOP, WHEN 예외, AND/OR 는 세지 않음)
    pub complexity: u32,
    pub lines: u32,
    /// SQL 문 하나하나 (커서 선언·커서 루프·SELECT INTO·DML·동적 SQL)
    #[serde(default)]
    pub statements: Vec<crate::flow::SqlStmt>,
    /// 커서와 그 데이터가 들어가는 DML
    #[serde(default)]
    pub cursors: Vec<crate::flow::CursorInfo>,
}

/// 함수처럼 쓰이지만 호출 관계에 넣지 않을 것 — SQL/PLSQL 내장 함수, 형식 이름
const BUILTINS: &[&str] = &[
    "ABS", "ACOS", "ADD_MONTHS", "APPEND", "ASCII", "ASCIISTR", "ASIN", "ATAN", "ATAN2", "AVG", "BFILENAME", "BIN_TO_NUM", "BITAND",
    "BLOB", "BOOLEAN", "CARDINALITY", "CAST", "CEIL", "CHAR", "CHARTOROWID", "CHR", "CLOB", "COALESCE", "COLLECT", "COMPOSE",
    "CONCAT", "CONVERT", "CORR", "COS", "COSH", "COUNT", "COVAR_POP", "COVAR_SAMP", "CUME_DIST", "CURRENT_DATE",
    "CURRENT_TIMESTAMP", "DATE", "DBTIMEZONE", "DECIMAL", "DECODE", "DECOMPOSE", "DELETE", "DENSE_RANK", "DEREF", "DUMP",
    "EMPTY_BLOB", "EMPTY_CLOB", "EXISTS", "EXP", "EXTEND", "EXTRACT", "FIRST", "FIRST_VALUE", "FLOAT", "FLOOR", "FROM_TZ",
    "GREATEST", "GROUPING", "GROUPING_ID", "HEXTORAW", "IN", "INITCAP", "INSTR", "INSTRB", "INTEGER", "INTERVAL", "LAG", "LAST",
    "LAST_DAY", "LAST_VALUE", "LEAD", "LEAST", "LENGTH", "LENGTHB", "LIMIT", "LISTAGG", "LN", "LNNVL", "LOCALTIMESTAMP", "LOG",
    "LOWER", "LPAD", "LTRIM", "MAX", "MEDIAN", "MIN", "MOD", "MONTHS_BETWEEN", "NANVL", "NCHAR", "NCLOB", "NEW_TIME", "NEXT",
    "NEXT_DAY", "NLSSORT", "NLS_INITCAP", "NLS_LOWER", "NLS_UPPER", "NOT", "NTILE", "NULLIF", "NUMBER", "NUMTODSINTERVAL",
    "NUMTOYMINTERVAL", "NVARCHAR2", "NVL", "NVL2", "ORA_HASH", "OVER", "PERCENT_RANK", "PERCENTILE_CONT", "PERCENTILE_DISC",
    "POWER", "PRIOR", "RAISE_APPLICATION_ERROR", "RANK", "RATIO_TO_REPORT", "RAW", "RAWTOHEX", "REF", "REGEXP_COUNT",
    "REGEXP_INSTR", "REGEXP_LIKE", "REGEXP_REPLACE", "REGEXP_SUBSTR", "REMAINDER", "REPLACE", "ROUND", "ROW", "ROW_NUMBER",
    "ROWIDTOCHAR", "RPAD", "RTRIM", "SESSIONTIMEZONE", "SIGN", "SIN", "SINH", "SOUNDEX", "SQLCODE", "SQLERRM", "SQRT",
    "STDDEV", "SUBSTR", "SUBSTRB", "SUM", "SYS_CONNECT_BY_PATH", "SYS_CONTEXT", "SYS_EXTRACT_UTC", "SYS_GUID", "SYSDATE",
    "SYSTIMESTAMP", "TABLE", "TAN", "TANH", "TIMESTAMP", "TO_BINARY_DOUBLE", "TO_BINARY_FLOAT", "TO_BLOB", "TO_CHAR", "TO_CLOB",
    "TO_DATE", "TO_DSINTERVAL", "TO_LOB", "TO_MULTI_BYTE", "TO_NCHAR", "TO_NCLOB", "TO_NUMBER", "TO_SINGLE_BYTE",
    "TO_TIMESTAMP", "TO_TIMESTAMP_TZ", "TO_YMINTERVAL", "TRANSLATE", "TREAT", "TRIM", "TRUNC", "TZ_OFFSET", "UID", "UNISTR",
    "UPPER", "USER", "USERENV", "VALUES", "VARCHAR", "VARCHAR2", "VARIANCE", "VSIZE", "WIDTH_BUCKET", "XMLAGG", "XMLELEMENT",
    "XMLTYPE", "PLS_INTEGER", "BINARY_INTEGER", "SIMPLE_INTEGER", "NATURAL", "POSITIVE", "UROWID", "ROWID", "LONG",
    "CHARACTER", "SMALLINT", "INT", "REAL", "DOUBLE", "NUMERIC", "DEC", "BINARY_DOUBLE", "BINARY_FLOAT", "VARRAY", "RECORD",
    "PRIOR", "ANY", "ALL", "SOME", "AND", "OR", "WHEN", "THEN", "ELSE", "ELSIF", "IF", "WHILE", "RETURN", "RETURNING",
    "USING", "INTO", "SELECT", "WITH", "WHERE", "ON", "BY", "FOR", "FORALL", "OPEN", "FETCH", "CLOSE", "BULK", "COLLECT",
    "LIKE", "BETWEEN", "IS", "AS", "SET", "VALUE", "CURSOR", "OF", "EXECUTE", "IMMEDIATE", "PIPE", "PIPELINED", "RAISE",
    "NULL", "TRUE", "FALSE", "MEMBER", "SUBMULTISET", "MULTISET", "PARTITION", "ROWS", "RANGE", "KEEP", "DENSE_RANK",
    "WITHIN", "GROUP", "SAMPLE", "PIVOT", "UNPIVOT", "CONTAINS", "SCORE", "LEVEL", "CONNECT", "START", "MODEL", "CASE",
    "UPDATE", "INSERT", "MERGE", "COMMIT", "ROLLBACK", "SAVEPOINT", "LOCK", "PRAGMA", "EXCEPTION_INIT", "RESTRICT_REFERENCES",
    "SERIALLY_REUSABLE", "AUTONOMOUS_TRANSACTION", "INLINE", "EXIT", "CONTINUE", "GOTO", "NOCOPY", "OUT", "DEFAULT",
    "CONSTANT", "TYPE", "SUBTYPE", "BEGIN", "END", "DECLARE", "EXCEPTION", "LOOP", "REVERSE", "OTHERS", "DISTINCT", "UNIQUE",
    "NOWAIT", "WAIT", "SKIP", "LOCKED", "ORDER", "HAVING", "UNION", "INTERSECT", "MINUS", "JOIN", "INNER", "LEFT", "RIGHT",
    "FULL", "OUTER", "CROSS", "NATURAL", "FROM", "OFFSET", "FETCH", "ONLY", "TIES", "PERCENT", "SQL", "ROWCOUNT", "FOUND",
    "NOTFOUND", "ISOPEN", "BULK_ROWCOUNT", "BULK_EXCEPTIONS", "ROWTYPE", "CHARSET", "SYSTEM", "TIME", "ZONE", "LOCAL",
    "YEAR", "MONTH", "DAY", "HOUR", "MINUTE", "SECOND",
];

/// 절 키워드 — FROM 목록이 여기서 끝난다, 그리고 별칭이 될 수 없다
const CLAUSE: &[&str] = &[
    "WHERE", "GROUP", "ORDER", "HAVING", "CONNECT", "START", "UNION", "INTERSECT", "MINUS", "JOIN", "INNER", "LEFT", "RIGHT",
    "FULL", "CROSS", "NATURAL", "ON", "USING", "INTO", "SET", "VALUES", "FOR", "WHEN", "THEN", "ELSE", "END", "LOOP", "RETURN",
    "RETURNING", "BULK", "LIMIT", "MODEL", "PIVOT", "UNPIVOT", "SAMPLE", "PARTITION", "AS", "OUTER", "APPLY", "WITH", "SELECT",
    "FETCH", "OFFSET", "LOG", "IS", "AND", "OR", "NOT", "BEGIN", "IF",
];

fn builtin(w: &str) -> bool {
    BUILTINS.contains(&w)
}

/// 식별자 사슬 a.b.c (@link) 를 읽는다. (이름, 다음 토큰 인덱스)
pub(crate) fn chain(t: &[Tok], mut i: usize) -> Option<(String, usize, Option<String>)> {
    let mut parts = vec![t.get(i)?.name()?];
    i += 1;
    while i + 1 < t.len() && t[i].sym(".") {
        match t[i + 1].name() {
            Some(n) => {
                parts.push(n);
                i += 2;
            }
            None => break,
        }
    }
    let mut link = None;
    if i + 1 < t.len() && t[i].sym("@") {
        if let Some(l) = t[i + 1].name() {
            // link.domain.com
            let mut l = l;
            i += 2;
            while i + 1 < t.len() && t[i].sym(".") {
                if let Some(n) = t[i + 1].name() {
                    l.push('.');
                    l.push_str(&n);
                    i += 2;
                } else {
                    break;
                }
            }
            link = Some(l);
        }
    }
    Some((parts.join("."), i, link))
}

#[derive(Default)]
struct Acc {
    tables: BTreeMap<String, (BTreeSet<char>, BTreeSet<u32>)>,
    calls: BTreeMap<String, BTreeSet<u32>>,
    sequences: BTreeSet<String>,
    links: BTreeSet<String>,
}

impl Acc {
    fn table(&mut self, name: String, op: char, line: u32, link: Option<String>) {
        let full = match &link {
            Some(l) => format!("{name}@{l}"),
            None => name,
        };
        if let Some(l) = link {
            self.links.insert(l);
        }
        let e = self.tables.entry(full).or_default();
        e.0.insert(op);
        e.1.insert(line);
    }
}

/// 선언부에서 지역 이름(변수·커서·형식·예외·인자)을 모은다. 호출 후보에서 걸러 내는 데 쓴다.
pub fn local_names(t: &[Tok]) -> HashSet<String> {
    let mut out = HashSet::new();
    for i in 0..t.len() {
        let starts = i == 0 || t[i - 1].sym(";") || t[i - 1].sym("(") || t[i - 1].sym(",") || t[i - 1].is("IS") || t[i - 1].is("AS") || t[i - 1].is("DECLARE");
        let Some(n) = t[i].word() else { continue };
        if matches!(n, "CURSOR" | "TYPE" | "SUBTYPE") {
            if let Some(x) = t.get(i + 1).and_then(|x| x.name()) {
                out.insert(x);
            }
            continue;
        }
        if !starts || builtin(n) || matches!(n, "PROCEDURE" | "FUNCTION" | "PRAGMA") {
            continue;
        }
        // 이름 [CONSTANT] [IN] [OUT] [NOCOPY] 형식 | EXCEPTION
        let nx = t.get(i + 1);
        if nx.is_some_and(|x| x.word().is_some() || matches!(x.kind, Kind::Quoted(_))) {
            out.insert(n.to_string());
        }
    }
    out
}

/// 토큰 조각에서 사실을 뽑는다. `locals` 는 호출 후보에서 뺄 이름.
pub fn extract(t: &[Tok], locals: &HashSet<String>) -> Facts {
    let mut acc = Acc::default();
    let mut f = Facts::default();
    let mut handles = BTreeSet::new();
    let mut complexity = 1u32;
    // 괄호를 연 함수 이름 (EXTRACT(x FROM d) 의 FROM 을 테이블로 오인하지 않게)
    let mut parens: Vec<Option<String>> = Vec::new();

    let mut i = 0;
    while i < t.len() {
        let tk = &t[i];
        let line = tk.line;
        let prev_w = if i > 0 { t[i - 1].word() } else { None };
        let prev_dot = i > 0 && (t[i - 1].sym(".") || t[i - 1].sym("%"));
        if tk.sym("(") {
            parens.push(prev_w.map(String::from));
            i += 1;
            continue;
        }
        if tk.sym(")") {
            parens.pop();
            i += 1;
            continue;
        }
        let Some(w) = tk.word() else {
            i += 1;
            continue;
        };
        if prev_dot {
            i += 1;
            continue;
        }
        // 시퀀스.NEXTVAL
        if let Some((n, _, _)) = chain(t, i) {
            if let Some(seq) = n.strip_suffix(".NEXTVAL").or_else(|| n.strip_suffix(".CURRVAL")) {
                acc.sequences.insert(seq.to_string());
            }
        }
        match w {
            "IF" | "ELSIF" | "WHILE" => {
                if prev_w != Some("END") {
                    complexity += 1;
                }
            }
            "WHEN" => complexity += 1,
            "LOOP" if prev_w != Some("END") => complexity += 1,
            "FOR" if prev_w != Some("OPEN") => {}
            _ => {}
        }
        match w {
            "INSERT" => {
                // INSERT [ALL|FIRST] INTO t
                let mut j = i + 1;
                while j < t.len() && !t[j].is("INTO") && j < i + 3 {
                    j += 1;
                }
                if t.get(j).is_some_and(|x| x.is("INTO")) {
                    if let Some((n, _, link)) = chain(t, j + 1) {
                        acc.table(n, 'C', line, link);
                    }
                }
            }
            // INSERT ALL 의 두 번째 이후 INTO
            "INTO" if prev_w != Some("INSERT") && prev_w != Some("ALL") && prev_w != Some("FIRST") && prev_w != Some("MERGE") => {
                if is_multi_insert(t, i) {
                    if let Some((n, _, link)) = chain(t, i + 1) {
                        acc.table(n, 'C', line, link);
                    }
                }
            }
            "TRUNCATE" if t.get(i + 1).is_some_and(|x| x.is("TABLE")) => {
                if let Some((n, _, link)) = chain(t, i + 2) {
                    acc.table(n, 'D', line, link);
                }
            }
            "UPDATE" if prev_w != Some("FOR") && !t.get(i + 1).is_some_and(|x| x.is("SET")) => {
                if let Some((n, _, link)) = chain(t, i + 1) {
                    acc.table(n, 'U', line, link);
                }
            }
            "DELETE" if !t.get(i + 1).is_some_and(|x| x.is("WHERE") || x.sym(";") || x.sym("(")) => {
                let j = if t.get(i + 1).is_some_and(|x| x.is("FROM")) { i + 2 } else { i + 1 };
                if let Some((n, _, link)) = chain(t, j) {
                    acc.table(n, 'D', line, link);
                }
            }
            "MERGE" if t.get(i + 1).is_some_and(|x| x.is("INTO")) => {
                if let Some((n, k, link)) = chain(t, i + 2) {
                    acc.table(n.clone(), 'C', line, link.clone());
                    acc.table(n, 'U', line, link);
                    // USING src
                    let mut k = k;
                    while k < t.len() && !t[k].is("USING") && k < i + 6 {
                        k += 1;
                    }
                    if t.get(k).is_some_and(|x| x.is("USING")) {
                        if let Some((s, _, l2)) = chain(t, k + 1) {
                            acc.table(s, 'R', line, l2);
                        }
                    }
                }
            }
            "FROM" if prev_w != Some("DELETE") => {
                let func = parens.last().cloned().flatten();
                if !matches!(func.as_deref(), Some("EXTRACT") | Some("TRIM") | Some("SUBSTRING")) {
                    from_list(t, i + 1, &mut acc);
                }
            }
            "JOIN" => {
                if let Some((n, _, link)) = chain(t, i + 1) {
                    if !builtin(&n) {
                        acc.table(n, 'R', line, link);
                    }
                }
            }
            "EXECUTE" if t.get(i + 1).is_some_and(|x| x.is("IMMEDIATE")) => {
                let what = match t.get(i + 2).map(|x| &x.kind) {
                    Some(Kind::Str(s)) => format!("EXECUTE IMMEDIATE '{}'", short(s, 60)),
                    _ => "EXECUTE IMMEDIATE (문자열 변수)".into(),
                };
                f.dynamic_sql.push(Mark { line, what });
            }
            "OPEN" => {
                // OPEN c FOR 'select …' | v_sql
                if let Some((_, k, _)) = chain(t, i + 1) {
                    if t.get(k).is_some_and(|x| x.is("FOR")) && !t.get(k + 1).is_some_and(|x| x.is("SELECT") || x.is("WITH") || x.sym("(")) {
                        f.dynamic_sql.push(Mark { line, what: "OPEN … FOR (동적 SQL)".into() });
                    }
                }
            }
            "COMMIT" | "ROLLBACK" | "SAVEPOINT" => {
                let what = if w == "ROLLBACK" && t.get(i + 1).is_some_and(|x| x.is("TO")) { "ROLLBACK TO SAVEPOINT".to_string() } else { w.to_string() };
                f.transactions.push(Mark { line, what });
            }
            "PRAGMA" if t.get(i + 1).is_some_and(|x| x.is("AUTONOMOUS_TRANSACTION")) => {
                f.transactions.push(Mark { line, what: "PRAGMA AUTONOMOUS_TRANSACTION".into() });
            }
            "RAISE_APPLICATION_ERROR" => {
                // ( -20001 , …
                let mut code = String::new();
                let mut j = i + 2;
                if t.get(j).is_some_and(|x| x.sym("-")) {
                    code.push('-');
                    j += 1;
                }
                if let Some(Kind::Num(x)) = t.get(j).map(|x| &x.kind) {
                    code.push_str(x);
                }
                let msg = t.iter().skip(j).take(6).find_map(|x| match &x.kind {
                    Kind::Str(s) => Some(short(s, 60)),
                    _ => None,
                });
                f.raises.push(Mark { line, what: format!("RAISE_APPLICATION_ERROR({code}){}", msg.map(|m| format!(" '{m}'")).unwrap_or_default()) });
            }
            "RAISE" => {
                let what = match t.get(i + 1).and_then(|x| x.name()) {
                    Some(n) => format!("RAISE {n}"),
                    None => "RAISE (다시 던짐)".into(),
                };
                f.raises.push(Mark { line, what });
            }
            "WHEN" if is_handler(t, i) => {
                // WHEN a OR b THEN
                let mut j = i + 1;
                while j < t.len() && !t[j].is("THEN") {
                    if let Some((n, k, _)) = chain(t, j) {
                        if n != "OR" {
                            handles.insert(n.clone());
                        }
                        j = k;
                    } else {
                        j += 1;
                    }
                }
                // THEN NULL ; (그리고 다음이 WHEN/END)
                if t.get(j + 1).is_some_and(|x| x.is("NULL")) && t.get(j + 2).is_some_and(|x| x.sym(";")) && t.get(j + 3).is_some_and(|x| x.is("WHEN") || x.is("END")) {
                    f.swallowed.push(line);
                }
            }
            _ => {}
        }

        // 호출 후보: 이름(.이름)* 다음이 '(' 이거나, 문장 처음에서 ';'
        if !builtin(w) && !locals.contains(w) {
            if let Some((n, k, _)) = chain(t, i) {
                let next_paren = t.get(k).is_some_and(|x| x.sym("("));
                let stmt_start = i == 0
                    || t[i - 1].sym(";")
                    || matches!(prev_w, Some("BEGIN") | Some("THEN") | Some("ELSE") | Some("LOOP") | Some("EXCEPTION"));
                let next_semi = t.get(k).is_some_and(|x| x.sym(";"));
                let after_name_kw = matches!(
                    prev_w,
                    Some("INTO") | Some("UPDATE") | Some("FROM") | Some("JOIN") | Some("TABLE") | Some("CURSOR") | Some("PROCEDURE")
                        | Some("FUNCTION") | Some("TYPE") | Some("SUBTYPE") | Some("MERGE") | Some("USING") | Some("RETURN")
                        | Some("END") | Some("DELETE") | Some("RAISE") | Some("OF") | Some("EXCEPTION_INIT")
                ) || (i > 0 && (t[i - 1].sym("%") || t[i - 1].sym("<<")));
                // 아래 "TYPE t IS TABLE OF x(10)" 같은 선언·형식 자리는 앞이 형식 자리인지로 걸러지지 않으므로
                // 지역 이름의 멤버(v_tab.COUNT, rec.f) 도 뺀다
                let first = n.split('.').next().unwrap_or("");
                let member_of_local = locals.contains(first);
                let typed_decl = i > 0 && t[i - 1].word().is_some_and(|p| locals.contains(p)) && !stmt_start;
                if !after_name_kw && !member_of_local && !typed_decl && (next_paren || (stmt_start && next_semi)) {
                    let last = n.rsplit('.').next().unwrap_or("");
                    if !builtin(last) || n.contains('.') {
                        acc.calls.entry(n).or_default().insert(line);
                    }
                }
                i = k.max(i + 1);
                continue;
            }
        }
        i += 1;
    }

    let ctes = cte_names(t);
    f.tables = acc
        .tables
        .into_iter()
        .filter(|(n, _)| n != "DUAL" && n != "SYS.DUAL" && !ctes.contains(n))
        .map(|(name, (ops, lines))| TableUse {
            name,
            ops: ["C", "R", "U", "D"].iter().filter(|o| ops.contains(&o.chars().next().unwrap())).copied().collect(),
            lines: lines.into_iter().collect(),
        })
        .collect();
    f.calls = acc.calls.into_iter().map(|(name, lines)| CallUse { name, lines: lines.into_iter().collect() }).collect();
    f.sequences = acc.sequences.into_iter().collect();
    f.db_links = acc.links.into_iter().collect();
    f.handles = handles.into_iter().collect();
    f.complexity = complexity;
    if let (Some(a), Some(b)) = (t.first(), t.last()) {
        f.lines = b.line - a.line + 1;
    }
    f
}

fn short(s: &str, n: usize) -> String {
    let s: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if s.chars().count() > n {
        format!("{}…", s.chars().take(n).collect::<String>())
    } else {
        s
    }
}

/// FROM a x, b, (SELECT …) v, TABLE(f) …  — 테이블만 R 로 넣는다
fn from_list(t: &[Tok], mut i: usize, acc: &mut Acc) {
    loop {
        let Some(tk) = t.get(i) else { return };
        if tk.sym("(") {
            // 인라인 뷰 — 안쪽 FROM 은 바깥 루프가 따로 본다. 닫는 괄호까지 건너뛴다
            let mut d = 0;
            while i < t.len() {
                if t[i].sym("(") {
                    d += 1;
                } else if t[i].sym(")") {
                    d -= 1;
                    if d == 0 {
                        break;
                    }
                }
                i += 1;
            }
            i += 1;
        } else if tk.is("TABLE") || tk.is("LATERAL") || tk.is("THE") {
            return;
        } else if let Some(w) = tk.word() {
            if CLAUSE.contains(&w) || w == "DUAL" && !t.get(i + 1).is_some_and(|x| x.sym(".")) {
                if w != "DUAL" {
                    return;
                }
                i += 1;
            } else {
                match chain(t, i) {
                    Some((n, k, link)) => {
                        // 함수 호출 FROM f(x) 는 테이블이 아니다
                        if t.get(k).is_some_and(|x| x.sym("(")) {
                            return;
                        }
                        acc.table(n, 'R', tk.line, link);
                        i = k;
                    }
                    None => return,
                }
            }
        } else if matches!(tk.kind, Kind::Quoted(_)) {
            match chain(t, i) {
                Some((n, k, link)) => {
                    acc.table(n, 'R', tk.line, link);
                    i = k;
                }
                None => return,
            }
        } else {
            return;
        }
        // 별칭
        if t.get(i).is_some_and(|x| x.is("AS")) {
            i += 1;
        }
        if let Some(w) = t.get(i).and_then(|x| x.word()) {
            if !CLAUSE.contains(&w) && w != "PARTITION" {
                i += 1;
            }
        } else if t.get(i).is_some_and(|x| matches!(x.kind, Kind::Quoted(_))) {
            i += 1;
        }
        if t.get(i).is_some_and(|x| x.sym(",")) {
            i += 1;
            continue;
        }
        return;
    }
}

/// INSERT ALL/FIRST 의 이어지는 INTO 인지 — 앞쪽에 같은 문장의 INSERT ALL 이 있는지 본다
fn is_multi_insert(t: &[Tok], i: usize) -> bool {
    let mut j = i;
    while j > 0 {
        j -= 1;
        if t[j].sym(";") {
            return false;
        }
        if t[j].is("INSERT") {
            return t.get(j + 1).is_some_and(|x| x.is("ALL") || x.is("FIRST"));
        }
    }
    false
}

/// WHEN 이 예외 처리기인지 (CASE 의 WHEN 이 아니라) — 앞쪽으로 EXCEPTION 이 CASE 보다 먼저 나오면
fn is_handler(t: &[Tok], i: usize) -> bool {
    let mut j = i;
    let mut depth = 0i32;
    while j > 0 {
        j -= 1;
        let after_end = j > 0 && t[j - 1].is("END");
        match t[j].word() {
            // END IF / END LOOP 는 IF·LOOP 를 세지 않으므로 같이 뺀다
            Some("END") if !t.get(j + 1).is_some_and(|x| x.is("IF") || x.is("LOOP")) => depth += 1,
            Some("CASE") if !after_end => {
                if depth == 0 {
                    return false;
                }
                depth -= 1;
            }
            Some("BEGIN") => {
                if depth == 0 {
                    return false;
                }
                depth -= 1;
            }
            Some("EXCEPTION") if depth == 0 => return true,
            Some("MERGE") if depth == 0 => return false,
            _ => {}
        }
    }
    false
}

/// 여러 조각의 사실을 합친다 (서브프로그램·단위 수준 요약에 쓴다)
pub fn merge(parts: &[&Facts]) -> Facts {
    let mut tables: BTreeMap<String, (BTreeSet<char>, BTreeSet<u32>)> = BTreeMap::new();
    let mut calls: BTreeMap<String, BTreeSet<u32>> = BTreeMap::new();
    let mut out = Facts::default();
    let mut seq = BTreeSet::new();
    let mut links = BTreeSet::new();
    let mut handles = BTreeSet::new();
    for p in parts {
        for t in &p.tables {
            let e = tables.entry(t.name.clone()).or_default();
            e.0.extend(t.ops.chars());
            e.1.extend(t.lines.iter().copied());
        }
        for c in &p.calls {
            calls.entry(c.name.clone()).or_default().extend(c.lines.iter().copied());
        }
        seq.extend(p.sequences.iter().cloned());
        links.extend(p.db_links.iter().cloned());
        handles.extend(p.handles.iter().cloned());
        out.dynamic_sql.extend(p.dynamic_sql.iter().cloned());
        out.transactions.extend(p.transactions.iter().cloned());
        out.raises.extend(p.raises.iter().cloned());
        out.swallowed.extend(p.swallowed.iter().copied());
        out.complexity += p.complexity.saturating_sub(1);
        out.lines += p.lines;
        out.statements.extend(p.statements.iter().cloned());
        out.cursors.extend(p.cursors.iter().cloned());
    }
    out.statements.sort_by_key(|s| s.line);
    out.cursors.sort_by_key(|c| c.line);
    out.complexity += 1;
    out.tables = tables
        .into_iter()
        .map(|(name, (ops, lines))| TableUse {
            name,
            ops: ["C", "R", "U", "D"].iter().filter(|o| ops.contains(&o.chars().next().unwrap())).copied().collect(),
            lines: lines.into_iter().collect(),
        })
        .collect();
    out.calls = calls.into_iter().map(|(name, lines)| CallUse { name, lines: lines.into_iter().collect() }).collect();
    out.sequences = seq.into_iter().collect();
    out.db_links = links.into_iter().collect();
    out.handles = handles.into_iter().collect();
    out
}

/// 동적 SQL(EXECUTE IMMEDIATE / OPEN FOR 문자열)에서 읽은 테이블을 테이블 목록에 넣는다
pub fn absorb_dynamic(f: &mut Facts) {
    let mut add: Vec<(String, char, u32)> = Vec::new();
    for s in f.statements.iter().filter(|s| s.kind.starts_with("EXECUTE") || s.kind.ends_with("(동적)")) {
        for w in &s.writes {
            for o in w.ops.chars() {
                add.push((w.name.clone(), o, s.line));
            }
        }
        for r in &s.reads {
            add.push((r.clone(), 'R', s.line));
        }
    }
    for (name, op, line) in add {
        match f.tables.iter_mut().find(|t| t.name == name) {
            Some(t) => {
                if !t.ops.contains(op) {
                    let mut set: Vec<char> = t.ops.chars().chain([op]).collect();
                    set.sort_by_key(|c| "CRUD".find(*c).unwrap_or(9));
                    t.ops = set.into_iter().collect();
                }
                if !t.lines.contains(&line) {
                    t.lines.push(line);
                    t.lines.sort();
                }
            }
            None => f.tables.push(TableUse { name, ops: op.to_string(), lines: vec![line] }),
        }
    }
    f.tables.sort_by(|a, b| a.name.cmp(&b.name));
}

/// WITH 절의 CTE 이름 (`WITH a AS (SELECT …), b AS (…)`) — 테이블이 아니다
pub fn cte_names(t: &[Tok]) -> HashSet<String> {
    let mut out = HashSet::new();
    for i in 0..t.len() {
        let starts = i > 0 && (t[i - 1].is("WITH") || t[i - 1].sym(","));
        if !starts {
            continue;
        }
        let Some(n) = t[i].name() else { continue };
        // 이름 [(컬럼, …)] AS (SELECT|WITH
        let mut j = i + 1;
        if t.get(j).is_some_and(|x| x.sym("(")) {
            while j < t.len() && !t[j].sym(")") {
                j += 1;
            }
            j += 1;
        }
        if t.get(j).is_some_and(|x| x.is("AS")) && t.get(j + 1).is_some_and(|x| x.sym("(")) && t.get(j + 2).is_some_and(|x| x.is("SELECT") || x.is("WITH")) {
            out.insert(n);
        }
    }
    out
}
