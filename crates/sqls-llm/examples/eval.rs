//! 평가 실행 — 사례 폴더를 모델에 돌려 통과율 표를 낸다 (sqls_llm::eval).
//!
//!   cargo run -p sqls-llm --example eval -- --model qwen3.5:4b
//!   cargo run -p sqls-llm --example eval -- --url http://gpu-server:11434 --model qwen3.5:4b --think on --runs 3
//!   cargo run -p sqls-llm --example eval -- --kind openai --url http://lmstudio:1234/v1 --model qwen3.5-4b --only top
//!
//! 옵션: --kind ollama|openai|anthropic · --url · --model · --cases 폴더(기본 evals) · --runs N · --think auto|off|on
//!       --repairs N(기본 2) · --no-guides · --num-ctx N · --only 아이디_일부 · --probe(모델 측정을 먼저) · --verbose
//! API 키는 OPENAI_API_KEY / ANTHROPIC_API_KEY 에서 읽는다.

use std::path::PathBuf;

use sqls_llm::eval::{self, Options};
use sqls_llm::{Client, ProviderConfig, ProviderKind};

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let val = |k: &str| args.iter().position(|a| a == k).and_then(|i| args.get(i + 1)).cloned();
    let flag = |k: &str| args.iter().any(|a| a == k);
    if flag("--help") || flag("-h") {
        println!("{}", include_str!("eval.rs").lines().take_while(|l| l.starts_with("//!")).map(|l| l.trim_start_matches("//!")).collect::<Vec<_>>().join("\n"));
        return;
    }
    let kind = match val("--kind").as_deref() {
        None | Some("ollama") => ProviderKind::Ollama,
        Some("openai") => ProviderKind::OpenaiCompatible,
        Some("anthropic") => ProviderKind::Anthropic,
        Some(k) => return eprintln!("알 수 없는 --kind: {k}"),
    };
    let Some(model) = val("--model") else { return eprintln!("--model 이 필요합니다 (--help)") };
    let mut cfg = ProviderConfig::new("eval", kind, &model);
    cfg.base_url = val("--url");
    cfg.num_ctx = val("--num-ctx").and_then(|n| n.parse().ok()).or(Some(16384));
    cfg.api_key = match kind {
        ProviderKind::Anthropic => std::env::var("ANTHROPIC_API_KEY").ok(),
        ProviderKind::OpenaiCompatible => std::env::var("OPENAI_API_KEY").ok(),
        ProviderKind::Ollama => None,
    };
    let opts = Options {
        think: val("--think"),
        repairs: val("--repairs").and_then(|n| n.parse().ok()).unwrap_or(2),
        guides: !flag("--no-guides"),
    };
    let runs: usize = val("--runs").and_then(|n| n.parse().ok()).unwrap_or(1).max(1);
    let dir = PathBuf::from(val("--cases").unwrap_or_else(|| "evals".into()));
    let mut cases = match eval::load_cases(&dir) {
        Ok(c) => c,
        Err(e) => return eprintln!("사례를 읽지 못했습니다: {e}"),
    };
    if let Some(only) = val("--only") {
        cases.retain(|c| c.id.contains(&only));
    }
    if cases.is_empty() {
        return eprintln!("돌릴 사례가 없습니다 ({})", dir.display());
    }
    let client = Client::new();
    if flag("--probe") {
        match client.probe(&cfg).await {
            Ok(p) => println!("모델 측정: {:.1} tok/s · JSON 형식 {} · 생각 모드 {} · 문맥 {}\n", p.tok_per_sec, p.json, p.think, p.context.map(|c| c.to_string()).unwrap_or("?".into())),
            Err(e) => eprintln!("모델 측정 실패: {e}\n"),
        }
    }
    println!("모델 {model} · 사례 {}개 × {runs}번 · 생각 {} · 고침 {}번 · 문법 카드 {}\n", cases.len(), opts.think.as_deref().unwrap_or("auto"), opts.repairs, if opts.guides { "켬" } else { "끔" });
    let mut results = Vec::new();
    for run in 0..runs {
        for c in &cases {
            eprint!("{} {}… ", c.id, if runs > 1 { format!("#{}", run + 1) } else { String::new() });
            match eval::run_case(&client, &cfg, c, &opts).await {
                Ok(mut r) => {
                    eprintln!("{} {:.1}s", if r.pass_final { "통과" } else { "실패" }, r.secs);
                    if flag("--verbose") {
                        eprintln!("  첫 답 문제: {:?}\n  SQL: {}", r.first_problems, r.sql.as_deref().unwrap_or("(없음)"));
                    }
                    if runs > 1 {
                        r.id = format!("{} #{}", r.id, run + 1);
                    }
                    results.push(r);
                }
                Err(e) => eprintln!("오류: {e}"),
            }
        }
    }
    println!("{}", eval::report(&results));
}
