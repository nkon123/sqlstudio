//! 분석 품질 평가 — 저장된 조각 결과만 읽는다 (모델을 다시 부르지 않는다).
//!
//! 사내에서 실제 모델로 `run` 한 뒤 `eval` 을 돌리면, 그 모델이 이 소스에서 얼마나 쓸 만한지 숫자로 나온다:
//! JSON 실패·재시도·고침, 빈 답, 언어, 코드 베끼기, **근거 없는 이름**(소스에 없는 테이블·프로시저 이름),
//! 범위 밖 줄 번호, 조각 크기별 실패율과 권장 크기, 문제가 큰 조각 목록. 두 결과 폴더(모델 A/B)를 나란히 비교할 수 있다.

use std::collections::{BTreeMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::llm::Lang;
use crate::store::{ChunkResult, Store};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Bucket {
    /// "1-40", "41-80" …
    pub lines: String,
    pub chunks: u32,
    pub failed: u32,
    pub ungrounded: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Issue {
    pub unit: String,
    pub chunk: String,
    pub lines: String,
    pub what: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Report {
    pub dir: String,
    pub models: Vec<String>,
    pub units: u32,
    /// 모델로 분석한 조각
    pub chunks: u32,
    pub ok: u32,
    pub failed: u32,
    /// 두 번 이상 물은 조각
    pub retried: u32,
    /// 답을 고쳐 읽은 조각 (종류별)
    pub repaired: BTreeMap<String, u32>,
    pub empty_summary: u32,
    pub empty_steps: u32,
    /// 요구한 언어가 아닌 요약
    pub wrong_language: u32,
    /// 코드를 그대로 베낀 요약·단계
    pub copied_code: u32,
    /// 소스·사실에 없는 이름을 말한 조각
    pub ungrounded: u32,
    /// 근거 없는 이름들 (많이 나온 순)
    pub ungrounded_names: Vec<(String, u32)>,
    pub risks: u32,
    pub risks_without_line: u32,
    pub avg_ms: u64,
    pub p95_ms: u64,
    pub avg_in_tokens: u64,
    pub avg_out_tokens: u64,
    pub buckets: Vec<Bucket>,
    /// 권장 조각 줄 수 (판단 근거가 부족하면 None)
    pub suggested_max_lines: Option<u32>,
    pub advice: Vec<String>,
    pub worst: Vec<Issue>,
}

fn is_hangul(c: char) -> bool {
    ('\u{AC00}'..='\u{D7A3}').contains(&c) || ('\u{3131}'..='\u{318E}').contains(&c)
}

/// 이름처럼 보이는 낱말 — 밑줄이 있거나 점으로 이은 대문자/소문자 식별자 (ORDER_HIST, audit_pkg.log)
fn name_like(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let flush = |cur: &mut String, out: &mut Vec<String>| {
        let w = cur.trim_matches('.').to_string();
        cur.clear();
        if w.len() < 4 || !w.chars().next().is_some_and(|c| c.is_ascii_alphabetic()) {
            return;
        }
        // 밑줄이나 점이 있는 식별자만 (일반 영어 단어는 빼고)
        if (w.contains('_') || w.contains('.')) && w.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '$' | '#')) {
            out.push(w.to_uppercase());
        }
    };
    for c in text.chars() {
        if c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '$' | '#') {
            cur.push(c);
        } else {
            flush(&mut cur, &mut out);
        }
    }
    flush(&mut cur, &mut out);
    out
}

/// 조각이 아는 이름 (코드·문맥·사실에 나온 낱말 전부, 대문자)
fn known_words(c: &ChunkResult) -> HashSet<String> {
    let mut s = HashSet::new();
    let mut add = |t: &str| {
        for w in t.split(|ch: char| !(ch.is_ascii_alphanumeric() || matches!(ch, '_' | '$' | '#'))) {
            if !w.is_empty() {
                s.insert(w.to_uppercase());
            }
        }
    };
    add(&c.chunk.code);
    add(&c.chunk.context);
    add(&c.chunk.signature);
    for t in &c.chunk.facts.tables {
        add(&t.name);
    }
    for t in &c.chunk.facts.calls {
        add(&t.name);
    }
    s
}

fn insight_texts(c: &ChunkResult) -> Vec<String> {
    let Some(i) = &c.insight else { return Vec::new() };
    let mut v = vec![i.summary.clone()];
    v.extend(i.steps.iter().cloned());
    v.extend(i.rules.iter().cloned());
    v.extend(i.risks.iter().map(|r| r.issue.clone()));
    v
}

/// 요약·단계가 코드 줄을 그대로 옮겼는지 (공백을 줄인 30자 이상이 코드에 그대로 있다)
fn copies_code(c: &ChunkResult) -> bool {
    let code: String = c.chunk.code.lines().map(|l| l.split_once('|').map(|x| x.1).unwrap_or(l).trim()).collect::<Vec<_>>().join(" ");
    let code = code.split_whitespace().collect::<Vec<_>>().join(" ").to_uppercase();
    insight_texts(c).iter().any(|t| {
        let t = t.split_whitespace().collect::<Vec<_>>().join(" ").to_uppercase();
        t.chars().count() >= 30 && code.contains(&t)
    })
}

fn bucket_of(lines: u32) -> usize {
    match lines {
        0..=40 => 0,
        41..=80 => 1,
        81..=120 => 2,
        _ => 3,
    }
}
const BUCKETS: [&str; 4] = ["1-40", "41-80", "81-120", "121+"];

/// 결과 폴더 하나를 평가한다
pub fn evaluate(store: &Store, lang: Lang) -> Report {
    let mut r = Report { dir: store.root().display().to_string(), ..Default::default() };
    let mut models: HashSet<String> = HashSet::new();
    let mut ms: Vec<u64> = Vec::new();
    let (mut tin, mut tout, mut ntok) = (0u64, 0u64, 0u64);
    let mut buckets: Vec<Bucket> = BUCKETS.iter().map(|b| Bucket { lines: b.to_string(), ..Default::default() }).collect();
    let mut names: BTreeMap<String, u32> = BTreeMap::new();
    let mut issues: Vec<(u32, Issue)> = Vec::new();

    for u in store.units() {
        r.units += 1;
        for c in store.chunks_of(&u) {
            // 정적 분석만 한 조각은 평가 대상이 아니다
            if c.llm.is_none() && c.error.is_none() {
                continue;
            }
            r.chunks += 1;
            let lines = c.chunk.end_line.saturating_sub(c.chunk.start_line) + 1;
            let b = &mut buckets[bucket_of(lines)];
            b.chunks += 1;
            let mut what: Vec<String> = Vec::new();
            let mut weight = 0u32;
            if let Some(m) = &c.llm {
                models.insert(m.model.clone());
                if m.elapsed_ms > 0 {
                    ms.push(m.elapsed_ms);
                }
                if let (Some(i), Some(o)) = (m.input_tokens, m.output_tokens) {
                    tin += i;
                    tout += o;
                    ntok += 1;
                }
                if m.attempts > 1 {
                    r.retried += 1;
                    what.push(format!("{}번 물음", m.attempts));
                    weight += 1;
                }
                for rep in &m.repaired {
                    *r.repaired.entry(rep.clone()).or_default() += 1;
                }
            }
            let Some(ins) = &c.insight else {
                r.failed += 1;
                b.failed += 1;
                what.push(c.error.clone().unwrap_or_else(|| "답 없음".into()));
                issues.push((10, Issue { unit: u.key.clone(), chunk: c.chunk.id.clone(), lines: format!("{}-{}", c.chunk.start_line, c.chunk.end_line), what }));
                continue;
            };
            r.ok += 1;
            if ins.summary.trim().is_empty() {
                r.empty_summary += 1;
                what.push("요약 없음".into());
                weight += 3;
            }
            if ins.steps.is_empty() {
                r.empty_steps += 1;
            }
            if lang == Lang::Ko && !ins.summary.is_empty() && !ins.summary.chars().any(is_hangul) {
                r.wrong_language += 1;
                what.push("한국어가 아님".into());
                weight += 1;
            }
            if copies_code(&c) {
                r.copied_code += 1;
                what.push("코드를 베낌".into());
                weight += 2;
            }
            let known = known_words(&c);
            let mut bad: Vec<String> = Vec::new();
            for t in insight_texts(&c) {
                for n in name_like(&t) {
                    let parts_known = n.split('.').all(|p| known.contains(p));
                    if !parts_known && !bad.contains(&n) {
                        bad.push(n);
                    }
                }
            }
            if !bad.is_empty() {
                r.ungrounded += 1;
                b.ungrounded += 1;
                for n in &bad {
                    *names.entry(n.clone()).or_default() += 1;
                }
                what.push(format!("근거 없는 이름: {}", bad.join(", ")));
                weight += 3;
            }
            r.risks += ins.risks.len() as u32;
            r.risks_without_line += ins.risks.iter().filter(|x| x.line.is_none()).count() as u32;
            if weight > 0 {
                issues.push((weight, Issue { unit: u.key.clone(), chunk: c.chunk.id.clone(), lines: format!("{}-{}", c.chunk.start_line, c.chunk.end_line), what }));
            }
        }
    }
    r.models = {
        let mut m: Vec<String> = models.into_iter().collect();
        m.sort();
        m
    };
    if !ms.is_empty() {
        ms.sort();
        r.avg_ms = ms.iter().sum::<u64>() / ms.len() as u64;
        r.p95_ms = ms[((ms.len() as f64) * 0.95).ceil() as usize - 1];
    }
    if ntok > 0 {
        r.avg_in_tokens = tin / ntok;
        r.avg_out_tokens = tout / ntok;
    }
    let mut nv: Vec<(String, u32)> = names.into_iter().collect();
    nv.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    nv.truncate(30);
    r.ungrounded_names = nv;
    issues.sort_by(|a, b| b.0.cmp(&a.0));
    r.worst = issues.into_iter().take(20).map(|(_, i)| i).collect();

    // 조각 크기 권고: 문제율(실패 + 근거 없는 이름)이 작은 조각의 두 배를 넘는 가장 작은 구간부터는 크다
    let rate = |b: &Bucket| if b.chunks == 0 { None } else { Some((b.failed + b.ungrounded) as f64 / b.chunks as f64) };
    let base = rate(&buckets[0]);
    let limits = [40u32, 80, 120];
    if let Some(base) = base {
        let mut suggestion = None;
        for (i, b) in buckets.iter().enumerate().skip(1) {
            if b.chunks < 5 {
                continue;
            }
            if let Some(x) = rate(b) {
                if x > (base * 2.0).max(base + 0.1) {
                    suggestion = Some(limits[i - 1]);
                    break;
                }
            }
        }
        r.suggested_max_lines = suggestion;
    }
    r.buckets = buckets;

    // 조언
    let pct = |n: u32| if r.chunks == 0 { 0.0 } else { n as f64 * 100.0 / r.chunks as f64 };
    if r.chunks == 0 {
        r.advice.push("모델로 분석한 조각이 없습니다 — `run --llm <공급자>` 를 먼저 돌리세요.".into());
    } else {
        if pct(r.failed) > 5.0 {
            r.advice.push(format!("JSON 실패 {:.0}% — 조각을 줄이거나(--max-lines), 구조화 출력을 지원하는 서버(Ollama 0.5+)인지 확인하세요.", pct(r.failed)));
        }
        if pct(r.retried) > 20.0 {
            r.advice.push(format!("다시 물은 조각 {:.0}% — 모델이 형식을 자주 어깁니다. 더 큰 모델이나 코드 특화 모델을 권합니다.", pct(r.retried)));
        }
        if pct(r.ungrounded) > 10.0 {
            r.advice.push(format!("근거 없는 이름 {:.0}% — 요약을 그대로 믿지 마세요. 호출·CRUD 는 정적 분석이라 영향이 없습니다.", pct(r.ungrounded)));
        }
        if pct(r.copied_code) > 15.0 {
            r.advice.push(format!("코드를 베낀 답 {:.0}% — 모델이 의미를 요약하지 못합니다. 조각을 줄여 보세요.", pct(r.copied_code)));
        }
        if pct(r.wrong_language) > 10.0 {
            r.advice.push(format!("한국어가 아닌 요약 {:.0}% — 작은 모델은 영어가 더 정확할 수 있습니다 (--lang en).", pct(r.wrong_language)));
        }
        if let Some(n) = r.suggested_max_lines {
            r.advice.push(format!("{n}줄을 넘는 조각에서 문제가 크게 늘었습니다 — --max-lines {n} 을 권합니다."));
        }
        if r.advice.is_empty() {
            r.advice.push("큰 문제가 보이지 않습니다. '문제가 큰 조각' 몇 개를 직접 열어 확인해 보세요.".into());
        }
    }
    r
}

/// Markdown 보고서. `other` 가 있으면 두 결과를 나란히.
pub fn markdown(a: &Report, other: Option<&Report>) -> String {
    let mut s = String::from("# 분석 품질 평가\n\n");
    let pct = |r: &Report, n: u32| if r.chunks == 0 { "-".to_string() } else { format!("{n} ({:.0}%)", n as f64 * 100.0 / r.chunks as f64) };
    let col = |r: &Report| -> Vec<String> {
        vec![
            r.models.join(", "),
            r.units.to_string(),
            r.chunks.to_string(),
            pct(r, r.ok),
            pct(r, r.failed),
            pct(r, r.retried),
            pct(r, r.ungrounded),
            pct(r, r.copied_code),
            pct(r, r.wrong_language),
            pct(r, r.empty_summary),
            format!("{} / 줄 없음 {}", r.risks, r.risks_without_line),
            format!("{:.1}s / {:.1}s", r.avg_ms as f64 / 1000.0, r.p95_ms as f64 / 1000.0),
            format!("{} / {}", r.avg_in_tokens, r.avg_out_tokens),
        ]
    };
    let rows = ["모델", "단위", "조각(모델 분석)", "성공", "JSON 실패", "다시 물음", "근거 없는 이름", "코드 베낌", "언어 다름", "요약 없음", "위험 지적", "시간 평균/p95", "토큰 입력/출력 평균"];
    let ca = col(a);
    let cb = other.map(col);
    match &cb {
        Some(_) => s.push_str("| 항목 | A | B |\n|---|---|---|\n"),
        None => s.push_str("| 항목 | 값 |\n|---|---|\n"),
    }
    for (i, name) in rows.iter().enumerate() {
        match &cb {
            Some(b) => s.push_str(&format!("| {name} | {} | {} |\n", ca[i], b[i])),
            None => s.push_str(&format!("| {name} | {} |\n", ca[i])),
        }
    }
    s.push('\n');
    let one = |s: &mut String, r: &Report, tag: &str| {
        s.push_str(&format!("## {tag}조언\n\n"));
        for x in &r.advice {
            s.push_str(&format!("- {x}\n"));
        }
        s.push_str(&format!("\n## {tag}조각 크기별\n\n| 줄 | 조각 | 실패 | 근거 없는 이름 |\n|---|---|---|---|\n"));
        for b in &r.buckets {
            s.push_str(&format!("| {} | {} | {} | {} |\n", b.lines, b.chunks, b.failed, b.ungrounded));
        }
        if !r.repaired.is_empty() {
            s.push_str(&format!("\n## {tag}고쳐 읽은 답\n\n"));
            for (k, v) in &r.repaired {
                s.push_str(&format!("- {k}: {v}\n"));
            }
        }
        if !r.ungrounded_names.is_empty() {
            s.push_str(&format!("\n## {tag}근거 없는 이름 (소스·사실에 없음)\n\n"));
            s.push_str(&r.ungrounded_names.iter().map(|(n, c)| format!("`{n}` {c}")).collect::<Vec<_>>().join(", "));
            s.push('\n');
        }
        if !r.worst.is_empty() {
            s.push_str(&format!("\n## {tag}문제가 큰 조각\n\n| 단위 | 조각 | 줄 | 문제 |\n|---|---|---|---|\n"));
            for w in &r.worst {
                s.push_str(&format!("| {} | `{}` | {} | {} |\n", w.unit, w.chunk, w.lines, w.what.join("; ").replace('|', "\\|")));
            }
        }
        s.push('\n');
    };
    one(&mut s, a, if other.is_some() { "A: " } else { "" });
    if let Some(b) = other {
        one(&mut s, b, "B: ");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert_eq!(name_like("ORDER_HIST 에 넣고 audit_pkg.log 를 부른다. Use the table."), vec!["ORDER_HIST", "AUDIT_PKG.LOG"]);
        assert!(name_like("합계 1000 초과").is_empty());
    }
}
