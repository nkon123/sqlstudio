//! SQL 문 단위 기록과 커서 → DML 흐름 — 모델 없이 소스에서.
//!
//! - SQL 문마다 하나: 커서 선언, 커서 루프의 쿼리, OPEN … FOR, SELECT INTO, INSERT/UPDATE/DELETE/MERGE, 동적 SQL.
//!   읽는 테이블, 쓰는 테이블(연산), 받는 변수(INTO), SQL 본문.
//! - 커서마다 하나: 읽는 테이블, 쓰이는 곳, 그리고 **그 커서의 데이터가 들어가는 DML** (테이블·연산·줄·근거).
//!   근거(via):
//!     `record R`     — 커서 루프 변수의 필드(R.COL)를 DML 이 쓴다
//!     `variable V`   — FETCH/SELECT INTO 로 받은 변수(컬렉션 포함)를 DML 이 쓴다
//!     `CURRENT OF`   — UPDATE/DELETE … WHERE CURRENT OF c
//!     `loop body`    — 커서 루프 안의 DML 이지만 루프 변수를 직접 쓰지는 않는다 (약한 연결)
//!
//! 서브프로그램 전체(조각으로 나뉘기 전)를 한 번에 본다 — FETCH 는 1부, INSERT 는 2부에 있어도 이어진다.

use std::collections::{BTreeMap, HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::facts::{chain, extract, TableUse};
use crate::plsql::{join_tokens, Kind, Tok};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Feed {
    pub cursor: String,
    pub via: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SqlStmt {
    pub line: u32,
    pub end_line: u32,
    /// CURSOR / FOR LOOP / OPEN FOR / SELECT INTO / SELECT / INSERT / UPDATE / DELETE / MERGE / EXECUTE IMMEDIATE
    pub kind: String,
    /// 커서 선언·루프·OPEN FOR 면 그 커서 이름
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    /// 쓰는 테이블 (C/U/D)
    #[serde(default)]
    pub writes: Vec<TableUse>,
    /// 읽는 테이블
    #[serde(default)]
    pub reads: Vec<String>,
    /// INTO 로 받는 변수
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub into: Vec<String>,
    /// 이 DML 에 데이터를 대는 커서
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fed_by: Vec<Feed>,
    /// SQL 본문 (공백 정리, 최대 2000자)
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CursorFeed {
    pub table: String,
    pub ops: String,
    pub line: u32,
    pub via: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CursorInfo {
    pub name: String,
    /// declared (CURSOR c IS) / implicit_loop (FOR r IN (SELECT …)) / ref_cursor (OPEN c FOR) / select_into
    pub kind: String,
    /// 선언(또는 쿼리) 줄. 바깥(패키지 전역·바깥 서브프로그램)에서 선언된 것이면 0
    pub line: u32,
    pub reads: Vec<String>,
    /// OPEN / FOR / FETCH 하는 줄
    #[serde(default)]
    pub used_at: Vec<u32>,
    /// 이 커서의 데이터가 들어가는 DML
    #[serde(default)]
    pub feeds: Vec<CursorFeed>,
}

/// 바깥에서 선언된 커서 (패키지 전역, 바깥 서브프로그램) → 읽는 테이블
pub type KnownCursors = HashMap<String, Vec<String>>;

fn is_word(t: &[Tok], i: usize, w: &str) -> bool {
    t.get(i).is_some_and(|x| x.is(w))
}

/// `from` 부터 괄호 밖 `;` 까지 (그 `;` 인덱스, 없으면 끝)
fn stmt_end(t: &[Tok], from: usize) -> usize {
    let mut d = 0i32;
    let mut k = from;
    while k < t.len() {
        if t[k].sym("(") {
            d += 1;
        } else if t[k].sym(")") {
            d -= 1;
        } else if d <= 0 && t[k].sym(";") {
            return k;
        }
        k += 1;
    }
    t.len().saturating_sub(1)
}

/// `open` 이 `(` 일 때 맞는 `)` 인덱스
fn close_paren(t: &[Tok], open: usize) -> usize {
    let mut d = 0i32;
    for (k, tk) in t.iter().enumerate().skip(open) {
        if tk.sym("(") {
            d += 1;
        } else if tk.sym(")") {
            d -= 1;
            if d == 0 {
                return k;
            }
        }
    }
    t.len().saturating_sub(1)
}

/// `LOOP` 토큰(i)에 맞는 `END LOOP` 의 END 인덱스
fn loop_end(t: &[Tok], i: usize) -> usize {
    let mut d = 0i32;
    let mut k = i;
    while k < t.len() {
        if t[k].is("LOOP") && !(k > 0 && t[k - 1].is("END")) {
            d += 1;
        } else if t[k].is("END") && is_word(t, k + 1, "LOOP") {
            d -= 1;
            if d == 0 {
                return k;
            }
        }
        k += 1;
    }
    t.len().saturating_sub(1)
}

/// INTO a, b, c (BULK COLLECT 포함) — 변수 이름들과 다음 인덱스
fn into_list(t: &[Tok], mut i: usize) -> (Vec<String>, usize) {
    let mut out = Vec::new();
    loop {
        match chain(t, i) {
            Some((n, k, _)) => {
                out.push(n.split('.').next().unwrap_or("").to_string());
                i = k;
                // v_tab(i) 같은 꼴
                if t.get(i).is_some_and(|x| x.sym("(")) {
                    i = close_paren(t, i) + 1;
                }
            }
            None => {
                // :NEW.x 같은 바인드
                if t.get(i).is_some_and(|x| x.sym(":")) {
                    i += 1;
                    continue;
                }
                break;
            }
        }
        if t.get(i).is_some_and(|x| x.sym(",")) {
            i += 1;
            continue;
        }
        break;
    }
    out.dedup();
    (out, i)
}

fn text_of(t: &[Tok]) -> String {
    let s = join_tokens(t);
    if s.chars().count() > 2000 {
        format!("{}…", s.chars().take(2000).collect::<String>())
    } else {
        s
    }
}

fn split_rw(f: &crate::facts::Facts) -> (Vec<TableUse>, Vec<String>) {
    let mut writes = Vec::new();
    let mut reads = Vec::new();
    for t in &f.tables {
        let w: String = t.ops.chars().filter(|c| matches!(c, 'C' | 'U' | 'D')).collect();
        if !w.is_empty() {
            writes.push(TableUse { name: t.name.clone(), ops: w, lines: t.lines.clone() });
        }
        if t.ops.contains('R') {
            reads.push(t.name.clone());
        }
    }
    (writes, reads)
}

struct Loop {
    var: String,
    cursor: String,
    /// 본문 토큰 범위 [start, end)
    start: usize,
    end: usize,
}

/// 서브프로그램(또는 전역 선언) 토큰에서 SQL 문과 커서 흐름을 뽑는다.
pub fn analyze(t: &[Tok], locals: &HashSet<String>, known: &KnownCursors) -> (Vec<SqlStmt>, Vec<CursorInfo>) {
    let mut stmts: Vec<SqlStmt> = Vec::new();
    // DML 문의 토큰 범위 (흐름을 붙일 때 다시 본다)
    let mut dml_ranges: Vec<(usize, usize, usize)> = Vec::new(); // (stmts 인덱스, 시작, 끝)
    let mut cursors: BTreeMap<String, CursorInfo> = BTreeMap::new();
    let mut loops: Vec<Loop> = Vec::new();
    // 변수 → (커서, 묶인 토큰 위치)
    let mut binds: Vec<(String, String, usize)> = Vec::new();

    let cursor_entry = |cursors: &mut BTreeMap<String, CursorInfo>, name: &str, kind: &str, line: u32, reads: Vec<String>| {
        cursors.entry(name.to_string()).or_insert_with(|| CursorInfo { name: name.to_string(), kind: kind.into(), line, reads, used_at: Vec::new(), feeds: Vec::new() });
    };
    let use_cursor = |cursors: &mut BTreeMap<String, CursorInfo>, name: &str, line: u32| {
        if !cursors.contains_key(name) {
            // 바깥에서 선언된 커서 (또는 모르는 커서 — 매개변수로 받은 ref cursor 등)
            let reads = known.get(name).cloned().unwrap_or_default();
            let kind = if known.contains_key(name) { "declared" } else { "ref_cursor" };
            cursors.insert(name.to_string(), CursorInfo { name: name.into(), kind: kind.into(), line: 0, reads, used_at: Vec::new(), feeds: Vec::new() });
        }
        let c = cursors.get_mut(name).unwrap();
        if !c.used_at.contains(&line) {
            c.used_at.push(line);
        }
    };

    let mut i = 0;
    while i < t.len() {
        let tk = &t[i];
        let prev_dot = i > 0 && t[i - 1].sym(".");
        let Some(w) = tk.word() else {
            i += 1;
            continue;
        };
        if prev_dot {
            i += 1;
            continue;
        }
        match w {
            // CURSOR c [(params)] [RETURN t] IS SELECT … ;
            "CURSOR" => {
                let Some(name) = t.get(i + 1).and_then(|x| x.name()) else {
                    i += 1;
                    continue;
                };
                let end = stmt_end(t, i);
                // IS 다음부터가 쿼리 (괄호 밖의 첫 IS)
                let mut q = i + 2;
                let mut d = 0;
                while q < end {
                    if t[q].sym("(") {
                        d += 1;
                    } else if t[q].sym(")") {
                        d -= 1;
                    } else if d == 0 && t[q].is("IS") {
                        break;
                    }
                    q += 1;
                }
                if q >= end {
                    // 커서 형식 선언 (TYPE … REF CURSOR) 이나 전방 선언
                    i = end + 1;
                    continue;
                }
                let body = &t[q + 1..end];
                let f = extract(body, locals);
                let (_, reads) = split_rw(&f);
                cursor_entry(&mut cursors, &name, "declared", tk.line, reads.clone());
                stmts.push(SqlStmt { line: tk.line, end_line: t[end].line, kind: "CURSOR".into(), cursor: Some(name), writes: vec![], reads, into: vec![], fed_by: vec![], text: text_of(body) });
                i = end + 1;
                continue;
            }
            // FOR r IN c [(args)] LOOP / FOR r IN (SELECT …) LOOP / 숫자 루프 (건너뛴다)
            "FOR" if is_word(t, i + 2, "IN") && !(i > 0 && (t[i - 1].is("OPEN") || t[i - 1].is("SELECT"))) => {
                let var = t.get(i + 1).and_then(|x| x.name()).unwrap_or_default();
                let mut j = i + 3;
                if is_word(t, j, "REVERSE") {
                    i = j;
                    continue;
                }
                if t.get(j).is_some_and(|x| x.sym("(")) && t.get(j + 1).is_some_and(|x| x.is("SELECT") || x.is("WITH")) {
                    let close = close_paren(t, j);
                    let body = &t[j + 1..close];
                    let f = extract(body, locals);
                    let (_, reads) = split_rw(&f);
                    let name = format!("{var}@{}", tk.line);
                    cursor_entry(&mut cursors, &name, "implicit_loop", t[j + 1].line, reads.clone());
                    use_cursor(&mut cursors, &name, tk.line);
                    stmts.push(SqlStmt { line: t[j + 1].line, end_line: t[close].line, kind: "FOR LOOP".into(), cursor: Some(name.clone()), writes: vec![], reads, into: vec![var.clone()], fed_by: vec![], text: text_of(body) });
                    let lp = close + 1;
                    if is_word(t, lp, "LOOP") {
                        loops.push(Loop { var, cursor: name, start: lp + 1, end: loop_end(t, lp) });
                    }
                    i = close + 1;
                    continue;
                }
                // 이름 [(인자)] LOOP — 사이에 .. 가 있으면 숫자 루프
                let mut k = j;
                let mut range = false;
                while k < t.len() && !t[k].is("LOOP") && !t[k].sym(";") {
                    if t[k].sym("..") {
                        range = true;
                    }
                    k += 1;
                }
                if !range && k < t.len() && t[k].is("LOOP") {
                    if let Some((c, _, _)) = chain(t, j) {
                        use_cursor(&mut cursors, &c, tk.line);
                        loops.push(Loop { var, cursor: c, start: k + 1, end: loop_end(t, k) });
                    }
                }
                j = k;
                i = j.max(i + 1);
                continue;
            }
            "OPEN" => {
                let Some((c, k, _)) = chain(t, i + 1) else {
                    i += 1;
                    continue;
                };
                let mut k = k;
                if t.get(k).is_some_and(|x| x.sym("(")) {
                    k = close_paren(t, k) + 1;
                }
                if is_word(t, k, "FOR") {
                    let end = stmt_end(t, k);
                    let body = &t[k + 1..end];
                    let is_sql = body.first().is_some_and(|x| x.is("SELECT") || x.is("WITH"));
                    let f = if is_sql { extract(body, locals) } else { dynamic_facts(body, locals) };
                    let (_, reads) = split_rw(&f);
                    cursors.insert(c.clone(), CursorInfo { name: c.clone(), kind: "ref_cursor".into(), line: tk.line, reads: reads.clone(), used_at: vec![], feeds: vec![] });
                    use_cursor(&mut cursors, &c, tk.line);
                    stmts.push(SqlStmt {
                        line: tk.line,
                        end_line: t[end].line,
                        kind: if is_sql { "OPEN FOR".into() } else { "OPEN FOR (동적)".into() },
                        cursor: Some(c),
                        writes: vec![],
                        reads,
                        into: vec![],
                        fed_by: vec![],
                        text: text_of(&t[i..end]),
                    });
                    i = end + 1;
                } else {
                    use_cursor(&mut cursors, &c, tk.line);
                    i = k;
                }
                continue;
            }
            // FETCH c [BULK COLLECT] INTO a, b [LIMIT n];
            "FETCH" => {
                if let Some((c, mut k, _)) = chain(t, i + 1) {
                    while k < t.len() && !t[k].is("INTO") && !t[k].sym(";") {
                        k += 1;
                    }
                    if is_word(t, k, "INTO") {
                        let (vars, _) = into_list(t, k + 1);
                        use_cursor(&mut cursors, &c, tk.line);
                        for v in vars {
                            binds.push((v, c.clone(), i));
                        }
                    }
                }
                i = stmt_end(t, i) + 1;
                continue;
            }
            "SELECT" | "WITH" => {
                let end = stmt_end(t, i);
                let body = &t[i..end];
                let f = extract(body, locals);
                let (_, reads) = split_rw(&f);
                // 괄호 밖의 INTO
                let mut into = Vec::new();
                let mut d = 0;
                for k in i..end {
                    if t[k].sym("(") {
                        d += 1;
                    } else if t[k].sym(")") {
                        d -= 1;
                    } else if d == 0 && t[k].is("INTO") {
                        into = into_list(t, k + 1).0;
                        break;
                    }
                }
                let kind = if into.is_empty() { "SELECT" } else { "SELECT INTO" };
                let mut cursor = None;
                if !into.is_empty() {
                    let name = format!("SELECT@{}", tk.line);
                    cursor_entry(&mut cursors, &name, "select_into", tk.line, reads.clone());
                    for v in &into {
                        binds.push((v.clone(), name.clone(), i));
                    }
                    cursor = Some(name);
                }
                stmts.push(SqlStmt { line: tk.line, end_line: t[end].line, kind: kind.into(), cursor, writes: vec![], reads, into, fed_by: vec![], text: text_of(body) });
                i = end + 1;
                continue;
            }
            "INSERT" | "UPDATE" | "DELETE" | "MERGE"
                if !(w == "UPDATE" && i > 0 && t[i - 1].is("FOR"))
                    && !(w == "DELETE" && t.get(i + 1).is_some_and(|x| x.sym("(") || x.sym(";")))
                    && !(w == "UPDATE" && is_word(t, i + 1, "SET")) =>
            {
                let end = stmt_end(t, i);
                let body = &t[i..end];
                let f = extract(body, locals);
                let (writes, reads) = split_rw(&f);
                let mut into = Vec::new();
                // RETURNING … INTO v
                for k in i..end {
                    if t[k].is("RETURNING") {
                        let mut q = k;
                        while q < end && !t[q].is("INTO") {
                            q += 1;
                        }
                        if q < end {
                            into = into_list(t, q + 1).0;
                        }
                        break;
                    }
                }
                dml_ranges.push((stmts.len(), i, end));
                stmts.push(SqlStmt { line: tk.line, end_line: t[end].line, kind: w.into(), cursor: None, writes, reads, into, fed_by: vec![], text: text_of(body) });
                i = end + 1;
                continue;
            }
            "EXECUTE" if is_word(t, i + 1, "IMMEDIATE") => {
                let end = stmt_end(t, i);
                let body = &t[i + 2..end];
                let f = dynamic_facts(body, locals);
                let (writes, reads) = split_rw(&f);
                let idx = stmts.len();
                if !writes.is_empty() {
                    dml_ranges.push((idx, i, end));
                }
                stmts.push(SqlStmt { line: tk.line, end_line: t[end].line, kind: "EXECUTE IMMEDIATE".into(), cursor: None, writes, reads, into: vec![], fed_by: vec![], text: text_of(&t[i..end]) });
                i = end + 1;
                continue;
            }
            _ => {}
        }
        i += 1;
    }

    // DML ← 커서
    for &(si, a, b) in &dml_ranges {
        let mut feeds: Vec<Feed> = Vec::new();
        let body = &t[a..b];
        for (k, x) in body.iter().enumerate() {
            let Some(n) = x.word() else { continue };
            let after_dot = body.get(k + 1).is_some_and(|y| y.sym("."));
            let before_dot = k > 0 && body[k - 1].sym(".");
            if before_dot {
                continue;
            }
            // 루프 변수의 필드
            if after_dot {
                for l in loops.iter().filter(|l| l.start <= a && a < l.end && l.var == n) {
                    add_feed(&mut feeds, &l.cursor, format!("record {n}"));
                }
            }
            // FETCH / SELECT INTO 로 받은 변수 (이 문장보다 앞에서 묶인 것 중 마지막)
            if let Some((_, c, _)) = binds.iter().filter(|(v, _, at)| v == n && *at < a).last() {
                add_feed(&mut feeds, c, format!("variable {n}"));
            }
            // WHERE CURRENT OF c
            if n == "CURRENT" && body.get(k + 1).is_some_and(|y| y.is("OF")) {
                if let Some((c, _, _)) = chain(body, k + 2) {
                    add_feed(&mut feeds, &c, "CURRENT OF".into());
                }
            }
        }
        // 루프 안이지만 직접 쓰지 않는 것 (가장 안쪽 루프만)
        if let Some(l) = loops.iter().filter(|l| l.start <= a && a < l.end).min_by_key(|l| l.end - l.start) {
            if !feeds.iter().any(|f| f.cursor == l.cursor) {
                add_feed(&mut feeds, &l.cursor, "loop body".into());
            }
        }
        let line = t[a].line;
        let st = &mut stmts[si];
        for f in &feeds {
            if let Some(c) = cursors.get_mut(&f.cursor) {
                for w in &st.writes {
                    c.feeds.push(CursorFeed { table: w.name.clone(), ops: w.ops.clone(), line, via: f.via.clone() });
                }
            }
        }
        st.fed_by = feeds;
    }

    // 쓰이지 않은 SELECT INTO 커서는 흐름이 없으면 목록에서 뺀다 (SELECT 문 기록은 남는다)
    let cursors: Vec<CursorInfo> = cursors.into_values().filter(|c| c.kind != "select_into" || !c.feeds.is_empty()).collect();
    (stmts, cursors)
}

fn add_feed(feeds: &mut Vec<Feed>, cursor: &str, via: String) {
    if !feeds.iter().any(|f| f.cursor == cursor && f.via == via) {
        feeds.push(Feed { cursor: cursor.to_string(), via });
    }
}

/// EXECUTE IMMEDIATE / OPEN FOR 의 문자열 상수를 SQL 로 읽어 본다 ('INSERT INTO ' || v 같은 것은 앞부분만)
fn dynamic_facts(body: &[Tok], locals: &HashSet<String>) -> crate::facts::Facts {
    let mut text = String::new();
    for x in body {
        match &x.kind {
            Kind::Str(s) => {
                text.push_str(s);
                text.push(' ');
            }
            Kind::Sym("||") => {}
            Kind::Word(w) if w == "USING" || w == "INTO" || w == "BULK" => break,
            // 변수가 끼면 그 자리는 모른다
            _ => text.push_str(" __VAR__ "),
        }
    }
    let toks = crate::plsql::lex(&text);
    // 줄 번호는 문장 첫 줄로
    let line = body.first().map(|x| x.line).unwrap_or(1);
    let toks: Vec<Tok> = toks.into_iter().map(|mut x| {
        x.line = line;
        x
    }).collect();
    let mut f = extract(&toks, locals);
    f.tables.retain(|t| t.name != "__VAR__");
    f
}

/// 이 토큰들에서 선언된 커서 이름 → 읽는 테이블 (전역 선언, 바깥 서브프로그램 선언용)
pub fn declared_cursors(t: &[Tok], locals: &HashSet<String>) -> KnownCursors {
    let (_, cs) = analyze(t, locals, &KnownCursors::new());
    cs.into_iter().filter(|c| c.kind == "declared" && c.line > 0).map(|c| (c.name, c.reads)).collect()
}

// ─────────────────────────────────────────────────────────────
// 긴 SQL 을 자를 자리, SQL 의 구조 요약
// ─────────────────────────────────────────────────────────────

/// SQL 절을 여는 키워드 (이 앞에서 자를 수 있다)
const CLAUSES: &[&str] = &["SELECT", "FROM", "WHERE", "GROUP", "HAVING", "ORDER", "UNION", "INTERSECT", "MINUS", "CONNECT", "MODEL", "WINDOW", "VALUES", "SET", "USING", "WHEN"];
const JOINS: &[&str] = &["JOIN", "LEFT", "RIGHT", "INNER", "FULL", "CROSS", "NATURAL"];

/// SQL 절 경계의 자를 자리. (그 줄 다음에서 자른다, 깊이)
///
/// PL/SQL 문장 경계(깊이 1~9)를 먼저 쓰고, 한 SQL 문이 한도를 넘을 때만 쓰이도록 깊이를 10 부터 준다:
/// 괄호 깊이 p 에서 절·CTE·JOIN 은 `10 + 2p`, 쉼표(컬럼 목록)·AND/OR 는 `11 + 2p`.
/// 바깥 절부터 자르고, 한 절(긴 SELECT 목록, 긴 WHERE)이 크면 그 안의 쉼표·AND 에서, 서브쿼리가 크면 그 안의 절에서 자른다.
pub fn sql_cuts(t: &[Tok]) -> Vec<(u32, usize)> {
    let mut out = Vec::new();
    let mut p: usize = 0;
    let mut in_sql = false;
    // 괄호 깊이별: WITH 절 안인지
    let mut with_at: Vec<usize> = Vec::new();
    for k in 0..t.len() {
        let tk = &t[k];
        let first_on_line = k == 0 || t[k - 1].line < tk.line;
        let before = |d: usize| (tk.line.saturating_sub(1), d);
        if tk.sym("(") {
            p += 1;
            continue;
        }
        if tk.sym(")") {
            p = p.saturating_sub(1);
            with_at.retain(|&w| w <= p);
            continue;
        }
        if tk.sym(";") {
            p = 0;
            in_sql = false;
            with_at.clear();
            continue;
        }
        let prev_dot = k > 0 && t[k - 1].sym(".");
        let Some(w) = tk.word() else {
            if in_sql && tk.sym(",") {
                // CTE 사이의 쉼표: , 이름 AS (
                if with_at.contains(&p) && t.get(k + 1).and_then(|x| x.name()).is_some() && t.get(k + 2).is_some_and(|x| x.is("AS")) {
                    out.push((t[k + 1].line.saturating_sub(1), 10 + 2 * p));
                } else if first_on_line {
                    out.push(before(11 + 2 * p));
                } else {
                    out.push((tk.line, 11 + 2 * p));
                }
            }
            continue;
        };
        if prev_dot {
            continue;
        }
        match w {
            "SELECT" | "WITH" | "INSERT" | "UPDATE" | "DELETE" | "MERGE" => {
                if w == "WITH" {
                    with_at.push(p);
                }
                if in_sql && !(k > 0 && t[k - 1].sym("(")) {
                    out.push(before(10 + 2 * p));
                }
                in_sql = true;
            }
            _ if !in_sql => {}
            "AND" | "OR" => out.push(before(11 + 2 * p)),
            _ if CLAUSES.contains(&w) => {
                // ORDER BY 가 OVER( … ) 안이면 그 안의 깊이로 들어간다 (p 가 이미 크다)
                out.push(before(10 + 2 * p));
            }
            _ if JOINS.contains(&w) && !(k > 0 && t[k - 1].word().is_some_and(|x| JOINS.contains(&x))) => out.push(before(10 + 2 * p)),
            _ => {}
        }
    }
    out.retain(|&(l, _)| l > 0);
    out.sort();
    out.dedup();
    out
}

/// SQL 한 문장의 맨 바깥 절 구조 — "WITH ACTIVE_CUST 4~13 · SELECT 27~80 · FROM 81~84 …"
pub fn sql_outline(t: &[Tok]) -> Vec<(String, u32, u32)> {
    let mut marks: Vec<(String, u32)> = Vec::new();
    let mut p = 0usize;
    let mut base: Option<usize> = None;
    let mut with_level: Option<usize> = None;
    for k in 0..t.len() {
        let tk = &t[k];
        if tk.sym("(") {
            p += 1;
            continue;
        }
        if tk.sym(")") {
            p = p.saturating_sub(1);
            continue;
        }
        let Some(w) = tk.word() else {
            // , 이름 AS (  — 다음 CTE
            if tk.sym(",") && with_level == Some(p) {
                if let (Some(n), true) = (t.get(k + 1).and_then(|x| x.name()), t.get(k + 2).is_some_and(|x| x.is("AS"))) {
                    marks.push((format!("WITH {n}"), t[k + 1].line));
                }
            }
            continue;
        };
        if k > 0 && t[k - 1].sym(".") {
            continue;
        }
        if base.is_none() && matches!(w, "SELECT" | "WITH" | "INSERT" | "UPDATE" | "DELETE" | "MERGE") {
            base = Some(p);
        }
        if base != Some(p) {
            continue;
        }
        let label = match w {
            "WITH" => {
                with_level = Some(p);
                t.get(k + 1).and_then(|x| x.name()).map(|n| format!("WITH {n}"))
            }
            "SELECT" => {
                with_level = None;
                Some("SELECT".to_string())
            }
            "GROUP" | "ORDER" | "CONNECT" => Some(format!("{w} BY")),
            "FROM" | "WHERE" | "HAVING" | "UNION" | "INTERSECT" | "MINUS" | "INSERT" | "UPDATE" | "DELETE" | "MERGE" | "SET" | "VALUES" | "USING" => {
                Some(w.to_string())
            }
            _ => None,
        };
        if let Some(l) = label {
            if marks.last().is_none_or(|(x, _)| x != &l || l.starts_with("WITH")) {
                marks.push((l, tk.line));
            }
        }
    }
    let end = t.last().map(|x| x.line).unwrap_or(0);
    let mut out = Vec::new();
    for (i, (l, a)) in marks.iter().enumerate() {
        let b = marks.get(i + 1).map(|(_, n)| n.saturating_sub(1).max(*a)).unwrap_or(end);
        out.push((l.clone(), *a, b));
    }
    out
}

/// 줄 범위 [a, b] 가 걸치는 절들
pub fn outline_text(outline: &[(String, u32, u32)], a: u32, b: u32) -> (String, String) {
    let all: Vec<String> = outline.iter().map(|(l, x, y)| if x == y { format!("{l} {x}") } else { format!("{l} {x}~{y}") }).collect();
    let here: Vec<String> = outline.iter().filter(|(_, x, y)| *x <= b && *y >= a).map(|(l, _, _)| l.clone()).collect();
    (all.join(" · "), here.join(", "))
}
