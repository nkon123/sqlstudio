//! 단위를 작은 모델에 줄 조각으로 나눈다.
//!
//! - 서브프로그램 하나 = 조각 하나가 기본. 중첩 서브프로그램은 따로 조각이 되고, 바깥 조각에는 한 줄 표시만 남긴다.
//! - 한 서브프로그램이 한도(줄·글자)를 넘으면 본문 문장 경계에서 자른다. 바깥 깊이(본문 바로 안의 문장)에서
//!   먼저 자르고, 한 문장(긴 IF/LOOP)이 한도를 넘으면 그 안의 깊이로 내려간다. 끝까지 안 되면 줄 수로 자른다.
//! - 2부 이후에는 머리(시그니처)와 짧은 선언부를 문맥으로 붙인다.
//! - 패키지 전역 선언(형식·상수·커서·전역 변수)은 따로 조각이 된다.

use std::collections::{BTreeMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::facts::{self, Facts};
use crate::flow;
use crate::plsql::{self, Structure, SubKind, Subprogram, Tok};

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Limits {
    /// 조각 하나의 최대 줄 수
    pub max_lines: u32,
    /// 조각 하나의 최대 글자 수 (한글 주석이 많으면 줄보다 이쪽이 먼저 찬다)
    pub max_chars: usize,
}

impl Default for Limits {
    fn default() -> Self {
        // 8K 컨텍스트 모델에서 지시문 + 답을 넣고도 남는 크기
        Self { max_lines: 120, max_chars: 6000 }
    }
}

/// 분석할 소스 하나
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnitSource {
    pub owner: String,
    pub name: String,
    /// PACKAGE BODY / PROCEDURE / FUNCTION / TRIGGER / TYPE BODY / PACKAGE / TYPE
    pub unit_type: String,
    pub text: String,
}

impl UnitSource {
    /// 저장 폴더·색인에 쓰는 키 "OWNER.NAME.PACKAGE_BODY"
    pub fn key(&self) -> String {
        unit_key(&self.owner, &self.name, &self.unit_type)
    }
}

pub fn unit_key(owner: &str, name: &str, unit_type: &str) -> String {
    format!("{owner}.{name}.{}", unit_type.replace(' ', "_"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChunkKind {
    /// 서브프로그램 전체
    Whole,
    /// 서브프로그램의 일부 (part / parts)
    Part,
    /// 패키지 전역 선언
    Globals,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Chunk {
    /// 단위 안에서 유일, 파일 이름으로 쓴다 — "004-CALC_TOTAL-p2"
    pub id: String,
    pub kind: ChunkKind,
    /// 서브프로그램 경로 (전역이면 None)
    pub subprogram: Option<String>,
    pub overload: u32,
    pub sub_kind: Option<SubKind>,
    pub part: u32,
    pub parts: u32,
    pub start_line: u32,
    pub end_line: u32,
    pub signature: String,
    /// 2부 이후에 붙이는 문맥 (선언부), 명세 주석
    pub context: String,
    /// 모델에 줄 코드 — 줄 번호가 붙어 있다
    pub code: String,
    pub facts: Facts,
    /// code + context + signature 의 해시 (바뀌지 않은 조각은 다시 분석하지 않는다)
    pub hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Plan {
    pub key: String,
    pub owner: String,
    pub name: String,
    pub unit_type: String,
    pub lines: u32,
    /// 소스 전체 해시
    pub hash: String,
    pub structure: Structure,
    pub chunks: Vec<Chunk>,
}

/// FNV-1a 64 — Rust 버전이 바뀌어도 같은 값 (저장된 해시와 비교하므로)
pub fn fnv(parts: &[&str]) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for p in parts {
        for b in p.as_bytes() {
            h ^= *b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
        h ^= 0xff;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{h:016x}")
}

fn safe(s: &str) -> String {
    let s: String = s
        .chars()
        .map(|c| if c.is_alphanumeric() || matches!(c, '_' | '$' | '#' | '.') { c } else { '_' })
        .collect();
    s.chars().take(60).collect()
}

struct Ctx<'a> {
    lines: Vec<&'a str>,
    toks: &'a [Tok],
    lim: Limits,
}

impl Ctx<'_> {
    fn text(&self, l: u32) -> &str {
        self.lines.get(l as usize - 1).copied().unwrap_or("")
    }

    /// [lo, hi] 중 빠진 줄(자식)을 뺀 크기 (줄, 글자)
    fn size(&self, lo: u32, hi: u32, skip: &[(u32, u32)]) -> (u32, usize) {
        let mut n = 0;
        let mut c = 0;
        let mut l = lo;
        while l <= hi {
            if let Some(&(_, b)) = skip.iter().find(|(a, b)| *a <= l && l <= *b) {
                n += 1;
                c += 60;
                l = b + 1;
                continue;
            }
            n += 1;
            c += self.text(l).trim_end().chars().count() + 7;
            l += 1;
        }
        (n, c)
    }

    fn fits(&self, lo: u32, hi: u32, skip: &[(u32, u32)]) -> bool {
        let (n, c) = self.size(lo, hi, skip);
        n <= self.lim.max_lines && c <= self.lim.max_chars
    }

    /// 줄 번호를 붙인 코드. 자식 범위는 한 줄 표시로 바꾼다.
    fn render(&self, lo: u32, hi: u32, skip: &[(u32, u32, String)]) -> String {
        let mut out = String::new();
        let mut l = lo;
        while l <= hi {
            if let Some((_, b, label)) = skip.iter().find(|(a, b, _)| *a <= l && l <= *b) {
                out.push_str(&format!("{l:>5}| -- ▶ {label} ({l}~{b}행, 따로 분석)\n"));
                l = b + 1;
                continue;
            }
            out.push_str(&format!("{l:>5}| {}\n", self.text(l).trim_end().replace('\t', "    ")));
            l += 1;
        }
        out
    }

    /// 줄 범위 안(자식 제외)의 토큰
    fn tokens(&self, lo: u32, hi: u32, skip: &[(u32, u32)]) -> Vec<Tok> {
        self.toks
            .iter()
            .filter(|t| t.line >= lo && t.line <= hi && !skip.iter().any(|(a, b)| *a <= t.line && t.line <= *b))
            .cloned()
            .collect()
    }

    /// [lo, hi] 를 한도 안 조각들로. `cuts` = (자를 수 있는 줄, 깊이) — 그 줄 다음에서 자를 수 있다.
    fn split(&self, lo: u32, hi: u32, cuts: &[(u32, usize)], depth: usize, skip: &[(u32, u32)], out: &mut Vec<(u32, u32)>) {
        if lo > hi {
            return;
        }
        if self.fits(lo, hi, skip) {
            out.push((lo, hi));
            return;
        }
        // 이 깊이의 자를 자리로 나눈 덩어리
        let mut spans = Vec::new();
        let mut s = lo;
        for &(l, d) in cuts {
            if d == depth && l >= s && l < hi {
                spans.push((s, l));
                s = l + 1;
            }
        }
        spans.push((s, hi));
        if spans.len() == 1 {
            if depth < 10 && cuts.iter().any(|&(l, d)| d > depth && l >= lo && l < hi) {
                return self.split(lo, hi, cuts, depth + 1, skip, out);
            }
            // 더 자를 자리가 없다 — 줄 수로
            let step = self.lim.max_lines.max(10);
            let mut a = lo;
            while a <= hi {
                let mut b = (a + step - 1).min(hi);
                while b > a && !self.fits(a, b, skip) {
                    b -= (b - a).div_ceil(4).max(1);
                }
                out.push((a, b));
                a = b + 1;
            }
            return;
        }
        // 덩어리를 한도 안에서 이어 붙인다
        let mut cur: Option<(u32, u32)> = None;
        for (a, b) in spans {
            if !self.fits(a, b, skip) {
                if let Some(c) = cur.take() {
                    out.push(c);
                }
                self.split(a, b, cuts, depth + 1, skip, out);
                continue;
            }
            cur = match cur {
                None => Some((a, b)),
                Some((ca, _)) if self.fits(ca, b, skip) => Some((ca, b)),
                Some(c) => {
                    out.push(c);
                    Some((a, b))
                }
            };
        }
        if let Some(c) = cur {
            out.push(c);
        }
    }
}

/// 명세(패키지 스펙)에서 서브프로그램 이름 → 바로 위 주석
pub fn spec_docs(spec: &str) -> BTreeMap<String, String> {
    let (st, _) = plsql::structure(spec);
    let lines: Vec<&str> = spec.lines().collect();
    let mut out = BTreeMap::new();
    for d in &st.decls {
        let mut l = d.line as usize;
        let mut doc = Vec::new();
        while l > 1 {
            let prev = lines.get(l - 2).map(|s| s.trim()).unwrap_or("");
            if prev.starts_with("--") || prev.starts_with("/*") || prev.starts_with('*') || prev.ends_with("*/") {
                doc.push(prev.to_string());
                l -= 1;
            } else {
                break;
            }
        }
        // 같은 줄 끝 주석 (PROCEDURE p; -- 설명)
        if let Some(cur) = lines.get(d.line as usize - 1) {
            if let Some(k) = cur.find("--") {
                doc.insert(0, cur[k..].trim().to_string());
            }
        }
        if !doc.is_empty() {
            doc.reverse();
            out.entry(d.name.clone()).or_insert_with(|| doc.join("\n"));
        }
    }
    out
}

/// 단위 하나를 조각으로. `spec` 이 있으면(패키지 본문) 명세의 주석을 문맥에 붙인다.
pub fn plan(src: &UnitSource, spec: Option<&str>, lim: Limits) -> Plan {
    let (st, toks) = plsql::structure(&src.text);
    let cx = Ctx { lines: src.text.lines().collect(), toks: &toks, lim };
    let docs = spec.map(spec_docs).unwrap_or_default();
    let mut chunks: Vec<Chunk> = Vec::new();

    // 전역 이름 (패키지 변수·형식·커서) — 호출 후보에서 뺀다
    let global_skip: Vec<(u32, u32)> = st.subprograms.iter().filter(|s| s.parent.is_none()).map(|s| (s.doc_line, s.end_line)).collect();
    let global_toks: Vec<Tok> = toks.iter().filter(|t| !global_skip.iter().any(|(a, b)| *a <= t.line && t.line <= *b)).cloned().collect();
    let globals = facts::local_names(&global_toks);
    // 패키지 전역 커서 → 읽는 테이블 (서브프로그램이 FOR r IN c_global 로 쓸 때 잇는다)
    let global_cursors = flow::declared_cursors(&global_toks, &globals);

    // 1) 전역 선언
    let is_container = st.unit_type.ends_with("BODY") || st.unit_type == "PACKAGE" || st.unit_type == "TYPE";
    if is_container {
        let code_lines: Vec<u32> = st
            .global_lines
            .iter()
            .flat_map(|&(a, b)| a..=b)
            .filter(|&l| {
                let t = cx.text(l).trim();
                !t.is_empty() && !t.starts_with("--")
            })
            .collect();
        if code_lines.len() > 2 {
            // 연속 범위를 한도 안에서 묶는다
            let mut ranges: Vec<(u32, u32)> = Vec::new();
            for &(a, b) in &st.global_lines {
                let cuts: Vec<(u32, usize)> = (a..=b).filter(|&l| cx.text(l).trim_end().ends_with(';')).map(|l| (l, 1)).collect();
                cx.split(a, b, &cuts, 1, &[], &mut ranges);
            }
            let ranges: Vec<(u32, u32)> =
                ranges.into_iter().filter(|&(a, b)| (a..=b).any(|l| code_lines.contains(&l))).collect();
            let n = ranges.len() as u32;
            for (k, (a, b)) in ranges.into_iter().enumerate() {
                let code = cx.render(a, b, &[]);
                let tk = cx.tokens(a, b, &[]);
                let mut f = facts::extract(&tk, &globals);
                let (stmts, cursors) = flow::analyze(&tk, &globals, &flow::KnownCursors::new());
                f.statements = stmts;
                f.cursors = cursors;
                facts::absorb_dynamic(&mut f);
                let signature = format!("{} {} 전역 선언", st.unit_type, st.name);
                chunks.push(Chunk {
                    id: String::new(),
                    kind: ChunkKind::Globals,
                    subprogram: None,
                    overload: 0,
                    sub_kind: None,
                    part: k as u32 + 1,
                    parts: n,
                    start_line: a,
                    end_line: b,
                    hash: fnv(&[&code, &signature]),
                    signature,
                    context: String::new(),
                    code,
                    facts: f,
                });
            }
        }
    }

    // 2) 서브프로그램
    for (idx, s) in st.subprograms.iter().enumerate() {
        let skip: Vec<(u32, u32)> = s.children.iter().map(|&c| (st.subprograms[c].doc_line, st.subprograms[c].end_line)).collect();
        let skip_l: Vec<(u32, u32, String)> = s
            .children
            .iter()
            .map(|&c| {
                let ch = &st.subprograms[c];
                (ch.doc_line, ch.end_line, format!("중첩 {} {}", kind_word(ch.kind), ch.name))
            })
            .collect();

        // 지역 이름 = 바깥 서브프로그램들 + 자기 선언부 + 전역
        let mut locals: HashSet<String> = globals.clone();
        let mut cur = Some(idx);
        while let Some(c) = cur {
            let sp = &st.subprograms[c];
            let lo = sp.start_line;
            let hi = if sp.begin_line > 0 { sp.begin_line } else { sp.end_line };
            let sk: Vec<(u32, u32)> = sp.children.iter().map(|&k| (st.subprograms[k].doc_line, st.subprograms[k].end_line)).collect();
            locals.extend(facts::local_names(&cx.tokens(lo, hi, &sk)));
            cur = sp.parent;
        }
        // 자기 이름은 재귀 호출일 수 있으므로 지역 이름에서 뺀다
        locals.remove(&s.name);

        // SQL 문·커서 흐름은 서브프로그램 전체로 (조각 경계를 넘는 FETCH → INSERT 도 잇는다)
        let mut known = global_cursors.clone();
        let mut up = s.parent;
        while let Some(c) = up {
            let sp = &st.subprograms[c];
            let hi = if sp.begin_line > 0 { sp.begin_line } else { sp.end_line };
            let sk: Vec<(u32, u32)> = sp.children.iter().map(|&k| (st.subprograms[k].doc_line, st.subprograms[k].end_line)).collect();
            for (k, v) in flow::declared_cursors(&cx.tokens(sp.start_line, hi, &sk), &locals) {
                known.entry(k).or_insert(v);
            }
            up = sp.parent;
        }
        let (mut sub_stmts, mut sub_cursors) = flow::analyze(&cx.tokens(s.doc_line, s.end_line, &skip), &locals, &known);

        let doc = docs.get(&s.name).cloned().unwrap_or_default();
        let ranges = sub_ranges(&cx, &toks, s, &skip);
        let n = ranges.len() as u32;
        let decl_ctx = if n > 1 { decl_context(&cx, s, &skip_l) } else { String::new() };
        let last = ranges.len().saturating_sub(1);
        for (k, (a, b)) in ranges.into_iter().enumerate() {
            let code = cx.render(a, b, &skip_l);
            let mut f = facts::extract(&cx.tokens(a, b, &skip), &locals);
            // 문장은 시작 줄로, 커서는 선언(없으면 처음 쓰인) 줄로 조각에 나눠 담는다. 마지막 조각이 남은 것을 받는다.
            let here = |l: u32| (l >= a && l <= b) || k == last;
            f.statements = sub_stmts.extract_if(.., |x| here(x.line)).collect();
            f.cursors = sub_cursors
                .extract_if(.., |c| {
                    let l = if c.line > 0 { c.line } else { c.used_at.first().copied().or(c.feeds.first().map(|x| x.line)).unwrap_or(a) };
                    here(l)
                })
                .collect();
            // 동적 SQL 문자열에서 읽은 테이블도 CRUD 에 넣는다
            facts::absorb_dynamic(&mut f);
            let mut context = String::new();
            if !doc.is_empty() {
                context.push_str("명세 주석:\n");
                context.push_str(&doc);
                context.push('\n');
            }
            if k > 0 && !decl_ctx.is_empty() {
                context.push_str(&decl_ctx);
            }
            chunks.push(Chunk {
                id: String::new(),
                kind: if n == 1 { ChunkKind::Whole } else { ChunkKind::Part },
                subprogram: Some(s.path.clone()),
                overload: s.overload,
                sub_kind: Some(s.kind),
                part: k as u32 + 1,
                parts: n,
                start_line: a,
                end_line: b,
                hash: fnv(&[&code, &context, &s.signature]),
                signature: s.signature.clone(),
                context,
                code,
                facts: f,
            });
        }
    }

    // 소스 순서대로 번호
    chunks.sort_by_key(|c| (c.start_line, c.part));
    for (i, c) in chunks.iter_mut().enumerate() {
        let base = match &c.subprogram {
            Some(p) if c.overload > 0 => format!("{}~{}", safe(p), c.overload + 1),
            Some(p) => safe(p),
            None => "_globals".into(),
        };
        c.id = if c.parts > 1 { format!("{:03}-{base}-p{}", i + 1, c.part) } else { format!("{:03}-{base}", i + 1) };
    }

    Plan {
        key: src.key(),
        owner: src.owner.clone(),
        name: src.name.clone(),
        unit_type: src.unit_type.clone(),
        lines: cx.lines.len() as u32,
        hash: fnv(&[&src.text]),
        structure: st,
        chunks,
    }
}

pub fn kind_word(k: SubKind) -> &'static str {
    match k {
        SubKind::Procedure => "PROCEDURE",
        SubKind::Function => "FUNCTION",
        SubKind::Trigger => "TRIGGER",
        SubKind::Init => "초기화 블록",
    }
}

/// 서브프로그램을 한도 안의 줄 범위들로
fn sub_ranges(cx: &Ctx, toks: &[Tok], s: &Subprogram, skip: &[(u32, u32)]) -> Vec<(u32, u32)> {
    let lo = s.doc_line;
    let hi = s.end_line;
    if cx.fits(lo, hi, skip) || s.begin_line == 0 {
        return vec![(lo, hi)];
    }
    let mut out = Vec::new();
    // 머리 + 선언부 — 선언부가 길면 따로 자른다
    let head_hi = s.begin_line.saturating_sub(1).max(lo);
    let head_cuts: Vec<(u32, usize)> = (lo..=head_hi).filter(|&l| cx.text(l).trim_end().ends_with(';')).map(|l| (l, 1)).collect();
    let mut head = Vec::new();
    cx.split(lo, head_hi, &head_cuts, 1, skip, &mut head);
    // 본문
    let cuts = plsql::statement_ends(toks, s);
    let mut body = Vec::new();
    cx.split(s.begin_line, hi, &cuts, 1, skip, &mut body);
    // 머리의 마지막 조각과 본문 첫 조각이 같이 들어가면 붙인다
    if let (Some(&(ha, _)), Some(&(_, bb))) = (head.last(), body.first()) {
        if cx.fits(ha, bb, skip) {
            head.pop();
            body[0].0 = ha;
        }
    }
    out.extend(head);
    out.extend(body);
    out
}

/// 2부 이후에 붙일 선언부 (짧을 때만 그대로, 길면 이름만)
fn decl_context(cx: &Ctx, s: &Subprogram, skip: &[(u32, u32, String)]) -> String {
    if s.begin_line <= s.is_line + 1 {
        return String::new();
    }
    let lo = s.is_line + 1;
    let hi = s.begin_line - 1;
    let lines: Vec<String> = (lo..=hi)
        .filter(|l| !skip.iter().any(|(a, b, _)| a <= l && l <= b))
        .map(|l| cx.text(l).trim().to_string())
        .filter(|t| !t.is_empty() && !t.starts_with("--"))
        .collect();
    let joined = lines.join("\n");
    if joined.chars().count() <= cx.lim.max_chars / 4 {
        format!("선언부 ({lo}~{hi}행):\n{joined}\n")
    } else {
        let names: Vec<String> = lines.iter().filter_map(|l| l.split_whitespace().next().map(String::from)).take(60).collect();
        format!("선언부 ({lo}~{hi}행, 이름만): {}\n", names.join(", "))
    }
}
