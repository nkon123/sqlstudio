//! 조각 하나를 모델에 묻고 답(JSON)을 받는다.
//!
//! 작은 모델을 전제로 한다:
//! - 지시문은 짧고, 답의 모양을 예시로 보여 준다. 스키마도 함께 보낸다 (지원하는 서버면 출력이 묶인다).
//! - 정적으로 뽑은 사실(테이블·호출)은 프롬프트에 넣어 "찾지 말고 의미를 말하라" 고 한다.
//! - 답이 깨져 있으면(코드 펜스, 끝 쉼표, 잘림) 고쳐 읽고, 그래도 안 되면 한 번 다시 묻는다.
//! - 모델이 낸 줄 번호는 조각 범위 안인지 확인한다.

use std::time::Instant;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqls_llm::{ChatRequest, Client, Message, ProviderConfig};

use crate::chunk::{Chunk, ChunkKind};
use crate::facts::Facts;

/// 프롬프트·스키마를 바꾸면 올린다 — 저장된 결과를 다시 분석하게 된다.
pub const PROMPT_VERSION: u32 = 2;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Risk {
    /// 단위 기준 줄 번호 (모델이 범위 밖을 대면 None)
    pub line: Option<u32>,
    pub issue: String,
}

/// 조각 하나에 대한 모델의 답
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Insight {
    /// 한두 문장
    pub summary: String,
    /// 하는 일 순서
    #[serde(default)]
    pub steps: Vec<String>,
    /// 업무 규칙·조건 (금액 기준, 상태 전이 등)
    #[serde(default)]
    pub rules: Vec<String>,
    #[serde(default)]
    pub risks: Vec<Risk>,
}

/// 모델 호출 기록 — 결과 파일에 같이 남긴다
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CallMeta {
    pub provider: String,
    pub model: String,
    /// 실제로 답한 모델 (fallback 이 있으면 다르다)
    pub answered_by: Option<String>,
    pub prompt_version: u32,
    pub elapsed_ms: u64,
    pub attempts: u32,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    /// 답을 고쳐 읽었으면 무엇을 고쳤는지
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub repaired: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Lang {
    #[default]
    Ko,
    En,
}

fn schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "summary": { "type": "string" },
            "steps": { "type": "array", "items": { "type": "string" } },
            "rules": { "type": "array", "items": { "type": "string" } },
            "risks": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": { "line": { "type": "integer" }, "issue": { "type": "string" } },
                    "required": ["line", "issue"],
                    "additionalProperties": false
                }
            }
        },
        "required": ["summary", "steps", "rules", "risks"],
        "additionalProperties": false
    })
}

fn system(lang: Lang) -> String {
    let lang_line = match lang {
        Lang::Ko => "Write every text value in Korean (한국어).",
        Lang::En => "Write every text value in English.",
    };
    format!(
        "You read one piece of Oracle PL/SQL code and describe what it does.\n\
         Reply with ONE JSON object and nothing else:\n\
         {{\"summary\": \"1-2 sentences\", \"steps\": [\"what it does, in order\"], \"rules\": [\"business rules and conditions\"], \"risks\": [{{\"line\": 0, \"issue\": \"problem\"}}]}}\n\
         - summary: the purpose, not a line-by-line paraphrase.\n\
         - steps: at most 8 short items.\n\
         - rules: concrete conditions in the code (thresholds, status values, error codes). Empty if none.\n\
         - risks: real problems only (swallowed errors, commit inside loop, row-by-row work, missing WHERE, hard-coded values). Use the line numbers shown at the left. Empty if none.\n\
         - The tables and calls listed under \"Known facts\" are already correct. Do not list them again; use them to explain meaning.\n\
         - Only describe this piece. Code marked \"▶ 따로 분석\" is analyzed elsewhere.\n\
         {lang_line}"
    )
}

fn facts_text(f: &Facts) -> String {
    let mut s = String::new();
    if !f.tables.is_empty() {
        let t: Vec<String> = f.tables.iter().map(|t| format!("{}({})", t.name, t.ops)).collect();
        s.push_str(&format!("- tables (C=insert R=select U=update D=delete): {}\n", t.join(", ")));
    }
    if !f.calls.is_empty() {
        let c: Vec<&str> = f.calls.iter().map(|c| c.name.as_str()).collect();
        s.push_str(&format!("- calls: {}\n", c.join(", ")));
    }
    if !f.transactions.is_empty() {
        let c: Vec<String> = f.transactions.iter().map(|m| format!("{}@{}", m.what, m.line)).collect();
        s.push_str(&format!("- transaction control: {}\n", c.join(", ")));
    }
    if !f.dynamic_sql.is_empty() {
        let c: Vec<String> = f.dynamic_sql.iter().map(|m| format!("{}@{}", m.what, m.line)).collect();
        s.push_str(&format!("- dynamic SQL: {}\n", c.join(", ")));
    }
    for c in &f.cursors {
        let feeds: Vec<String> = c.feeds.iter().map(|x| format!("{}({})@{}", x.table, x.ops, x.line)).collect();
        s.push_str(&format!(
            "- cursor {} reads {}{}\n",
            c.name,
            if c.reads.is_empty() { "?".to_string() } else { c.reads.join(", ") },
            if feeds.is_empty() { String::new() } else { format!(" -> feeds {}", feeds.join(", ")) }
        ));
    }
    if !f.swallowed.is_empty() {
        s.push_str(&format!("- exception handlers that do nothing at lines: {:?}\n", f.swallowed));
    }
    if s.is_empty() {
        s.push_str("- (none)\n");
    }
    s
}

/// 조각 → 사용자 메시지
pub fn user_message(unit_label: &str, c: &Chunk) -> String {
    let what = match (&c.subprogram, c.kind) {
        (None, _) => format!("global declarations ({}/{})", c.part, c.parts),
        (Some(p), ChunkKind::Part) => format!("{p} part {}/{}", c.part, c.parts),
        (Some(p), _) => p.clone(),
    };
    let mut m = format!(
        "Unit: {unit_label}\nPiece: {what}, lines {}-{}\nSignature: {}\n",
        c.start_line, c.end_line, c.signature
    );
    if !c.context.is_empty() {
        m.push_str(&format!("Context:\n{}\n", c.context.trim_end()));
    }
    m.push_str(&format!("Known facts:\n{}Code:\n{}", facts_text(&c.facts), c.code));
    m
}

/// 답에서 JSON 객체를 꺼낸다. 고친 것을 `repaired` 에 적는다.
pub fn parse_insight(text: &str, lo: u32, hi: u32, repaired: &mut Vec<String>) -> Option<Insight> {
    let v = extract_json(text, repaired)?;
    Some(coerce(&v, lo, hi, repaired))
}

fn extract_json(text: &str, repaired: &mut Vec<String>) -> Option<Value> {
    let t = text.trim();
    if let Ok(v @ Value::Object(_)) = serde_json::from_str::<Value>(t) {
        return Some(v);
    }
    // 생각 블록(<think>…</think>)을 내는 모델
    let t = match t.rfind("</think>") {
        Some(k) => {
            repaired.push("생각 블록 제거".into());
            &t[k + 8..]
        }
        None => t,
    };
    let start = t.find('{')?;
    if start > 0 || t.contains("```") {
        repaired.push("JSON 앞뒤 글 제거".into());
    }
    // 짝 맞는 } 까지 (문자열 안 괄호는 건너뛴다)
    let b: Vec<char> = t[start..].chars().collect();
    let mut depth = 0i32;
    let mut in_str = false;
    let mut esc = false;
    let mut end = None;
    let mut stack: Vec<char> = Vec::new();
    for (i, &c) in b.iter().enumerate() {
        if in_str {
            if esc {
                esc = false;
            } else if c == '\\' {
                esc = true;
            } else if c == '"' {
                in_str = false;
            }
            continue;
        }
        match c {
            '"' => in_str = true,
            '{' | '[' => {
                depth += 1;
                stack.push(c);
            }
            '}' | ']' => {
                depth -= 1;
                stack.pop();
                if depth == 0 {
                    end = Some(i);
                    break;
                }
            }
            _ => {}
        }
    }
    let mut s: String = match end {
        Some(e) => b[..=e].iter().collect(),
        None => {
            // 잘렸다 — 닫아 준다
            repaired.push("잘린 답 닫기".into());
            let mut s: String = b.iter().collect();
            if in_str {
                s.push('"');
            }
            let s2 = s.trim_end().trim_end_matches(',').to_string();
            s = s2;
            while let Some(c) = stack.pop() {
                s.push(if c == '{' { '}' } else { ']' });
            }
            s
        }
    };
    if let Ok(v) = serde_json::from_str::<Value>(&s) {
        return Some(v);
    }
    // 끝 쉼표, 똑똑한 따옴표
    let before = s.clone();
    s = s.replace(['“', '”'], "\"");
    let mut out = String::with_capacity(s.len());
    let ch: Vec<char> = s.chars().collect();
    let mut in_str = false;
    let mut esc = false;
    for (i, &c) in ch.iter().enumerate() {
        if in_str {
            out.push(c);
            if esc {
                esc = false;
            } else if c == '\\' {
                esc = true;
            } else if c == '"' {
                in_str = false;
            }
            continue;
        }
        if c == '"' {
            in_str = true;
        }
        if c == ',' {
            let next = ch[i + 1..].iter().find(|x| !x.is_whitespace());
            if matches!(next, Some('}') | Some(']')) {
                continue;
            }
        }
        out.push(c);
    }
    if out != before {
        repaired.push("끝 쉼표·따옴표 고침".into());
    }
    serde_json::from_str::<Value>(&out).ok()
}

fn as_text(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.trim().to_string()).filter(|s| !s.is_empty()),
        Value::Null => None,
        Value::Object(m) => {
            // {"step": "..."} / {"description": "..."} 같은 모양
            for k in ["text", "description", "step", "rule", "issue", "summary", "name"] {
                if let Some(s) = m.get(k).and_then(as_text) {
                    return Some(s);
                }
            }
            Some(v.to_string())
        }
        other => Some(other.to_string()),
    }
}

fn as_list(v: Option<&Value>) -> Vec<String> {
    match v {
        Some(Value::Array(a)) => a.iter().filter_map(as_text).collect(),
        Some(Value::String(s)) if !s.trim().is_empty() => {
            s.lines().map(|l| l.trim().trim_start_matches(['-', '*', '•']).trim().to_string()).filter(|l| !l.is_empty()).collect()
        }
        _ => Vec::new(),
    }
}

fn get<'a>(v: &'a Value, keys: &[&str]) -> Option<&'a Value> {
    let m = v.as_object()?;
    for k in keys {
        if let Some(x) = m.get(*k) {
            return Some(x);
        }
        // 대소문자
        if let Some((_, x)) = m.iter().find(|(kk, _)| kk.eq_ignore_ascii_case(k)) {
            return Some(x);
        }
    }
    None
}

fn coerce(v: &Value, lo: u32, hi: u32, repaired: &mut Vec<String>) -> Insight {
    let summary = get(v, &["summary", "purpose", "description", "요약"]).and_then(as_text).unwrap_or_default();
    let mut steps = as_list(get(v, &["steps", "flow", "단계"]));
    steps.truncate(12);
    let rules = as_list(get(v, &["rules", "business_rules", "conditions", "규칙"]));
    let mut risks = Vec::new();
    if let Some(Value::Array(a)) = get(v, &["risks", "issues", "problems", "위험"]) {
        for r in a {
            let (line, issue) = match r {
                Value::Object(_) => {
                    let line = get(r, &["line", "lines", "줄"]).and_then(|x| match x {
                        Value::Number(n) => n.as_u64().map(|n| n as u32),
                        Value::String(s) => s.trim_matches(|c: char| !c.is_ascii_digit()).split(|c: char| !c.is_ascii_digit()).next().and_then(|d| d.parse().ok()),
                        Value::Array(a) => a.first().and_then(|n| n.as_u64()).map(|n| n as u32),
                        _ => None,
                    });
                    (line, get(r, &["issue", "problem", "description", "risk", "text"]).and_then(as_text))
                }
                other => (None, as_text(other)),
            };
            let Some(issue) = issue else { continue };
            let line = match line {
                Some(l) if l >= lo && l <= hi => Some(l),
                Some(0) | None => None,
                Some(_) => {
                    repaired.push("범위 밖 줄 번호 버림".into());
                    None
                }
            };
            risks.push(Risk { line, issue });
        }
    }
    Insight { summary, steps, rules, risks }
}

#[derive(Debug)]
pub enum AskError {
    /// 공급자 오류 (연결·API) — 다음 조각도 실패할 가능성이 크다
    Provider(String),
    /// 답을 끝내 못 읽었다 — 이 조각만의 문제
    Unreadable { raw: String, meta: CallMeta },
}

/// 사람이 읽을 단위 이름
pub fn unit_label(unit_type: &str, owner: &str, name: &str) -> String {
    format!("{unit_type} {owner}.{name}")
}

/// 조각 하나 분석. 스키마를 거절하는 서버면 스키마 없이 다시 보낸다.
pub async fn ask_chunk(client: &Client, cfg: &ProviderConfig, unit_label: &str, c: &Chunk, lang: Lang) -> Result<(Insight, CallMeta), AskError> {
    let req = ChatRequest { system: system(lang), messages: vec![Message::user(user_message(unit_label, c))], json_schema: Some(schema()) };
    ask(client, cfg, req, c.start_line, c.end_line).await
}

/// 요약을 묻는 일반형 (서브프로그램·단위 요약에 쓴다). `body` 는 이미 만든 사용자 메시지.
pub async fn ask_summary(client: &Client, cfg: &ProviderConfig, system_prompt: String, body: String) -> Result<(Insight, CallMeta), AskError> {
    let req = ChatRequest { system: system_prompt, messages: vec![Message::user(body)], json_schema: Some(schema()) };
    ask(client, cfg, req, 1, u32::MAX).await
}

async fn ask(client: &Client, cfg: &ProviderConfig, mut req: ChatRequest, lo: u32, hi: u32) -> Result<(Insight, CallMeta), AskError> {
    let started = Instant::now();
    let mut meta = CallMeta { provider: cfg.name.clone(), model: cfg.model.clone(), prompt_version: PROMPT_VERSION, ..Default::default() };
    let mut cfg = cfg.clone();
    // 분석은 창작이 아니다 — 지정이 없으면 낮게
    if cfg.temperature.is_none() && cfg.kind != sqls_llm::ProviderKind::Anthropic {
        cfg.temperature = Some(0.1);
    }
    // 조각 답은 짧다. 긴 답(같은 말 반복)을 끊는다.
    cfg.max_tokens = cfg.max_tokens.min(2048);
    let mut last_raw = String::new();
    for attempt in 1..=3u32 {
        meta.attempts = attempt;
        let resp = match client.chat(&cfg, &req, |_| {}).await {
            Ok(r) => r,
            Err(sqls_llm::Error::Api { status: 400, message }) if req.json_schema.is_some() => {
                // 구조화 출력을 모르는 서버 — 스키마를 빼고 다시
                tracing::info!("스키마 없이 다시 보냅니다: {message}");
                meta.repaired.push("서버가 JSON 스키마를 거절 — 스키마 없이".into());
                req.json_schema = None;
                continue;
            }
            Err(e) => return Err(AskError::Provider(e.to_string())),
        };
        meta.answered_by = resp.model.clone();
        meta.input_tokens = resp.input_tokens;
        meta.output_tokens = resp.output_tokens;
        if resp.refused() {
            return Err(AskError::Unreadable { raw: "(모델이 답을 거절했습니다)".into(), meta });
        }
        let mut rep = Vec::new();
        if let Some(ins) = parse_insight(&resp.text, lo, hi, &mut rep) {
            if !ins.summary.is_empty() || !ins.steps.is_empty() {
                meta.repaired.extend(rep);
                meta.elapsed_ms = started.elapsed().as_millis() as u64;
                return Ok((ins, meta));
            }
        }
        last_raw = resp.text.clone();
        if attempt == 3 {
            break;
        }
        // 다시 묻는다 — 앞의 답을 보여 주고 형식만 고치게
        let hint = if resp.truncated() {
            "The answer was cut off. Reply again with a SHORTER JSON object (fewer, shorter items)."
        } else {
            "That was not the JSON object asked for. Reply again with ONLY the JSON object: {\"summary\": \"...\", \"steps\": [], \"rules\": [], \"risks\": []}"
        };
        let mut t: String = resp.text.chars().take(1500).collect();
        if t.is_empty() {
            t.push_str("(empty)");
        }
        req.messages.truncate(1);
        req.messages.push(Message::assistant(t));
        req.messages.push(Message::user(hint));
    }
    meta.elapsed_ms = started.elapsed().as_millis() as u64;
    Err(AskError::Unreadable { raw: last_raw.chars().take(4000).collect(), meta })
}

/// 서브프로그램 요약 (여러 조각) / 단위 요약 / 통합 요약에 쓰는 지시문
pub fn rollup_system(lang: Lang, what: &str) -> String {
    let lang_line = match lang {
        Lang::Ko => "Write every text value in Korean (한국어).",
        Lang::En => "Write every text value in English.",
    };
    format!(
        "You combine short analyses of pieces of Oracle PL/SQL code into one description of {what}.\n\
         Reply with ONE JSON object and nothing else:\n\
         {{\"summary\": \"2-3 sentences\", \"steps\": [\"main responsibilities or flow\"], \"rules\": [\"important business rules\"], \"risks\": [{{\"line\": 0, \"issue\": \"problem\"}}]}}\n\
         - Use only what the pieces say. Do not invent tables, procedures or rules.\n\
         - steps: at most 8. rules: at most 10, merged and de-duplicated. risks: the important ones, keep their line numbers.\n\
         {lang_line}"
    )
}
