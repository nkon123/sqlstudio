//! Pro*C(.pc) 소스를 분석용 PL/SQL 모양으로 옮긴다.
//!
//! 파일 하나 = 패키지 본문 하나(`PC_<파일이름>`), C 함수 하나 = 프로시저 하나로 보고,
//! `EXEC SQL …;` 는 같은 줄 위치의 PL/SQL 문장으로 바꾼다. 그러면 지금의 파서·사실 뽑기·통합·흐름도가 그대로 돈다.
//!   · 호스트 변수 `:a:ind`, `:rec.f`, `:arr[i]` → 바인드 `:a`, `:rec_f`, `:arr`
//!   · `DECLARE c CURSOR FOR SELECT …` 는 기억해 두고, `OPEN c` 자리에서 `FOR r IN (SELECT …) LOOP … END LOOP` (그 함수가 읽는 것)
//!   · `EXEC SQL EXECUTE BEGIN … END; END-EXEC` → 그 PL/SQL 블록 그대로 (프로시저 호출 관계가 이어진다)
//!   · C 코드는 버린다. 다만 이 파일 안의 다른 C 함수를 부르는 곳은 호출로 남긴다 (main → load → 테이블)
//! 줄 번호는 원본과 같게 둔다 (여러 줄 문장은 첫 줄에 모으고 나머지 줄은 비운다).

use std::collections::{HashMap, HashSet};
use std::path::Path;

use crate::chunk::UnitSource;

/// Pro*C 소스 확장자
pub const EXTS: &[&str] = &["pc", "pcc", "pcpp"];

pub fn is_proc_file(p: &Path) -> bool {
    p.extension().and_then(|e| e.to_str()).is_some_and(|e| EXTS.contains(&e.to_ascii_lowercase().as_str()))
}

/// 파일 이름 → 단위 이름 (`order_load.pc` → `PC_ORDER_LOAD`)
pub fn unit_name(path: &Path) -> String {
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("X");
    let s: String = stem.chars().map(|c| if c.is_alphanumeric() || c == '_' { c.to_ascii_uppercase() } else { '_' }).collect();
    format!("PC_{s}")
}

pub struct Translated {
    pub unit: UnitSource,
    pub functions: usize,
    pub statements: usize,
}

/// `owner` 는 이 프로그램이 접속하는 계정 — 스키마 없이 쓴 테이블 이름이 이 스키마로 풀린다.
/// EXEC SQL 이 하나도 없으면 None
pub fn translate(text: &str, owner: &str, name: &str) -> Option<Translated> {
    let chars: Vec<char> = text.chars().collect();
    // 1) 함수 이름 먼저 (파일 안의 함수 호출을 알아보려고)
    let mut first = Scan::new(&chars, HashSet::new(), true);
    first.run();
    let fnset: HashSet<String> = first.names.iter().cloned().collect();
    // 2) 옮기기
    let mut s = Scan::new(&chars, fnset, false);
    s.run();
    if s.stmts == 0 {
        return None;
    }
    // 끝 줄바꿈은 걷어 END 를 마지막 줄에 붙인다 (줄 수를 원본과 같게)
    let body = format!("PACKAGE BODY {name} IS {} END {name};", s.out.trim_end_matches('\n'));
    Some(Translated { unit: UnitSource { owner: owner.to_uppercase(), name: name.to_string(), unit_type: "PACKAGE BODY".into(), text: body }, functions: s.names.len(), statements: s.stmts })
}

fn ident_start(c: char) -> bool {
    c.is_alphabetic() || c == '_'
}
fn ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$' || c == '#'
}

struct Scan<'a> {
    c: &'a [char],
    i: usize,
    fnset: HashSet<String>,
    collect: bool,
    out: String,
    depth: usize,
    in_fn: Option<String>,
    header: String,
    names: Vec<String>,
    cursors: HashMap<String, String>,
    prepared: HashMap<String, String>,
    stmts: usize,
    bol: bool,
    prev: (char, char),
}

impl<'a> Scan<'a> {
    fn new(c: &'a [char], fnset: HashSet<String>, collect: bool) -> Self {
        Self { c, i: 0, fnset, collect, out: String::new(), depth: 0, in_fn: None, header: String::new(), names: Vec::new(), cursors: HashMap::new(), prepared: HashMap::new(), stmts: 0, bol: true, prev: (' ', ' ') }
    }

    fn at(&self, i: usize) -> char {
        self.c.get(i).copied().unwrap_or('\0')
    }

    fn sig(&mut self, ch: char) {
        self.prev = (self.prev.1, ch);
    }

    fn run(&mut self) {
        let n = self.c.len();
        while self.i < n {
            let ch = self.c[self.i];
            if ch == '\n' {
                self.out.push('\n');
                if self.depth == 0 {
                    self.header.push(' ');
                }
                self.i += 1;
                self.bol = true;
                continue;
            }
            if ch.is_whitespace() {
                if self.depth == 0 {
                    self.header.push(' ');
                }
                self.i += 1;
                continue;
            }
            if self.bol && ch == '#' {
                // 전처리기 줄 (\ 로 이어지면 다음 줄까지)
                while self.i < n && self.c[self.i] != '\n' {
                    if self.c[self.i] == '\\' && self.at(self.i + 1) == '\n' {
                        self.out.push('\n');
                        self.i += 2;
                        continue;
                    }
                    self.i += 1;
                }
                continue;
            }
            self.bol = false;
            if ch == '/' && self.at(self.i + 1) == '*' {
                self.i += 2;
                while self.i < n && !(self.c[self.i] == '*' && self.at(self.i + 1) == '/') {
                    if self.c[self.i] == '\n' {
                        self.out.push('\n');
                    }
                    self.i += 1;
                }
                self.i = (self.i + 2).min(n);
                continue;
            }
            if ch == '/' && self.at(self.i + 1) == '/' {
                while self.i < n && self.c[self.i] != '\n' {
                    self.i += 1;
                }
                continue;
            }
            if ch == '"' || ch == '\'' {
                self.i += 1;
                while self.i < n && self.c[self.i] != ch {
                    if self.c[self.i] == '\\' {
                        self.i += 1;
                    }
                    if self.at(self.i) == '\n' {
                        self.out.push('\n');
                    }
                    self.i += 1;
                }
                self.i += 1;
                if self.depth == 0 {
                    self.header.push_str("\"\"");
                }
                self.sig('"');
                continue;
            }
            if ident_start(ch) {
                let s = self.i;
                while self.i < n && ident_char(self.c[self.i]) {
                    self.i += 1;
                }
                let word: String = self.c[s..self.i].iter().collect();
                if word.eq_ignore_ascii_case("EXEC") && self.exec(s) {
                    continue;
                }
                let up = word.to_uppercase();
                if self.in_fn.is_some() && !self.collect && self.fnset.contains(&up) && self.next_sig(self.i) == '(' && self.prev.1 != '.' && !(self.prev.1 == '>' && self.prev.0 == '-') {
                    self.out.push_str(&format!(" {up}; "));
                }
                if self.depth == 0 {
                    self.header.push_str(&word);
                }
                self.sig('a');
                continue;
            }
            match ch {
                '{' => {
                    if self.depth == 0 {
                        if let Some(name) = func_name(&self.header) {
                            if !self.collect {
                                self.out.push_str(&format!(" PROCEDURE {name} IS BEGIN "));
                            }
                            self.names.push(name.clone());
                            self.in_fn = Some(name);
                        }
                        self.header.clear();
                    }
                    self.depth += 1;
                }
                '}' => {
                    if self.depth > 0 {
                        self.depth -= 1;
                        if self.depth == 0 {
                            if let Some(name) = self.in_fn.take() {
                                if !self.collect {
                                    self.out.push_str(&format!(" END {name}; "));
                                }
                            }
                            self.header.clear();
                        }
                    }
                }
                ';' => {
                    if self.depth == 0 {
                        self.header.clear();
                    }
                }
                _ => {
                    if self.depth == 0 {
                        self.header.push(ch);
                    }
                }
            }
            self.sig(ch);
            self.i += 1;
        }
    }

    /// 공백(줄바꿈 포함)을 건너뛴 다음 글자
    fn next_sig(&self, mut j: usize) -> char {
        while j < self.c.len() && self.c[j].is_whitespace() {
            j += 1;
        }
        self.at(j)
    }

    fn word_at(&self, mut j: usize) -> (String, usize) {
        while j < self.c.len() && self.c[j].is_whitespace() {
            j += 1;
        }
        let s = j;
        while j < self.c.len() && (ident_char(self.c[j]) || self.c[j] == '-') {
            j += 1;
        }
        (self.c[s..j].iter().collect(), j)
    }

    /// `EXEC SQL …` / `EXEC ORACLE …` 한 덩어리. 아니면 false (그냥 낱말 EXEC)
    fn exec(&mut self, exec_start: usize) -> bool {
        let (w1, j1) = self.word_at(self.i);
        let is_sql = w1.eq_ignore_ascii_case("SQL");
        if !is_sql && !w1.eq_ignore_ascii_case("ORACLE") {
            return false;
        }
        let n = self.c.len();
        let mut start = j1;
        let mut end;
        let mut after;
        let (w2, j2) = self.word_at(j1);
        let (w3, _) = self.word_at(j2);
        if is_sql && w2.eq_ignore_ascii_case("EXECUTE") && (w3.eq_ignore_ascii_case("BEGIN") || w3.eq_ignore_ascii_case("DECLARE")) {
            // 블록: END-EXEC 까지 (안의 ; 는 블록의 것)
            start = j2;
            end = start;
            while end < n && !self.matches_ci(end, "END-EXEC") {
                end += 1;
            }
            after = (end + 8).min(n);
            let mut k = after;
            while k < n && self.c[k].is_whitespace() && self.c[k] != '\n' {
                k += 1;
            }
            if self.at(k) == ';' {
                after = k + 1;
            }
        } else {
            end = start;
            while end < n {
                let ch = self.c[end];
                if ch == '\'' || ch == '"' {
                    end += 1;
                    while end < n && self.c[end] != ch {
                        end += 1;
                    }
                } else if ch == '/' && self.at(end + 1) == '*' {
                    end += 2;
                    while end < n && !(self.c[end] == '*' && self.at(end + 1) == '/') {
                        end += 1;
                    }
                    end += 1;
                } else if ch == '-' && self.at(end + 1) == '-' {
                    while end < n && self.c[end] != '\n' {
                        end += 1;
                    }
                    continue;
                } else if ch == ';' || self.matches_ci(end, "END-EXEC") {
                    break;
                }
                end += 1;
            }
            let end = end.min(n);
            after = if self.at(end) == ';' { end + 1 } else { (end + 8).min(n) };
            let raw: String = self.c[start.min(end)..end].iter().collect();
            return self.finish(exec_start, after, is_sql, raw, false);
        }
        let end = end.min(n);
        let raw: String = self.c[start.min(end)..end].iter().collect();
        self.finish(exec_start, after, is_sql, raw, true)
    }

    fn finish(&mut self, exec_start: usize, after: usize, is_sql: bool, raw: String, block: bool) -> bool {
        let nl = self.c[exec_start..after.min(self.c.len())].iter().filter(|c| **c == '\n').count();
        if is_sql && !self.collect {
            let sql = clean_sql(&raw);
            if let Some(t) = self.translate(&sql, block) {
                if self.in_fn.is_some() {
                    self.out.push(' ');
                    self.out.push_str(&t);
                    self.out.push(' ');
                    self.stmts += 1;
                }
            }
        }
        for _ in 0..nl {
            self.out.push('\n');
        }
        self.bol = nl > 0;
        self.i = after;
        self.sig(';');
        true
    }

    fn matches_ci(&self, at: usize, pat: &str) -> bool {
        pat.chars().enumerate().all(|(k, p)| self.at(at + k).eq_ignore_ascii_case(&p))
    }

    /// EXEC SQL 문장 하나 → PL/SQL 문장 (없으면 None)
    fn translate(&mut self, sql: &str, block: bool) -> Option<String> {
        let mut s = sql.trim().to_string();
        if block {
            // 블록(BEGIN … END, 앞의 EXECUTE 는 이미 건너뛰었다)
            let body = hostvars(s.trim());
            return Some(if body.ends_with(';') { body } else { format!("{body};") });
        }
        // AT :db / AT name — 다른 접속 지정은 떼고 본다
        let (w, rest) = first_word(&s);
        if w.eq_ignore_ascii_case("AT") {
            let (_, r2) = first_word(rest);
            s = r2.trim().to_string();
        }
        // FOR :n / FOR 10 — 배열 처리 횟수
        let (w, rest) = first_word(&s);
        if w.eq_ignore_ascii_case("FOR") {
            let (cnt, r2) = split_token(rest);
            let (next, _) = first_word(r2);
            if (cnt.starts_with(':') || cnt.chars().all(|c| c.is_ascii_digit())) && matches!(next.to_uppercase().as_str(), "INSERT" | "UPDATE" | "DELETE" | "SELECT" | "MERGE") {
                s = r2.trim().to_string();
            }
        }
        let (w, rest) = first_word(&s);
        match w.to_uppercase().as_str() {
            "SELECT" | "INSERT" | "UPDATE" | "DELETE" | "MERGE" => Some(format!("{};", hostvars(&s))),
            "DECLARE" => {
                // DECLARE c CURSOR FOR select — 기억만 (OPEN 에서 쓴다)
                let (name, r2) = first_word(rest);
                let (kw, r3) = first_word(r2);
                if kw.eq_ignore_ascii_case("CURSOR") {
                    let (f, r4) = first_word(r3);
                    if f.eq_ignore_ascii_case("FOR") {
                        let q = r4.trim();
                        let (qw, _) = first_word(q);
                        if qw.eq_ignore_ascii_case("SELECT") || qw.eq_ignore_ascii_case("WITH") || q.starts_with('(') {
                            self.cursors.insert(name.to_uppercase(), hostvars(q));
                        }
                    }
                }
                None
            }
            "OPEN" => {
                let (name, _) = first_word(rest);
                self.cursors.get(&name.to_uppercase()).map(|q| format!("FOR r_{} IN ({q}) LOOP NULL; END LOOP;", name.to_uppercase()))
            }
            "FETCH" | "CLOSE" | "SAVEPOINT" => Some("NULL;".into()),
            "COMMIT" => Some("COMMIT;".into()),
            "ROLLBACK" => {
                // ROLLBACK TO SAVEPOINT 는 트랜잭션 끝이 아니다
                if rest.trim_start().to_uppercase().contains("SAVEPOINT") { Some("NULL;".into()) } else { Some("ROLLBACK;".into()) }
            }
            "EXECUTE" => {
                let (kw, r2) = first_word(rest);
                if kw.eq_ignore_ascii_case("IMMEDIATE") {
                    Some(format!("EXECUTE IMMEDIATE {};", dyn_text(&hostvars(r2.trim()))))
                } else if let Some(t) = self.prepared.get(&kw.to_uppercase()) {
                    Some(format!("EXECUTE IMMEDIATE {t};"))
                } else {
                    Some("EXECUTE IMMEDIATE :stmt;".into())
                }
            }
            "PREPARE" => {
                let (name, r2) = first_word(rest);
                let (from, r3) = first_word(r2);
                if from.eq_ignore_ascii_case("FROM") {
                    self.prepared.insert(name.to_uppercase(), dyn_text(&hostvars(r3.trim())));
                }
                None
            }
            "CALL" => Some(format!("{};", hostvars(rest.trim()))),
            "TRUNCATE" | "DROP" | "ALTER" | "CREATE" | "LOCK" => Some(format!("EXECUTE IMMEDIATE '{}';", s.replace('\'', "''"))),
            _ => None,
        }
    }
}

/// `"문자열"` 이면 SQL 문자열로, 아니면(호스트 변수 `:stmt`) 그대로
fn dyn_text(s: &str) -> String {
    let t = s.trim();
    if t.starts_with('"') {
        let inner = t[1..].trim_end_matches('"');
        return format!("'{}'", inner.replace('\'', "''"));
    }
    t.to_string()
}

fn first_word(s: &str) -> (&str, &str) {
    let s = s.trim_start();
    let end = s.find(|c: char| c.is_whitespace() || c == '(' || c == ';').unwrap_or(s.len());
    (&s[..end], &s[end..])
}

fn split_token(s: &str) -> (&str, &str) {
    let s = s.trim_start();
    let end = s.find(char::is_whitespace).unwrap_or(s.len());
    (&s[..end], &s[end..])
}

/// 함수 머리(`int main(int argc, char **argv)`)에서 함수 이름
fn func_name(header: &str) -> Option<String> {
    let h = header.trim();
    if h.is_empty() || h.contains('=') {
        return None;
    }
    let first: String = h.chars().take_while(|c| ident_char(*c)).collect();
    if matches!(first.as_str(), "typedef" | "struct" | "union" | "enum" | "extern") && !h.contains('(') {
        return None;
    }
    let p = h.find('(')?;
    let before = h[..p].trim_end();
    let name: String = before.chars().rev().take_while(|c| ident_char(*c)).collect::<Vec<_>>().into_iter().rev().collect();
    if name.is_empty() || !ident_start(name.chars().next()?) {
        return None;
    }
    if matches!(name.as_str(), "if" | "while" | "for" | "switch" | "sizeof" | "return") {
        return None;
    }
    Some(name.to_uppercase())
}

/// 주석을 걷고 공백을 한 칸으로 (문자열은 그대로)
fn clean_sql(raw: &str) -> String {
    let c: Vec<char> = raw.chars().collect();
    let mut o = String::new();
    let mut i = 0;
    while i < c.len() {
        let ch = c[i];
        if ch == '\'' || ch == '"' {
            o.push(ch);
            i += 1;
            while i < c.len() {
                o.push(c[i]);
                if c[i] == ch {
                    if ch == '\'' && i + 1 < c.len() && c[i + 1] == '\'' {
                        i += 1;
                        o.push('\'');
                    } else {
                        break;
                    }
                }
                i += 1;
            }
            i += 1;
        } else if ch == '-' && c.get(i + 1) == Some(&'-') {
            while i < c.len() && c[i] != '\n' {
                i += 1;
            }
        } else if ch == '/' && c.get(i + 1) == Some(&'*') {
            i += 2;
            while i < c.len() && !(c[i] == '*' && c.get(i + 1) == Some(&'/')) {
                i += 1;
            }
            i += 2;
            o.push(' ');
        } else if ch.is_whitespace() {
            if !o.ends_with(' ') {
                o.push(' ');
            }
            i += 1;
        } else {
            o.push(ch);
            i += 1;
        }
    }
    o.trim().to_string()
}

/// 호스트 변수를 바인드로: `:a:ind` `:a INDICATOR :ind` → `:a`, `:rec.f` → `:rec_f`, `:arr[i]` → `:arr`
fn hostvars(s: &str) -> String {
    let c: Vec<char> = s.chars().collect();
    let mut o = String::new();
    let mut i = 0;
    let read_var = |mut j: usize| -> (String, usize) {
        let mut name = String::new();
        while j < c.len() && ident_char(c[j]) {
            name.push(c[j]);
            j += 1;
        }
        loop {
            if j + 1 < c.len() && c[j] == '.' && ident_start(c[j + 1]) {
                name.push('_');
                j += 1;
                while j < c.len() && ident_char(c[j]) {
                    name.push(c[j]);
                    j += 1;
                }
            } else if j < c.len() && c[j] == '[' {
                while j < c.len() && c[j] != ']' {
                    j += 1;
                }
                j += 1;
            } else {
                break;
            }
        }
        (name, j)
    };
    while i < c.len() {
        let ch = c[i];
        if ch == '\'' {
            o.push(ch);
            i += 1;
            while i < c.len() {
                o.push(c[i]);
                if c[i] == '\'' {
                    if i + 1 < c.len() && c[i + 1] == '\'' {
                        i += 1;
                        o.push('\'');
                    } else {
                        break;
                    }
                }
                i += 1;
            }
            i += 1;
        } else if ch == ':' && i + 1 < c.len() && ident_start(c[i + 1]) {
            let (name, mut j) = read_var(i + 1);
            // 지시자: 바로 붙은 :ind, 또는 INDICATOR :ind
            if j + 1 < c.len() && c[j] == ':' && ident_start(c[j + 1]) {
                j = read_var(j + 1).1;
            } else {
                let mut k = j;
                while k < c.len() && c[k] == ' ' {
                    k += 1;
                }
                let w: String = c[k..].iter().take(9).collect();
                if w.eq_ignore_ascii_case("INDICATOR") {
                    k += 9;
                    while k < c.len() && c[k] == ' ' {
                        k += 1;
                    }
                    if k + 1 < c.len() && c[k] == ':' && ident_start(c[k + 1]) {
                        j = read_var(k + 1).1;
                    }
                }
            }
            o.push(':');
            o.push_str(&name);
            i = j;
        } else {
            o.push(ch);
            i += 1;
        }
    }
    o
}

#[cfg(test)]
mod tests {
    use super::*;

    const PC: &str = r#"
#include <stdio.h>
EXEC SQL INCLUDE sqlca;
EXEC SQL BEGIN DECLARE SECTION;
  int  v_id;
  char v_name[32];
  short i_name;
EXEC SQL END DECLARE SECTION;

/* 주문 읽어서 이력으로 */
EXEC SQL DECLARE c_ord CURSOR FOR
    SELECT id, name, amount   -- 마감 전
      FROM orders o JOIN customers c ON c.id = o.cust_id
     WHERE o.status = 'OPEN';

static int load_orders(void)
{
    EXEC SQL OPEN c_ord;
    for (;;) {
        EXEC SQL FETCH c_ord INTO :v_id, :v_name:i_name, :v_amt;
        if (sqlca.sqlcode == 1403) break;
        EXEC SQL INSERT INTO order_hist (id, name, amount, ts)
                 VALUES (:v_id, :v_name INDICATOR :i_name, :v_amt, SYSDATE);
        EXEC SQL UPDATE orders SET status = 'DONE' WHERE id = :v_id;
        printf("EXEC SQL DELETE FROM never_in_string");
    }
    EXEC SQL CLOSE c_ord;
    EXEC SQL COMMIT WORK;
    return 0;
}

int main(int argc, char **argv)
{
    EXEC SQL WHENEVER SQLERROR GOTO err;
    EXEC SQL CONNECT :user IDENTIFIED BY :pw;
    load_orders();
    EXEC SQL EXECUTE
        BEGIN
            audit_pkg.log('load', :cnt);
        END;
    END-EXEC;
    EXEC SQL DELETE FROM order_tmp WHERE batch = :b;
    EXEC SQL EXECUTE IMMEDIATE "TRUNCATE TABLE tmp_x";
    EXEC SQL COMMIT WORK RELEASE;
    return 0;
err:
    EXEC SQL ROLLBACK WORK RELEASE;
    return 1;
}
"#;

    #[test]
    fn translates_proc_to_plsql_with_same_lines() {
        let t = translate(PC, "scott", "PC_LOAD").expect("EXEC SQL 이 있다");
        assert_eq!(t.functions, 2);
        let out = &t.unit.text;
        assert!(out.starts_with("PACKAGE BODY PC_LOAD IS "), "{out}");
        assert!(out.contains("PROCEDURE LOAD_ORDERS IS BEGIN"));
        assert!(out.contains("PROCEDURE MAIN IS BEGIN"));
        // 줄 수는 원본과 같다 (맨 앞 줄에 머리를 붙이므로)
        assert_eq!(out.lines().count(), PC.lines().count());
        // 호스트 변수·지시자
        assert!(out.contains("VALUES (:v_id, :v_name, :v_amt, SYSDATE)"), "{out}");
        // 문자열 안의 EXEC SQL 은 무시
        assert!(!out.to_uppercase().contains("NEVER_IN_STRING"));
        // 커서는 OPEN 자리에서 읽기로
        assert!(out.contains("FOR r_C_ORD IN (SELECT id, name, amount FROM orders o JOIN customers c"), "{out}");
        assert!(!out.contains("마감 전"));
        // 같은 파일 안의 함수 호출
        assert!(out.contains(" LOAD_ORDERS; "), "{out}");
        // PL/SQL 블록·동적 SQL
        assert!(out.contains("audit_pkg.log('load', :cnt);"), "{out}");
        assert!(out.contains("EXECUTE IMMEDIATE 'TRUNCATE TABLE tmp_x';"), "{out}");
    }

    #[test]
    fn facts_from_translated_proc() {
        let t = translate(PC, "SCOTT", "PC_LOAD").unwrap();
        let (st, toks) = crate::plsql::structure(&t.unit.text);
        assert_eq!(st.name, "PC_LOAD");
        let locals = crate::facts::local_names(&toks);
        let f = crate::facts::extract(&toks, &locals);
        let tab = |n: &str| f.tables.iter().find(|t| t.name == n).map(|t| t.ops.clone());
        assert_eq!(tab("ORDERS").as_deref().map(|o| o.contains('R') && o.contains('U')), Some(true), "{:?}", f.tables);
        assert_eq!(tab("CUSTOMERS").as_deref(), Some("R"));
        assert_eq!(tab("ORDER_HIST").as_deref(), Some("C"));
        assert_eq!(tab("ORDER_TMP").as_deref(), Some("D"));
        assert!(f.calls.iter().any(|c| c.name == "AUDIT_PKG.LOG"), "{:?}", f.calls);
    }

    #[test]
    fn no_exec_sql_is_none() {
        assert!(translate("int main(){return 0;}", "X", "PC_X").is_none());
    }

    #[test]
    fn hostvar_forms() {
        assert_eq!(hostvars("SELECT a INTO :x:ix, :rec.f, :arr[3] FROM t WHERE b = :y INDICATOR :iy AND c = 'a:b'"), "SELECT a INTO :x, :rec_f, :arr FROM t WHERE b = :y AND c = 'a:b'");
    }
}
