//! 구절 인식 자동완성.
//!
//! 키 하나마다 불린다. 그래서 DB 에 묻지 않는다 — 접속 때 읽어 둔 [`SchemaCache`] 만 본다.
//! 커서가 있는 문장만 토큰으로 나눠, 지금 어느 구절에 있는지(FROM 뒤 테이블 자리,
//! `a.` 뒤 컬럼, WHERE/ON 의 조건, INSERT 컬럼 목록 …)와 그 문장에서 보이는
//! 테이블·별칭·CTE·인라인 뷰를 알아낸 뒤, 그 구절에 맞는 후보만 점수를 매겨 돌려준다.
//!
//! 캐시에 없는 테이블의 컬럼이 필요하면 [`Analysis::missing`] 에 담아 돌려준다.
//! 호출부(앱)가 메타 세션으로 채워 넣고 한 번 더 부른다.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use serde::Serialize;

// ─────────────────────────────────────────────────────────────
// 캐시
// ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ObjKind {
    Table,
    View,
    Synonym,
    Sequence,
    Package,
    Procedure,
    Function,
    Type,
}

impl ObjKind {
    pub fn from_oracle(t: &str) -> Option<Self> {
        Some(match t {
            "TABLE" => ObjKind::Table,
            "VIEW" | "MATERIALIZED VIEW" => ObjKind::View,
            "SYNONYM" => ObjKind::Synonym,
            "SEQUENCE" => ObjKind::Sequence,
            "PACKAGE" => ObjKind::Package,
            "PROCEDURE" => ObjKind::Procedure,
            "FUNCTION" => ObjKind::Function,
            "TYPE" => ObjKind::Type,
            _ => return None,
        })
    }
    fn table_like(self) -> bool {
        matches!(self, ObjKind::Table | ObjKind::View | ObjKind::Synonym)
    }
}

#[derive(Debug, Clone)]
pub struct ObjInfo {
    pub owner: String,
    pub name: String,
    pub kind: ObjKind,
    pub comment: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ColInfo {
    pub name: String,
    pub data_type: String,
    pub nullable: bool,
    pub comment: Option<String>,
}

/// 외래 키 하나 (복합 키는 컬럼이 여러 개)
#[derive(Debug, Clone)]
pub struct ForeignKey {
    pub owner: String,
    pub table: String,
    pub cols: Vec<String>,
    pub r_owner: String,
    pub r_table: String,
    pub r_cols: Vec<String>,
}

type Key = (String, String);

/// 오라클 기본 스키마 — 공개 동의어 중 이것들을 가리키는 것은 뒤로 미룬다
const SYSTEM_SCHEMAS: &[&str] = &[
    "SYS", "SYSTEM", "XDB", "MDSYS", "CTXSYS", "ORDSYS", "ORDPLUGINS", "OLAPSYS", "WMSYS",
    "EXFSYS", "DBSNMP", "APPQOSSYS", "OUTLN", "ORACLE_OCM", "SI_INFORMTN_SCHEMA", "ORDDATA",
    "APEX_040000", "APEX_030200", "FLOWS_FILES", "ANONYMOUS", "LBACSYS", "DVSYS", "OWBSYS",
    "SYSMAN", "MGMT_VIEW", "XS$NULL", "SPATIAL_CSW_ADMIN_USR", "SPATIAL_WFS_ADMIN_USR",
];

pub fn is_system_schema(s: &str) -> bool {
    SYSTEM_SCHEMAS.contains(&s) || s.starts_with("APEX_")
}

/// 접속 사용자 기준의 스키마 정보. 접속 때 한 번 채우고, DDL 뒤에 다시 읽는다.
#[derive(Debug, Default, Clone)]
pub struct SchemaCache {
    pub user: String,
    /// 접속 사용자의 객체
    objects: Vec<ObjInfo>,
    by_name: HashMap<String, usize>,
    /// 동의어 이름 → 대상 (개인 동의어가 공개 동의어보다 앞선다)
    synonyms: HashMap<String, Key>,
    /// 공개 동의어 (이름, 대상, 대상이 오라클 기본 스키마인지) — 테이블 자리 후보용
    public_synonyms: Vec<(String, Key, bool)>,
    columns: HashMap<Key, Arc<Vec<ColInfo>>>,
    pks: HashMap<Key, Vec<String>>,
    fks: Vec<ForeignKey>,
    schemas: Vec<String>,
    /// 다른 스키마의 객체 (그 스키마를 처음 쓸 때 읽는다)
    schema_objects: HashMap<String, Arc<Vec<ObjInfo>>>,
    /// 패키지 → 프로시저/함수 이름
    package_members: HashMap<Key, Arc<Vec<String>>>,
    /// 다른 스키마 객체의 종류 (owner, name) → kind — 컬럼을 읽은 객체, 동의어 대상
    known_kinds: HashMap<Key, ObjKind>,
}

impl SchemaCache {
    pub fn new(user: &str) -> Self {
        Self { user: user.to_uppercase(), ..Default::default() }
    }

    pub fn set_objects(&mut self, objs: Vec<ObjInfo>) {
        self.by_name = objs.iter().enumerate().map(|(i, o)| (o.name.clone(), i)).collect();
        for o in &objs {
            self.known_kinds.insert((o.owner.clone(), o.name.clone()), o.kind);
        }
        self.objects = objs;
    }

    pub fn add_synonym(&mut self, name: &str, target_owner: &str, target: &str, public: bool) {
        let key = (target_owner.to_string(), target.to_string());
        if public {
            self.public_synonyms.push((name.to_string(), key.clone(), is_system_schema(target_owner)));
            self.synonyms.entry(name.to_string()).or_insert(key);
        } else {
            self.synonyms.insert(name.to_string(), key);
        }
    }

    pub fn set_columns(&mut self, owner: &str, table: &str, cols: Vec<ColInfo>) {
        self.columns.insert((owner.to_string(), table.to_string()), Arc::new(cols));
    }

    pub fn set_pk(&mut self, owner: &str, table: &str, cols: Vec<String>) {
        self.pks.insert((owner.to_string(), table.to_string()), cols);
    }

    pub fn add_fk(&mut self, fk: ForeignKey) {
        self.fks.push(fk);
    }

    pub fn set_schemas(&mut self, s: Vec<String>) {
        self.schemas = s;
    }

    pub fn set_schema_objects(&mut self, owner: &str, objs: Vec<ObjInfo>) {
        for o in &objs {
            self.known_kinds.insert((o.owner.clone(), o.name.clone()), o.kind);
        }
        self.schema_objects.insert(owner.to_string(), Arc::new(objs));
    }

    pub fn set_package_members(&mut self, owner: &str, pkg: &str, names: Vec<String>) {
        self.package_members.insert((owner.to_string(), pkg.to_string()), Arc::new(names));
    }

    pub fn set_kind(&mut self, owner: &str, name: &str, kind: ObjKind) {
        self.known_kinds.insert((owner.to_string(), name.to_string()), kind);
    }

    pub fn has_columns(&self, owner: &str, table: &str) -> bool {
        self.columns.contains_key(&(owner.to_string(), table.to_string()))
    }

    pub fn stats(&self) -> (usize, usize) {
        (self.objects.len(), self.columns.values().map(|c| c.len()).sum())
    }

    /// 이름(대문자) → (owner, name). 내 객체 → 동의어 순서. 동의어는 한 단계 따라간다.
    fn resolve(&self, owner: Option<&str>, name: &str) -> Option<Key> {
        if let Some(o) = owner {
            let key = (o.to_string(), name.to_string());
            // 다른 스키마의 동의어일 수도 있지만 드물다 — 그대로 쓴다
            return Some(key);
        }
        if let Some(&i) = self.by_name.get(name) {
            let o = &self.objects[i];
            if o.kind != ObjKind::Synonym {
                return Some((o.owner.clone(), o.name.clone()));
            }
        }
        self.synonyms.get(name).cloned()
    }

    fn kind_of(&self, key: &Key) -> Option<ObjKind> {
        self.known_kinds.get(key).copied()
    }

    fn cols(&self, key: &Key) -> Option<&Arc<Vec<ColInfo>>> {
        self.columns.get(key)
    }
}

// ─────────────────────────────────────────────────────────────
// 토큰
// ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
enum T {
    /// 일반 식별자/키워드 (대문자로 바꾼 값)
    Word(String),
    /// "따옴표 식별자" (그대로)
    QWord(String),
    Str,
    Num,
    Bind,
    P(char),
}

#[derive(Debug, Clone)]
struct Tok {
    t: T,
    s: usize,
    e: usize,
    depth: i32,
}

fn is_ident_start(c: char) -> bool {
    c.is_alphabetic() || c == '_'
}
fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$' || c == '#'
}

/// 토큰과, 주석·문자열 구간(커서가 그 안이면 자동완성을 하지 않는다)
fn tokenize(src: &str) -> (Vec<Tok>, Vec<(usize, usize)>) {
    let b = src.as_bytes();
    let n = b.len();
    let mut out = Vec::new();
    let mut dead = Vec::new();
    let mut depth = 0i32;
    let mut i = 0;
    while i < n {
        let c = b[i];
        if c.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        let start = i;
        if c == b'-' && i + 1 < n && b[i + 1] == b'-' {
            while i < n && b[i] != b'\n' {
                i += 1;
            }
            dead.push((start, i));
            continue;
        }
        if c == b'/' && i + 1 < n && b[i + 1] == b'*' {
            i += 2;
            while i + 1 < n && !(b[i] == b'*' && b[i + 1] == b'/') {
                i += 1;
            }
            i = (i + 2).min(n);
            dead.push((start, i));
            continue;
        }
        if (c == b'q' || c == b'Q' || c == b'n' || c == b'N') && i + 2 < n {
            // q'[..]' / nq'[..]'
            let qpos = if (c == b'n' || c == b'N') && (b[i + 1] == b'q' || b[i + 1] == b'Q') { i + 1 } else { i };
            if (b[qpos] == b'q' || b[qpos] == b'Q') && qpos + 2 < n && b[qpos + 1] == b'\'' {
                let open = b[qpos + 2];
                let close = match open { b'[' => b']', b'(' => b')', b'{' => b'}', b'<' => b'>', o => o };
                i = qpos + 3;
                while i + 1 < n && !(b[i] == close && b[i + 1] == b'\'') {
                    i += 1;
                }
                i = (i + 2).min(n);
                dead.push((start, i));
                out.push(Tok { t: T::Str, s: start, e: i, depth });
                continue;
            }
        }
        if c == b'\'' {
            i += 1;
            while i < n {
                if b[i] == b'\'' {
                    if i + 1 < n && b[i + 1] == b'\'' {
                        i += 2;
                        continue;
                    }
                    break;
                }
                i += 1;
            }
            i = (i + 1).min(n);
            dead.push((start, i));
            out.push(Tok { t: T::Str, s: start, e: i, depth });
            continue;
        }
        if c == b'"' {
            i += 1;
            while i < n && b[i] != b'"' {
                i += 1;
            }
            let inner = src[start + 1..i.min(n)].to_string();
            i = (i + 1).min(n);
            out.push(Tok { t: T::QWord(inner), s: start, e: i, depth });
            continue;
        }
        if c == b':' && i + 1 < n && (b[i + 1] as char).is_ascii_alphanumeric() {
            i += 1;
            while i < n && is_ident(b[i] as char) {
                i += 1;
            }
            out.push(Tok { t: T::Bind, s: start, e: i, depth });
            continue;
        }
        if c.is_ascii_digit() {
            while i < n && (b[i].is_ascii_alphanumeric() || b[i] == b'.') {
                i += 1;
            }
            out.push(Tok { t: T::Num, s: start, e: i, depth });
            continue;
        }
        let ch = src[i..].chars().next().unwrap_or('\0');
        if is_ident_start(ch) {
            let mut j = i;
            for (k, cc) in src[i..].char_indices() {
                if !is_ident(cc) {
                    j = i + k;
                    break;
                }
                j = i + k + cc.len_utf8();
            }
            out.push(Tok { t: T::Word(src[i..j].to_uppercase()), s: i, e: j, depth });
            i = j;
            continue;
        }
        if ch == '(' {
            out.push(Tok { t: T::P('('), s: i, e: i + 1, depth });
            depth += 1;
        } else if ch == ')' {
            depth -= 1;
            out.push(Tok { t: T::P(')'), s: i, e: i + 1, depth });
        } else {
            out.push(Tok { t: T::P(ch), s: i, e: i + ch.len_utf8(), depth });
        }
        i += ch.len_utf8().max(1);
    }
    (out, dead)
}

fn word(t: &Tok) -> Option<&str> {
    match &t.t {
        T::Word(w) => Some(w),
        _ => None,
    }
}

fn ident(t: &Tok) -> Option<String> {
    match &t.t {
        T::Word(w) => Some(w.clone()),
        T::QWord(w) => Some(w.clone()),
        _ => None,
    }
}

fn is_w(t: Option<&Tok>, w: &str) -> bool {
    t.and_then(word) == Some(w)
}

/// 테이블 별칭이 될 수 없는 단어
const NOT_ALIAS: &[&str] = &[
    "ON", "USING", "WHERE", "JOIN", "INNER", "LEFT", "RIGHT", "FULL", "OUTER", "CROSS", "NATURAL",
    "GROUP", "ORDER", "HAVING", "CONNECT", "START", "UNION", "INTERSECT", "MINUS", "SET", "VALUES",
    "SELECT", "FROM", "PARTITION", "SAMPLE", "AS", "WITH", "FOR", "MODEL", "PIVOT", "UNPIVOT",
    "RETURNING", "LOG", "WHEN", "INTO", "AND", "OR", "BY",
];

// ─────────────────────────────────────────────────────────────
// 분석
// ─────────────────────────────────────────────────────────────

/// 문장 안에서 보이는 테이블 하나
#[derive(Debug, Clone)]
struct TableRef {
    owner: Option<String>,
    name: Option<String>,
    alias: Option<String>,
    /// CTE·인라인 뷰처럼 SELECT 목록에서 컬럼을 얻는 경우
    derived: Option<Vec<String>>,
    /// 몇 번째 블록에서 왔는지 (0 = 커서가 있는 블록)
    level: usize,
}

impl TableRef {
    fn shown(&self) -> String {
        self.alias.clone().or_else(|| self.name.clone()).unwrap_or_default()
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Ctx {
    /// 자동완성 안 함 (주석/문자열 안, 숫자 뒤)
    None,
    /// 문장 맨 앞
    Start,
    /// 테이블 이름 자리
    Table { join: bool },
    /// 테이블 이름 다음 (별칭 또는 다음 구절)
    AfterTable { in_from: bool },
    /// 컬럼/식 자리 — 구절 이름
    Expr(&'static str),
    /// ON 바로 뒤 — 조인 조건을 추천한다
    JoinOn,
    /// `x.` 뒤
    Qualified(Vec<String>),
    /// INSERT INTO t ( 안
    InsertColumns,
    /// EXEC / CALL / BEGIN 뒤 — 프로시저
    Procedure,
}

/// 캐시에 없어 채워야 하는 것
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Missing {
    Columns { owner: String, table: String },
    Schema { owner: String },
    Package { owner: String, name: String },
}

#[derive(Debug, Clone, Serialize)]
pub struct Item {
    pub label: String,
    /// column | table | view | synonym | sequence | package | procedure | function | schema
    /// | alias | keyword | join | snippet
    pub kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub info: Option<String>,
    /// 넣을 글자가 label 과 다를 때
    #[serde(skip_serializing_if = "Option::is_none")]
    pub apply: Option<String>,
    /// 화면 정렬 가중치 (-99..99)
    pub boost: i32,
    #[serde(skip)]
    score: i32,
}

#[derive(Debug, Clone, Serialize)]
pub struct Completion {
    /// 바꿀 구간의 시작 (입력 텍스트 기준 바이트 위치)
    pub from: usize,
    pub items: Vec<Item>,
    /// 분석된 구절 (디버깅·테스트용)
    pub context: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub missing: Vec<Missing>,
}

pub struct Analysis {
    from: usize,
    prefix: String,
    ctx: Ctx,
    refs: Vec<TableRef>,
    ctes: HashMap<String, Vec<String>>,
    lower: bool,
    /// INSERT INTO 대상 / 방금 JOIN 한 테이블
    target: Option<TableRef>,
}

/// 커서가 주석·문자열 안인지. 끝나지 않은 문자열·블록 주석의 끝, 한 줄 주석의 줄 끝도 안이다.
fn in_dead_zone(src: &str, dead: &[(usize, usize)], cursor: usize) -> bool {
    dead.iter().any(|&(s, e)| {
        if cursor <= s {
            return false;
        }
        if cursor < e {
            return true;
        }
        if cursor > e {
            return false;
        }
        // cursor == e: 닫혔으면 밖, 안 닫혔으면 안
        let lit = &src[s..e];
        if lit.starts_with("--") {
            return true;
        }
        if lit.starts_with("/*") {
            return !(lit.len() >= 4 && lit.ends_with("*/"));
        }
        // 'abc' / q'[abc]'
        !(lit.len() >= 2 && lit.ends_with('\''))
    })
}

/// 커서가 있는 문장의 범위 — `;`, 한 줄짜리 `/`, 빈 줄로 끊는다 (주석·문자열 밖에서만)
fn statement_bounds(src: &str, toks: &[Tok], cursor: usize) -> (usize, usize) {
    let mut start = 0;
    let mut end = src.len();
    let mut prev_e = 0;
    for t in toks {
        // 토큰 사이에 빈 줄이 있으면 문장 경계
        let gap = &src[prev_e..t.s];
        let blank = gap.matches('\n').count() >= 2 && gap.split('\n').skip(1).any(|l| l.trim().is_empty());
        if blank {
            if t.s <= cursor {
                start = t.s;
            } else if prev_e >= cursor {
                end = prev_e;
                break;
            }
        }
        let sep = t.t == T::P(';') || (t.t == T::P('/') && {
            let ls = src[..t.s].rfind('\n').map(|p| p + 1).unwrap_or(0);
            let le = src[t.e..].find('\n').map(|p| t.e + p).unwrap_or(src.len());
            src[ls..le].trim() == "/"
        });
        if sep {
            if t.e <= cursor {
                start = t.e;
            } else if t.s >= cursor {
                end = t.s;
                break;
            }
        }
        prev_e = t.e;
    }
    // 커서 뒤가 빈 줄로 끊기는 경우
    if end == src.len() {
        if let Some(p) = src[cursor.min(src.len())..].find("\n\n") {
            let cand = cursor + p;
            if src[cursor..cand].trim().is_empty() || !src[cand..].trim().is_empty() {
                end = end.min(cand.max(cursor));
            }
        }
    }
    (start, end.max(start))
}

const BLOCK_LEAD: &[&str] = &["BEGIN", "DECLARE", "THEN", "ELSE", "LOOP", "IS", "AS"];

pub fn analyze(src: &str, cursor: usize) -> Analysis {
    let cursor = cursor.min(src.len());
    let (all, dead) = tokenize(src);
    let empty = |ctx| Analysis {
        from: cursor,
        prefix: String::new(),
        ctx,
        refs: vec![],
        ctes: HashMap::new(),
        lower: false,
        target: None,
    };
    if in_dead_zone(src, &dead, cursor) {
        return empty(Ctx::None);
    }

    let (ss, se) = statement_bounds(src, &all, cursor);
    let mut toks: Vec<Tok> = all.into_iter().filter(|t| t.s >= ss && t.e <= se.max(cursor)).collect();
    let keyword_lower = {
        let (mut lo, mut up) = (0, 0);
        for t in &toks {
            if let T::Word(w) = &t.t {
                if KEYWORDS.contains(&w.as_str()) {
                    if src[t.s..t.e].chars().any(|c| c.is_lowercase()) { lo += 1 } else { up += 1 }
                }
            }
        }
        lo > up
    };
    // PL/SQL 블록 안의 문장: 앞의 BEGIN/DECLARE 등을 떼어 낸다
    while toks.first().is_some_and(|t| word(t).is_some_and(|w| BLOCK_LEAD.contains(&w))) {
        toks.remove(0);
    }
    // 깊이를 문장 시작 기준으로 맞춘다
    if let Some(base) = toks.first().map(|t| t.depth) {
        for t in &mut toks {
            t.depth -= base;
        }
    }

    // 입력 중인 단어
    let mut from = cursor;
    let mut prefix = String::new();
    let mut before_end = toks.partition_point(|t| t.e <= cursor);
    if before_end > 0 {
        let t = &toks[before_end - 1];
        if t.e == cursor && matches!(t.t, T::Word(_) | T::QWord(_)) {
            from = t.s;
            prefix = src[t.s..cursor].trim_start_matches('"').to_string();
            before_end -= 1;
        } else if t.e == cursor && t.t == T::Num {
            return empty(Ctx::None);
        }
    }
    // 단어 한가운데
    if before_end < toks.len() && toks[before_end].s < cursor && toks[before_end].e > cursor {
        let t = &toks[before_end];
        if matches!(t.t, T::Word(_)) {
            from = t.s;
            prefix = src[t.s..cursor].to_string();
        }
    }
    let before = &toks[..before_end];
    let lower = if !prefix.is_empty() { !prefix.chars().any(|c| c.is_uppercase()) } else { keyword_lower };

    let ctes = parse_ctes(&toks);
    let cursor_depth = before.last().map(|t| if t.t == T::P('(') { t.depth + 1 } else { t.depth }).unwrap_or(0);

    // 커서를 감싸는 괄호들 (안쪽부터)
    let mut enclosing: Vec<usize> = Vec::new();
    {
        let mut stack: Vec<usize> = Vec::new();
        for (i, t) in before.iter().enumerate() {
            match t.t {
                T::P('(') => stack.push(i),
                T::P(')') => {
                    stack.pop();
                }
                _ => {}
            }
        }
        enclosing.extend(stack.iter().rev());
    }
    // 쿼리 블록: SELECT/WITH 로 시작하는 괄호 + 문장 전체
    let mut blocks: Vec<(usize, i32)> = Vec::new(); // (블록 첫 토큰 위치, 깊이)
    for &p in &enclosing {
        if p + 1 < toks.len() && matches!(word(&toks[p + 1]), Some("SELECT") | Some("WITH")) {
            blocks.push((p + 1, toks[p].depth + 1));
        }
    }
    blocks.push((0, 0));

    // 블록마다 테이블 참조
    let mut refs: Vec<TableRef> = Vec::new();
    for (level, &(bs, bd)) in blocks.iter().enumerate() {
        let be = block_end(&toks, bs, bd);
        for mut r in parse_refs(&toks[bs..be], bd, &ctes) {
            r.level = level;
            refs.push(r);
        }
    }

    // 구절
    let (bs, bd) = blocks[0];
    let block_before: Vec<&Tok> = before[bs.min(before.len())..].iter().filter(|t| t.depth == bd).collect();
    let mut target = None;

    // `x.` / `x.y.`
    let mut ctx = None;
    {
        let mut parts = Vec::new();
        let mut k = before.len();
        while k >= 2 && before[k - 1].t == T::P('.') && matches!(before[k - 2].t, T::Word(_) | T::QWord(_)) {
            // 점과 단어가 붙어 있어야 한다
            if before[k - 1].s != before[k - 2].e || (k == before.len() && before[k - 1].e != from) {
                break;
            }
            parts.insert(0, ident(&before[k - 2]).unwrap());
            k -= 2;
            if k == 0 || before[k - 1].t != T::P('.') {
                break;
            }
        }
        if !parts.is_empty() {
            ctx = Some(Ctx::Qualified(parts));
        }
    }

    // INSERT INTO t ( … — 바로 감싸는 괄호가 INSERT 대상의 컬럼 목록
    if ctx.is_none() {
        if let Some(&p) = enclosing.first() {
            let inside_query = blocks.iter().any(|&(b, _)| b == p + 1);
            if !inside_query && p >= 2 {
                // INTO [owner.]t [alias] (
                let mut q = p;
                let mut names = Vec::new();
                while q > 0 && matches!(toks[q - 1].t, T::Word(_) | T::QWord(_) | T::P('.')) {
                    q -= 1;
                    if is_w(Some(&toks[q]), "INTO") {
                        break;
                    }
                    names.insert(0, toks[q].clone());
                }
                if is_w(Some(&toks[q]), "INTO") && !names.is_empty() {
                    let r = table_from_tokens(&names, &ctes);
                    target = Some(r);
                    ctx = Some(Ctx::InsertColumns);
                }
            }
        }
    }

    if ctx.is_none() {
        ctx = Some(clause_ctx(&block_before, cursor_depth != bd));
    }
    let mut ctx = ctx.unwrap();

    // ON 바로 뒤면 조인 조건 추천
    if matches!(ctx, Ctx::Expr("ON")) && block_before.last().is_some_and(|t| is_w(Some(t), "ON")) {
        ctx = Ctx::JoinOn;
    }
    if matches!(ctx, Ctx::JoinOn | Ctx::Table { join: true }) {
        // 가장 최근에 JOIN 한 테이블
        target = last_joined(&block_before, &ctes);
    }

    Analysis { from, prefix, ctx, refs, ctes, lower, target }
}

/// 블록의 끝 (같은 깊이의 닫는 괄호 직전, 없으면 끝)
fn block_end(toks: &[Tok], start: usize, depth: i32) -> usize {
    for (i, t) in toks.iter().enumerate().skip(start) {
        if t.t == T::P(')') && t.depth < depth {
            return i;
        }
    }
    toks.len()
}

fn table_from_tokens(names: &[Tok], ctes: &HashMap<String, Vec<String>>) -> TableRef {
    let ids: Vec<String> = names.iter().filter_map(ident).collect();
    let (owner, name, alias) = match ids.as_slice() {
        [n] => (None, Some(n.clone()), None),
        [a, b] if names.get(1).map(|t| t.t == T::P('.')).unwrap_or(false) => (Some(a.clone()), Some(b.clone()), None),
        [n, a] => (None, Some(n.clone()), Some(a.clone())),
        [o, n, a, ..] => (Some(o.clone()), Some(n.clone()), Some(a.clone())),
        _ => (None, None, None),
    };
    let derived = if owner.is_none() { name.as_ref().and_then(|n| ctes.get(n).cloned()) } else { None };
    TableRef { owner, name, alias, derived, level: 0 }
}

/// WITH a AS (...), b (x, y) AS (...)
fn parse_ctes(toks: &[Tok]) -> HashMap<String, Vec<String>> {
    let mut out = HashMap::new();
    if !is_w(toks.first(), "WITH") {
        return out;
    }
    let mut i = 1;
    while i < toks.len() {
        let Some(name) = ident(&toks[i]) else { break };
        i += 1;
        let mut cols: Option<Vec<String>> = None;
        if toks.get(i).map(|t| t.t == T::P('(')).unwrap_or(false) && !is_w(toks.get(i + 1), "SELECT") {
            let d = toks[i].depth;
            let mut c = Vec::new();
            i += 1;
            while i < toks.len() && !(toks[i].t == T::P(')') && toks[i].depth == d) {
                if let Some(w) = ident(&toks[i]) {
                    c.push(w);
                }
                i += 1;
            }
            i += 1;
            cols = Some(c);
        }
        if !is_w(toks.get(i), "AS") {
            break;
        }
        i += 1;
        if toks.get(i).map(|t| t.t != T::P('(')).unwrap_or(true) {
            break;
        }
        let d = toks[i].depth;
        let qs = i + 1;
        let mut j = qs;
        while j < toks.len() && !(toks[j].t == T::P(')') && toks[j].depth == d) {
            j += 1;
        }
        let names = cols.unwrap_or_else(|| select_list_names(&toks[qs..j.min(toks.len())], d + 1));
        out.insert(name, names);
        i = j + 1;
        if toks.get(i).map(|t| t.t == T::P(',')).unwrap_or(false) {
            i += 1;
        } else {
            break;
        }
    }
    out
}

/// SELECT 목록의 컬럼 이름 (별칭 우선). `*` 는 알 수 없으므로 뺀다.
fn select_list_names(toks: &[Tok], depth: i32) -> Vec<String> {
    let mut i = 0;
    while i < toks.len() && !(is_w(toks.get(i), "SELECT") && toks[i].depth == depth) {
        i += 1;
    }
    i += 1;
    if matches!(toks.get(i).and_then(word), Some("DISTINCT") | Some("UNIQUE") | Some("ALL")) {
        i += 1;
    }
    let mut out = Vec::new();
    let mut item: Vec<&Tok> = Vec::new();
    let flush = |item: &mut Vec<&Tok>, out: &mut Vec<String>| {
        if let Some(last) = item.last() {
            if let Some(n) = ident(last) {
                let prev_dot = item.len() >= 2 && item[item.len() - 2].t == T::P('.');
                if item.len() == 1 || prev_dot || item.len() >= 2 {
                    if !KEYWORDS.contains(&n.as_str()) || prev_dot || item.len() > 1 {
                        out.push(n);
                    }
                }
            }
        }
        item.clear();
    };
    while i < toks.len() {
        let t = &toks[i];
        if t.depth == depth {
            if is_w(Some(t), "FROM") || is_w(Some(t), "INTO") {
                break;
            }
            if t.t == T::P(',') {
                flush(&mut item, &mut out);
                i += 1;
                continue;
            }
        }
        if t.depth == depth {
            item.push(t);
        }
        i += 1;
    }
    flush(&mut item, &mut out);
    out
}

/// 블록 안(같은 깊이)의 FROM/JOIN/INTO/UPDATE/USING 뒤 테이블들
fn parse_refs(toks: &[Tok], depth: i32, ctes: &HashMap<String, Vec<String>>) -> Vec<TableRef> {
    let mut out = Vec::new();
    let mut i = 0;
    let mut in_from = false;
    while i < toks.len() {
        let t = &toks[i];
        if t.depth != depth {
            i += 1;
            continue;
        }
        let w = word(t).unwrap_or("");
        let starts_table = match w {
            "FROM" => {
                in_from = true;
                true
            }
            "JOIN" | "USING" if !(w == "USING" && toks.get(i + 1).map(|x| x.t == T::P('(')).unwrap_or(false)) => true,
            "INTO" | "UPDATE" | "TABLE" => {
                in_from = false;
                true
            }
            "DELETE" => !is_w(toks.get(i + 1), "FROM"),
            "WHERE" | "GROUP" | "ORDER" | "HAVING" | "CONNECT" | "START" | "UNION" | "INTERSECT" | "MINUS" | "SET" | "VALUES" => {
                in_from = false;
                false
            }
            _ => false,
        } || (t.t == T::P(',') && in_from);
        if !starts_table {
            i += 1;
            continue;
        }
        i += 1;
        let Some(nt) = toks.get(i) else { break };
        if nt.t == T::P('(') {
            // 인라인 뷰
            let d = nt.depth;
            let qs = i + 1;
            let mut j = qs;
            while j < toks.len() && !(toks[j].t == T::P(')') && toks[j].depth == d) {
                j += 1;
            }
            let cols = select_list_names(&toks[qs..j.min(toks.len())], d + 1);
            let mut k = j + 1;
            if is_w(toks.get(k), "AS") {
                k += 1;
            }
            let alias = toks.get(k).and_then(ident).filter(|a| !NOT_ALIAS.contains(&a.as_str()));
            if alias.is_some() {
                k += 1;
            }
            out.push(TableRef { owner: None, name: None, alias, derived: Some(cols), level: 0 });
            i = k;
            continue;
        }
        let Some(n1) = ident(nt) else { continue };
        if KEYWORDS.contains(&n1.as_str()) && !ctes.contains_key(&n1) && matches!(n1.as_str(), "SELECT" | "WHERE" | "SET") {
            continue;
        }
        let mut owner = None;
        let mut name = n1;
        let mut k = i + 1;
        if toks.get(k).map(|x| x.t == T::P('.')).unwrap_or(false) {
            match toks.get(k + 1).and_then(ident) {
                Some(n2) => {
                    owner = Some(name);
                    name = n2;
                    k += 2;
                }
                // "FROM erp." — 아직 치는 중. 테이블이 아니라 스키마다
                None => {
                    i = k + 1;
                    continue;
                }
            }
        }
        // @dblink
        if toks.get(k).map(|x| x.t == T::P('@')).unwrap_or(false) {
            k += 2;
        }
        if is_w(toks.get(k), "AS") {
            k += 1;
        }
        let alias = toks
            .get(k)
            .filter(|x| x.depth == depth)
            .and_then(ident)
            .filter(|a| !NOT_ALIAS.contains(&a.as_str()));
        if alias.is_some() {
            k += 1;
        }
        let derived = if owner.is_none() { ctes.get(&name).cloned() } else { None };
        out.push(TableRef { owner, name: Some(name), alias, derived, level: 0 });
        i = k;
    }
    out
}

fn last_joined(block_before: &[&Tok], ctes: &HashMap<String, Vec<String>>) -> Option<TableRef> {
    let j = block_before.iter().rposition(|t| is_w(Some(t), "JOIN"))?;
    let names: Vec<Tok> = block_before[j + 1..]
        .iter()
        .take_while(|t| !is_w(Some(t), "ON"))
        .map(|t| (*t).clone())
        .collect();
    if names.is_empty() {
        return None;
    }
    Some(table_from_tokens(&names, ctes))
}

/// 블록 안 커서 앞 토큰들로 구절을 정한다
fn clause_ctx(bt: &[&Tok], in_paren: bool) -> Ctx {
    let Some(last) = bt.last() else { return Ctx::Start };
    // 마지막 구절 키워드
    let mut clause = "";
    let mut ci = 0;
    for (i, t) in bt.iter().enumerate().rev() {
        let w = word(t).unwrap_or("");
        let c = match w {
            "SELECT" | "FROM" | "WHERE" | "HAVING" | "ON" | "SET" | "VALUES" | "INTO" | "UPDATE"
            | "JOIN" | "USING" | "RETURNING" | "WHEN" | "THEN" | "ELSE" => w,
            "BY" => match bt.get(i.wrapping_sub(1)).and_then(|x| word(x)) {
                Some("GROUP") => "GROUP BY",
                Some("ORDER") => "ORDER BY",
                Some("CONNECT") => "CONNECT BY",
                Some("PARTITION") => "PARTITION BY",
                _ => "",
            },
            "WITH" if bt.get(i.wrapping_sub(1)).and_then(|x| word(x)) == Some("START") => "START WITH",
            "DELETE" | "INSERT" | "MERGE" | "TRUNCATE" => w,
            "EXEC" | "EXECUTE" | "CALL" => "EXEC",
            _ => "",
        };
        if !c.is_empty() {
            clause = c;
            ci = i;
            break;
        }
    }
    let since: Vec<&&Tok> = bt[ci + 1..].iter().collect();
    let lw = word(last).unwrap_or("");
    match clause {
        "" => {
            if bt.len() <= 1 && matches!(lw, "") {
                Ctx::Start
            } else {
                Ctx::Expr("")
            }
        }
        "EXEC" => Ctx::Procedure,
        "FROM" | "JOIN" | "INTO" | "UPDATE" | "USING" | "DELETE" | "TRUNCATE" => {
            let join = clause == "JOIN";
            // 마지막 쉼표 뒤
            let items: Vec<&&&Tok> = since.iter().rev().take_while(|t| t.t != T::P(',')).collect();
            if clause == "FROM" && since.iter().any(|t| t.t == T::P(',')) && items.is_empty() {
                return Ctx::Table { join: false };
            }
            let idents = items.iter().filter(|t| matches!(t.t, T::Word(_) | T::QWord(_))).count();
            let dotted = items.iter().any(|t| t.t == T::P('.'));
            if clause == "DELETE" && since.is_empty() {
                return Ctx::Table { join: false };
            }
            if clause == "TRUNCATE" {
                return if is_w(Some(last), "TABLE") { Ctx::Table { join: false } } else { Ctx::Start };
            }
            if since.is_empty() || (dotted && idents == 1 && last.t == T::P('.')) {
                Ctx::Table { join }
            } else if clause == "INTO" && in_paren {
                Ctx::InsertColumns
            } else {
                Ctx::AfterTable { in_from: clause == "FROM" || clause == "JOIN" }
            }
        }
        "ON" => Ctx::Expr("ON"),
        "SELECT" => Ctx::Expr("SELECT"),
        "WHERE" => Ctx::Expr("WHERE"),
        "HAVING" => Ctx::Expr("HAVING"),
        "SET" => Ctx::Expr("SET"),
        "VALUES" => Ctx::Expr("VALUES"),
        "GROUP BY" => Ctx::Expr("GROUP BY"),
        "ORDER BY" => Ctx::Expr("ORDER BY"),
        "CONNECT BY" => Ctx::Expr("CONNECT BY"),
        "START WITH" => Ctx::Expr("START WITH"),
        "PARTITION BY" => Ctx::Expr("SELECT"),
        "WHEN" | "THEN" | "ELSE" => Ctx::Expr("SELECT"),
        "RETURNING" => Ctx::Expr("SELECT"),
        "INSERT" | "MERGE" => Ctx::Start,
        _ => Ctx::Expr(""),
    }
}

// ─────────────────────────────────────────────────────────────
// 후보
// ─────────────────────────────────────────────────────────────

pub const KEYWORDS: &[&str] = &[
    "SELECT", "FROM", "WHERE", "AND", "OR", "NOT", "IN", "EXISTS", "BETWEEN", "LIKE", "IS", "NULL",
    "GROUP", "BY", "ORDER", "HAVING", "ASC", "DESC", "NULLS", "FIRST", "LAST", "DISTINCT",
    "JOIN", "INNER", "LEFT", "RIGHT", "FULL", "OUTER", "CROSS", "ON", "USING", "UNION", "ALL",
    "INTERSECT", "MINUS", "INSERT", "INTO", "VALUES", "UPDATE", "SET", "DELETE", "MERGE", "WHEN",
    "MATCHED", "THEN", "ELSE", "END", "CASE", "AS", "WITH", "CONNECT", "START", "PRIOR", "LEVEL",
    "ROWNUM", "ROWID", "SYSDATE", "SYSTIMESTAMP", "USER", "DUAL", "CREATE", "ALTER", "DROP",
    "TRUNCATE", "TABLE", "VIEW", "INDEX", "SEQUENCE", "BEGIN", "DECLARE", "EXCEPTION", "COMMIT",
    "ROLLBACK", "FOR", "OVER", "PARTITION", "EXEC", "EXECUTE", "CALL", "RETURNING", "LOOP",
    "ANY", "SOME", "ESCAPE", "NOCYCLE", "SIBLINGS",
];

const FUNCTIONS: &[(&str, &str)] = &[
    ("NVL", "NVL(expr, 대체값)"), ("NVL2", "NVL2(expr, NULL 아닐 때, NULL 일 때)"),
    ("DECODE", "DECODE(expr, 값1, 결과1, ..., 기본값)"), ("COALESCE", "COALESCE(expr1, expr2, ...)"),
    ("TO_CHAR", "TO_CHAR(값, 'YYYY-MM-DD')"), ("TO_DATE", "TO_DATE('문자', 'YYYY-MM-DD')"),
    ("TO_NUMBER", "TO_NUMBER(문자)"), ("TRUNC", "TRUNC(날짜|숫자 [, 단위])"), ("ROUND", "ROUND(숫자 [, 자리])"),
    ("SUBSTR", "SUBSTR(문자, 시작 [, 길이])"), ("INSTR", "INSTR(문자, 찾을 문자)"), ("LENGTH", "LENGTH(문자)"),
    ("UPPER", "UPPER(문자)"), ("LOWER", "LOWER(문자)"), ("TRIM", "TRIM(문자)"), ("LPAD", "LPAD(문자, 길이, 채울 문자)"),
    ("RPAD", "RPAD(문자, 길이, 채울 문자)"), ("REPLACE", "REPLACE(문자, 찾을, 바꿀)"),
    ("REGEXP_LIKE", "REGEXP_LIKE(문자, 패턴)"), ("REGEXP_SUBSTR", "REGEXP_SUBSTR(문자, 패턴)"),
    ("REGEXP_REPLACE", "REGEXP_REPLACE(문자, 패턴, 바꿀)"),
    ("COUNT", "COUNT(*)"), ("SUM", "SUM(expr)"), ("AVG", "AVG(expr)"), ("MIN", "MIN(expr)"), ("MAX", "MAX(expr)"),
    ("LISTAGG", "LISTAGG(expr, ',') WITHIN GROUP (ORDER BY ...)  — 11.2+"),
    ("ROW_NUMBER", "ROW_NUMBER() OVER (PARTITION BY ... ORDER BY ...)"), ("RANK", "RANK() OVER (...)"),
    ("DENSE_RANK", "DENSE_RANK() OVER (...)"), ("LAG", "LAG(expr [, n]) OVER (ORDER BY ...)"),
    ("LEAD", "LEAD(expr [, n]) OVER (ORDER BY ...)"), ("ADD_MONTHS", "ADD_MONTHS(날짜, 개월)"),
    ("MONTHS_BETWEEN", "MONTHS_BETWEEN(날짜1, 날짜2)"), ("LAST_DAY", "LAST_DAY(날짜)"),
    ("GREATEST", "GREATEST(a, b, ...)"), ("LEAST", "LEAST(a, b, ...)"), ("ABS", "ABS(숫자)"), ("MOD", "MOD(a, b)"),
    ("CAST", "CAST(expr AS 형식)"), ("EXTRACT", "EXTRACT(YEAR FROM 날짜)"), ("SYS_GUID", "SYS_GUID()"),
];

/// 구절 다음에 올 만한 키워드 (앞쪽이 먼저)
fn next_keywords(ctx: &Ctx) -> &'static [&'static str] {
    match ctx {
        Ctx::Start => &[
            "SELECT", "INSERT INTO", "UPDATE", "DELETE FROM", "MERGE INTO", "WITH", "CREATE", "ALTER",
            "DROP", "TRUNCATE TABLE", "BEGIN", "DECLARE", "EXEC", "COMMIT", "ROLLBACK", "COMMENT ON",
        ],
        Ctx::AfterTable { in_from: true } => &[
            "WHERE", "JOIN", "LEFT OUTER JOIN", "INNER JOIN", "ON", "GROUP BY", "ORDER BY",
            "CONNECT BY", "START WITH", "UNION ALL", "UNION", "MINUS", "FOR UPDATE",
        ],
        Ctx::AfterTable { in_from: false } => &["SET", "VALUES", "SELECT", "WHERE", "USING", "("],
        Ctx::Expr("SELECT") => &["FROM", "DISTINCT", "CASE", "AS", "ROWNUM", "SYSDATE", "LEVEL", "OVER"],
        Ctx::Expr("WHERE") | Ctx::Expr("HAVING") | Ctx::Expr("ON") | Ctx::JoinOn => &[
            "AND", "OR", "NOT", "IN", "EXISTS", "BETWEEN", "LIKE", "IS NULL", "IS NOT NULL",
            "GROUP BY", "ORDER BY", "ROWNUM", "SYSDATE", "JOIN", "LEFT OUTER JOIN",
        ],
        Ctx::Expr("GROUP BY") => &["HAVING", "ORDER BY", "ROLLUP", "CUBE"],
        Ctx::Expr("ORDER BY") => &["ASC", "DESC", "NULLS FIRST", "NULLS LAST"],
        Ctx::Expr("SET") => &["WHERE", "SYSDATE"],
        Ctx::Expr("CONNECT BY") => &["PRIOR", "NOCYCLE", "LEVEL", "START WITH", "ORDER SIBLINGS BY"],
        Ctx::Expr("START WITH") => &["CONNECT BY", "PRIOR"],
        Ctx::Expr("VALUES") => &["SYSDATE", "NULL"],
        _ => &["AND", "OR", "FROM", "WHERE"],
    }
}

/// 이름 일치 점수. 없으면 None.
fn match_score(label: &str, prefix: &str) -> Option<i32> {
    if prefix.is_empty() {
        return Some(0);
    }
    let l = label.to_uppercase();
    let p = prefix.to_uppercase();
    if l == p {
        return Some(1100);
    }
    if l.starts_with(&p) {
        return Some(1000 - (l.len() as i32 - p.len() as i32).min(200));
    }
    // 단어 경계 (ORD → CUST_ORD_ITEM)
    if l.split(['_', '$', '#', '.', ' ']).skip(1).any(|part| part.starts_with(&p)) {
        return Some(600);
    }
    if p.len() >= 2 && l.contains(&p) {
        return Some(400);
    }
    // 머리글자 (COI → CUST_ORD_ITEM)
    if p.len() >= 2 {
        let initials: String = l.split(['_', '$', '#']).filter_map(|s| s.chars().next()).collect();
        if initials.starts_with(&p) {
            return Some(500);
        }
    }
    None
}

/// [`match_score`] 의 할당 없는 판. `p` 는 대문자로 바꾼 입력, `label` 은 사전의 이름(대개 대문자).
fn fast_score(label: &str, p: &str) -> Option<i32> {
    if p.is_empty() {
        return Some(0);
    }
    let lb = label.as_bytes();
    let pb = p.as_bytes();
    if lb.len() >= pb.len() && lb[..pb.len()].eq_ignore_ascii_case(pb) {
        if lb.len() == pb.len() {
            return Some(1100);
        }
        return Some(1000 - (lb.len() as i32 - pb.len() as i32).min(200));
    }
    let sep = |c: u8| c == b'_' || c == b'$' || c == b'#' || c == b'.' || c == b' ';
    // 단어 경계 / 머리글자
    let mut initials_ok = pb.len() >= 2;
    let mut ii = 0;
    let mut boundary = false;
    for (i, &c) in lb.iter().enumerate() {
        let at_start = i == 0 || sep(lb[i - 1]);
        if at_start && !sep(c) {
            if i > 0 && lb.len() - i >= pb.len() && lb[i..i + pb.len()].eq_ignore_ascii_case(pb) {
                boundary = true;
                break;
            }
            if initials_ok && ii < pb.len() {
                if c.eq_ignore_ascii_case(&pb[ii]) {
                    ii += 1;
                } else if ii < pb.len() {
                    initials_ok = false;
                }
            }
        }
    }
    if boundary {
        return Some(600);
    }
    if initials_ok && ii == pb.len() {
        return Some(500);
    }
    if pb.len() >= 2 && lb.windows(pb.len()).any(|w| w.eq_ignore_ascii_case(pb)) {
        return Some(400);
    }
    None
}

fn hash_key(label: &str, kind: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for b in label.bytes() {
        b.to_ascii_uppercase().hash(&mut h);
    }
    kind.hash(&mut h);
    h.finish()
}

struct Out<'a> {
    prefix: &'a str,
    /// 대문자로 바꾼 입력 (한 번만 만든다)
    upper: String,
    lower: bool,
    items: Vec<Item>,
    seen: HashSet<u64>,
    limit: usize,
    /// 이보다 낮은 점수는 만들지 않는다 (상위 limit 개만 남기므로)
    floor: i32,
}

impl<'a> Out<'a> {
    fn case(&self, s: &str) -> String {
        // 따옴표가 필요한 이름(소문자 포함)은 그대로
        if s.chars().any(|c| c.is_lowercase()) {
            return format!("\"{s}\"");
        }
        if self.lower { s.to_lowercase() } else { s.to_string() }
    }

    /// 이미 만든 글자 (스니펫, 조인 조건 등 — 개수가 적다)
    fn push(&mut self, label: String, kind: &'static str, base: i32, detail: Option<String>, info: Option<String>, apply: Option<String>) {
        let Some(m) = match_score(&label, self.prefix) else { return };
        if !self.seen.insert(hash_key(&label, kind)) {
            return;
        }
        self.items.push(Item { label, kind, detail, info, apply, boost: 0, score: m + base });
    }

    /// 사전의 이름 — 수만 개를 훑으므로, 맞는 것만 글자를 만든다
    fn name(
        &mut self,
        name: &str,
        kind: &'static str,
        base: i32,
        detail: impl FnOnce() -> Option<String>,
        info: Option<&String>,
    ) {
        let Some(m) = fast_score(name, &self.upper) else { return };
        if m + base < self.floor || !self.seen.insert(hash_key(name, kind)) {
            return;
        }
        let label = self.case(name);
        self.items.push(Item { label, kind, detail: detail(), info: info.cloned(), apply: None, boost: 0, score: m + base });
        self.prune();
    }

    /// 후보가 많이 쌓이면 상위 limit 개만 남기고, 그 밑의 점수는 이후에 만들지도 않는다
    fn prune(&mut self) {
        if self.items.len() < self.limit * 4 {
            return;
        }
        self.items.sort_unstable_by(|x, y| y.score.cmp(&x.score));
        self.items.truncate(self.limit);
        self.floor = self.items.last().map(|i| i.score).unwrap_or(i32::MIN);
    }
}

/// 분석 결과와 캐시로 후보를 만든다. 캐시에 없어 못 만든 것은 `missing` 에 담는다.
pub fn suggest(a: &Analysis, cache: &SchemaCache, limit: usize) -> Completion {
    let mut out = Out {
        prefix: &a.prefix,
        upper: a.prefix.to_uppercase(),
        lower: a.lower,
        items: Vec::new(),
        seen: HashSet::new(),
        limit: limit.max(1),
        floor: i32::MIN,
    };
    let mut missing: Vec<Missing> = Vec::new();

    // 별칭 → 테이블
    let resolve_ref = |r: &TableRef| -> Option<Key> {
        if r.derived.is_some() {
            return None;
        }
        cache.resolve(r.owner.as_deref(), r.name.as_deref()?)
    };
    let ref_cols = |r: &TableRef, missing: &mut Vec<Missing>| -> Option<Vec<(String, Option<String>, Option<String>)>> {
        if let Some(d) = &r.derived {
            return Some(d.iter().map(|c| (c.clone(), None, None)).collect());
        }
        let key = resolve_ref(r)?;
        match cache.cols(&key) {
            Some(cols) => Some(cols.iter().map(|c| (c.name.clone(), Some(c.data_type.clone()), c.comment.clone())).collect()),
            None => {
                let m = Missing::Columns { owner: key.0.clone(), table: key.1.clone() };
                if !missing.contains(&m) {
                    missing.push(m);
                }
                None
            }
        }
    };

    match &a.ctx {
        Ctx::None => {
            return Completion { from: a.from, items: vec![], context: "none".into(), missing };
        }
        Ctx::Qualified(parts) => {
            let q = parts.last().unwrap().clone();
            let owner = if parts.len() >= 2 { Some(parts[parts.len() - 2].clone()) } else { None };
            // 1) 별칭/테이블 이름 (가까운 블록 먼저)
            let mut refs: Vec<&TableRef> = a.refs.iter().collect();
            refs.sort_by_key(|r| r.level);
            let hit = refs.iter().find(|r| {
                owner.is_none() && (r.alias.as_deref() == Some(q.as_str()) || (r.alias.is_none() && r.name.as_deref() == Some(q.as_str())))
            });
            let table_ref = hit.map(|r| (*r).clone()).or_else(|| {
                // 2) 범위 밖이지만 이름이 테이블인 경우 (EMP.col), 3) OWNER.TABLE.
                let key = cache.resolve(owner.as_deref(), &q)?;
                let kind = cache.kind_of(&key);
                if owner.is_some() || kind.map(|k| k.table_like()).unwrap_or(false) {
                    Some(TableRef { owner: Some(key.0), name: Some(key.1), alias: None, derived: None, level: 0 })
                } else {
                    None
                }
            });
            if let Some(r) = table_ref {
                if let Some(cols) = ref_cols(&r, &mut missing) {
                    let pk = resolve_ref(&r).and_then(|k| cache.pks.get(&k).cloned()).unwrap_or_default();
                    for (i, (c, ty, cm)) in cols.iter().enumerate() {
                        let base = 300 - i as i32 + if pk.contains(c) { 20 } else { 0 };
                        out.name(c, "column", base, || ty.clone(), cm.as_ref());
                    }
                }
            } else if parts.len() == 1 {
                // 시퀀스 / 패키지 / 스키마
                let key = cache.resolve(None, &q);
                let kind = key.as_ref().and_then(|k| cache.kind_of(k));
                match kind {
                    Some(ObjKind::Sequence) => {
                        for (i, s) in ["NEXTVAL", "CURRVAL"].iter().enumerate() {
                            let l = out.case(s);
                            out.push(l, "keyword", 300 - i as i32, Some("시퀀스".into()), None, None);
                        }
                    }
                    Some(ObjKind::Package) | None if key.is_some() || cache.synonyms.contains_key(&q) => {
                        let key = key.unwrap();
                        match cache.package_members.get(&key) {
                            Some(m) => {
                                for p in m.iter() {
                                    let l = out.case(p);
                                    out.push(l, "procedure", 300, Some(format!("{}.{}", key.0, key.1)), None, None);
                                }
                            }
                            None => missing.push(Missing::Package { owner: key.0, name: key.1 }),
                        }
                    }
                    _ => {}
                }
                // 스키마 이름
                if cache.schemas.iter().any(|s| s == &q) || q == cache.user {
                    if q == cache.user {
                        for o in &cache.objects {
                            out.name(&o.name, kind_name(o.kind), 200, || None, o.comment.as_ref());
                        }
                    } else {
                        match cache.schema_objects.get(&q) {
                            Some(objs) => {
                                for o in objs.iter() {
                                    out.name(&o.name, kind_name(o.kind), 200, || Some(q.clone()), o.comment.as_ref());
                                }
                            }
                            None => missing.push(Missing::Schema { owner: q.clone() }),
                        }
                    }
                }
            }
        }
        Ctx::Table { join } => {
            // CTE
            for name in a.ctes.keys() {
                let l = out.case(name);
                out.push(l, "table", 450, Some("WITH".into()), None, None);
            }
            // 조인이면 이미 있는 테이블과 FK 로 이어진 테이블을 먼저
            let mut related: HashSet<Key> = HashSet::new();
            if *join {
                let in_scope: Vec<Key> = a.refs.iter().filter_map(|r| resolve_ref(r)).collect();
                for fk in &cache.fks {
                    let a_key = (fk.owner.clone(), fk.table.clone());
                    let b_key = (fk.r_owner.clone(), fk.r_table.clone());
                    if in_scope.contains(&a_key) {
                        related.insert(b_key.clone());
                    }
                    if in_scope.contains(&b_key) {
                        related.insert(a_key);
                    }
                }
            }
            for o in &cache.objects {
                if !o.kind.table_like() {
                    continue;
                }
                let base = match o.kind {
                    ObjKind::Table => 300,
                    ObjKind::View => 280,
                    _ => 250,
                } + if related.contains(&(o.owner.clone(), o.name.clone())) { 250 } else { 0 };
                out.name(&o.name, kind_name(o.kind), base, || Some(kind_label(o.kind).into()), o.comment.as_ref());
            }
            // 공개 동의어 — 앱 스키마를 가리키는 것만 앞에, 시스템 것은 뒤로
            if !a.prefix.is_empty() {
                for (name, key, system) in &cache.public_synonyms {
                    // 오라클 기본 동의어(수천 개)는 앞글자가 맞을 때만 — 중간 일치는 잡음이다
                    if *system && fast_score(name, &out.upper).map_or(true, |m| m < 1000) {
                        continue;
                    }
                    let base = if *system { -150 } else { 220 };
                    out.name(name, "synonym", base, || Some(format!("→ {}.{}", key.0, key.1)), None);
                }
            }
            for s in &cache.schemas {
                if is_system_schema(s) {
                    continue;
                }
                out.name(s, "schema", 100, || Some("스키마".into()), None);
            }
        }
        Ctx::InsertColumns => {
            if let Some(t) = &a.target {
                if let Some(cols) = ref_cols(t, &mut missing) {
                    let all: Vec<String> = cols.iter().map(|c| out.case(&c.0)).collect();
                    if a.prefix.is_empty() {
                        let l = all.join(", ");
                        out.push(l, "snippet", 900, Some("모든 컬럼".into()), None, None);
                    }
                    for (i, (c, ty, cm)) in cols.iter().enumerate() {
                        out.name(c, "column", 400 - i as i32, || ty.clone(), cm.as_ref());
                    }
                }
            }
        }
        Ctx::Procedure => {
            for o in &cache.objects {
                if matches!(o.kind, ObjKind::Procedure | ObjKind::Package | ObjKind::Function) {
                    out.name(&o.name, kind_name(o.kind), 300, || Some(kind_label(o.kind).into()), None);
                }
            }
            if !a.prefix.is_empty() {
                for (name, key, system) in &cache.public_synonyms {
                    if name.starts_with("DBMS_") || name.starts_with("UTL_") || !*system {
                        out.name(name, "package", 150, || Some(format!("→ {}.{}", key.0, key.1)), None);
                    }
                }
            }
        }
        Ctx::Start | Ctx::AfterTable { .. } => {}
        Ctx::Expr(_) | Ctx::JoinOn => {
            // 조인 조건 (FK → 같은 이름의 PK 컬럼)
            if a.ctx == Ctx::JoinOn {
                if let Some(t) = &a.target {
                    join_conditions(&mut out, t, &a.refs, cache, &resolve_ref);
                }
            }
            // 범위의 테이블 컬럼
            let mut scope: Vec<&TableRef> = a.refs.iter().collect();
            scope.sort_by_key(|r| r.level);
            let multi = scope.iter().filter(|r| r.level == 0).count() > 1;
            // 이름이 둘 이상 테이블에 있으면 별칭을 붙인다
            let mut col_owner: HashMap<String, usize> = HashMap::new();
            let mut per_ref: Vec<(&TableRef, Vec<(String, Option<String>, Option<String>)>)> = Vec::new();
            for r in &scope {
                if let Some(cols) = ref_cols(r, &mut missing) {
                    for c in &cols {
                        *col_owner.entry(c.0.clone()).or_default() += 1;
                    }
                    per_ref.push((r, cols));
                }
            }
            for (r, cols) in &per_ref {
                let shown = r.shown();
                let pk = resolve_ref(r).and_then(|k| cache.pks.get(&k).cloned()).unwrap_or_default();
                for (i, (c, ty, cm)) in cols.iter().enumerate() {
                    let ambiguous = col_owner.get(c).copied().unwrap_or(0) > 1;
                    let base = 350 - (r.level as i32 * 120) - (i as i32).min(100) + if pk.contains(c) { 15 } else { 0 };
                    let label = out.case(c);
                    let apply = (ambiguous && !shown.is_empty()).then(|| format!("{}.{}", out.case(&shown), label));
                    let detail = Some(match ty {
                        Some(t) if multi || r.level > 0 => format!("{t} · {shown}"),
                        Some(t) => t.clone(),
                        None => shown.clone(),
                    });
                    out.push(label, "column", base, detail, cm.clone(), apply);
                }
            }
            // 별칭 자체
            for r in &scope {
                if let Some(al) = &r.alias {
                    let l = out.case(al);
                    let detail = r.name.clone().unwrap_or_else(|| "인라인 뷰".into());
                    out.push(l, "alias", 280, Some(detail), None, None);
                }
            }
            // SELECT 목록: "모든 컬럼" 펼치기
            if a.ctx == Ctx::Expr("SELECT") && a.prefix.is_empty() {
                for (r, cols) in &per_ref {
                    if r.level != 0 || cols.is_empty() {
                        continue;
                    }
                    let q = if multi { format!("{}.", out.case(&r.shown())) } else { String::new() };
                    let l = cols.iter().map(|c| format!("{q}{}", out.case(&c.0))).collect::<Vec<_>>().join(", ");
                    out.push(l, "snippet", 120, Some(format!("{} 의 모든 컬럼", r.shown())), None, None);
                }
            }
            // 함수
            for (f, sig) in FUNCTIONS {
                let l = out.case(f);
                let apply = Some(format!("{l}("));
                out.push(l, "function", 120, Some((*sig).into()), None, apply);
            }
        }
    }

    // 다음 키워드 (점 뒤, 컬럼 목록 안에서는 키워드가 올 수 없다)
    let kws: &[&str] = if matches!(a.ctx, Ctx::Qualified(_) | Ctx::InsertColumns) { &[] } else { next_keywords(&a.ctx) };
    for (i, k) in kws.iter().enumerate() {
        let l = out.case(k);
        out.push(l, "keyword", 200 - i as i32 * 5 - if matches!(a.ctx, Ctx::Start | Ctx::AfterTable { .. }) { 0 } else { 150 }, None, None, None);
    }

    let mut items = out.items;
    items.sort_by(|x, y| y.score.cmp(&x.score).then_with(|| x.label.cmp(&y.label)));
    items.truncate(limit);
    // 화면 정렬 가중치: 점수 순위를 -99..99 로
    let n = items.len().max(1) as i32;
    for (i, it) in items.iter_mut().enumerate() {
        it.boost = 99 - (i as i32 * 198 / n);
    }
    Completion { from: a.from, items, context: ctx_name(&a.ctx), missing }
}

fn join_conditions(
    out: &mut Out,
    t: &TableRef,
    refs: &[TableRef],
    cache: &SchemaCache,
    resolve_ref: &dyn Fn(&TableRef) -> Option<Key>,
) {
    let Some(tk) = resolve_ref(t) else { return };
    let t_alias = out.case(&t.shown());
    let mut made = 0;
    for r in refs.iter().filter(|r| r.level == 0) {
        let Some(rk) = resolve_ref(r) else { continue };
        if rk == tk && r.shown() == t.shown() {
            continue;
        }
        let r_alias = out.case(&r.shown());
        for fk in &cache.fks {
            let pairs: Option<(&str, &str, &[String], &[String])> = if (fk.owner.as_str(), fk.table.as_str()) == (tk.0.as_str(), tk.1.as_str())
                && (fk.r_owner.as_str(), fk.r_table.as_str()) == (rk.0.as_str(), rk.1.as_str())
            {
                Some((t_alias.as_str(), r_alias.as_str(), &fk.cols, &fk.r_cols))
            } else if (fk.owner.as_str(), fk.table.as_str()) == (rk.0.as_str(), rk.1.as_str())
                && (fk.r_owner.as_str(), fk.r_table.as_str()) == (tk.0.as_str(), tk.1.as_str())
            {
                Some((t_alias.as_str(), r_alias.as_str(), &fk.r_cols, &fk.cols))
            } else {
                None
            };
            if let Some((ta, ra, tc, rc)) = pairs {
                let cond = tc
                    .iter()
                    .zip(rc.iter())
                    .map(|(a, b)| format!("{ta}.{} = {ra}.{}", out.case(a), out.case(b)))
                    .collect::<Vec<_>>()
                    .join(&format!(" {} ", out.case("AND")));
                out.push(cond, "join", 700, Some("외래 키".into()), None, None);
                made += 1;
            }
        }
        // FK 가 없으면: 한쪽 PK 와 이름이 같은 컬럼
        if made == 0 {
            let (Some(tc), Some(rc)) = (cache.cols(&tk), cache.cols(&rk)) else { continue };
            let pk_t = cache.pks.get(&tk).cloned().unwrap_or_default();
            let pk_r = cache.pks.get(&rk).cloned().unwrap_or_default();
            for c in tc.iter() {
                if rc.iter().any(|x| x.name == c.name) && (pk_t.contains(&c.name) || pk_r.contains(&c.name)) {
                    let n = out.case(&c.name);
                    let cond = format!("{t_alias}.{n} = {r_alias}.{n}");
                    out.push(cond, "join", 600, Some("같은 이름 컬럼".into()), None, None);
                }
            }
        }
    }
}

fn kind_name(k: ObjKind) -> &'static str {
    match k {
        ObjKind::Table => "table",
        ObjKind::View => "view",
        ObjKind::Synonym => "synonym",
        ObjKind::Sequence => "sequence",
        ObjKind::Package => "package",
        ObjKind::Procedure => "procedure",
        ObjKind::Function => "function",
        ObjKind::Type => "type",
    }
}

fn kind_label(k: ObjKind) -> &'static str {
    match k {
        ObjKind::Table => "테이블",
        ObjKind::View => "뷰",
        ObjKind::Synonym => "동의어",
        ObjKind::Sequence => "시퀀스",
        ObjKind::Package => "패키지",
        ObjKind::Procedure => "프로시저",
        ObjKind::Function => "함수",
        ObjKind::Type => "타입",
    }
}

fn ctx_name(c: &Ctx) -> String {
    match c {
        Ctx::None => "none".into(),
        Ctx::Start => "start".into(),
        Ctx::Table { join: true } => "table(join)".into(),
        Ctx::Table { join: false } => "table".into(),
        Ctx::AfterTable { .. } => "after_table".into(),
        Ctx::Expr(s) => format!("expr({})", s.to_lowercase()),
        Ctx::JoinOn => "join_on".into(),
        Ctx::Qualified(p) => format!("qualified({})", p.join(".")),
        Ctx::InsertColumns => "insert_columns".into(),
        Ctx::Procedure => "procedure".into(),
    }
}

/// 한 번에: 분석 + 후보
pub fn complete(src: &str, cursor: usize, cache: &SchemaCache, limit: usize) -> Completion {
    suggest(&analyze(src, cursor), cache, limit)
}

pub mod load;

#[cfg(test)]
mod tests;
