//! PL/SQL 분석 화면의 명령 — sqls-analyze 를 앱에서 돌린다.
//!
//! - 사전 조회는 프로필의 메타 세션(읽기 전용)으로 한다. 사용자의 작업 세션을 막지 않는다.
//! - 결과는 CLI 와 같은 폴더(설정 파일 옆 analysis/<프로필>)에 쌓인다 — 앱에서 시작하고 CLI 로 이어 가도 된다.
//! - 모델에는 소스 텍스트만 간다. 외부 공급자면 화면이 먼저 확인을 받고 `allow_remote` 로 알려 준다.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use serde::Serialize;
use sqls_analyze::bundle;
use sqls_analyze::integrate::{self, Integrated};
use sqls_analyze::run::{analyze_unit, Event, Llm, Options};
use sqls_analyze::source::{self, UnitRef};
use sqls_analyze::store::{ChunkResult, Store, UnitResult};
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

fn store_for(st: &AppState, id: u64) -> R<Store> {
    let p = profile_of(st, id)?;
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
    Ok(refs
        .into_iter()
        .map(|u| {
            let saved = store.read_unit(&sqls_analyze::chunk::unit_key(&u.owner, &u.name, &u.unit_type));
            UnitRow {
                analyzed_at: saved.as_ref().map(|x| x.analyzed_at),
                chunks: saved.as_ref().map(|x| x.stats.chunks),
                failed: saved.as_ref().map(|x| x.stats.failed),
                model: saved.as_ref().and_then(|x| x.model.clone()),
                summary: saved.as_ref().and_then(|x| x.summary.as_ref().map(|i| i.summary.clone())),
                warning: saved.as_ref().and_then(|x| x.warning.clone()),
                unit: u,
            }
        })
        .collect())
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
    st.analyses.lock().unwrap().insert(run, cancel.clone());

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
            st.analyses.lock().unwrap().remove(&run);
        }
    });
    Ok(run)
}

/// 멈춤 — 진행 중인 조각까지 저장하고 멈춘다
#[tauri::command]
pub fn analysis_cancel(st: State<'_, AppState>, run: u64) {
    if let Some(c) = st.analyses.lock().unwrap().get(&run) {
        c.store(true, Ordering::Relaxed);
    }
}

/// 통합 분석 결과 (없으면 None). `rebuild` 면 저장된 단위로 다시 만든다 (모델 없이).
#[tauri::command]
pub fn analysis_result(st: State<'_, AppState>, id: Option<u64>, set: Option<String>, rebuild: bool) -> R<Option<Integrated>> {
    let store = store_of(&st, id, set.as_deref())?;
    if rebuild {
        let overview = store.read_integrated::<Integrated>("integrated.json").and_then(|g| g.overview);
        let (g, _) = integrate::write_all(&store, overview).map_err(|e| ErrView::msg("io", e.to_string()))?;
        return Ok(Some(g));
    }
    Ok(store.read_integrated("integrated.json"))
}

/// 프로필의 통합 분석 결과 — 흐름도 창용 (메인 창의 접속이 끊겨도 저장된 결과는 읽는다)
#[tauri::command]
pub fn analysis_result_for(profile: String) -> R<Option<Integrated>> {
    let store = Store::open(sqls_analyze::default_dir(&profile)).map_err(|e| ErrView::msg("io", e.to_string()))?;
    Ok(store.read_integrated("integrated.json"))
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
        (None, Some(id)) => profile_of(&st, id)?,
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
        .min_inner_size(700.0, 450.0);
    if let Some(dir) = crate::webview_dir() {
        b = b.data_directory(dir);
    }
    b.build()
        .map_err(|e| ErrView::msg("invalid", format!("창을 열 수 없습니다: {e}")))?;
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
pub fn analysis_sets() -> Vec<bundle::SetInfo> {
    bundle::list_sets(&sqls_analyze::analysis_root())
}

/// 결과 하나의 단위 목록 — 접속 없이 볼 때 (DB 목록 대신)
#[tauri::command]
pub fn analysis_set_units(set: String) -> R<Vec<UnitRow>> {
    let store = Store::open(set_dir(&set)?).map_err(|e| ErrView::msg("io", e.to_string()))?;
    let mut rows: Vec<UnitRow> = store
        .units()
        .into_iter()
        .map(|x| UnitRow {
            analyzed_at: Some(x.analyzed_at),
            chunks: Some(x.stats.chunks),
            failed: Some(x.stats.failed),
            model: x.model.clone(),
            summary: x.summary.as_ref().map(|i| i.summary.clone()),
            warning: x.warning.clone(),
            unit: UnitRef { owner: x.owner, name: x.name, unit_type: x.unit_type },
        })
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

/// 가져오기 전에 무엇인지 본다
#[tauri::command]
pub fn analysis_peek(path: String) -> R<bundle::Manifest> {
    bundle::peek(std::path::Path::new(&path)).map_err(|e| ErrView::msg("invalid", e.to_string()))
}

/// 파일을 이 PC 의 결과로 가져온다. 같은 이름이 있으면 overwrite 일 때만 바꿔 끼운다. 돌려주는 것은 붙인 이름
#[tauri::command]
pub fn analysis_import(path: String, name: String, overwrite: bool) -> R<String> {
    let name = sqls_analyze::safe_set_name(&name);
    let dest = sqls_analyze::analysis_root().join(&name);
    bundle::import(std::path::Path::new(&path), &dest, overwrite).map_err(|e| match e.kind() {
        std::io::ErrorKind::AlreadyExists => ErrView::msg("exists", format!("'{name}' 결과가 이미 있습니다")),
        _ => ErrView::msg("io", e.to_string()),
    })?;
    Ok(name)
}

/// 가져온 결과를 지운다 (접속 프로필의 결과는 지우지 않는다 — 폴더에서 직접)
#[tauri::command]
pub fn analysis_remove_set(name: String) -> R<()> {
    let d = set_dir(&name)?;
    if !d.join("imported.json").is_file() {
        return Err(ErrView::msg("invalid", "가져온 결과만 여기서 지울 수 있습니다"));
    }
    std::fs::remove_dir_all(&d).map_err(|e| ErrView::msg("io", e.to_string()))
}
