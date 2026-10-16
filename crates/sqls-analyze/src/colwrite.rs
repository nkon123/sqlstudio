//! 열 하나에 값을 넣는 문장 찾기 — "이 값은 어디서 오나".
//!
//! 저장된 SQL 문(statements.json)에서 그 테이블을 쓰는(C/U) 문장만 고른 뒤, 문장 본문에서 그 열이
//! INSERT 열 목록·UPDATE SET·MERGE 에 나오는지 본다. 모델은 쓰지 않는다.

use serde::{Deserialize, Serialize};

use crate::facts::TableUse;
use crate::plsql::{lex, Kind, Tok};

/// statements.json 한 줄에서 필요한 것만
#[derive(Debug, Clone, Deserialize)]
pub struct StmtLite {
    pub node: String,
    pub unit: String,
    pub line: u32,
    pub end_line: u32,
    pub kind: String,
    #[serde(default)]
    pub writes: Vec<TableUse>,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ColWrite {
    pub node: String,
    pub unit: String,
    pub line: u32,
    pub end_line: u32,
    /// INSERT / UPDATE / MERGE
    pub kind: String,
    /// set — 그 열에 값을 넣음 / all — 열 목록 없는 INSERT (모든 열) / maybe — 본문이 잘려 확인 못 함
    pub how: String,
    pub text: String,
}

/// 쓰는 테이블 이름이 찾는 테이블인지. 스키마가 없으면 단위의 스키마(키의 첫 마디)로 본다
fn same_table(written: &str, unit: &str, owner: Option<&str>, table: &str) -> bool {
    let base = written.split('@').next().unwrap_or(written);
    match base.rsplit_once('.') {
        Some((o, t)) => t == table && owner.is_none_or(|w| w == o),
        None => base == table && owner.is_none_or(|w| unit.split('.').next() == Some(w)),
    }
}

/// `table` 의 `column` 에 값을 넣는 문장들 (파일 순서 그대로)
pub fn column_writes(stmts: &[StmtLite], owner: Option<&str>, table: &str, column: &str) -> Vec<ColWrite> {
    let mut out = Vec::new();
    for s in stmts {
        if !s.writes.iter().any(|w| w.ops.contains(['C', 'U']) && same_table(&w.name, &s.unit, owner, table)) {
            continue;
        }
        let toks = lex(&s.text);
        let Some(how) = writes_column(&toks, owner, table, column, &s.unit) else {
            // 본문을 2000자에서 잘라 저장했다 — 잘린 뒤에 그 열이 있을 수 있다
            if s.text.ends_with('…') {
                out.push(row(s, "maybe"));
            }
            continue;
        };
        out.push(row(s, how));
    }
    out
}

fn row(s: &StmtLite, how: &str) -> ColWrite {
    ColWrite { node: s.node.clone(), unit: s.unit.clone(), line: s.line, end_line: s.end_line, kind: s.kind.clone(), how: how.into(), text: s.text.clone() }
}

fn is_sym(t: Option<&Tok>, s: &str) -> bool {
    matches!(t.map(|t| &t.kind), Some(Kind::Sym(x)) if *x == s)
}

fn name_of(t: &Tok) -> Option<String> {
    match &t.kind {
        Kind::Word(w) => Some(w.clone()),
        Kind::Quoted(w) => Some(w.clone()),
        _ => None,
    }
}

/// 괄호 깊이 (각 토큰 앞에서)
fn depths(t: &[Tok]) -> Vec<i32> {
    let mut d = 0;
    t.iter()
        .map(|x| {
            if is_sym(Some(x), ")") {
                d -= 1;
            }
            let here = d;
            if is_sym(Some(x), "(") {
                d += 1;
            }
            here
        })
        .collect()
}

/// i 에서 시작하는 [owner.]table[@link] — (이름, 다음 위치)
fn table_at(t: &[Tok], mut i: usize) -> Option<(String, usize)> {
    let mut name = name_of(t.get(i)?)?;
    i += 1;
    while is_sym(t.get(i), ".") {
        name.push('.');
        name.push_str(&name_of(t.get(i + 1)?)?);
        i += 2;
    }
    if is_sym(t.get(i), "@") {
        i += 2;
    }
    Some((name, i))
}

/// (a, b, c) 안의 열 이름 — 별칭.열 이면 열. i 는 '(' 위치. (이름들, ')' 다음)
fn column_list(t: &[Tok], i: usize, d: &[i32]) -> (Vec<String>, usize) {
    let base = d[i];
    let mut names = Vec::new();
    let mut last: Option<String> = None;
    let mut j = i + 1;
    while j < t.len() && !(is_sym(t.get(j), ")") && d[j] == base) {
        if d[j] == base + 1 && is_sym(t.get(j), ",") {
            names.extend(last.take());
        } else if let Some(n) = name_of(&t[j]) {
            last = Some(n);
        }
        j += 1;
    }
    names.extend(last);
    (names, j + 1)
}

/// 문장이 그 열에 값을 넣으면 how ("set" | "all")
fn writes_column(t: &[Tok], owner: Option<&str>, table: &str, column: &str, unit: &str) -> Option<&'static str> {
    let d = depths(t);
    let w = |i: usize| t.get(i).and_then(|x| x.word());
    let target = |name: &str| same_table(name, unit, owner, table);
    let mut i = 0;
    let mut merge_target = false;
    let mut update_target = false;
    while i < t.len() {
        match w(i) {
            // INSERT [ALL|FIRST] INTO t [alias] (cols) … / MERGE INTO t … — INTO 마다 본다
            Some("INTO") if matches!(w(i.wrapping_sub(1)), Some("INSERT" | "ALL" | "FIRST" | "MERGE" | "THEN" | "ELSE")) || is_sym(t.get(i.wrapping_sub(1)), ")") => {
                let is_merge = w(i.wrapping_sub(1)) == Some("MERGE");
                let Some((name, mut k)) = table_at(t, i + 1) else { i += 1; continue };
                if is_merge {
                    merge_target = target(&name);
                    i = k;
                    continue;
                }
                if !target(&name) {
                    i = k;
                    continue;
                }
                // 별칭
                if t.get(k).and_then(|x| x.word()).is_some_and(|a| !matches!(a, "VALUES" | "SELECT" | "WITH")) && !is_sym(t.get(k), "(") {
                    k += 1;
                }
                if is_sym(t.get(k), "(") && !matches!(w(k + 1), Some("SELECT" | "WITH")) {
                    let (cols, _) = column_list(t, k, &d);
                    if cols.iter().any(|c| c == column) {
                        return Some("set");
                    }
                } else {
                    return Some("all");
                }
                i = k;
            }
            // MERGE … WHEN NOT MATCHED THEN INSERT (cols)
            Some("INSERT") if merge_target && w(i.wrapping_sub(1)) == Some("THEN") => {
                if is_sym(t.get(i + 1), "(") {
                    let (cols, k) = column_list(t, i + 1, &d);
                    if cols.iter().any(|c| c == column) {
                        return Some("set");
                    }
                    i = k;
                } else {
                    return Some("all");
                }
            }
            // UPDATE t [alias] SET … / MERGE … THEN UPDATE SET …
            Some("UPDATE") => {
                if w(i.wrapping_sub(1)) == Some("THEN") {
                    update_target = merge_target;
                    i += 1;
                } else if let Some((name, k)) = table_at(t, i + 1) {
                    update_target = target(&name);
                    i = k;
                } else {
                    i += 1;
                }
            }
            Some("SET") if update_target => {
                let base = d[i];
                let mut j = i + 1;
                // 항목마다: a = …, t.a = …, (a, b) = (SELECT …)
                let mut start = true;
                while j < t.len() && d[j] >= base {
                    if d[j] == base && matches!(w(j), Some("WHERE" | "RETURNING" | "RETURN" | "LOG" | "WHEN" | "DELETE")) {
                        break;
                    }
                    if d[j] == base && is_sym(t.get(j), ",") {
                        start = true;
                        j += 1;
                        continue;
                    }
                    if start && d[j] == base {
                        start = false;
                        if is_sym(t.get(j), "(") {
                            let (cols, _) = column_list(t, j, &d);
                            if cols.iter().any(|c| c == column) {
                                return Some("set");
                            }
                        } else {
                            // 별칭.열 = …
                            let mut k = j;
                            let mut last = None;
                            while let Some(n) = t.get(k).and_then(name_of) {
                                last = Some(n);
                                if is_sym(t.get(k + 1), ".") {
                                    k += 2;
                                } else {
                                    break;
                                }
                            }
                            if last.as_deref() == Some(column) && is_sym(t.get(k + 1), "=") {
                                return Some("set");
                            }
                        }
                    }
                    j += 1;
                }
                update_target = false;
                i = j;
            }
            _ => i += 1,
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn st(unit: &str, table: &str, ops: &str, text: &str) -> StmtLite {
        StmtLite {
            node: format!("{unit}.P"),
            unit: unit.into(),
            line: 1,
            end_line: 1,
            kind: text.split_whitespace().next().unwrap_or("").to_uppercase(),
            writes: vec![TableUse { name: table.into(), ops: ops.into(), lines: vec![1] }],
            text: text.into(),
        }
    }

    fn hits(s: &[StmtLite], col: &str) -> Vec<(String, String)> {
        column_writes(s, Some("APP"), "CUST", col).into_iter().map(|w| (w.text, w.how)).collect()
    }

    #[test]
    fn insert_update_merge() {
        let s = vec![
            st("APP.PKG.PACKAGE_BODY", "CUST", "C", "INSERT INTO cust (id, grade, nm) VALUES (1, 'A', :nm)"),
            st("APP.PKG.PACKAGE_BODY", "CUST", "C", "INSERT INTO cust (id, nm) VALUES (1, :nm)"),
            st("APP.PKG.PACKAGE_BODY", "CUST", "C", "INSERT INTO app.cust c SELECT * FROM cust_tmp"),
            st("APP.PKG.PACKAGE_BODY", "CUST", "U", "UPDATE cust c SET c.grade = 'B', nm = 'x' WHERE id = 1"),
            st("APP.PKG.PACKAGE_BODY", "CUST", "U", "UPDATE cust SET nm = (SELECT grade FROM t) WHERE grade = 'A'"),
            st("APP.PKG.PACKAGE_BODY", "CUST", "U", "UPDATE cust SET (nm, grade) = (SELECT a, b FROM t) WHERE 1 = 1"),
            st("APP.PKG.PACKAGE_BODY", "CUST", "CU", "MERGE INTO cust c USING t ON (c.id = t.id) WHEN MATCHED THEN UPDATE SET c.grade = t.g WHEN NOT MATCHED THEN INSERT (id) VALUES (t.id)"),
            st("APP.PKG.PACKAGE_BODY", "CUST", "CU", "MERGE INTO cust c USING t ON (c.id = t.id) WHEN MATCHED THEN UPDATE SET c.nm = t.n WHEN NOT MATCHED THEN INSERT (id, grade) VALUES (t.id, 'Z')"),
            // 다른 스키마의 같은 이름, 지우기만
            st("ERP.PKG.PACKAGE_BODY", "CUST", "U", "UPDATE cust SET grade = 1"),
            st("APP.PKG.PACKAGE_BODY", "CUST", "D", "DELETE FROM cust WHERE grade = 'A'"),
        ];
        let got = hits(&s, "GRADE");
        let texts: Vec<&str> = got.iter().map(|(t, _)| t.as_str()).collect();
        assert_eq!(texts, vec![
            s[0].text.as_str(), s[2].text.as_str(), s[3].text.as_str(), s[5].text.as_str(), s[6].text.as_str(), s[7].text.as_str(),
        ]);
        assert_eq!(got[1].1, "all");
        assert!(got.iter().filter(|(_, h)| h == "set").count() == 5);
    }

    #[test]
    fn insert_all_and_qualified() {
        let s = vec![
            st("APP.X.PROCEDURE", "APP.CUST", "C", "INSERT ALL INTO log (a) VALUES (1) INTO app.cust (id, grade) VALUES (2, 'A') SELECT * FROM dual"),
            st("APP.X.PROCEDURE", "ERP.CUST", "C", "INSERT INTO erp.cust (id, grade) VALUES (2, 'A')"),
        ];
        assert_eq!(hits(&s, "GRADE").len(), 1);
    }
}
