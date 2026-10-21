//! PL/SQL 분석 화면의 명령 — sqls-analyze 를 앱에서 돌린다.
//!
//! - 사전 조회는 프로필의 메타 세션(읽기 전용)으로 한다. 사용자의 작업 세션을 막지 않는다.
//! - 결과는 CLI 와 같은 폴더(설정 파일 옆 analysis/<프로필>)에 쌓인다 — 앱에서 시작하고 CLI 로 이어 가도 된다.
//! - 모델에는 소스 텍스트만 간다. 외부 공급자면 화면이 먼저 확인을 받고 `allow_remote` 로 알려 준다.

use sqls_core::sync::{MutexExt, RwLockExt};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use serde::Serialize;
use sqls_analyze::bundle;
use sqls_analyze::integrate::{self, Integrated};
use sqls_analyze::run::{analyze_unit, Event, Llm, Options};
use sqls_analyze::source::{self, UnitRef};
use sqls_analyze::store::{ChunkResult, Store, UnitResult};
use tauri::ipc::Channel;
use tauri::{AppHandle, Emitter, State};

use crate::complete::hub_for;
use crate::state::{AppState, ErrView};

type R<T> = Result<T, ErrView>;

static NEXT_RUN: AtomicU64 = AtomicU64::new(1);

fn profile_of(st: &AppState, id: u64) -> R<String> {
    st.sessions
        .lock()
        .unwrap()
        .get(&id)
        .map(|s| s.profile.clone())
        .ok_or_else(|| ErrView::msg("session_closed", "세션이 닫혀 있습니다"))
}

/// 접속 이름을 바꿔 결과 폴더를 못 찾으면, 옛 이름으로 남은 결과를 이어 붙인다 (이 접속의 결과가 없고, 옛 결과가 이 접속 계정의 것일 때만)
fn adopt_old_results(st: &AppState, id: u64, profile: &str) {
    let Ok(owner) = session_user(st, id) else { return };
    let names: Vec<String> = st.cfg.rd().connections.iter().map(|p| p.name.clone()).collect();
    if let Some(old) = sqls_analyze::adopt_renamed(profile, &names, &owner) {
        tracing::info!("분석 결과 '{old}' 를 '{profile}' 로 이어 붙임 (접속 이름이 바뀐 것으로 보임)");
    }
}

fn store_for(st: &AppState, id: u64) -> R<Store> {
    let p = profile_of(st, id)?;
    adopt_old_results(st, id, &p);
    Store::open(sqls_analyze::default_dir(&p)).map_err(|e| ErrView::msg("io", e.to_string()))
}

/// 이 PC 의 결과 폴더 하나 (가져온 것 포함). 이름은 폴더 이름 그대로 — 경로를 벗어나지 못하게
fn set_dir(name: &str) -> R<std::path::PathBuf> {
    if name.is_empty() || name.starts_with('.') || name.contains(['/', '\\', ':']) || name.contains("..") {
        return Err(ErrView::msg("invalid", format!("잘못된 결과 이름: {name}")));
    }
    let d = sqls_analyze::analysis_root().join(name);
    if !d.join("units").is_dir() {
        return Err(ErrView::msg("invalid", format!("'{name}' 분석 결과가 이 PC 에 없습니다")));
    }
    Ok(d)
}

/// 결과를 읽을 곳: 고른 결과(set) — 접속 없이도 된다, 아니면 탭 접속의 프로필
fn store_of(st: &AppState, id: Option<u64>, set: Option<&str>) -> R<Store> {
    match (set.filter(|s| !s.is_empty()), id) {
        (Some(name), _) => Store::open(set_dir(name)?).map_err(|e| ErrView::msg("io", e.to_string())),
        (None, Some(id)) => store_for(st, id),
        (None, None) => Err(ErrView::msg("invalid", "접속하거나 볼 분석 결과를 고르세요")),
    }
}

/// 메타 세션 — 접속 직후·다시 접속 중이면 잠깐 기다린다
async fn meta_session(st: &AppState, id: u64) -> R<Arc<crate::complete::MetaHub>> {
    let hub = hub_for(st, id)?;
    if hub.wait_session(std::time::Duration::from_secs(10)).await.is_some() {
        return Ok(hub);
    }
    Err(ErrView::msg("session_closed", "사전 조회 세션이 준비되지 않았습니다 (접속 중이거나 다시 접속하는 중)"))
}

#[derive(Serialize)]
pub struct UnitRow {
    #[serde(flatten)]
    unit: UnitRef,
    /// 분석한 적이 있으면
    analyzed_at: Option<u64>,
    chunks: Option<u32>,
    failed: Option<u32>,
    model: Option<String>,
    summary: Option<String>,
    warning: Option<String>,
}

/// 스키마의 분석 대상 목록 + 저장된 결과 상태
#[tauri::command]
pub async fn analysis_list(st: State<'_, AppState>, id: u64, owner: Option<String>, name_like: Option<String>) -> R<Vec<UnitRow>> {
    let hub = meta_session(&st, id).await?;
    let s = hub.session().ok_or_else(|| ErrView::msg("session_closed", "사전 조회 세션이 끊겼습니다"))?;
    let owner = owner.filter(|o| !o.trim().is_empty()).unwrap_or_else(|| s.info().user.clone());
    let pat = name_like.filter(|p| !p.trim().is_empty()).map(|p| if p.contains('%') { p } else { format!("%{p}%") });
    let refs = source::list_units(&s, &owner, &[], pat.as_deref()).await?;
    let store = store_for(&st, id)?;
    // 저장된 결과는 요약 색인으로 (단위마다 unit.json 을 다 읽으면 수천 개에서 몇 초)
    let briefs = blocking(move || store.briefs()).await?;
    let by_key: std::collections::HashMap<&str, &sqls_analyze::store::UnitBrief> = briefs.iter().map(|b| (b.key.as_str(), b)).collect();
    Ok(refs
        .into_iter()
        .map(|u| {
            let key = sqls_analyze::chunk::unit_key(&u.owner, &u.name, &u.unit_type);
            row_of(by_key.get(key.as_str()).copied(), u)
        })
        .collect())
}

fn row_of(saved: Option<&sqls_analyze::store::UnitBrief>, unit: UnitRef) -> UnitRow {
    UnitRow {
        analyzed_at: saved.map(|x| x.analyzed_at),
        chunks: saved.map(|x| x.chunks),
        failed: saved.map(|x| x.failed),
        model: saved.and_then(|x| x.model.clone()),
        summary: saved.and_then(|x| x.summary.clone()),
        warning: saved.and_then(|x| x.warning.clone()),
        unit,
    }
}

/// 디스크를 오래 읽는 일은 작업 스레드에서 (동기 명령은 앱의 메인 스레드에서 돌아 그동안 창이 멈춘다)
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> R<T> {
    tauri::async_runtime::spawn_blocking(f).await.map_err(|e| ErrView::msg("io", e.to_string()))
}

#[derive(Serialize, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Progress {
    Event { run: u64, event: Event },
    Fetch { run: u64, index: usize, total: usize, key: String, error: Option<String> },
    Finished { run: u64, error: Option<String>, units: usize, nodes: usize, tables: usize, findings: usize, dir: String },
}

/// 분석 시작. `provider` 가 None 이면 정적 분석만. 진행은 "analysis-progress" 이벤트로.
#[tauri::command]
pub async fn analysis_start(
    app: AppHandle,
    st: State<'_, AppState>,
    id: u64,
    units: Vec<UnitRef>,
    provider: Option<String>,
    allow_remote: bool,
    options: Options,
) -> R<u64> {
    if units.is_empty() {
        return Err(ErrView::msg("invalid", "분석할 단위를 고르세요"));
    }
    let cfg = match &provider {
        Some(n) => {
            let p = crate::ai::provider(&st, n)?;
            if p.is_remote() && !allow_remote {
                return Err(ErrView::msg("invalid", format!("'{}' 는 외부 서버입니다. 소스 코드가 밖으로 나갑니다.", p.name)));
            }
            Some(p)
        }
        None => None,
    };
    let hub = meta_session(&st, id).await?;
    let store = store_for(&st, id)?;
    let client = st.llm.clone();
    let run = NEXT_RUN.fetch_add(1, Ordering::Relaxed);
    let cancel = Arc::new(AtomicBool::new(false));
    st.analyses.lk().insert(run, cancel.clone());

    tauri::async_runtime::spawn(async move {
        let emit = {
            let app = app.clone();
            move |p: Progress| {
                let _ = app.emit("analysis-progress", p);
            }
        };
        let on: sqls_analyze::run::OnEvent = {
            let emit = emit.clone();
            Arc::new(move |e: Event| emit(Progress::Event { run, event: e }))
        };
        let total = units.len();
        let mut stop: Option<String> = None;
        let mut deps = sqls_analyze::deps::Fetcher::default();
        for (i, u) in units.iter().enumerate() {
            if cancel.load(Ordering::Relaxed) {
                stop = Some("중지했습니다".into());
                break;
            }
            let key = sqls_analyze::chunk::unit_key(&u.owner, &u.name, &u.unit_type);
            // 접속이 끊겼으면(방화벽·DB 재시작) 다시 접속해 이 단위부터 이어 간다. 그래도 안 되면 멈춘다 — 남은 단위를 하나씩 실패시키지 않는다
            let mut r = match hub.session() {
                Some(s) => source::fetch_unit(&s, u, &store, &mut deps).await,
                None => Err(sqls_core::Error::SessionClosed),
            };
            if matches!(&r, Err(e) if e.is_connection_lost()) {
                emit(Progress::Event { run, event: Event::Rollup { key: "접속".into(), what: "DB 접속이 끊겼습니다 — 다시 접속하는 중…".into() } });
                r = match hub.reconnect().await {
                    Some(s) => source::fetch_unit(&s, u, &store, &mut deps).await,
                    None => Err(sqls_core::Error::SessionClosed),
                };
            }
            let (src, spec) = match r {
                Ok(x) => x,
                Err(e) if e.is_connection_lost() => {
                    stop = Some(format!("DB 에 다시 접속하지 못해 멈췄습니다 ({e}). 다시 시작하면 분석한 것은 건너뛰고 이어서 합니다."));
                    break;
                }
                Err(e) => {
                    emit(Progress::Fetch { run, index: i, total, key, error: Some(e.to_string()) });
                    continue;
                }
            };
            emit(Progress::Fetch { run, index: i, total, key, error: None });
            let llm = cfg.as_ref().map(|c| Llm { client: &client, cfg: c });
            if let Err(e) = analyze_unit(&store, &src, spec.as_deref(), llm, &options, on.clone(), cancel.clone()).await {
                // 모델 서버가 죽었으면 나머지도 실패한다 — 멈춘다
                if e.contains("모델 서버") || cancel.load(Ordering::Relaxed) {
                    stop = Some(e);
                    break;
                }
            }
        }
        // 사전과 파서가 다른 곳만 모델에 묻는다 (사전만 있는 테이블의 연산, 동적 SQL 의 테이블)
        if let (Some(c), None) = (cfg.as_ref(), &stop) {
            let on = on.clone();
            let say = move |what: String| on(Event::Rollup { key: "사전 대조".into(), what });
            sqls_analyze::deps::supplement(&store, &client, c, &cancel, &say).await;
        }
        let overview = store.read_integrated::<Integrated>("integrated.json").and_then(|g| g.overview);
        let fin = match integrate::write_all(&store, overview) {
            Ok((g, _)) => Progress::Finished {
                run,
                error: stop,
                units: g.units,
                nodes: g.nodes.len(),
                tables: g.tables.len(),
                findings: g.findings.len(),
                dir: store.root().display().to_string(),
            },
            Err(e) => Progress::Finished { run, error: Some(e.to_string()), units: 0, nodes: 0, tables: 0, findings: 0, dir: store.root().display().to_string() },
        };
        emit(fin);
        if let Some(st) = tauri::Manager::try_state::<AppState>(&app) {
            st.analyses.lk().remove(&run);
        }
    });
    Ok(run)
}

/// 0.5.10 은 코드 파일 단위를 `PC_…`·`XQ_…` 이름의 패키지로 저장했다 — 새 이름(파일 이름 그대로 · 형식 Proc/XML)으로 바뀌었으니
/// 같은 파일의 옛 결과를 치운다 (안 그러면 같은 파일이 두 번 보인다). 지운 게 있으면 통합 분석을 다시 만든다
fn drop_legacy_code_units(store: &Store, units: &[source::CodeUnit]) {
    let mut removed = 0;
    for c in units {
        let key = sqls_analyze::chunk::unit_key(&c.unit.owner, &c.legacy_name, "PACKAGE BODY");
        let dir = store.unit_dir(&key);
        if dir.is_dir() && std::fs::remove_dir_all(&dir).is_ok() {
            removed += 1;
        }
    }
    if removed > 0 {
        tracing::info!("옛 이름(PC_·XQ_)의 코드 분석 결과 {removed}개를 치움");
        let overview = store.read_integrated::<Integrated>("integrated.json").and_then(|g| g.overview);
        let _ = integrate::write_all(store, overview);
    }
}

#[derive(Serialize)]
pub struct CodeRow {
    /// 폴더 기준 상대 경로 (화면에 보이는 이름)
    rel: String,
    /// 파일 전체 경로 — 분석할 파일을 고를 때의 키
    path: String,
    kind: &'static str,
    /// 함수(Pro*C)·쿼리(XML) 수
    count: usize,
    #[serde(flatten)]
    row: UnitRow,
}

fn session_user(st: &AppState, id: u64) -> R<String> {
    let g = st.sessions.lk();
    g.get(&id).map(|s| s.session.info().user.clone()).ok_or_else(|| ErrView::msg("session_closed", "세션이 닫혀 있습니다"))
}

/// 코드 폴더의 분석 대상 목록 — Pro*C(.pc) 와 XML 쿼리 파일(Anyframe). 파일을 읽기만 하고 분석은 하지 않는다 (저장된 결과 상태를 함께)
#[tauri::command]
pub async fn analysis_code_list(st: State<'_, AppState>, id: u64, folders: Vec<String>) -> R<Vec<CodeRow>> {
    let owner = session_user(&st, id)?;
    let roots: Vec<std::path::PathBuf> = folders.iter().map(std::path::PathBuf::from).collect();
    if let Some(bad) = roots.iter().find(|r| !r.is_dir()) {
        return Err(ErrView::msg("invalid", format!("폴더를 찾을 수 없습니다: {}", bad.display())));
    }
    let store = store_for(&st, id)?;
    // 큰 폴더를 읽는 동안 화면이 멈추지 않게 별도 스레드에서
    let multi = roots.len() > 1;
    let units = tauri::async_runtime::spawn_blocking(move || source::scan_code(&roots, &owner))
        .await
        .map_err(|e| ErrView::msg("io", e.to_string()))?
        .map_err(|e| ErrView::msg("io", e.to_string()))?;
    drop_legacy_code_units(&store, &units);
    Ok(units
        .into_iter()
        .map(|c| {
            let saved = store.read_unit(&sqls_analyze::chunk::unit_key(&c.unit.owner, &c.unit.name, &c.unit.unit_type));
            let inner = c.path.strip_prefix(&c.root).unwrap_or(&c.path).display().to_string();
            // 폴더가 여럿이면 어느 폴더의 것인지 폴더 이름을 앞에 붙인다
            let rel = if multi { format!("{}/{inner}", c.root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()) } else { inner };
            CodeRow {
                rel,
                path: c.path.display().to_string(),
                kind: c.kind,
                count: c.count,
                row: UnitRow {
                    analyzed_at: saved.as_ref().map(|x| x.analyzed_at),
                    chunks: saved.as_ref().map(|x| x.stats.chunks),
                    failed: saved.as_ref().map(|x| x.stats.failed),
                    model: saved.as_ref().and_then(|x| x.model.clone()),
                    summary: saved.as_ref().and_then(|x| x.summary.as_ref().map(|i| i.summary.clone())),
                    warning: saved.as_ref().and_then(|x| x.warning.clone()),
                    unit: UnitRef { owner: c.unit.owner, name: c.unit.name, unit_type: c.unit.unit_type },
                },
            }
        })
        .collect())
}

/// 코드 폴더 분석 — 고른 파일(paths)만. 진행·끝은 analysis_start 와 같은 "analysis-progress" 이벤트
#[tauri::command]
pub async fn analysis_code_start(
    app: AppHandle,
    st: State<'_, AppState>,
    id: u64,
    folders: Vec<String>,
    paths: Vec<String>,
    provider: Option<String>,
    allow_remote: bool,
    options: Options,
) -> R<u64> {
    let owner = session_user(&st, id)?;
    let want: std::collections::HashSet<String> = paths.into_iter().collect();
    if want.is_empty() {
        return Err(ErrView::msg("invalid", "분석할 파일을 고르세요"));
    }
    let roots: Vec<std::path::PathBuf> = folders.iter().map(std::path::PathBuf::from).collect();
    let units: Vec<_> = tauri::async_runtime::spawn_blocking(move || source::scan_code(&roots, &owner))
        .await
        .map_err(|e| ErrView::msg("io", e.to_string()))?
        .map_err(|e| ErrView::msg("io", e.to_string()))?
        .into_iter()
        .filter(|c| want.contains(&c.path.display().to_string()))
        .map(|c| c.unit)
        .collect();
    if units.is_empty() {
        return Err(ErrView::msg("invalid", "고른 파일에서 분석할 쿼리를 찾지 못했습니다 (파일이 바뀌었는지 목록을 다시 읽어 보세요)"));
    }
    let cfg = match &provider {
        Some(n) => {
            let p = crate::ai::provider(&st, n)?;
            if p.is_remote() && !allow_remote {
                return Err(ErrView::msg("invalid", format!("'{}' 는 외부 서버입니다. 소스 코드가 밖으로 나갑니다.", p.name)));
            }
            Some(p)
        }
        None => None,
    };
    let store = store_for(&st, id)?;
    // 테이블 이름을 사전(동의어)으로 푸는 데 쓰는 사전 조회 세션 — 없으면 풀지 못할 뿐 분석은 한다
    let hub = hub_for(&st, id).ok();
    let client = st.llm.clone();
    let run = NEXT_RUN.fetch_add(1, Ordering::Relaxed);
    let cancel = Arc::new(AtomicBool::new(false));
    st.analyses.lk().insert(run, cancel.clone());
    tauri::async_runtime::spawn(async move {
        let emit = {
            let app = app.clone();
            move |p: Progress| {
                let _ = app.emit("analysis-progress", p);
            }
        };
        let on: sqls_analyze::run::OnEvent = {
            let emit = emit.clone();
            Arc::new(move |e: Event| emit(Progress::Event { run, event: e }))
        };
        let total = units.len();
        let mut stop: Option<String> = None;
        let mut fetcher = sqls_analyze::deps::Fetcher::default();
        for (i, src) in units.iter().enumerate() {
            if cancel.load(Ordering::Relaxed) {
                stop = Some("중지했습니다".into());
                break;
            }
            emit(Progress::Fetch { run, index: i, total, key: sqls_analyze::chunk::unit_key(&src.owner, &src.name, &src.unit_type), error: None });
            let llm = cfg.as_ref().map(|c| Llm { client: &client, cfg: c });
            match analyze_unit(&store, src, None, llm, &options, on.clone(), cancel.clone()).await {
                Ok(u) => {
                    // 스키마 없는 테이블 이름을 사전으로 풀어 저장 — DB 프로시저가 쓰는 실제 이름(소유 스키마·동의어 대상)과 같아져야 흐름이 이어진다
                    if let Some(s) = hub.as_ref().and_then(|h| h.session()) {
                        let names: Vec<String> = u.facts.tables.iter().map(|t| t.name.clone()).collect();
                        match fetcher.resolve_names(&s, &src.owner, &names).await {
                            Ok(deps) => {
                                let d = sqls_analyze::deps::UnitDeps { key: u.key.clone(), fetched_at: sqls_analyze::store::now(), deps };
                                if let Err(e) = store.write_deps(&d) {
                                    tracing::warn!("{} 사전 이름 저장 실패: {e}", u.key);
                                }
                            }
                            Err(e) => tracing::warn!("{} 테이블 이름을 사전으로 풀지 못함: {e}", u.key),
                        }
                    }
                }
                Err(e) => {
                    if e.contains("모델 서버") || cancel.load(Ordering::Relaxed) {
                        stop = Some(e);
                        break;
                    }
                }
            }
        }
        let overview = store.read_integrated::<Integrated>("integrated.json").and_then(|g| g.overview);
        let fin = match integrate::write_all(&store, overview) {
            Ok((g, _)) => Progress::Finished { run, error: stop, units: g.units, nodes: g.nodes.len(), tables: g.tables.len(), findings: g.findings.len(), dir: store.root().display().to_string() },
            Err(e) => Progress::Finished { run, error: Some(e.to_string()), units: 0, nodes: 0, tables: 0, findings: 0, dir: store.root().display().to_string() },
        };
        emit(fin);
        if let Some(st) = tauri::Manager::try_state::<AppState>(&app) {
            st.analyses.lk().remove(&run);
        }
    });
    Ok(run)
}

/// 멈춤 — 진행 중인 조각까지 저장하고 멈춘다
#[tauri::command]
pub fn analysis_cancel(st: State<'_, AppState>, run: u64) {
    if let Some(c) = st.analyses.lk().get(&run) {
        c.store(true, Ordering::Relaxed);
    }
}

/// 통합 분석 결과 (없으면 None). `rebuild` 면 저장된 단위로 다시 만든다 (모델 없이).
#[tauri::command]
pub async fn analysis_result(st: State<'_, AppState>, id: Option<u64>, set: Option<String>, rebuild: bool) -> R<Option<Integrated>> {
    let store = store_of(&st, id, set.as_deref())?;
    blocking(move || -> R<Option<Integrated>> {
        if rebuild {
            let overview = store.read_integrated::<Integrated>("integrated.json").and_then(|g| g.overview);
            let (g, _) = integrate::write_all(&store, overview).map_err(|e| ErrView::msg("io", e.to_string()))?;
            return Ok(Some(integrate::for_screen(g)));
        }
        Ok(store.read_integrated("integrated.json").map(integrate::for_screen))
    })
    .await?
}

/// 프로필의 통합 분석 결과 — 흐름도 창용 (메인 창의 접속이 끊겨도 저장된 결과는 읽는다)
#[tauri::command]
pub async fn analysis_result_for(profile: String) -> R<Option<Integrated>> {
    let store = Store::open(sqls_analyze::default_dir(&profile)).map_err(|e| ErrView::msg("io", e.to_string()))?;
    blocking(move || store.read_integrated("integrated.json").map(integrate::for_screen)).await
}

static NEXT_DFD: AtomicU64 = AtomicU64::new(1);

/// 데이터 흐름도를 별도 창으로. center 는 노드 id (OWNER.TABLE, OWNER.PKG.PROC), dir 은 up | down | both
/// async 여야 한다 — Windows 에서 동기 명령 안에서 창을 만들면 교착된다 (창이 멈춘다)
#[tauri::command]
pub async fn open_dfd_window(
    app: AppHandle,
    st: State<'_, AppState>,
    id: Option<u64>,
    set: Option<String>,
    center: Option<String>,
    dir: Option<String>,
    unit: Option<String>,
) -> R<()> {
    // 흐름도 창은 결과 폴더 이름으로 읽는다 (가져온 결과면 그 이름)
    let profile = match (set.filter(|s| !s.is_empty()), id) {
        (Some(name), _) => {
            set_dir(&name)?;
            name
        }
        (None, Some(id)) => {
            let p = profile_of(&st, id)?;
            adopt_old_results(&st, id, &p);
            p
        }
        (None, None) => return Err(ErrView::msg("invalid", "접속하거나 볼 분석 결과를 고르세요")),
    };
    let enc = |s: &str| s.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-_.".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect::<String>();
    let mut url = format!("dfd.html?profile={}", enc(&profile));
    if let Some(c) = center.filter(|c| !c.is_empty()) {
        url.push_str(&format!("&center={}", enc(&c)));
    }
    if let Some(d) = dir.filter(|d| matches!(d.as_str(), "up" | "down" | "both")) {
        url.push_str(&format!("&dir={d}"));
    }
    if let Some(u) = unit.filter(|u| matches!(u.as_str(), "proc" | "pkg")) {
        url.push_str(&format!("&unit={u}"));
    }
    let label = format!("dfd-{}", NEXT_DFD.fetch_add(1, Ordering::Relaxed));
    let mut b = tauri::WebviewWindowBuilder::new(&app, label, tauri::WebviewUrl::App(url.into()))
        .title(format!("데이터 흐름도 — {profile}"))
        .inner_size(1360.0, 860.0)
        .min_inner_size(700.0, 450.0)
        .additional_browser_args(crate::WEBVIEW_ARGS);
    if let Some(dir) = crate::webview_dir() {
        b = b.data_directory(dir);
    }
    let win = b.build().map_err(|e| {
        tracing::warn!("흐름도 창을 열 수 없습니다: {e}");
        ErrView::msg("invalid", format!("창을 열 수 없습니다: {e}"))
    })?;
    tracing::info!("흐름도 창 열림: {} ({profile})", win.label());
    // 메뉴가 닫히며 메인 창이 다시 앞으로 오면 새 창이 그 뒤로 가 사라진 것처럼 보인다 — 몇 번 앞으로 가져온다
    tauri::async_runtime::spawn(async move {
        for ms in [0u64, 250, 800] {
            tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
            let _ = win.unminimize();
            let _ = win.show();
            let _ = win.set_focus();
        }
    });
    Ok(())
}

#[derive(Serialize)]
pub struct UnitDetail {
    unit: UnitResult,
    chunks: Vec<ChunkResult>,
}

#[tauri::command]
pub fn analysis_unit(st: State<'_, AppState>, id: Option<u64>, set: Option<String>, key: String) -> R<UnitDetail> {
    let store = store_of(&st, id, set.as_deref())?;
    let unit = store.read_unit(&key).ok_or_else(|| ErrView::msg("invalid", format!("{key} 의 분석 결과가 없습니다")))?;
    let chunks = store.chunks_of(&unit);
    Ok(UnitDetail { unit, chunks })
}

#[derive(Serialize)]
pub struct ColumnWrites {
    owner: Option<String>,
    table: String,
    column: String,
    writes: Vec<sqls_analyze::colwrite::ColWrite>,
}

#[derive(Serialize)]
pub struct ColumnSource {
    lineage: sqls_core::complete::Lineage,
    /// 분석 결과(문장 목록)가 있는지 — 없으면 writes 는 비어 있다
    analyzed: bool,
    /// 닿은 테이블 열마다 그 열에 값을 넣는 문장
    writes: Vec<ColumnWrites>,
}

/// 읽어 둔 문장 목록 (파일과 고친 때가 같으면 다시 읽지 않는다)
type Stmts = Arc<Vec<sqls_analyze::colwrite::StmtLite>>;
static STMTS: std::sync::Mutex<Option<(std::path::PathBuf, std::time::SystemTime, Stmts)>> = std::sync::Mutex::new(None);

fn statements(path: &std::path::Path) -> Option<Stmts> {
    let mtime = std::fs::metadata(path).and_then(|m| m.modified()).ok()?;
    if let Some((p, t, v)) = STMTS.lk().as_ref() {
        if p == path && *t == mtime {
            return Some(v.clone());
        }
    }
    let f = std::fs::File::open(path).ok()?;
    let v: Vec<sqls_analyze::colwrite::StmtLite> = serde_json::from_reader(std::io::BufReader::new(f)).ok()?;
    let v = Arc::new(v);
    *STMTS.lk() = Some((path.to_path_buf(), mtime, v.clone()));
    Some(v)
}

/// 결과 그리드 열이 만들어진 길 — 어느 테이블의 어느 열을 어떤 식으로(쿼리를 캐시로 풀어서),
/// 그리고 분석 결과에서 그 열들에 값을 넣는 문장들. 우클릭으로 고를 때만 돈다.
/// 문장 목록은 처음 한 번 읽어 두고 파일이 바뀔 때만 다시 읽는다
#[tauri::command]
pub async fn column_source(st: State<'_, AppState>, id: u64, sql: String, pos: Option<usize>, name: String) -> R<ColumnSource> {
    let lineage = crate::complete::column_lineage(&st, id, &sql, pos, &name).await?;
    let cols: Vec<(Option<String>, String, String)> = lineage.sources().into_iter().map(|s| (s.owner.clone(), s.table.clone(), s.column.clone())).collect();
    if cols.is_empty() {
        return Ok(ColumnSource { lineage, analyzed: false, writes: vec![] });
    }
    let path = store_for(&st, id)?.root().join("integrated").join("statements.json");
    let found = tauri::async_runtime::spawn_blocking(move || {
        statements(&path).map(|v| {
            cols.into_iter()
                .map(|(owner, table, column)| {
                    let writes = sqls_analyze::colwrite::column_writes(&v, owner.as_deref(), &table, &column);
                    ColumnWrites { owner, table, column, writes }
                })
                .collect::<Vec<_>>()
        })
    })
    .await
    .map_err(|e| ErrView::msg("io", e.to_string()))?;
    Ok(ColumnSource { lineage, analyzed: found.is_some(), writes: found.unwrap_or_default() })
}

/// 결과 폴더 경로
#[tauri::command]
pub fn analysis_dir(st: State<'_, AppState>, id: Option<u64>, set: Option<String>) -> R<String> {
    Ok(store_of(&st, id, set.as_deref())?.root().display().to_string())
}

/// 품질 평가 (저장된 조각 결과만 읽는다 — 모델을 부르지 않는다). Markdown 과 숫자.
#[tauri::command]
pub fn analysis_eval(st: State<'_, AppState>, id: Option<u64>, set: Option<String>) -> R<serde_json::Value> {
    let store = store_of(&st, id, set.as_deref())?;
    let r = sqls_analyze::eval::evaluate(&store, sqls_analyze::llm::Lang::Ko);
    let md = sqls_analyze::eval::markdown(&r, None);
    let _ = store.write_text("integrated/eval.md", &md);
    Ok(serde_json::json!({ "report": r, "markdown": md }))
}

// ── 결과 묶음: 내보내기·가져오기 ─────────────────────────────

/// 이 PC 의 분석 결과들 (접속 프로필의 것 + 가져온 것)
#[tauri::command]
pub async fn analysis_sets() -> R<Vec<bundle::SetInfo>> {
    blocking(|| {
        let root = sqls_analyze::analysis_root();
        bundle::purge_trash(&root);
        bundle::list_sets(&root)
    })
    .await
}

/// 결과 하나의 단위 목록 — 접속 없이 볼 때 (DB 목록 대신)
#[tauri::command]
pub async fn analysis_set_units(set: String) -> R<Vec<UnitRow>> {
    let store = Store::open(set_dir(&set)?).map_err(|e| ErrView::msg("io", e.to_string()))?;
    let briefs = blocking(move || store.briefs()).await?;
    let mut rows: Vec<UnitRow> = briefs
        .iter()
        .map(|b| row_of(Some(b), UnitRef { owner: b.owner.clone(), name: b.name.clone(), unit_type: b.unit_type.clone() }))
        .collect();
    rows.sort_by(|a, b| (&a.unit.owner, &a.unit.name).cmp(&(&b.unit.owner, &b.unit.name)));
    Ok(rows)
}

/// 결과를 파일 하나(.sqla)로. 탭 접속의 결과면 DB 정보도 머리에 적는다
#[tauri::command]
pub fn analysis_export(st: State<'_, AppState>, id: Option<u64>, set: Option<String>, path: String) -> R<bundle::Manifest> {
    let store = store_of(&st, id, set.as_deref())?;
    let name = match (&set, id) {
        (Some(n), _) if !n.is_empty() => n.clone(),
        (_, Some(id)) => profile_of(&st, id)?,
        _ => String::new(),
    };
    let mut m = bundle::Manifest::new(&name, env!("CARGO_PKG_VERSION"));
    if set.as_deref().is_none_or(str::is_empty) {
        if let Some(Ok((s, _, _))) = id.map(|i| st.session(i)) {
            m.db_user = Some(format!("{}@{}", s.info().user, s.info().connect_string));
            m.server = Some(s.info().server_version.clone());
        }
    }
    bundle::export(&store, std::path::Path::new(&path), m).map_err(|e| ErrView::msg("io", e.to_string()))
}

/// 가져올 수 있는 곳 하나 — 접속 프로필의 결과 폴더, 또는 따로 둔(가져온) 결과
#[derive(Serialize)]
pub struct ImportTarget {
    name: String,
    /// "connection" | "imported"
    kind: &'static str,
    /// 있는 단위 수 (0 이면 비어 있다 — 그대로 푼다, 아니면 합친다)
    units: usize,
    /// 묶음을 만든 DB 와 같아 보이면 (접속: 계정@접속 문자열, 가져온 것: 같은 곳에서 내보낸 것)
    same_db: bool,
    /// 같은 파일(같은 곳·같은 시각에 내보낸 것)을 이미 가져온 때
    already: Option<u64>,
}

#[derive(Serialize)]
pub struct ImportPeek {
    manifest: bundle::Manifest,
    targets: Vec<ImportTarget>,
    /// 고를 곳 (없으면 새 이름으로 따로)
    suggested: Option<String>,
}

fn same_user(a: Option<&str>, b: &str) -> bool {
    a.is_some_and(|a| a.eq_ignore_ascii_case(b))
}

/// 가져오기 전에 무엇인지, 어디로 가져갈지 본다
#[tauri::command]
pub fn analysis_peek(st: State<'_, AppState>, path: String) -> R<ImportPeek> {
    let m = bundle::peek(std::path::Path::new(&path)).map_err(|e| ErrView::msg("invalid", e.to_string()))?;
    let sets = bundle::list_sets(&sqls_analyze::analysis_root());
    let already = |dir: &std::path::Path| {
        bundle::import_log(dir).iter().rev().find(|l| l.profile == m.profile && l.exported_at == m.exported_at).map(|l| l.imported_at)
    };
    let mut targets = Vec::new();
    for p in st.cfg.rd().connections.iter() {
        let dir = sqls_analyze::default_dir(&p.name);
        let units = sets.iter().find(|x| std::path::Path::new(&x.dir) == dir).map(|x| x.units).unwrap_or(0);
        let same_db = match &m.db_user {
            Some(u) => u.eq_ignore_ascii_case(&format!("{}@{}", p.user, p.connect_string)),
            None => p.name == m.profile,
        };
        targets.push(ImportTarget { name: p.name.clone(), kind: "connection", units, same_db, already: already(&dir) });
    }
    for x in sets.iter().filter(|x| x.imported.is_some()) {
        let i = x.imported.as_ref().unwrap();
        let same_db = i.profile == m.profile && (m.db_user.is_none() || same_user(i.db_user.as_deref(), m.db_user.as_deref().unwrap_or("")));
        if targets.iter().any(|t| t.name == x.name) {
            continue;
        }
        targets.push(ImportTarget { name: x.name.clone(), kind: "imported", units: x.units, same_db, already: already(std::path::Path::new(&x.dir)) });
    }
    // 같은 DB 의 접속 (이름까지 같으면 그것) → 같은 곳에서 가져온 결과 → 없으면 새로
    let suggested = targets
        .iter()
        .filter(|t| t.same_db)
        .min_by_key(|t| (t.kind != "connection", t.name != m.profile))
        .map(|t| t.name.clone());
    Ok(ImportPeek { manifest: m, targets, suggested })
}

#[derive(Serialize)]
pub struct ImportDone {
    name: String,
    /// 접속 프로필의 결과로 들어갔으면 true
    connection: bool,
    #[serde(flatten)]
    report: bundle::MergeReport,
}

/// 파일을 이 PC 의 결과로 가져온다. `target` 이 접속 이름이면 그 접속의 결과로 (분석 표시·값의 출처에 쓰인다),
/// 아니면 그 이름의 따로 둔 결과로. 이미 결과가 있으면 합친다 — 단위마다 더 나중에 분석한 것이 남는다
#[tauri::command]
pub async fn analysis_import(st: State<'_, AppState>, path: String, target: String, on_step: Channel<ImportStep>) -> R<ImportDone> {
    let target = target.trim().to_string();
    let connection = st.cfg.rd().connections.iter().any(|p| p.name == target);
    let (name, dest) = if connection {
        (target.clone(), sqls_analyze::default_dir(&target))
    } else {
        let n = sqls_analyze::safe_set_name(&target);
        let d = sqls_analyze::analysis_root().join(&n);
        (n, d)
    };
    // 큰 파일은 오래 걸린다 — 작업 스레드에서, 단계·개수를 화면에 알리며 (창이 멈춘 것처럼 보이지 않게).
    // 멈추거나(중지·앱 종료) 실패해도 풀어 둔 것이 남아, 같은 파일을 같은 곳으로 다시 가져오면 이어서 한다
    IMPORT_STOP.store(false, Ordering::Relaxed);
    let report = tauri::async_runtime::spawn_blocking(move || {
        let on = step_sender(on_step);
        bundle::import_resumable(std::path::Path::new(&path), &dest, !connection, &target, &on, &IMPORT_STOP).map(|(_, r)| r)
    })
    .await
    .map_err(|e| ErrView::msg("io", e.to_string()))?
    .map_err(|e| ErrView::msg("io", e.to_string()))?;
    Ok(ImportDone { name, connection, report })
}

/// 가져오기·합치기 중지 — 항목·단위 사이에서 멈추고 풀어 둔 것은 남긴다 (나중에 이어서)
static IMPORT_STOP: AtomicBool = AtomicBool::new(false);

#[tauri::command]
pub fn analysis_import_stop() {
    IMPORT_STOP.store(true, Ordering::Relaxed);
}

/// 중단된 가져오기 (이어서 · 버리기)
#[tauri::command]
pub fn analysis_pending_imports() -> Vec<bundle::Pending> {
    bundle::pending_imports(&sqls_analyze::analysis_root())
}

#[tauri::command]
pub fn analysis_discard_import(id: String) -> R<()> {
    bundle::discard_pending(&sqls_analyze::analysis_root(), &id).map_err(|e| ErrView::msg("io", e.to_string()))
}

/// 가져오기·합치기 진행 (화면의 상태 줄)
#[derive(Serialize, Clone)]
pub struct ImportStep {
    step: bundle::Step,
    done: usize,
    total: usize,
}

/// 진행을 화면으로 — 단계가 바뀔 때와 끝났을 때는 바로, 그 밖에는 0.1초에 한 번만 (항목이 수만 개일 수 있다)
fn step_sender(ch: Channel<ImportStep>) -> impl Fn(bundle::Step, usize, usize) {
    let last = std::sync::Mutex::new((None::<bundle::Step>, std::time::Instant::now()));
    move |step, done, total| {
        let mut l = last.lk();
        let now = std::time::Instant::now();
        if l.0 == Some(step) && done != total && now.duration_since(l.1) < std::time::Duration::from_millis(100) {
            return;
        }
        *l = (Some(step), now);
        let _ = ch.send(ImportStep { step, done, total });
    }
}

/// 따로 둔(가져온) 결과를 접속의 결과에 합친다 — 전에 다른 이름으로 가져와 접속에서 분석 표시가 안 보이던 것.
/// 따로 둔 결과는 그대로 둔다 (지우는 것은 화면이 묻고 따로)
#[tauri::command]
pub async fn analysis_merge_set(st: State<'_, AppState>, set: String, into: String, on_step: Channel<ImportStep>) -> R<bundle::MergeReport> {
    if !st.cfg.rd().connections.iter().any(|p| p.name == into) {
        return Err(ErrView::msg("invalid", format!("'{into}' 접속이 없습니다")));
    }
    let from = set_dir(&set)?;
    let dest = sqls_analyze::default_dir(&into);
    if from == dest {
        return Err(ErrView::msg("invalid", "같은 결과입니다"));
    }
    IMPORT_STOP.store(false, Ordering::Relaxed);
    tauri::async_runtime::spawn_blocking(move || {
        let on = step_sender(on_step);
        // 합치기는 단위마다 바꿔 끼우므로 멈춰도 다시 하면 남은 것만 — 통합은 다시 만든다
        bundle::merge_dir_ctl(&from, &dest, &on, &IMPORT_STOP, true)
    })
    .await
    .map_err(|e| ErrView::msg("io", e.to_string()))?
    .map_err(|e| ErrView::msg("io", e.to_string()))
}

/// 가져온 결과를 지운다 (접속 프로필의 결과는 지우지 않는다 — 폴더에서 직접)
#[tauri::command]
pub fn analysis_remove_set(name: String) -> R<()> {
    let d = set_dir(&name)?;
    if !d.join("imported.json").is_file() {
        return Err(ErrView::msg("invalid", "가져온 결과만 여기서 지울 수 있습니다"));
    }
    // 파일이 수만 개라 몇 초 걸린다 — 이름만 바로 바꾸고 실제로 지우는 것은 뒤에서
    bundle::remove_set_dir(&d).map_err(|e| ErrView::msg("io", e.to_string()))
}

// ── 흐름도 보고서 (결과 폴더의 integrated/reports/<id>.json) ─────────────
// 결과 폴더에 두므로 .sqla 내보내기에 함께 들어간다. 이름은 결과 폴더 이름(접속 프로필 또는 가져온 결과).

fn reports_dir(profile: &str) -> R<std::path::PathBuf> {
    let root = sqls_analyze::default_dir(profile);
    if !root.join("units").is_dir() {
        return Err(ErrView::msg("invalid", format!("'{profile}' 분석 결과가 이 PC 에 없습니다")));
    }
    Ok(root.join("integrated").join("reports"))
}

fn report_path(profile: &str, id: &str) -> R<std::path::PathBuf> {
    if id.is_empty() || id.len() > 64 || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
        return Err(ErrView::msg("invalid", format!("잘못된 보고서 id: {id}")));
    }
    Ok(reports_dir(profile)?.join(format!("{id}.json")))
}

#[derive(Serialize)]
pub struct ReportInfo {
    id: String,
    name: String,
    center: Option<String>,
    updated_at: Option<u64>,
}

/// 보고서 목록 (이름순)
#[tauri::command]
pub fn dfd_reports(profile: String) -> R<Vec<ReportInfo>> {
    let dir = reports_dir(&profile)?;
    let mut out: Vec<ReportInfo> = std::fs::read_dir(&dir)
        .map(|rd| {
            rd.flatten()
                .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
                .filter_map(|e| {
                    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(e.path()).ok()?).ok()?;
                    Some(ReportInfo {
                        id: v["id"].as_str()?.to_string(),
                        name: v["name"].as_str().unwrap_or("").to_string(),
                        center: v["basis"]["center"].as_str().map(String::from),
                        updated_at: v["updated_at"].as_u64(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    Ok(out)
}

#[tauri::command]
pub fn dfd_report_get(profile: String, id: String) -> R<serde_json::Value> {
    let p = report_path(&profile, &id)?;
    let text = std::fs::read_to_string(&p).map_err(|e| ErrView::msg("io", format!("보고서를 읽을 수 없습니다: {e}")))?;
    serde_json::from_str(&text).map_err(|e| ErrView::msg("invalid", format!("보고서 파일이 깨졌습니다: {e}")))
}

/// 저장 — 임시 파일에 쓰고 바꿔 끼운다 (쓰다 꺼져도 앞의 것이 남는다)
#[tauri::command]
pub fn dfd_report_save(profile: String, report: serde_json::Value) -> R<()> {
    let id = report["id"].as_str().unwrap_or("").to_string();
    let p = report_path(&profile, &id)?;
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d).map_err(|e| ErrView::msg("io", e.to_string()))?;
    }
    let tmp = p.with_extension("tmp");
    let bytes = serde_json::to_vec_pretty(&report).map_err(|e| ErrView::msg("invalid", e.to_string()))?;
    std::fs::write(&tmp, bytes).and_then(|_| std::fs::rename(&tmp, &p)).map_err(|e| ErrView::msg("io", e.to_string()))
}

#[tauri::command]
pub fn dfd_report_delete(profile: String, id: String) -> R<()> {
    std::fs::remove_file(report_path(&profile, &id)?).map_err(|e| ErrView::msg("io", e.to_string()))
}
