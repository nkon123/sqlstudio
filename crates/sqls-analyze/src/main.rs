//! sqlstudio-analyze — PL/SQL 을 조각내어 (로컬) 모델로 분석하고, 결과를 이어 통합 분석한다.
//!
//! ```text
//! # DB 의 스키마 전체 (비밀번호: 환경변수 SQLSTUDIO_PW_<접속이름>)
//! sqlstudio-analyze run --connection ERP-DEV --schema ERP --llm "로컬 (Ollama)"
//! # 일부만, 모델 없이 정적 분석만
//! sqlstudio-analyze run --connection ERP-DEV --name "ORD%" --static
//! # 파일에서 (DB 접속 없이)
//! sqlstudio-analyze run --files D:\src\plsql --owner ERP --llm "로컬 (Ollama)" --out D:\analysis\erp
//! # 통합 분석만 다시 (모델 없이). --overview 를 주면 전체 요약을 모델로
//! sqlstudio-analyze integrate --out D:\analysis\erp [--llm 이름 --overview]
//! # 묻기
//! sqlstudio-analyze show --out D:\analysis\erp --table ERP.ORDERS
//! sqlstudio-analyze show --out D:\analysis\erp --node ERP.ORDER_PKG.CLOSE_ORDER
//! # 모델 품질 평가 (저장된 결과만 읽는다). 두 모델 비교는 --compare 로
//! sqlstudio-analyze run --connection ERP-DEV --name "ORD%" --limit 20 --llm "로컬 (Ollama)" --out D:\eval\qwen7b
//! sqlstudio-analyze eval --out D:\eval\qwen7b [--compare D:\eval\gemma]
//! ```
//!
//! 중간에 멈추거나(Ctrl+C) 죽어도 다시 돌리면 끝난 조각은 건너뛴다.

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use sqls_analyze::chunk::Limits;
use sqls_analyze::integrate::{self, Integrated};
use sqls_analyze::llm::Lang;
use sqls_analyze::run::{analyze_unit, Event, Llm, Options};
use sqls_analyze::source;
use sqls_analyze::store::Store;
use sqls_core::config::{password_env_name, Config};
use sqls_llm::ProviderConfig;

const USAGE: &str = "사용법:
  sqlstudio-analyze run (--connection <접속> [--schema <스키마>] [--name <LIKE 패턴>] [--type <형식,…>]
                         | --files <파일/폴더>… [--owner <스키마>])
                        [--llm <공급자 이름> | --static] [--out <폴더>] [--jobs N]
                        [--max-lines N] [--max-chars N] [--lang ko|en] [--force] [--no-rollup] [--allow-remote]
  sqlstudio-analyze integrate (--out <폴더> | --connection <접속>) [--llm <공급자> --overview]
  sqlstudio-analyze show (--out <폴더> | --connection <접속>) [--table <테이블>] [--node <서브프로그램>]
  sqlstudio-analyze eval (--out <폴더> | --connection <접속>) [--compare <다른 결과 폴더>] [--lang ko|en]
  run 의 --limit N: 앞의 N 개 단위만 (모델을 시험할 때)
  공통: --config <config.toml>";

#[derive(Default)]
struct Args {
    cmd: String,
    connection: Option<String>,
    schema: Option<String>,
    name: Option<String>,
    types: Vec<String>,
    files: Vec<PathBuf>,
    owner: Option<String>,
    llm: Option<String>,
    static_only: bool,
    out: Option<PathBuf>,
    jobs: Option<usize>,
    max_lines: Option<u32>,
    max_chars: Option<usize>,
    lang: Lang,
    force: bool,
    no_rollup: bool,
    allow_remote: bool,
    overview: bool,
    table: Option<String>,
    node: Option<String>,
    compare: Option<PathBuf>,
    limit: Option<usize>,
}

fn parse() -> Result<Args, String> {
    let mut a = Args::default();
    let mut it = std::env::args().skip(1).peekable();
    a.cmd = it.next().ok_or("명령이 없습니다")?;
    if a.cmd == "-h" || a.cmd == "--help" {
        return Err(String::new());
    }
    let need = |it: &mut std::iter::Peekable<std::iter::Skip<std::env::Args>>, f: &str| it.next().ok_or(format!("{f} 다음에 값이 필요합니다"));
    while let Some(x) = it.next() {
        match x.as_str() {
            "--config" => std::env::set_var("SQLSTUDIO_CONFIG", need(&mut it, &x)?),
            "--connection" | "-c" => a.connection = Some(need(&mut it, &x)?),
            "--schema" => a.schema = Some(need(&mut it, &x)?),
            "--name" => a.name = Some(need(&mut it, &x)?),
            "--type" => a.types = need(&mut it, &x)?.split(',').map(|s| s.trim().to_uppercase()).filter(|s| !s.is_empty()).collect(),
            "--files" => {
                while let Some(p) = it.peek() {
                    if p.starts_with("--") {
                        break;
                    }
                    a.files.push(PathBuf::from(it.next().unwrap()));
                }
                if a.files.is_empty() {
                    return Err("--files 다음에 경로가 필요합니다".into());
                }
            }
            "--owner" => a.owner = Some(need(&mut it, &x)?),
            "--llm" => a.llm = Some(need(&mut it, &x)?),
            "--static" => a.static_only = true,
            "--out" | "-o" => a.out = Some(PathBuf::from(need(&mut it, &x)?)),
            "--jobs" | "-j" => a.jobs = Some(need(&mut it, &x)?.parse().map_err(|_| "--jobs 는 숫자")?),
            "--max-lines" => a.max_lines = Some(need(&mut it, &x)?.parse().map_err(|_| "--max-lines 는 숫자")?),
            "--max-chars" => a.max_chars = Some(need(&mut it, &x)?.parse().map_err(|_| "--max-chars 는 숫자")?),
            "--lang" => a.lang = if need(&mut it, &x)?.eq_ignore_ascii_case("en") { Lang::En } else { Lang::Ko },
            "--force" => a.force = true,
            "--no-rollup" => a.no_rollup = true,
            "--allow-remote" => a.allow_remote = true,
            "--overview" => a.overview = true,
            "--table" => a.table = Some(need(&mut it, &x)?.to_uppercase()),
            "--node" => a.node = Some(need(&mut it, &x)?.to_uppercase()),
            "--compare" => a.compare = Some(PathBuf::from(need(&mut it, &x)?)),
            "--limit" => a.limit = Some(need(&mut it, &x)?.parse().map_err(|_| "--limit 는 숫자")?),
            other => return Err(format!("알 수 없는 인자: {other}")),
        }
    }
    Ok(a)
}

fn providers(cfg: &Config) -> Vec<ProviderConfig> {
    cfg.llm_providers.iter().filter_map(|t| t.clone().try_into::<ProviderConfig>().ok()).collect()
}

fn out_dir(a: &Args) -> Result<PathBuf, String> {
    if let Some(o) = &a.out {
        return Ok(o.clone());
    }
    match &a.connection {
        Some(c) => Ok(sqls_analyze::default_dir(c)),
        None => Err("--out 또는 --connection 이 필요합니다".into()),
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_env("SQLSTUDIO_LOG").unwrap_or_else(|_| "warn".into()))
        .init();
    let a = match parse() {
        Ok(a) => a,
        Err(e) => {
            if !e.is_empty() {
                eprintln!("{e}\n");
            }
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    let r = match a.cmd.as_str() {
        "run" => cmd_run(&a).await,
        "integrate" => cmd_integrate(&a).await,
        "show" => cmd_show(&a),
        "eval" => cmd_eval(&a),
        other => Err(format!("알 수 없는 명령: {other}\n\n{USAGE}")),
    };
    match r {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::from(1)
        }
    }
}

fn pick_llm(cfg: &Config, a: &Args) -> Result<Option<ProviderConfig>, String> {
    if a.static_only {
        return Ok(None);
    }
    let all = providers(cfg);
    let p = match &a.llm {
        Some(n) => all.into_iter().find(|p| &p.name == n).ok_or(format!("[[llm]] 에 '{n}' 이 없습니다"))?,
        None => return Err("--llm <공급자 이름> 또는 --static 을 주세요 ([[llm]] 의 name)".into()),
    };
    // 소스 코드가 이 PC 밖으로 나간다 — 명시적으로 허락했을 때만
    if p.is_remote() && !a.allow_remote {
        return Err(format!(
            "'{}' 는 외부 서버({})입니다. 소스 코드가 밖으로 나갑니다. 그래도 하려면 --allow-remote 를 붙이세요.",
            p.name,
            p.base_url()
        ));
    }
    Ok(Some(p))
}

async fn cmd_run(a: &Args) -> Result<(), String> {
    let cfg = Config::load().map_err(|e| e.to_string())?;
    let llm_cfg = pick_llm(&cfg, a)?;
    let store = Store::open(out_dir(a)?).map_err(|e| e.to_string())?;
    let mut lim = Limits::default();
    if let Some(n) = a.max_lines {
        lim.max_lines = n.max(20);
    }
    if let Some(n) = a.max_chars {
        lim.max_chars = n.max(1000);
    }
    let opts = Options {
        limits: lim,
        lang: a.lang,
        jobs: a.jobs.unwrap_or(if llm_cfg.as_ref().is_some_and(|p| p.is_remote()) { 4 } else { 1 }),
        force: a.force,
        rollup: !a.no_rollup,
    };

    // 소스 모으기
    let mut units: Vec<(sqls_analyze::chunk::UnitSource, Option<String>)> = Vec::new();
    let mut session = None;
    if !a.files.is_empty() {
        let owner = a.owner.clone().unwrap_or_else(|| "SRC".into());
        units = source::from_files(&a.files, &owner).map_err(|e| e.to_string())?;
    } else if let Some(c) = &a.connection {
        let p = cfg.profile(c).ok_or(format!("접속 '{c}' 이 설정에 없습니다"))?;
        let pw = p.stored_password().ok_or(format!("비밀번호가 없습니다 — 환경변수 {} 를 두거나, 앱에서 '비밀번호 저장' 으로 접속해 두세요", password_env_name(&p.name)))?;
        sqls_core::session::init_client(cfg.oracle.client_lib_dir.clone()).map_err(|e| format!("Oracle Client: {e}"))?;
        let mut spec = p.to_spec(pw);
        // 사전 조회만 한다
        spec.read_only = true;
        spec.module = "SQLStudio analyze".into();
        let s = sqls_core::Session::connect(spec).await.map_err(|e| e.to_string())?;
        let owner = a.schema.clone().unwrap_or_else(|| s.info().user.clone()).to_uppercase();
        let refs = source::list_units(&s, &owner, &a.types, a.name.as_deref()).await.map_err(|e| e.to_string())?;
        eprintln!("{owner}: 단위 {}개", refs.len());
        session = Some((s, refs));
    } else {
        return Err("--connection 또는 --files 가 필요합니다".into());
    }

    let cancel = Arc::new(AtomicBool::new(false));
    {
        let c = cancel.clone();
        tokio::spawn(async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                eprintln!("\n멈추는 중 — 진행 중인 조각까지 저장합니다. 다시 돌리면 이어서 합니다.");
                c.store(true, Ordering::Relaxed);
            }
        });
    }
    let client = sqls_llm::Client::new();
    let total = session.as_ref().map(|(_, r)| r.len()).unwrap_or(units.len());
    let total = a.limit.map(|n| n.min(total)).unwrap_or(total);
    let started = Instant::now();
    let mut failed_units = 0;
    let on: sqls_analyze::run::OnEvent = Arc::new(|e: Event| match e {
        Event::Chunk { id, done, total, status, elapsed_ms, message, .. } => {
            let t = if elapsed_ms > 0 { format!(" {:.1}s", elapsed_ms as f64 / 1000.0) } else { String::new() };
            let m = message.map(|m| format!(" — {m}")).unwrap_or_default();
            if status != "static" && status != "cached" || done == total {
                eprintln!("    [{done}/{total}] {id} {status}{t}{m}");
            }
        }
        Event::Rollup { what, .. } => eprintln!("    요약: {what}"),
        _ => {}
    });

    for i in 0..total {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let (src, spec) = match &session {
            Some((s, refs)) => match source::fetch(s, &refs[i]).await {
                Ok(x) => x,
                Err(e) => {
                    eprintln!("[{}/{total}] {}.{} 소스를 읽지 못했습니다: {e}", i + 1, refs[i].owner, refs[i].name);
                    failed_units += 1;
                    continue;
                }
            },
            None => units[i].clone(),
        };
        let plan_n = sqls_analyze::chunk::plan(&src, spec.as_deref(), lim).chunks.len();
        eprintln!("[{}/{total}] {} {}.{} — {}행, 조각 {plan_n}개", i + 1, src.unit_type, src.owner, src.name, src.text.lines().count());
        let llm = llm_cfg.as_ref().map(|c| Llm { client: &client, cfg: c });
        match analyze_unit(&store, &src, spec.as_deref(), llm, &opts, on.clone(), cancel.clone()).await {
            Ok(u) => {
                if u.stats.failed > 0 {
                    failed_units += 1;
                }
            }
            Err(e) => {
                eprintln!("  멈춤: {e}");
                failed_units += 1;
                if !cancel.load(Ordering::Relaxed) && e.contains("모델 서버") {
                    break;
                }
            }
        }
    }
    if let Some((s, _)) = session {
        s.close();
    }
    let (g, files) = integrate::write_all(&store, None).map_err(|e| e.to_string())?;
    eprintln!(
        "\n끝 ({:.0}초). 단위 {} · 서브프로그램 {} · 테이블 {} · 확인할 것 {}{}",
        started.elapsed().as_secs_f64(),
        g.units,
        g.nodes.len(),
        g.tables.len(),
        g.findings.len(),
        if failed_units > 0 { format!(" · 실패가 있는 단위 {failed_units}개 (다시 돌리면 실패한 조각만 묻습니다)") } else { String::new() }
    );
    for f in files {
        eprintln!("  {}", f.display());
    }
    Ok(())
}

async fn cmd_integrate(a: &Args) -> Result<(), String> {
    let store = Store::open(out_dir(a)?).map_err(|e| e.to_string())?;
    let overview = if a.overview {
        let cfg = Config::load().map_err(|e| e.to_string())?;
        let p = pick_llm(&cfg, a)?.ok_or("--overview 에는 --llm 이 필요합니다")?;
        eprintln!("전체 요약을 만드는 중 ({})", p.name);
        let o = integrate::overview(&store, &sqls_llm::Client::new(), &p, a.lang, Limits::default().max_chars).await;
        if o.is_none() {
            eprintln!("전체 요약을 만들지 못했습니다 (단위 요약이 없거나 모델 오류)");
        }
        o
    } else {
        store.read_integrated::<Integrated>("integrated.json").and_then(|g| g.overview)
    };
    let (g, files) = integrate::write_all(&store, overview).map_err(|e| e.to_string())?;
    eprintln!("단위 {} · 서브프로그램 {} · 호출 {} · 테이블 {}", g.units, g.nodes.len(), g.edges.len(), g.tables.len());
    for f in files {
        eprintln!("  {}", f.display());
    }
    Ok(())
}

fn cmd_show(a: &Args) -> Result<(), String> {
    let store = Store::open(out_dir(a)?).map_err(|e| e.to_string())?;
    let g: Integrated = store.read_integrated("integrated.json").ok_or("통합 분석 결과가 없습니다 — 먼저 run 또는 integrate")?;
    if let Some(t) = &a.table {
        let rows: Vec<_> = g.tables.iter().filter(|r| &r.table == t || r.table.ends_with(&format!(".{t}"))).collect();
        if rows.is_empty() {
            return Err(format!("{t} 를 쓰는 곳이 없습니다"));
        }
        for r in rows {
            println!("{}", r.table);
            for (n, o) in &r.by {
                println!("  {o:<4} {n}");
            }
            if !r.fed_from.is_empty() {
                println!("  데이터가 들어오는 곳 (커서 흐름): {}", r.fed_from.join(", "));
            }
            if !r.feeds_into.is_empty() {
                println!("  데이터가 나가는 곳 (커서 흐름): {}", r.feeds_into.join(", "));
            }
            for f in g.flows.iter().filter(|f| &f.to == &r.table) {
                println!("    {} 의 커서 {} ({}) → {} {}  {}행 [{}]", f.node, f.cursor, f.from.join(", "), f.ops, f.to, f.line, f.via);
            }
            println!("  영향 받는 시작점 {}개:", r.impacted_entries.len());
            for e in &r.impacted_entries {
                println!("    {e}");
            }
        }
        return Ok(());
    }
    if let Some(n) = &a.node {
        let node = g.nodes.iter().find(|x| &x.id == n || x.id.ends_with(&format!(".{n}"))).ok_or(format!("{n} 가 없습니다"))?;
        println!("{}  ({} {}~{}행)", node.id, node.kind, node.start_line, node.end_line);
        if let Some(s) = &node.summary {
            println!("  {s}");
        }
        println!("  부르는 곳:");
        for e in g.edges.iter().filter(|e| e.to == node.id) {
            println!("    {} ({:?})", e.from, e.lines);
        }
        println!("  부르는 것:");
        for e in g.edges.iter().filter(|e| e.from == node.id) {
            println!("    {}{} ({:?})", e.to, if e.resolved { "" } else { " [밖]" }, e.lines);
        }
        println!("  테이블:");
        for t in g.tables.iter().filter(|t| t.by.contains_key(&node.id)) {
            println!("    {:<4} {}", t.by[&node.id], t.table);
        }
        let flows: Vec<_> = g.flows.iter().filter(|f| f.node == node.id).collect();
        if !flows.is_empty() {
            println!("  커서 → DML:");
            for f in flows {
                println!("    {} ({}) reads {} → {} {}  {}행 [{}]", f.cursor, f.cursor_kind, f.from.join(", "), f.ops, f.to, f.line, f.via);
            }
        }
        return Ok(());
    }
    println!("단위 {} · 서브프로그램 {} · 호출 {} · 테이블 {} · 시작점 {} · 순환 {} · 확인할 것 {}", g.units, g.nodes.len(), g.edges.len(), g.tables.len(), g.entries.len(), g.cycles.len(), g.findings.len());
    println!("보고서: {}", store.root().join("integrated").join("report.md").display());
    Ok(())
}

fn cmd_eval(a: &Args) -> Result<(), String> {
    let store = Store::open(out_dir(a)?).map_err(|e| e.to_string())?;
    let ra = sqls_analyze::eval::evaluate(&store, a.lang);
    let rb = match &a.compare {
        Some(d) => Some(sqls_analyze::eval::evaluate(&Store::open(d).map_err(|e| e.to_string())?, a.lang)),
        None => None,
    };
    let md = sqls_analyze::eval::markdown(&ra, rb.as_ref());
    store.write_integrated("eval.json", &serde_json::json!({ "a": ra, "b": rb })).map_err(|e| e.to_string())?;
    let p = store.write_text("integrated/eval.md", &md).map_err(|e| e.to_string())?;
    println!("{md}");
    eprintln!("보고서: {}", p.display());
    Ok(())
}
