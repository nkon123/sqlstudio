//! 값 추적 — "이 행(키)의 이 값은 어떻게 나왔나".
//!
//! 그 열에 값을 넣는 문장(colwrite)마다, 문장을 "원본 쿼리(FROM·조건) + 그 열에 들어간 식 + 대상 키와 이어지는 식"
//! 으로 풀고([`paths`]), 이 행의 키 값으로 그 원본 쿼리를 다시 돌리는 SELECT 를 만든다([`build`]).
//! 다시 계산한 값이 지금 값과 같으면 그 문장이 이 값을 만든 길이다. 0건이면 조건마다 맞는지(O/X)를 본다.
//! 값에 쓰인 원본 열은 원본 행의 키와 함께 읽어, 한 단계 더 거슬러 오를 수 있게 한다.
//!
//! 모델은 쓰지 않는다. DB 에 보내는 것은 여기서 만든 SELECT 뿐이고, 돌리는 것은 앱이 정한다(운영 DB 는 보이기만).
//!
//! 다루는 문장: INSERT … SELECT · INSERT … VALUES (커서 레코드 `R.COL`, SELECT INTO 로 받은 변수를 거슬러) ·
//! MERGE (UPDATE·INSERT 갈래) · UPDATE … SET (`(a, b) = (SELECT …)` 포함).

use serde::Serialize;

use crate::colwrite::StmtLite;
use crate::plsql::{lex, Kind, Tok};

/// 원본 쿼리 조각
#[derive(Debug, Clone)]
pub struct Source {
    /// 앞에 붙일 WITH … (없으면 빈칸)
    pub with: Vec<Tok>,
    /// FROM 뒤 (JOIN 포함, WHERE 앞까지)
    pub from: Vec<Tok>,
    /// WHERE 의 AND 조각
    pub conds: Vec<Vec<Tok>>,
    /// START WITH · CONNECT BY (행을 고르는 데 쓰인다)
    pub hier: Vec<Tok>,
    /// GROUP BY · HAVING (값을 만드는 데만)
    pub group: Vec<Tok>,
    /// 별칭 → 테이블 이름 (테이블만, 인라인 뷰는 빼고)
    pub tables: Vec<(String, String)>,
}

/// 문장 하나가 그 열을 채우는 길 하나 (MERGE 는 UPDATE·INSERT 두 갈래)
#[derive(Debug, Clone)]
pub struct Path {
    pub label: String,
    /// 그 열에 들어간 식 (원본 쪽)
    pub expr: Vec<Tok>,
    /// (대상 키 열, 원본 쪽 식) — 비면 키로 거르지 못한다
    pub keys: Vec<(String, Vec<Tok>)>,
    pub src: Option<Source>,
    /// 값을 아는 변수: 대상 키가 변수로 들어갔으면 그 변수는 이 행의 키 값이다 (변수 이름, 대상 키 열)
    pub key_vars: Vec<(String, String)>,
    pub notes: Vec<String>,
}

/// 값에 쓰인 원본 열 — 다음 단계로 거슬러 오를 곳
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Input {
    pub alias: String,
    pub table: String,
    pub column: String,
}

impl Path {
    pub fn expr_text(&self) -> String {
        sql_text(&self.expr)
    }

    /// 원본 쿼리를 사람이 읽을 글로 (화면 표시용)
    pub fn source_text(&self) -> Option<String> {
        let s = self.src.as_ref()?;
        let mut out = String::new();
        if !s.with.is_empty() {
            out.push_str(&sql_text(&s.with));
            out.push(' ');
        }
        out.push_str(&format!("FROM {}", sql_text(&s.from)));
        if !s.conds.is_empty() {
            out.push_str(&format!(" WHERE {}", s.conds.iter().map(|c| sql_text(c)).collect::<Vec<_>>().join(" AND ")));
        }
        for t in [&s.hier, &s.group] {
            if !t.is_empty() {
                out.push(' ');
                out.push_str(&sql_text(t));
            }
        }
        Some(out)
    }

    /// 식에 쓰인 원본 열 (`별칭.열` — 별칭이 원본 테이블인 것). 테이블이 하나뿐이면 맨 이름도 후보로 (앱이 컬럼 목록으로 거른다)
    pub fn inputs(&self) -> (Vec<Input>, Vec<String>) {
        let Some(s) = &self.src else { return (vec![], vec![]) };
        let mut out: Vec<Input> = Vec::new();
        let mut bare: Vec<String> = Vec::new();
        let t = &self.expr;
        for i in 0..t.len() {
            let Some(w) = name(&t[i]) else { continue };
            let after_dot = i > 0 && sym(&t[i - 1], ".");
            if after_dot {
                continue;
            }
            if t.get(i + 1).is_some_and(|x| sym(x, ".")) {
                let Some(col) = t.get(i + 2).and_then(name) else { continue };
                if t.get(i + 3).is_some_and(|x| sym(x, "(")) {
                    continue;
                }
                if let Some((a, tb)) = s.tables.iter().find(|(a, _)| *a == w) {
                    let inp = Input { alias: a.clone(), table: tb.clone(), column: col };
                    if !out.contains(&inp) {
                        out.push(inp);
                    }
                }
            } else if s.tables.len() == 1 && !t.get(i + 1).is_some_and(|x| sym(x, "(")) && !is_keyword(&w) && !bare.contains(&w) {
                bare.push(w);
            }
        }
        (out, bare)
    }
}

// ── 토큰 도우미 ──────────────────────────────────────

/// 토큰 → SQL 글. `plsql::join_tokens` 에 더해, 키워드 뒤 괄호(`AS (`, `VALUES (`)와 바인드(`= :A`) 앞은 띄운다
pub fn sql_text(t: &[Tok]) -> String {
    let s = crate::plsql::join_tokens(t);
    const KW: &[&str] = &["AS", "IN", "VALUES", "AND", "OR", "NOT", "EXISTS", "FROM", "ON", "USING", "WHERE", "SELECT", "THEN", "ELSE", "WHEN", "BY", "INTO", "JOIN", "SET", "ANY", "ALL"];
    let c: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len() + 8);
    let mut i = 0;
    let mut word = String::new();
    while i < c.len() {
        let ch = c[i];
        if ch == '\'' || ch == '"' {
            // 문자열·따옴표 이름은 그대로
            let q = ch;
            out.push(ch);
            i += 1;
            while i < c.len() {
                out.push(c[i]);
                if c[i] == q {
                    if c.get(i + 1) == Some(&q) {
                        out.push(q);
                        i += 2;
                        continue;
                    }
                    break;
                }
                i += 1;
            }
            i += 1;
            word.clear();
            continue;
        }
        if ch == '(' && KW.contains(&word.as_str()) && !out.ends_with(' ') {
            out.push(' ');
        }
        if ch == ':' && c.get(i + 1).is_some_and(|n| n.is_alphabetic()) && out.chars().last().is_some_and(|p| p.is_alphanumeric() || p == '_' || p == ')') {
            out.push(' ');
        }
        if ch.is_alphanumeric() || ch == '_' || ch == '$' || ch == '#' {
            word.push(ch);
        } else {
            word.clear();
        }
        out.push(ch);
        i += 1;
    }
    out
}

fn sym(t: &Tok, s: &str) -> bool {
    matches!(&t.kind, Kind::Sym(x) if *x == s)
}

fn word(t: &Tok) -> Option<&str> {
    t.word()
}

fn name(t: &Tok) -> Option<String> {
    match &t.kind {
        Kind::Word(w) => Some(w.clone()),
        Kind::Quoted(w) => Some(w.clone()),
        _ => None,
    }
}

fn is_keyword(w: &str) -> bool {
    matches!(
        w,
        "SELECT" | "FROM" | "WHERE" | "AND" | "OR" | "NOT" | "NULL" | "IS" | "IN" | "CASE" | "WHEN" | "THEN" | "ELSE" | "END" | "AS" | "DISTINCT"
            | "SYSDATE" | "SYSTIMESTAMP" | "USER" | "ROWNUM" | "LEVEL" | "LIKE" | "BETWEEN" | "EXISTS" | "TRUE" | "FALSE" | "DATE" | "INTERVAL"
            | "DAY" | "MONTH" | "YEAR" | "HOUR" | "MINUTE" | "SECOND" | "TO" | "ON" | "BY" | "ORDER" | "GROUP" | "UNION" | "ALL" | "ANY" | "SOME"
    )
}

/// 각 토큰 앞의 괄호 깊이
fn depths(t: &[Tok]) -> Vec<i32> {
    let mut d = 0;
    t.iter()
        .map(|x| {
            if sym(x, ")") {
                d -= 1;
            }
            let here = d;
            if sym(x, "(") {
                d += 1;
            }
            here
        })
        .collect()
}

/// i 의 '(' 와 짝인 ')' 위치
fn close_of(t: &[Tok], i: usize) -> Option<usize> {
    let mut d = 0;
    for (j, x) in t.iter().enumerate().skip(i) {
        if sym(x, "(") {
            d += 1;
        } else if sym(x, ")") {
            d -= 1;
            if d == 0 {
                return Some(j);
            }
        }
    }
    None
}

/// 깊이 0 의 쉼표로 나눈다
fn split_commas(t: &[Tok]) -> Vec<Vec<Tok>> {
    let d = depths(t);
    let mut out = vec![vec![]];
    for (i, x) in t.iter().enumerate() {
        if d[i] == 0 && sym(x, ",") {
            out.push(vec![]);
        } else {
            out.last_mut().unwrap().push(x.clone());
        }
    }
    out.retain(|v| !v.is_empty());
    out
}

/// 깊이 0 의 AND 로 나눈다 (BETWEEN a AND b 의 AND 는 빼고). 바깥 괄호 하나로만 싸였으면 벗긴다
fn split_and(t: &[Tok]) -> Vec<Vec<Tok>> {
    let t = strip_parens(t);
    let d = depths(t);
    let mut out: Vec<Vec<Tok>> = vec![vec![]];
    let mut between = false;
    for (i, x) in t.iter().enumerate() {
        if d[i] == 0 && word(x) == Some("BETWEEN") {
            between = true;
        }
        if d[i] == 0 && word(x) == Some("AND") {
            if between {
                between = false;
            } else {
                out.push(vec![]);
                continue;
            }
        }
        out.last_mut().unwrap().push(x.clone());
    }
    // OR 가 깊이 0 에 있으면 나누지 않는다 (a AND b OR c)
    if t.iter().enumerate().any(|(i, x)| d[i] == 0 && word(x) == Some("OR")) {
        return vec![t.to_vec()];
    }
    out.retain(|v| !v.is_empty());
    out
}

fn strip_parens(t: &[Tok]) -> &[Tok] {
    if t.len() >= 2 && sym(&t[0], "(") && close_of(t, 0) == Some(t.len() - 1) && !matches!(t.get(1).and_then(word), Some("SELECT" | "WITH")) {
        strip_parens(&t[1..t.len() - 1])
    } else {
        t
    }
}

/// i 의 [owner.]이름[@링크] — (이름, 다음 위치)
fn table_at(t: &[Tok], mut i: usize) -> Option<(String, usize)> {
    let mut n = name(t.get(i)?)?;
    i += 1;
    while t.get(i).is_some_and(|x| sym(x, ".")) {
        n.push('.');
        n.push_str(&name(t.get(i + 1)?)?);
        i += 2;
    }
    if t.get(i).is_some_and(|x| sym(x, "@")) {
        n.push('@');
        n.push_str(&name(t.get(i + 1)?)?);
        i += 2;
    }
    Some((n, i))
}

fn leaf(table: &str) -> String {
    let base = table.split('@').next().unwrap_or(table);
    base.rsplit('.').next().unwrap_or(base).to_string()
}

/// 이름이 찾는 테이블인지 (소유자를 붙였든 안 붙였든)
fn same_table(written: &str, table: &str) -> bool {
    leaf(written) == leaf(table)
}

fn toks(s: &str) -> Vec<Tok> {
    lex(s)
}

// ── SELECT 풀기 ─────────────────────────────────────

#[derive(Debug, Clone)]
struct Sel {
    with: Vec<Tok>,
    /// (식, 이름 — 별칭 또는 열 이름)
    items: Vec<(Vec<Tok>, Option<String>)>,
    into: Vec<String>,
    from: Vec<Tok>,
    conds: Vec<Vec<Tok>>,
    hier: Vec<Tok>,
    group: Vec<Tok>,
    set_op: bool,
}

/// SELECT 항목 하나 → (식, 이름)
fn item_of(t: &[Tok]) -> (Vec<Tok>, Option<String>) {
    let n = t.len();
    if n >= 3 && word(&t[n - 2]) == Some("AS") {
        return (t[..n - 2].to_vec(), name(&t[n - 1]));
    }
    // 식 뒤에 바로 붙은 별칭: 앞 토큰이 피연산자(이름·숫자·문자·닫는 괄호)고 '.' 뒤가 아니면
    if n >= 2 {
        let last = &t[n - 1];
        let prev = &t[n - 2];
        let operand = matches!(prev.kind, Kind::Word(_) | Kind::Quoted(_) | Kind::Num(_) | Kind::Str(_)) || sym(prev, ")");
        if name(last).is_some() && operand && !sym(prev, ".") && !word(prev).is_some_and(|w| matches!(w, "AND" | "OR" | "NOT" | "IS" | "IN" | "LIKE" | "THEN" | "ELSE" | "WHEN")) && !word(last).is_some_and(|w| w == "END" && word(prev).is_some()) {
            // CASE … END 의 END 는 별칭이 아니다
            if word(last) != Some("END") {
                return (t[..n - 1].to_vec(), name(last));
            }
        }
    }
    // a.col · col
    let nm = name(&t[n - 1]).filter(|_| n == 1 || (n == 3 && sym(&t[1], ".")));
    (t.to_vec(), nm)
}

fn parse_select(t: &[Tok]) -> Option<Sel> {
    let mut t = strip_parens(t);
    // (SELECT …) — 서브쿼리 괄호
    while t.len() >= 2 && sym(&t[0], "(") && close_of(t, 0) == Some(t.len() - 1) {
        t = &t[1..t.len() - 1];
    }
    let d = depths(t);
    let mut i = 0;
    let mut with = Vec::new();
    if t.first().and_then(word) == Some("WITH") {
        let s = (1..t.len()).find(|&j| d[j] == 0 && word(&t[j]) == Some("SELECT"))?;
        with = t[..s].to_vec();
        i = s;
    }
    if t.get(i).and_then(word) != Some("SELECT") {
        return None;
    }
    i += 1;
    if matches!(t.get(i).and_then(word), Some("DISTINCT" | "UNIQUE" | "ALL")) {
        i += 1;
    }
    let at0 = |j: usize, ws: &[&str]| d[j] == 0 && word(&t[j]).is_some_and(|w| ws.contains(&w));
    let stop_items = (i..t.len()).find(|&j| at0(j, &["FROM", "INTO", "BULK"]))?;
    let items = split_commas(&t[i..stop_items]).iter().map(|x| item_of(x)).collect();
    let mut j = stop_items;
    let mut into = Vec::new();
    if word(&t[j]) != Some("FROM") {
        let f = (j..t.len()).find(|&k| at0(k, &["FROM"]))?;
        into = t[j..f].iter().filter_map(|x| if sym(x, ".") { None } else { name(x) }).filter(|w| !matches!(w.as_str(), "INTO" | "BULK" | "COLLECT")).collect();
        j = f;
    }
    j += 1;
    let ends = ["WHERE", "GROUP", "HAVING", "ORDER", "CONNECT", "START", "UNION", "MINUS", "INTERSECT", "FOR", "FETCH", "OFFSET", "MODEL"];
    let from_end = (j..t.len()).find(|&k| at0(k, &ends)).unwrap_or(t.len());
    let from = t[j..from_end].to_vec();
    let mut k = from_end;
    let mut conds = Vec::new();
    let mut hier = Vec::new();
    let mut group = Vec::new();
    let mut set_op = false;
    while k < t.len() {
        let w = word(&t[k]).unwrap_or("");
        let next = (k + 1..t.len()).find(|&m| at0(m, &ends) && !(w == "START" && word(&t[m]) == Some("WITH")) && !(matches!(w, "GROUP" | "ORDER" | "CONNECT") && word(&t[m]) == Some("BY"))).unwrap_or(t.len());
        let body = &t[k..next];
        match w {
            "WHERE" => conds = split_and(&body[1..]),
            "START" | "CONNECT" => hier.extend_from_slice(body),
            "GROUP" | "HAVING" => group.extend_from_slice(body),
            "UNION" | "MINUS" | "INTERSECT" => {
                set_op = true;
                break;
            }
            _ => {}
        }
        k = next;
    }
    Some(Sel { with, items, into, from, conds, hier, group, set_op })
}

/// FROM 의 테이블 (별칭 → 이름). 인라인 뷰는 빼고, 별칭이 없으면 테이블 이름이 별칭
fn from_tables(from: &[Tok]) -> Vec<(String, String)> {
    let d = depths(from);
    let mut out = Vec::new();
    // 항목의 시작: 맨 앞, 깊이 0 의 쉼표 뒤, JOIN 뒤
    let mut starts = vec![0];
    for (i, x) in from.iter().enumerate() {
        if d[i] == 0 && (sym(x, ",") || word(x) == Some("JOIN")) {
            starts.push(i + 1);
        }
    }
    for s in starts {
        if from.get(s).is_some_and(|x| sym(x, "(")) {
            continue;
        }
        let Some((tb, k)) = table_at(from, s) else { continue };
        let alias = from.get(k).and_then(|x| match &x.kind {
            Kind::Word(w) if !matches!(w.as_str(), "ON" | "JOIN" | "LEFT" | "RIGHT" | "INNER" | "FULL" | "CROSS" | "NATURAL" | "OUTER" | "USING" | "WHERE" | "PARTITION" | "SAMPLE") => Some(w.clone()),
            Kind::Quoted(w) => Some(w.clone()),
            _ => None,
        });
        out.push((alias.unwrap_or_else(|| leaf(&tb)), tb));
    }
    out
}

fn source_of(sel: &Sel) -> Source {
    Source {
        with: sel.with.clone(),
        from: sel.from.clone(),
        conds: sel.conds.clone(),
        hier: sel.hier.clone(),
        group: sel.group.clone(),
        tables: from_tables(&sel.from),
    }
}

/// 식이 맨 이름 하나(변수·매개변수)면 그 이름
fn bare_name(t: &[Tok]) -> Option<String> {
    let t = strip_parens(t);
    (t.len() == 1).then(|| name(&t[0])).flatten().filter(|w| !is_keyword(w))
}

/// `R.X` 를 그 커서 쿼리의 X 항목 식으로 바꾼다. 못 바꾸면 None
fn replace_record(t: &[Tok], rec: &str, items: &[(Vec<Tok>, Option<String>)]) -> Option<Vec<Tok>> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < t.len() {
        if name(&t[i]).as_deref() == Some(rec) && t.get(i + 1).is_some_and(|x| sym(x, ".")) && !(i > 0 && sym(&t[i - 1], ".")) {
            let f = name(t.get(i + 2)?)?;
            let (e, _) = items.iter().find(|(_, n)| n.as_deref() == Some(f.as_str()))?;
            out.push(Tok { kind: Kind::Sym("("), line: 0 });
            out.extend(e.iter().cloned());
            out.push(Tok { kind: Kind::Sym(")"), line: 0 });
            i += 3;
            continue;
        }
        out.push(t[i].clone());
        i += 1;
    }
    Some(out)
}

fn uses_name(t: &[Tok], n: &str) -> bool {
    t.iter().enumerate().any(|(i, x)| name(x).as_deref() == Some(n) && !(i > 0 && sym(&t[i - 1], ".")))
}

// ── 문장 → 길 ───────────────────────────────────────

/// 문장(INSERT·MERGE·UPDATE)이 `table.column` 을 채우는 길들.
/// `key_cols` 는 대상 테이블의 키 열, `table_cols` 는 열 목록 없는 INSERT 를 위치로 맞출 때.
/// `node_stmts` 는 같은 서브프로그램의 다른 문장 (커서 쿼리·SELECT INTO 를 거슬러 오를 때), `feeds` 는 이 문장에 들어오는 커서 (커서, "record R" · "variable V")
pub fn paths(stmt: &StmtLite, node_stmts: &[StmtLite], text: &str, table: &str, column: &str, key_cols: &[String], table_cols: &[String]) -> Vec<Path> {
    let t = toks(text);
    let t: Vec<Tok> = t.into_iter().filter(|x| !sym(x, ";")).collect();
    let col = column.to_uppercase();
    match t.first().and_then(word) {
        Some("INSERT") => insert_paths(stmt, node_stmts, &t, table, &col, key_cols, table_cols),
        Some("MERGE") => merge_paths(&t, table, &col, key_cols),
        Some("UPDATE") => update_paths(&t, table, &col, key_cols),
        _ => vec![],
    }
}

fn note_path(label: &str, note: impl Into<String>) -> Path {
    Path { label: label.into(), expr: vec![], keys: vec![], src: None, key_vars: vec![], notes: vec![note.into()] }
}

fn insert_paths(stmt: &StmtLite, node_stmts: &[StmtLite], t: &[Tok], table: &str, col: &str, key_cols: &[String], table_cols: &[String]) -> Vec<Path> {
    let mut i = 1;
    if matches!(t.get(i).and_then(word), Some("ALL" | "FIRST")) {
        return vec![note_path("INSERT ALL", "여러 테이블에 넣는 INSERT 는 아직 거슬러 오르지 못합니다")];
    }
    if t.get(i).and_then(word) != Some("INTO") {
        return vec![];
    }
    let Some((tb, mut k)) = table_at(t, i + 1) else { return vec![] };
    if !same_table(&tb, table) {
        return vec![];
    }
    if t.get(k).and_then(word).is_some_and(|a| !matches!(a, "VALUES" | "SELECT" | "WITH")) && !t.get(k).is_some_and(|x| sym(x, "(")) {
        k += 1;
    }
    let mut cols: Vec<String> = table_cols.iter().map(|c| c.to_uppercase()).collect();
    if t.get(k).is_some_and(|x| sym(x, "(")) && !matches!(t.get(k + 1).and_then(word), Some("SELECT" | "WITH")) {
        let Some(e) = close_of(t, k) else { return vec![] };
        cols = split_commas(&t[k + 1..e]).iter().filter_map(|c| c.iter().rev().find_map(name)).collect();
        k = e + 1;
    }
    if cols.is_empty() {
        return vec![note_path("INSERT", "열 목록이 없는 INSERT 인데 테이블 열 순서를 모릅니다")];
    }
    let Some(pos) = cols.iter().position(|c| c == col) else { return vec![] };
    i = k;
    let mut notes = Vec::new();
    if t.get(i).and_then(word) == Some("VALUES") {
        let Some(open) = (i + 1 < t.len()).then_some(i + 1).filter(|&o| sym(&t[o], "(")) else { return vec![] };
        let Some(e) = close_of(t, open) else { return vec![] };
        let vals = split_commas(&t[open + 1..e]);
        if vals.len() != cols.len() {
            return vec![note_path("INSERT … VALUES", "열 수와 값 수가 다릅니다")];
        }
        let expr = vals[pos].clone();
        let key_exprs: Vec<(String, Vec<Tok>)> = key_cols.iter().filter_map(|kc| cols.iter().position(|c| c == &kc.to_uppercase()).map(|p| (kc.clone(), vals[p].clone()))).collect();
        // 키가 변수로 들어갔으면 그 변수 = 이 행의 키 값
        let key_vars: Vec<(String, String)> = key_exprs.iter().filter_map(|(kc, e)| bare_name(e).map(|v| (v, kc.clone()))).collect();
        // ① 커서 레코드 R.X
        for f in &stmt_feeds(stmt) {
            let Some(rec) = f.1.strip_prefix("record ").map(str::to_uppercase) else { continue };
            if !uses_name(&expr, &rec) {
                continue;
            }
            let Some(cur) = node_stmts.iter().find(|s| s.cursor.as_deref().map(str::to_uppercase) == Some(f.0.to_uppercase()) && matches!(s.kind.as_str(), "CURSOR" | "FOR LOOP")) else { continue };
            let Some(sel) = parse_select(&toks(&cur.text)) else { continue };
            if sel.set_op {
                return vec![note_path("INSERT … VALUES (커서)", format!("커서 {} 에 UNION 이 있어 다시 계산하지 않습니다", f.0))];
            }
            let Some(e2) = replace_record(&expr, &rec, &sel.items) else { continue };
            let keys: Vec<(String, Vec<Tok>)> = key_exprs.iter().filter_map(|(kc, ke)| replace_record(ke, &rec, &sel.items).filter(|r| !uses_name(r, &rec)).map(|r| (kc.clone(), r))).collect();
            if keys.len() < key_cols.len() {
                notes.push("키 열 일부를 커서 쿼리와 잇지 못했습니다".into());
            }
            return vec![Path { label: format!("INSERT … VALUES ← 커서 {} ({}줄)", f.0, cur.line), expr: e2, keys, src: Some(source_of(&sel)), key_vars, notes }];
        }
        // ② SELECT INTO 로 받은 변수
        if let Some(v) = bare_name(&expr) {
            let fed = node_stmts
                .iter()
                .filter(|s| s.line < stmt.line && s.kind.contains("SELECT") && s.into.iter().any(|x| x.eq_ignore_ascii_case(&v)))
                .max_by_key(|s| s.line);
            if let Some(s) = fed {
                if let Some(sel) = parse_select(&toks(&s.text)) {
                    if let Some(p) = sel.into.iter().position(|x| x.eq_ignore_ascii_case(&v)) {
                        if let Some((e2, _)) = sel.items.get(p) {
                            return vec![Path {
                                label: format!("INSERT … VALUES ← 변수 {v} (SELECT INTO {}줄)", s.line),
                                expr: e2.clone(),
                                keys: vec![],
                                src: Some(source_of(&sel)),
                                key_vars,
                                notes: vec!["키는 SELECT INTO 의 조건(변수)으로 이어집니다".into()],
                            }];
                        }
                    }
                }
            }
        }
        return vec![Path { label: "INSERT … VALUES".into(), expr, keys: vec![], src: None, key_vars, notes: vec!["변수·상수·함수 값이라 다시 계산할 원본 쿼리가 없습니다".into()] }];
    }
    let Some(sel) = parse_select(&t[i..]) else { return vec![] };
    if sel.set_op {
        return vec![note_path("INSERT … SELECT", "UNION 이 있어 다시 계산하지 않습니다")];
    }
    if sel.items.len() != cols.len() {
        return vec![note_path("INSERT … SELECT", "열 수와 SELECT 항목 수가 다릅니다")];
    }
    let keys: Vec<(String, Vec<Tok>)> = key_cols.iter().filter_map(|kc| cols.iter().position(|c| c == &kc.to_uppercase()).map(|p| (kc.clone(), sel.items[p].0.clone()))).collect();
    if keys.len() < key_cols.len() {
        notes.push("키 열 일부가 INSERT 열에 없어 키로 거르지 못합니다".into());
    }
    vec![Path { label: "INSERT … SELECT".into(), expr: sel.items[pos].0.clone(), keys, src: Some(source_of(&sel)), key_vars: vec![], notes }]
}

fn stmt_feeds(s: &StmtLite) -> Vec<(String, String)> {
    s.fed_by.iter().map(|f| (f.cursor.clone(), f.via.clone())).collect()
}

/// `별칭.열` 또는 `열` 로 시작하는 SET 항목에서 열 이름
fn set_target(lhs: &[Tok]) -> Option<String> {
    lhs.iter().rev().find_map(name)
}

fn merge_paths(t: &[Tok], table: &str, col: &str, key_cols: &[String]) -> Vec<Path> {
    // MERGE INTO 대상 [별칭] USING (원본) | 테이블 [별칭] ON ( 조건 ) WHEN …
    let Some((tb, mut k)) = table_at(t, 2) else { return vec![] };
    if !same_table(&tb, table) {
        return vec![];
    }
    let talias = match t.get(k).and_then(word) {
        Some(a) if a != "USING" => {
            k += 1;
            a.to_string()
        }
        _ => leaf(&tb),
    };
    if t.get(k).and_then(word) != Some("USING") {
        return vec![];
    }
    k += 1;
    let mut using: Vec<Tok> = Vec::new();
    if t.get(k).is_some_and(|x| sym(x, "(")) {
        let Some(e) = close_of(t, k) else { return vec![] };
        using.extend_from_slice(&t[k..=e]);
        k = e + 1;
    } else {
        let Some((_, k2)) = table_at(t, k) else { return vec![] };
        using.extend_from_slice(&t[k..k2]);
        k = k2;
    }
    if let Some(a) = t.get(k).filter(|x| word(x) != Some("ON")) {
        using.push(a.clone());
        k += 1;
    }
    if t.get(k).and_then(word) != Some("ON") || !t.get(k + 1).is_some_and(|x| sym(x, "(")) {
        return vec![];
    }
    let Some(on_end) = close_of(t, k + 1) else { return vec![] };
    let on = t[k + 2..on_end].to_vec();
    // FROM 대상 별칭, (원본) 별칭 WHERE ON 조건 — 지금은 대상 행이 있으니 INSERT 갈래도 이렇게 다시 계산한다
    let mut from: Vec<Tok> = t[2..table_at(t, 2).unwrap().1].to_vec();
    from.push(Tok { kind: Kind::Word(talias.clone()), line: 0 });
    from.push(Tok { kind: Kind::Sym(","), line: 0 });
    from.extend(using);
    let base = Source { with: vec![], from: from.clone(), conds: split_and(&on), hier: vec![], group: vec![], tables: from_tables(&from) };
    let keys: Vec<(String, Vec<Tok>)> = key_cols.iter().map(|kc| (kc.clone(), toks(&format!("{talias}.{}", kc.to_uppercase())))).collect();
    let d = depths(t);
    let mut out = Vec::new();
    let mut i = on_end + 1;
    while i < t.len() {
        if d[i] == 0 && word(&t[i]) == Some("UPDATE") && t.get(i + 1).and_then(word) == Some("SET") {
            let s = i + 2;
            let e = (s..t.len()).find(|&j| d[j] == 0 && matches!(word(&t[j]), Some("WHERE" | "DELETE" | "WHEN"))).unwrap_or(t.len());
            for item in split_commas(&t[s..e]) {
                let Some(eq) = item.iter().position(|x| sym(x, "=")) else { continue };
                if set_target(&item[..eq]).as_deref() == Some(col) {
                    let mut src = base.clone();
                    if t.get(e).and_then(word) == Some("WHERE") {
                        let we = (e + 1..t.len()).find(|&j| d[j] == 0 && matches!(word(&t[j]), Some("DELETE" | "WHEN"))).unwrap_or(t.len());
                        src.conds.extend(split_and(&t[e + 1..we]));
                    }
                    let expr = item[eq + 1..].to_vec();
                    let mut notes = Vec::new();
                    if uses_name(&expr, &talias) {
                        notes.push("대상의 이전 값을 써서 계산합니다 — 지금 값으로 다시 계산하면 다를 수 있습니다".into());
                    }
                    out.push(Path { label: "MERGE · 있으면 UPDATE".into(), expr, keys: keys.clone(), src: Some(src), key_vars: vec![], notes });
                }
            }
            i = e;
            continue;
        }
        if d[i] == 0 && word(&t[i]) == Some("INSERT") && word(&t[i.saturating_sub(1)]) == Some("THEN") {
            let mut j = i + 1;
            let mut cols: Vec<String> = Vec::new();
            if t.get(j).is_some_and(|x| sym(x, "(")) {
                let Some(e) = close_of(t, j) else { break };
                cols = split_commas(&t[j + 1..e]).iter().filter_map(|c| c.iter().rev().find_map(name)).collect();
                j = e + 1;
            }
            if t.get(j).and_then(word) == Some("VALUES") && t.get(j + 1).is_some_and(|x| sym(x, "(")) {
                let Some(e) = close_of(t, j + 1) else { break };
                let vals = split_commas(&t[j + 2..e]);
                if let Some(p) = cols.iter().position(|c| c == col) {
                    if let Some(v) = vals.get(p) {
                        out.push(Path { label: "MERGE · 없으면 INSERT".into(), expr: v.clone(), keys: keys.clone(), src: Some(base.clone()), key_vars: vec![], notes: vec![] });
                    }
                } else if cols.is_empty() {
                    out.push(note_path("MERGE · 없으면 INSERT", "열 목록 없는 INSERT 갈래는 위치를 모릅니다"));
                }
                i = e + 1;
                continue;
            }
        }
        i += 1;
    }
    out
}

fn update_paths(t: &[Tok], table: &str, col: &str, key_cols: &[String]) -> Vec<Path> {
    let Some((tb, mut k)) = table_at(t, 1) else { return vec![] };
    if !same_table(&tb, table) {
        return vec![];
    }
    let alias = match t.get(k).and_then(word) {
        Some(a) if a != "SET" => {
            k += 1;
            a.to_string()
        }
        _ => leaf(&tb),
    };
    if t.get(k).and_then(word) != Some("SET") {
        return vec![];
    }
    let d = depths(t);
    let s = k + 1;
    let e = (s..t.len()).find(|&j| d[j] == 0 && matches!(word(&t[j]), Some("WHERE" | "RETURNING" | "RETURN" | "LOG"))).unwrap_or(t.len());
    let mut conds = Vec::new();
    let mut notes = Vec::new();
    if t.get(e).and_then(word) == Some("WHERE") {
        let we = (e + 1..t.len()).find(|&j| d[j] == 0 && matches!(word(&t[j]), Some("RETURNING" | "RETURN" | "LOG"))).unwrap_or(t.len());
        let w = &t[e + 1..we];
        if w.first().and_then(word) == Some("CURRENT") {
            notes.push("WHERE CURRENT OF — 커서가 고른 행이라 조건 없이 이 키의 행으로 봅니다".into());
        } else {
            conds = split_and(w);
        }
    }
    let mut from: Vec<Tok> = t[1..table_at(t, 1).unwrap().1].to_vec();
    from.push(Tok { kind: Kind::Word(alias.clone()), line: 0 });
    let src = Source { with: vec![], from: from.clone(), conds, hier: vec![], group: vec![], tables: from_tables(&from) };
    let keys: Vec<(String, Vec<Tok>)> = key_cols.iter().map(|kc| (kc.clone(), toks(&format!("{alias}.{}", kc.to_uppercase())))).collect();
    for item in split_commas(&t[s..e]) {
        let Some(eq) = item.iter().position(|x| sym(x, "=")) else { continue };
        let lhs = &item[..eq];
        let rhs = &item[eq + 1..];
        if sym(&lhs[0], "(") {
            // (a, b) = (SELECT x, y FROM …) — 그 위치의 항목만 남긴 스칼라 서브쿼리로
            let names: Vec<String> = split_commas(&lhs[1..lhs.len().saturating_sub(1)]).iter().filter_map(|c| set_target(c)).collect();
            let Some(p) = names.iter().position(|n| n == col) else { continue };
            let Some(sel) = parse_select(rhs) else { continue };
            let Some((it, _)) = sel.items.get(p) else { continue };
            let mut sub = String::from("(");
            if !sel.with.is_empty() {
                sub.push_str(&sql_text(&sel.with));
                sub.push(' ');
            }
            sub.push_str(&format!("SELECT {} FROM {}", sql_text(it), sql_text(&sel.from)));
            if !sel.conds.is_empty() {
                sub.push_str(&format!(" WHERE {}", sel.conds.iter().map(|c| sql_text(c)).collect::<Vec<_>>().join(" AND ")));
            }
            for x in [&sel.hier, &sel.group] {
                if !x.is_empty() {
                    sub.push(' ');
                    sub.push_str(&sql_text(x));
                }
            }
            sub.push(')');
            return vec![Path { label: "UPDATE … SET (…) = (SELECT …)".into(), expr: toks(&sub), keys, src: Some(src), key_vars: vec![], notes }];
        }
        if set_target(lhs).as_deref() == Some(col) {
            let expr = rhs.to_vec();
            if uses_name(&expr, col) || uses_name(&expr, &alias) {
                notes.push("이전 값을 써서 계산합니다 (예: 열 = 열 + 1) — 지금 값으로 다시 계산하면 다릅니다".into());
            }
            return vec![Path { label: "UPDATE … SET".into(), expr, keys, src: Some(src), key_vars: vec![], notes }];
        }
    }
    vec![]
}

// ── 다시 계산하는 SELECT ────────────────────────────

/// 만든 쿼리들. 바인드는 이름(앞의 ':' 없이) → 값
#[derive(Debug, Clone, Serialize)]
pub struct Queries {
    /// 이 행의 키로 다시 계산한 값 — 열 V__ (키가 있으면 K1__ …)
    pub value: String,
    /// 값을 만든 원본 행 (원본 키와 쓰인 열) — 열 이름은 `cols` 순서
    pub rows: Option<String>,
    pub row_cols: Vec<RowCol>,
    /// 0건일 때: 조인은 두고 거르는 조건마다 O/X
    pub flags: Option<String>,
    pub flag_conds: Vec<String>,
    pub binds: Vec<(String, String)>,
    /// 값을 모르는 변수 때문에 뺀 조건
    pub dropped: Vec<String>,
}

/// 원본 행 쿼리의 열 하나가 무엇인지
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct RowCol {
    /// I(쓰인 열) · P(그 테이블의 키 열)
    pub kind: &'static str,
    pub alias: String,
    pub table: String,
    pub column: String,
}

fn bind_name(v: &str) -> String {
    let clean: String = v.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '_').collect();
    format!("V_{}", clean.chars().take(24).collect::<String>())
}

/// 변수를 바인드로 바꾼다 (`.` 앞뒤·함수 호출은 빼고)
fn subst(t: &[Tok], vars: &[(String, String)]) -> Vec<Tok> {
    let mut out = Vec::new();
    for (i, x) in t.iter().enumerate() {
        if let Some(n) = name(x) {
            let dotted = (i > 0 && sym(&t[i - 1], ".")) || t.get(i + 1).is_some_and(|y| sym(y, ".") || sym(y, "("));
            // 이미 바인드(:X)인 것도 이름이 같으면 그 값으로
            let is_bind = i > 0 && sym(&t[i - 1], ":");
            if !dotted {
                if let Some((v, _)) = vars.iter().find(|(v, _)| v.eq_ignore_ascii_case(&n)) {
                    if !is_bind {
                        out.push(Tok { kind: Kind::Sym(":"), line: 0 });
                    }
                    out.push(Tok { kind: Kind::Word(bind_name(v)), line: 0 });
                    continue;
                }
            }
        }
        out.push(x.clone());
    }
    out
}

/// 조건이 두 개 이상의 별칭을 잇거나 (+) 가 있으면 조인 조건
fn is_join(c: &[Tok], tables: &[(String, String)]) -> bool {
    if c.windows(3).any(|w| sym(&w[0], "(") && sym(&w[1], "+") && sym(&w[2], ")")) {
        return true;
    }
    let mut seen: Vec<&str> = Vec::new();
    for (i, x) in c.iter().enumerate() {
        if let Some(n) = x.word() {
            if c.get(i + 1).is_some_and(|y| sym(y, ".")) && !(i > 0 && sym(&c[i - 1], ".")) {
                if let Some((a, _)) = tables.iter().find(|(a, _)| a == n) {
                    if !seen.contains(&a.as_str()) {
                        seen.push(a);
                    }
                }
            }
        }
    }
    seen.len() >= 2
}

/// `key_vals`: (대상 키 열, 값). `vars`: 값을 아는 변수 (이름, 값) — 키 변수는 여기서 더한다.
/// `unknown`: 값을 모르는 변수 — 그 변수를 쓰는 조건은 빼고(넓게) 본다. 식에 쓰이면 Err.
/// `pks`: 원본 테이블(별칭)마다 키 열 — 원본 행을 다음 단계 키로 쓰려고
pub fn build(p: &Path, key_vals: &[(String, String)], vars: &[(String, String)], unknown: &[String], inputs: &[Input], pks: &[(String, Vec<String>)]) -> Result<Queries, String> {
    let Some(src) = &p.src else { return Err(p.notes.first().cloned().unwrap_or_else(|| "다시 계산할 원본 쿼리가 없습니다".into())) };
    let mut vars: Vec<(String, String)> = vars.to_vec();
    for (v, kc) in &p.key_vars {
        if let Some((_, val)) = key_vals.iter().find(|(k, _)| k.eq_ignore_ascii_case(kc)) {
            if !vars.iter().any(|(x, _)| x.eq_ignore_ascii_case(v)) {
                vars.push((v.clone(), val.clone()));
            }
        }
    }
    if let Some(u) = unknown.iter().find(|u| uses_name(&p.expr, u)) {
        return Err(format!("식이 값을 모르는 변수 {u} 를 씁니다 — 값을 넣고 다시 돌리세요"));
    }
    let j = |t: &[Tok]| sql_text(&subst(t, &vars));
    let mut dropped = Vec::new();
    let conds: Vec<&Vec<Tok>> = src
        .conds
        .iter()
        .filter(|c| {
            let drop = unknown.iter().any(|u| uses_name(c, u));
            if drop {
                dropped.push(sql_text(c));
            }
            !drop
        })
        .collect();
    let with = if src.with.is_empty() { String::new() } else { format!("{} ", j(&src.with)) };
    let from = j(&src.from);
    let where_of = |cs: &[&Vec<Tok>]| if cs.is_empty() { String::new() } else { format!(" WHERE {}", cs.iter().map(|c| format!("({})", j(c))).collect::<Vec<_>>().join(" AND ")) };
    let hier = if src.hier.is_empty() { String::new() } else { format!(" {}", j(&src.hier)) };
    let group = if src.group.is_empty() { String::new() } else { format!(" {}", j(&src.group)) };
    // 키 필터 (바깥에서): K1__ = :K1
    let mut binds: Vec<(String, String)> = Vec::new();
    let mut kcols = String::new();
    let mut kfilter: Vec<String> = Vec::new();
    for (n, (kc, ke)) in p.keys.iter().enumerate() {
        let Some((_, val)) = key_vals.iter().find(|(k, _)| k.eq_ignore_ascii_case(kc)) else { continue };
        kcols.push_str(&format!(", {} AS K{}__", j(ke), n + 1));
        kfilter.push(format!("K{}__ = :K{}", n + 1, n + 1));
        binds.push((format!("K{}", n + 1), val.clone()));
    }
    for (v, val) in &vars {
        let b = bind_name(v);
        let used = [&p.expr].into_iter().chain(src.conds.iter()).chain([&src.from, &src.with, &src.hier, &src.group]).any(|t| uses_name(t, v));
        if used && !binds.iter().any(|(x, _)| *x == b) {
            binds.push((b, val.clone()));
        }
    }
    let outer = |extra: &str, limit: u32| {
        let mut w: Vec<String> = kfilter.clone();
        w.push(format!("ROWNUM <= {limit}"));
        format!("{extra}WHERE {}", w.join(" AND "))
    };
    let value = format!("{with}SELECT * FROM (SELECT {} AS V__{kcols} FROM {from}{}{hier}{group}) {}", j(&p.expr), where_of(&conds), outer("", 5));
    // 원본 행: 집계 전 행 — 쓰인 열과 그 테이블의 키
    let mut row_cols: Vec<RowCol> = Vec::new();
    for inp in inputs {
        row_cols.push(RowCol { kind: "I", alias: inp.alias.clone(), table: inp.table.clone(), column: inp.column.clone() });
        if let Some((_, ks)) = pks.iter().find(|(a, _)| *a == inp.alias) {
            for k in ks {
                if !row_cols.iter().any(|r| r.alias == inp.alias && r.column == *k) {
                    row_cols.push(RowCol { kind: "P", alias: inp.alias.clone(), table: inp.table.clone(), column: k.clone() });
                }
            }
        }
    }
    let rows = (!row_cols.is_empty()).then(|| {
        let cols: Vec<String> = row_cols.iter().enumerate().map(|(n, r)| format!("{}.{} AS C{}__", r.alias, r.column, n + 1)).collect();
        format!("{with}SELECT * FROM (SELECT {}{kcols} FROM {from}{}{hier}) {}", cols.join(", "), where_of(&conds), outer("", 20))
    });
    // 조건 진단: 조인은 WHERE 에 두고, 거르는 조건은 CASE 로
    let (joins, filters): (Vec<&Vec<Tok>>, Vec<&Vec<Tok>>) = conds.iter().partition(|c| is_join(c, &src.tables));
    let flag_conds: Vec<String> = filters.iter().map(|c| sql_text(c)).collect();
    let flags = (!filters.is_empty()).then(|| {
        let fs: Vec<String> = filters.iter().enumerate().map(|(n, c)| format!("CASE WHEN {} THEN 'O' ELSE 'X' END AS F{}__", j(c), n + 1)).collect();
        format!("{with}SELECT * FROM (SELECT {}{kcols} FROM {from}{}) {}", fs.join(", "), where_of(&joins), outer("", 20))
    });
    Ok(Queries { value, rows, row_cols, flags, flag_conds, binds, dropped })
}

/// 같은 값인지 — 앞뒤 공백, 숫자 표기(1 · 1.0 · .5)를 맞춰 본다
pub fn same_value(a: Option<&str>, b: Option<&str>) -> bool {
    match (a.map(str::trim), b.map(str::trim)) {
        (None, None) => true,
        (Some(x), Some(y)) => x == y || matches!((x.parse::<f64>(), y.parse::<f64>()), (Ok(p), Ok(q)) if (p - q).abs() < 1e-9),
        (Some(""), None) | (None, Some("")) => true,
        _ => false,
    }
}

/// ORA-00904 에서 맨 이름(변수일 것) — `"V_DT": invalid identifier` → V_DT. `"R"."X"` 처럼 점이 있으면 None
pub fn unknown_identifier(err: &str) -> Option<String> {
    let i = err.find("ORA-00904")?;
    let rest = &err[i..];
    let a = rest.find('"')?;
    let b = rest[a + 1..].find('"')? + a + 1;
    let name = &rest[a + 1..b];
    (!rest[b + 1..].trim_start().starts_with('.')).then(|| name.to_string())
}

/// 저장된 본문이 잘렸으면(…) 소스 줄에서 그 문장을 다시 잘라 낸다 — `src` 는 line..end_line 줄을 이은 것
pub fn cut_statement(src: &str, kind: &str) -> Option<String> {
    let t = lex(src);
    let first = kind.split_whitespace().next().unwrap_or(kind).to_uppercase();
    let s = t.iter().position(|x| x.word() == Some(first.as_str()))?;
    let d = depths(&t);
    let e = (s..t.len()).find(|&j| d[j] == d[s] && sym(&t[j], ";")).unwrap_or(t.len());
    Some(sql_text(&t[s..e]))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn st(kind: &str, line: u32, text: &str) -> StmtLite {
        StmtLite { node: "P".into(), unit: "APP.PKG.PACKAGE_BODY".into(), line, end_line: line, kind: kind.into(), writes: vec![], text: text.into(), cursor: None, into: vec![], fed_by: vec![] }
    }

    fn keys(v: &[(&str, &str)]) -> Vec<(String, String)> {
        v.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect()
    }

    #[test]
    fn merge_using_subquery() {
        let s = st("MERGE", 212, "MERGE INTO ORD_SUM T USING (SELECT D.ORD_NO, SUM(D.AMT) AMT FROM ORD_DTL D WHERE D.STATUS = 'OK' GROUP BY D.ORD_NO) S ON (T.ORD_NO = S.ORD_NO) WHEN MATCHED THEN UPDATE SET T.TOT_AMT = NVL(S.AMT, 0), T.UPD_DT = SYSDATE WHEN NOT MATCHED THEN INSERT (ORD_NO, TOT_AMT) VALUES (S.ORD_NO, NVL(S.AMT, 0))");
        let ps = paths(&s, &[], &s.text, "APP.ORD_SUM", "tot_amt", &["ORD_NO".into()], &[]);
        assert_eq!(ps.len(), 2, "{ps:?}");
        assert_eq!(ps[0].label, "MERGE · 있으면 UPDATE");
        assert_eq!(ps[0].expr_text(), "NVL(S.AMT, 0)");
        assert_eq!(ps[1].expr_text(), "NVL(S.AMT, 0)");
        let q = build(&ps[0], &keys(&[("ORD_NO", "A123")]), &[], &[], &[], &[]).unwrap();
        assert!(q.value.starts_with("SELECT * FROM (SELECT NVL(S.AMT, 0) AS V__, T.ORD_NO AS K1__ FROM ORD_SUM T, (SELECT"), "{}", q.value);
        assert!(q.value.contains("WHERE (T.ORD_NO = S.ORD_NO)") && q.value.ends_with("WHERE K1__ = :K1 AND ROWNUM <= 5"), "{}", q.value);
        assert_eq!(q.binds, keys(&[("K1", "A123")]));
        // 원본 열: S 는 인라인 뷰라 테이블 입력이 아니다
        assert!(ps[0].inputs().0.is_empty());
    }

    #[test]
    fn insert_select_with_group_and_filters() {
        let s = st("INSERT", 10, "INSERT INTO ORD_SUM (ORD_NO, TOT_AMT) SELECT D.ORD_NO, SUM(D.AMT) FROM ORD_DTL D, ORD_HDR H WHERE H.ORD_NO = D.ORD_NO AND D.STATUS = 'OK' AND H.DT BETWEEN :A AND :B GROUP BY D.ORD_NO");
        let ps = paths(&s, &[], &s.text, "ORD_SUM", "TOT_AMT", &["ORD_NO".into()], &[]);
        assert_eq!(ps.len(), 1);
        let p = &ps[0];
        assert_eq!(p.expr_text(), "SUM(D.AMT)");
        let (inp, _) = p.inputs();
        assert_eq!(inp, vec![Input { alias: "D".into(), table: "ORD_DTL".into(), column: "AMT".into() }]);
        let q = build(p, &keys(&[("ORD_NO", "A123")]), &[], &[], &inp, &[("D".into(), vec!["ORD_NO".into(), "SEQ".into()])]).unwrap();
        assert!(q.value.contains("GROUP BY D.ORD_NO) WHERE K1__ = :K1"), "{}", q.value);
        // 원본 행은 GROUP BY 없이, 쓰인 열 + 키
        let rows = q.rows.unwrap();
        assert!(rows.contains("D.AMT AS C1__, D.ORD_NO AS C2__, D.SEQ AS C3__, D.ORD_NO AS K1__") && !rows.contains("GROUP BY"), "{rows}");
        // 조인(H.ORD_NO = D.ORD_NO)은 WHERE 에, 나머지는 O/X
        assert_eq!(q.flag_conds, vec!["D.STATUS = 'OK'", "H.DT BETWEEN :A AND :B"]);
        assert!(q.flags.unwrap().contains("WHERE (H.ORD_NO = D.ORD_NO)"));
    }

    #[test]
    fn values_from_cursor_record() {
        let cur = StmtLite { cursor: Some("C1".into()), ..st("CURSOR", 3, "SELECT A.ID, A.NM, A.AMT * 2 DBL FROM SRC A WHERE A.FLAG = P_FLAG") };
        let ins = StmtLite {
            fed_by: vec![crate::flow::Feed { cursor: "C1".into(), via: "record R".into() }],
            ..st("INSERT", 9, "INSERT INTO TGT (ID, NM, AMT2) VALUES (R.ID, R.NM, R.DBL)")
        };
        let ps = paths(&ins, &[cur.clone(), ins.clone()], &ins.text, "TGT", "AMT2", &["ID".into()], &[]);
        assert_eq!(ps.len(), 1, "{ps:?}");
        assert!(ps[0].label.contains("커서 C1"));
        assert_eq!(ps[0].expr_text(), "(A.AMT * 2)");
        assert_eq!(sql_text(&ps[0].keys[0].1), "(A.ID)");
        // 값을 모르는 커서 매개변수 P_FLAG 는 조건에서 뺀다
        let q = build(&ps[0], &keys(&[("ID", "7")]), &[], &["P_FLAG".into()], &[], &[]).unwrap();
        assert_eq!(q.dropped, vec!["A.FLAG = P_FLAG"]);
        assert!(!q.value.contains("P_FLAG"), "{}", q.value);
        // 값을 넣으면 바인드로
        let q = build(&ps[0], &keys(&[("ID", "7")]), &keys(&[("P_FLAG", "Y")]), &[], &[], &[]).unwrap();
        assert!(q.value.contains("A.FLAG = :V_P_FLAG") && q.binds.contains(&("V_P_FLAG".into(), "Y".into())), "{}", q.value);
    }

    #[test]
    fn values_from_select_into_with_key_variable() {
        let sel = StmtLite { into: vec!["V_AMT".into()], ..st("SELECT INTO", 5, "SELECT SUM(AMT) INTO V_AMT FROM ORD_DTL WHERE ORD_NO = P_ORD_NO") };
        let ins = st("INSERT", 8, "INSERT INTO ORD_SUM (ORD_NO, TOT_AMT) VALUES (P_ORD_NO, NVL(V_AMT, 0))");
        // NVL(V_AMT, 0) 는 맨 이름이 아니다 — 변수 그대로면 SELECT INTO 를 따라간다
        assert!(paths(&ins, std::slice::from_ref(&sel), &ins.text, "ORD_SUM", "TOT_AMT", &["ORD_NO".into()], &[])[0].src.is_none());
        let ins = st("INSERT", 8, "INSERT INTO ORD_SUM (ORD_NO, TOT_AMT) VALUES (P_ORD_NO, V_AMT)");
        let ps = paths(&ins, &[sel], &ins.text, "ORD_SUM", "TOT_AMT", &["ORD_NO".into()], &[]);
        assert!(ps[0].label.contains("변수 V_AMT"), "{:?}", ps[0].label);
        // 키 변수 P_ORD_NO = 이 행의 키 값
        let q = build(&ps[0], &keys(&[("ORD_NO", "A123")]), &[], &[], &[], &[]).unwrap();
        assert!(q.value.contains("ORD_NO = :V_P_ORD_NO") && q.binds.contains(&("V_P_ORD_NO".into(), "A123".into())), "{}", q.value);
    }

    #[test]
    fn update_set_and_tuple() {
        let s = st("UPDATE", 1, "UPDATE EMP E SET E.SAL = (SELECT AVG(SAL) FROM EMP X WHERE X.DEPTNO = E.DEPTNO), E.COMM = NULL WHERE E.JOB = 'CLERK'");
        let ps = paths(&s, &[], &s.text, "EMP", "SAL", &["EMPNO".into()], &[]);
        let q = build(&ps[0], &keys(&[("EMPNO", "7369")]), &[], &[], &[], &[]).unwrap();
        assert!(q.value.contains("AS V__, E.EMPNO AS K1__ FROM EMP E WHERE (E.JOB = 'CLERK')"), "{}", q.value);
        let s = st("UPDATE", 1, "UPDATE T SET (A, B) = (SELECT X, Y FROM S WHERE S.ID = T.ID) WHERE T.F = 1");
        let ps = paths(&s, &[], &s.text, "T", "B", &["ID".into()], &[]);
        assert_eq!(ps[0].expr_text(), "(SELECT Y FROM S WHERE S.ID = T.ID)");
        // 다른 테이블·다른 열은 없다
        assert!(paths(&s, &[], &s.text, "U", "B", &[], &[]).is_empty());
        assert!(paths(&s, &[], &s.text, "T", "Z", &[], &[]).is_empty());
    }

    #[test]
    fn insert_without_column_list_and_with_clause() {
        let s = st("INSERT", 1, "INSERT INTO T WITH W AS (SELECT ID, V FROM S) SELECT W.ID, W.V * 10 FROM W");
        let ps = paths(&s, &[], &s.text, "T", "AMT", &["ID".into()], &["ID".into(), "AMT".into()]);
        assert_eq!(ps[0].expr_text(), "W.V * 10");
        let q = build(&ps[0], &keys(&[("ID", "1")]), &[], &[], &[], &[]).unwrap();
        assert!(q.value.starts_with("WITH W AS (SELECT ID, V FROM S) SELECT * FROM (SELECT W.V * 10 AS V__"), "{}", q.value);
    }

    #[test]
    fn helpers() {
        assert!(same_value(Some("0"), Some("0.0")) && same_value(Some(" A"), Some("A")) && !same_value(Some("1"), None));
        assert_eq!(unknown_identifier("ORA-00904: \"V_BASE_DT\": invalid identifier").as_deref(), Some("V_BASE_DT"));
        assert_eq!(unknown_identifier("ORA-00904: \"R\".\"X\": invalid identifier"), None);
        assert_eq!(cut_statement("  x := 1; INSERT INTO t (a)\n VALUES (f(1, 2)); y := 2;", "INSERT").unwrap(), "INSERT INTO T(A) VALUES (F(1, 2))");
    }
}
