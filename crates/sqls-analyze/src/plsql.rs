//! PL/SQL 소스 구조 — 렉서와 단위·서브프로그램 경계.
//!
//! 컴파일러가 아니다. 목표는 "작은 모델에 줄 수 있게 자르기" 와 "확실한 사실 뽑기" 에 필요한 만큼만
//! 정확한 것: 주석·문자열에 속지 않고, 서브프로그램과 블록(BEGIN/IF/LOOP/CASE … END)의 짝을 맞춘다.
//! 문법이 틀린 소스에서도 패닉하지 않고, 짝이 안 맞으면 남은 부분을 하나로 본다.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    /// 대문자로 바꾼 식별자·키워드
    Word(String),
    /// "따옴표 식별자" — 대소문자를 그대로 둔다
    Quoted(String),
    /// 문자열 상수 (따옴표 안의 내용)
    Str(String),
    Num(String),
    /// 기호. `:=`, `=>`, `||`, `..` 는 두 글자 그대로
    Sym(&'static str),
}

#[derive(Debug, Clone)]
pub struct Tok {
    pub kind: Kind,
    /// 1부터
    pub line: u32,
}

impl Tok {
    pub fn word(&self) -> Option<&str> {
        match &self.kind {
            Kind::Word(w) => Some(w),
            _ => None,
        }
    }
    pub fn is(&self, w: &str) -> bool {
        self.word() == Some(w)
    }
    pub fn sym(&self, s: &str) -> bool {
        matches!(&self.kind, Kind::Sym(x) if *x == s)
    }
    /// 이름으로 쓸 수 있는 토큰 (식별자 또는 따옴표 식별자)
    pub fn name(&self) -> Option<String> {
        match &self.kind {
            Kind::Word(w) => Some(w.clone()),
            Kind::Quoted(q) => Some(q.clone()),
            _ => None,
        }
    }
}

const SYMS2: &[&str] = &[":=", "=>", "||", "..", "<=", ">=", "<>", "!=", "**", "<<", ">>"];
const SYMS1: &[&str] = &[
    "(", ")", ",", ";", ".", "+", "-", "*", "/", "=", "<", ">", "%", "@", ":", "[", "]", "{", "}", "|", "&", "!", "?", "^", "~",
];

fn ident_start(c: char) -> bool {
    c.is_alphabetic() || c == '_'
}
fn ident_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | '$' | '#')
}

/// 토큰으로 나눈다. 주석은 버린다.
pub fn lex(src: &str) -> Vec<Tok> {
    let chars: Vec<char> = src.chars().collect();
    let n = chars.len();
    let mut out = Vec::with_capacity(n / 4);
    let mut i = 0;
    let mut line = 1u32;
    while i < n {
        let c = chars[i];
        if c == '\n' {
            line += 1;
            i += 1;
            continue;
        }
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        // -- 주석
        if c == '-' && i + 1 < n && chars[i + 1] == '-' {
            while i < n && chars[i] != '\n' {
                i += 1;
            }
            continue;
        }
        // /* */ 주석
        if c == '/' && i + 1 < n && chars[i + 1] == '*' {
            i += 2;
            while i < n && !(chars[i] == '*' && i + 1 < n && chars[i + 1] == '/') {
                if chars[i] == '\n' {
                    line += 1;
                }
                i += 1;
            }
            i = (i + 2).min(n);
            continue;
        }
        let start_line = line;
        // q'[...]' / nq'[...]'
        let q_at = if (c == 'q' || c == 'Q') && i + 2 < n && chars[i + 1] == '\'' {
            Some(i)
        } else if (c == 'n' || c == 'N') && i + 3 < n && (chars[i + 1] == 'q' || chars[i + 1] == 'Q') && chars[i + 2] == '\'' {
            Some(i + 1)
        } else {
            None
        };
        if let Some(q) = q_at {
            let open = chars[q + 2];
            let close = match open {
                '[' => ']',
                '(' => ')',
                '{' => '}',
                '<' => '>',
                o => o,
            };
            let mut j = q + 3;
            let mut s = String::new();
            while j < n && !(chars[j] == close && j + 1 < n && chars[j + 1] == '\'') {
                if chars[j] == '\n' {
                    line += 1;
                }
                s.push(chars[j]);
                j += 1;
            }
            out.push(Tok { kind: Kind::Str(s), line: start_line });
            i = (j + 2).min(n);
            continue;
        }
        // 'string' / N'string'
        if c == '\'' || ((c == 'n' || c == 'N') && i + 1 < n && chars[i + 1] == '\'') {
            let mut j = if c == '\'' { i + 1 } else { i + 2 };
            let mut s = String::new();
            while j < n {
                if chars[j] == '\'' {
                    if j + 1 < n && chars[j + 1] == '\'' {
                        s.push('\'');
                        j += 2;
                        continue;
                    }
                    break;
                }
                if chars[j] == '\n' {
                    line += 1;
                }
                s.push(chars[j]);
                j += 1;
            }
            out.push(Tok { kind: Kind::Str(s), line: start_line });
            i = (j + 1).min(n);
            continue;
        }
        if c == '"' {
            let mut j = i + 1;
            let mut s = String::new();
            while j < n && chars[j] != '"' {
                if chars[j] == '\n' {
                    line += 1;
                }
                s.push(chars[j]);
                j += 1;
            }
            out.push(Tok { kind: Kind::Quoted(s), line: start_line });
            i = (j + 1).min(n);
            continue;
        }
        if ident_start(c) {
            let mut j = i;
            while j < n && ident_char(chars[j]) {
                j += 1;
            }
            let w: String = chars[i..j].iter().collect::<String>().to_uppercase();
            out.push(Tok { kind: Kind::Word(w), line });
            i = j;
            continue;
        }
        if c.is_ascii_digit() || (c == '.' && i + 1 < n && chars[i + 1].is_ascii_digit()) {
            let mut j = i;
            while j < n && (chars[j].is_ascii_alphanumeric() || chars[j] == '.') {
                // 1..10 (범위) 는 숫자가 아니다
                if chars[j] == '.' && j + 1 < n && chars[j + 1] == '.' {
                    break;
                }
                j += 1;
            }
            let j = j.max(i + 1);
            out.push(Tok { kind: Kind::Num(chars[i..j].iter().collect()), line });
            i = j;
            continue;
        }
        if i + 1 < n {
            let two: String = [c, chars[i + 1]].iter().collect();
            if let Some(s) = SYMS2.iter().find(|s| **s == two) {
                out.push(Tok { kind: Kind::Sym(s), line });
                i += 2;
                continue;
            }
        }
        let one = c.to_string();
        let s = SYMS1.iter().find(|s| **s == one).copied().unwrap_or("?");
        out.push(Tok { kind: Kind::Sym(s), line });
        i += 1;
    }
    out
}

// ─────────────────────────────────────────────────────────────
// 구조
// ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubKind {
    Procedure,
    Function,
    /// 트리거 본문 전체
    Trigger,
    /// 패키지 본문의 초기화 블록 (BEGIN … END 패키지)
    Init,
}

/// 서브프로그램 하나 (본문이 있는 것만; 선언만 있는 것은 `Decl`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Subprogram {
    pub name: String,
    pub kind: SubKind,
    /// 바깥 서브프로그램 이름들 + 자기 이름 (중첩 함수면 "OUTER.INNER")
    pub path: String,
    /// 같은 path 의 몇 번째 (오버로드 구분, 0부터)
    pub overload: u32,
    /// 머리(PROCEDURE …) 줄
    pub start_line: u32,
    /// 머리 위에 붙은 주석의 첫 줄 (없으면 start_line)
    pub doc_line: u32,
    /// IS/AS 다음 줄부터 BEGIN 앞까지가 선언부
    pub is_line: u32,
    /// 본문 BEGIN 줄 (없으면 0 — 외부 프로시저)
    pub begin_line: u32,
    /// 끝 `;` 줄
    pub end_line: u32,
    /// 이름부터 IS/AS 앞까지 — "PROCEDURE p(a IN NUMBER)"
    pub signature: String,
    /// 바로 안에 든 서브프로그램 (인덱스 — Unit::subprograms 기준)
    pub children: Vec<usize>,
    pub parent: Option<usize>,
    /// 토큰 범위 [start, end] (자식 포함)
    #[serde(skip)]
    pub tok_start: usize,
    #[serde(skip)]
    pub tok_end: usize,
    /// 본문 BEGIN 토큰
    #[serde(skip)]
    pub tok_begin: usize,
}

/// 명세(패키지 스펙, 전방 선언)에 있는 선언
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Decl {
    pub name: String,
    pub kind: SubKind,
    pub line: u32,
    pub signature: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Structure {
    /// PACKAGE / PACKAGE BODY / PROCEDURE / FUNCTION / TRIGGER / TYPE / TYPE BODY
    pub unit_type: String,
    pub name: String,
    /// 소스에 스키마가 적혀 있으면
    pub owner: Option<String>,
    pub subprograms: Vec<Subprogram>,
    /// 스펙의 공개 선언, 본문의 전방 선언
    pub decls: Vec<Decl>,
    /// 패키지 본문의 전역 선언부 줄 범위 (서브프로그램 사이 줄 포함, 서브프로그램 줄 제외)
    pub global_lines: Vec<(u32, u32)>,
    /// 짝이 안 맞는 등 구조를 끝까지 못 읽었으면 그 이유
    pub warning: Option<String>,
}

struct P<'a> {
    t: &'a [Tok],
    lines: &'a [&'a str],
    subs: Vec<Subprogram>,
    decls: Vec<Decl>,
    warning: Option<String>,
}

const SUB_PREFIX: &[&str] = &["MEMBER", "STATIC", "CONSTRUCTOR", "MAP", "ORDER", "OVERRIDING", "FINAL", "INSTANTIABLE", "NOT"];

impl P<'_> {
    fn w(&self, i: usize) -> Option<&str> {
        self.t.get(i).and_then(|t| t.word())
    }
    fn line(&self, i: usize) -> u32 {
        self.t.get(i).map(|t| t.line).unwrap_or_else(|| self.t.last().map(|t| t.line).unwrap_or(1))
    }

    /// 블록 짝 맞추기. `i` 는 BEGIN (또는 CASE/IF/LOOP) 다음 토큰. 맞는 END 뒤의 `;` 인덱스를 돌려준다.
    /// `on_semi(idx, depth)` 는 블록 안의 `;` 와 EXCEPTION 마다 불린다 (depth 1 = 이 블록 바로 안).
    fn match_block(&mut self, mut i: usize, opener: char, mut on_semi: impl FnMut(usize, usize)) -> usize {
        let mut stack: Vec<char> = vec![opener];
        while i < self.t.len() {
            let tk = &self.t[i];
            match tk.word() {
                Some("BEGIN") => stack.push('B'),
                Some("CASE") => stack.push('C'),
                Some("IF") if !self.prev_is_end(i) => stack.push('I'),
                Some("LOOP") if !self.prev_is_end(i) => stack.push('L'),
                Some("END") => {
                    let want = match self.w(i + 1) {
                        Some("IF") => Some('I'),
                        Some("LOOP") => Some('L'),
                        Some("CASE") => Some('C'),
                        _ => None,
                    };
                    match want {
                        Some(k) => {
                            // 맞는 것까지 꺼낸다 (짝이 어긋난 소스에서도 앞으로 간다)
                            while let Some(top) = stack.pop() {
                                if top == k {
                                    break;
                                }
                            }
                            i += 1;
                        }
                        None => {
                            stack.pop();
                        }
                    }
                    if stack.is_empty() {
                        // END [이름] ;
                        let mut j = i + 1;
                        while j < self.t.len() && !self.t[j].sym(";") && j < i + 4 {
                            j += 1;
                        }
                        // CASE 식의 END 뒤에는 ; 가 아닐 수 있다 (opener 가 C 일 때만 여기 온다)
                        return if j < self.t.len() && self.t[j].sym(";") { j } else { i };
                    }
                }
                Some("EXCEPTION") => on_semi(i, stack.len()),
                _ => {
                    if tk.sym(";") {
                        on_semi(i, stack.len());
                    }
                }
            }
            i += 1;
        }
        self.warning.get_or_insert_with(|| "BEGIN/END 짝이 맞지 않습니다 — 끝까지 한 덩어리로 봅니다".into());
        self.t.len().saturating_sub(1)
    }

    fn prev_is_end(&self, i: usize) -> bool {
        i > 0 && self.t[i - 1].is("END")
    }

    /// 머리 위에 붙은 주석 줄 (빈 줄 없이 이어진 것만)
    fn doc_line(&self, start_line: u32) -> u32 {
        let mut l = start_line;
        while l > 1 {
            let prev = self.lines.get(l as usize - 2).map(|s| s.trim()).unwrap_or("");
            if prev.starts_with("--") || prev.starts_with("/*") || prev.starts_with('*') || prev.ends_with("*/") {
                l -= 1;
            } else {
                break;
            }
        }
        l
    }

    fn signature(&self, from: usize, to: usize) -> String {
        let mut s = String::new();
        let mut prev_word = false;
        for tk in &self.t[from..to] {
            let (txt, is_word) = match &tk.kind {
                Kind::Word(w) => (w.clone(), true),
                Kind::Quoted(q) => (format!("\"{q}\""), true),
                Kind::Str(x) => (format!("'{x}'"), true),
                Kind::Num(x) => (x.clone(), true),
                Kind::Sym(x) => (x.to_string(), false),
            };
            if (is_word && (prev_word || s.ends_with(')'))) || matches!(txt.as_str(), ":=" | "=>") || (!s.is_empty() && s.ends_with(',')) {
                s.push(' ');
            }
            s.push_str(&txt);
            if matches!(txt.as_str(), ":=" | "=>") {
                s.push(' ');
            }
            prev_word = is_word;
        }
        s
    }

    /// 선언부를 훑는다. 서브프로그램을 만나면 따라 들어간다. BEGIN 이나 (짝 없는) END 를 만나면 그 인덱스.
    fn decl_section(&mut self, mut i: usize, parent: Option<usize>, path: &str) -> usize {
        while i < self.t.len() {
            match self.w(i) {
                Some("BEGIN") | Some("END") => return i,
                Some("PROCEDURE") | Some("FUNCTION") => {
                    i = self.subprogram(i, parent, path) + 1;
                    continue;
                }
                // 선언 기본값의 CASE 식 (END 로 끝난다)
                Some("CASE") => {
                    i = self.match_block(i + 1, 'C', |_, _| {}) + 1;
                    continue;
                }
                _ => {}
            }
            i += 1;
        }
        i
    }

    /// `i` = PROCEDURE/FUNCTION 토큰. 끝(`;`) 인덱스를 돌려준다.
    fn subprogram(&mut self, i: usize, parent: Option<usize>, path: &str) -> usize {
        let kind = if self.t[i].is("FUNCTION") { SubKind::Function } else { SubKind::Procedure };
        // 머리 앞의 MEMBER/STATIC 같은 수식어도 머리에 넣는다
        let mut head = i;
        while head > 0 && self.w(head - 1).is_some_and(|w| SUB_PREFIX.contains(&w)) {
            head -= 1;
        }
        let mut name = self.t.get(i + 1).and_then(|t| t.name()).unwrap_or_else(|| "?".into());
        let mut j = i + 2;
        // CREATE PROCEDURE 스키마.이름
        if self.t.get(j).is_some_and(|t| t.sym(".")) {
            if let Some(n) = self.t.get(j + 1).and_then(|t| t.name()) {
                name = n;
                j += 2;
            }
        }
        // IS/AS 또는 ; 까지 (괄호 밖)
        let mut depth = 0i32;
        while j < self.t.len() {
            let tk = &self.t[j];
            if tk.sym("(") {
                depth += 1;
            } else if tk.sym(")") {
                depth -= 1;
            } else if depth <= 0 && (tk.sym(";") || tk.is("IS") || tk.is("AS")) {
                break;
            }
            j += 1;
        }
        let signature = self.signature(head, j.min(self.t.len()));
        let start_line = self.line(head);
        if j >= self.t.len() || self.t[j].sym(";") {
            self.decls.push(Decl { name, kind, line: start_line, signature });
            return j.min(self.t.len().saturating_sub(1));
        }
        let is_tok = j;
        let full_path = if path.is_empty() { name.clone() } else { format!("{path}.{name}") };
        let overload = self.subs.iter().filter(|s| s.path == full_path).count() as u32;
        let idx = self.subs.len();
        self.subs.push(Subprogram {
            name: name.clone(),
            kind,
            path: full_path.clone(),
            overload,
            start_line,
            doc_line: self.doc_line(start_line),
            is_line: self.line(is_tok),
            begin_line: 0,
            end_line: start_line,
            signature,
            children: Vec::new(),
            parent,
            tok_start: head,
            tok_end: is_tok,
            tok_begin: 0,
        });
        if let Some(p) = parent {
            self.subs[p].children.push(idx);
        }
        // AS LANGUAGE C / EXTERNAL — 본문이 없다
        if matches!(self.w(is_tok + 1), Some("LANGUAGE") | Some("EXTERNAL")) {
            let mut k = is_tok + 1;
            while k < self.t.len() && !self.t[k].sym(";") {
                k += 1;
            }
            let k = k.min(self.t.len() - 1);
            self.subs[idx].end_line = self.line(k);
            self.subs[idx].tok_end = k;
            return k;
        }
        let b = self.decl_section(is_tok + 1, Some(idx), &full_path);
        let end = if self.w(b) == Some("BEGIN") {
            self.subs[idx].begin_line = self.line(b);
            self.subs[idx].tok_begin = b;
            self.match_block(b + 1, 'B', |_, _| {})
        } else {
            // BEGIN 없이 END — 짝이 어긋난 소스
            self.warning.get_or_insert_with(|| format!("{full_path}: BEGIN 이 없습니다"));
            b
        };
        let end = end.min(self.t.len().saturating_sub(1));
        self.subs[idx].end_line = self.line(end);
        self.subs[idx].tok_end = end;
        end
    }
}

/// 단위 소스(ALL_SOURCE 를 이은 것 또는 CREATE 문 하나)의 구조
pub fn structure(src: &str) -> (Structure, Vec<Tok>) {
    let toks = lex(src);
    let lines: Vec<&str> = src.lines().collect();
    let mut p = P { t: &toks, lines: &lines, subs: Vec::new(), decls: Vec::new(), warning: None };
    let mut out = Structure::default();
    let mut i = 0;
    // CREATE [OR REPLACE] [EDITIONABLE | NONEDITIONABLE]
    if p.w(i) == Some("CREATE") {
        i += 1;
        if p.w(i) == Some("OR") {
            i += 2;
        }
    }
    while matches!(p.w(i), Some("EDITIONABLE") | Some("NONEDITIONABLE") | Some("EDITIONING")) {
        i += 1;
    }
    let mut unit_type = p.w(i).unwrap_or("").to_string();
    let head = i;
    i += 1;
    if matches!(unit_type.as_str(), "PACKAGE" | "TYPE") && p.w(i) == Some("BODY") {
        unit_type.push_str(" BODY");
        i += 1;
    }
    // 이름 (스키마.이름)
    let mut name = p.t.get(i).and_then(|t| t.name()).unwrap_or_default();
    if p.t.get(i + 1).is_some_and(|t| t.sym(".")) {
        out.owner = Some(name.clone());
        name = p.t.get(i + 2).and_then(|t| t.name()).unwrap_or_default();
        i += 2;
    }
    i += 1;
    out.unit_type = unit_type.clone();
    out.name = name.clone();
    // wrap 된 소스 (DBMS_* 등) — 읽을 수 없다
    if p.w(i) == Some("WRAPPED") {
        out.warning = Some("wrap 된(암호화된) 소스라 분석할 수 없습니다".into());
        return (out, toks);
    }

    match unit_type.as_str() {
        "PROCEDURE" | "FUNCTION" => {
            // 단위 자체가 서브프로그램
            p.subprogram(head, None, "");
        }
        "TRIGGER" => {
            // 본문 BEGIN 또는 DECLARE 까지가 머리
            let mut j = i;
            while j < toks.len() && !matches!(p.w(j), Some("BEGIN") | Some("DECLARE") | Some("COMPOUND")) {
                j += 1;
            }
            let signature = p.signature(head, j.min(toks.len()));
            let decl_start = j;
            let b = if p.w(j) == Some("DECLARE") { p.decl_section(j + 1, None, &name) } else { j };
            let (begin_line, end) = if p.w(b) == Some("BEGIN") {
                (p.line(b), p.match_block(b + 1, 'B', |_, _| {}))
            } else {
                (0, toks.len().saturating_sub(1))
            };
            let start_line = p.line(head);
            p.subs.push(Subprogram {
                name: name.clone(),
                kind: SubKind::Trigger,
                path: name.clone(),
                overload: 0,
                start_line,
                doc_line: start_line,
                is_line: p.line(decl_start),
                begin_line,
                end_line: p.line(end),
                signature,
                children: Vec::new(),
                parent: None,
                tok_start: head,
                tok_end: end,
                tok_begin: b,
            });
        }
        _ => {
            // PACKAGE [BODY] / TYPE [BODY]: … IS|AS 선언부 [BEGIN 초기화] END
            while i < toks.len() && !matches!(p.w(i), Some("IS") | Some("AS")) {
                i += 1;
            }
            let b = p.decl_section(i + 1, None, "");
            if p.w(b) == Some("BEGIN") && unit_type.ends_with("BODY") {
                let end = p.match_block(b + 1, 'B', |_, _| {});
                let l = p.line(b);
                p.subs.push(Subprogram {
                    name: "(초기화)".into(),
                    kind: SubKind::Init,
                    path: "(초기화)".into(),
                    overload: 0,
                    start_line: l,
                    doc_line: l,
                    is_line: l,
                    begin_line: l,
                    end_line: p.line(end),
                    signature: format!("{unit_type} {name} 초기화 블록"),
                    children: Vec::new(),
                    parent: None,
                    tok_start: b,
                    tok_end: end,
                    tok_begin: b,
                });
            }
        }
    }

    // 전역 선언부 = 단위 머리 다음 줄 ~ 끝, 최상위 서브프로그램 줄 제외
    if unit_type.ends_with("BODY") || unit_type == "PACKAGE" || unit_type == "TYPE" {
        let first = toks.get(head).map(|t| t.line).unwrap_or(1);
        let last = lines.len() as u32;
        let mut tops: Vec<(u32, u32)> = p.subs.iter().filter(|s| s.parent.is_none()).map(|s| (s.doc_line, s.end_line)).collect();
        tops.sort();
        let mut cur = first;
        for (a, b) in tops {
            if a > cur {
                out.global_lines.push((cur, a - 1));
            }
            cur = cur.max(b + 1);
        }
        if cur <= last {
            out.global_lines.push((cur, last));
        }
    }

    out.subprograms = p.subs;
    out.decls = p.decls;
    out.warning = p.warning;
    (out, toks)
}

/// 서브프로그램 본문(BEGIN 다음 ~ END 앞)에서 `;` 가 있는 줄과 그 깊이.
/// 쪼갤 자리를 고르는 데 쓴다 (깊이 1 = 본문 바로 안의 문장 끝).
pub fn statement_ends(toks: &[Tok], s: &Subprogram) -> Vec<(u32, usize)> {
    if s.tok_begin == 0 && s.kind != SubKind::Init {
        return Vec::new();
    }
    let lines: Vec<&str> = Vec::new();
    let mut p = P { t: toks, lines: &lines, subs: Vec::new(), decls: Vec::new(), warning: None };
    let mut out = Vec::new();
    let begin = s.tok_begin;
    p.match_block(begin + 1, 'B', |i, d| {
        // EXCEPTION 절은 그 앞 줄에서 자른다
        let l = if toks[i].is("EXCEPTION") { toks[i].line.saturating_sub(1) } else { toks[i].line };
        out.push((l, d))
    });
    out.sort();
    out.dedup();
    out
}
