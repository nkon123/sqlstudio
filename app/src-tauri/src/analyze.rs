//! PL/SQL 분석 화면의 명령 — sqls-analyze 를 앱에서 돌린다.
//!
//! - 사전 조회는 프로필의 메타 세션(읽기 전용)으로 한다. 사용자의 작업 세션을 막지 않는다.
//! - 결과는 CLI 와 같은 폴더(설정 파일 옆 analysis/<프로필>)에 쌓인다 — 앱에서 시작하고 CLI 로 이어 가도 된다.
//! - 모델에는 소스 텍스트만 간다. 외부 공급자면 화면이 먼저 확인을 받고 `allow_remote` 로 알려 준다.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use serde::Serialize;
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

/// 메타 세션 — 접속 직후면 잠깐 기다린다
async fn meta_session(st: &AppState, id: u64) -> R<Arc<crate::complete::MetaHub>> {
    let hub = hub_for(st, id)?;
    for _ in 0..100 {
        if hub.session().is_some() {
            return Ok(hub);
        }
        if hub.status().phase == "error" {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    Err(ErrView::msg("session_closed", "사전 조회 세션이 아직 준비되지 않았습니다"))
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
    let s = hub.session().unwrap();
    let owner = owner.filter(|o| !o.trim().is_empty()).unwrap_or_else(|| s.info().user.clone());
    let pat = name_like.filter(|p| !p.trim().is_empty()).map(|p| if p.contains('%') { p } else { format!("%{p}%") });
    let refs = source::list_units(s, &owner, &[], pat.as_deref()).await?;
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
        let s = hub.session().unwrap();
        let total = units.len();
        let mut stop: Option<String> = None;
        for (i, u) in units.iter().enumerate() {
            if cancel.load(Ordering::Relaxed) {
                stop = Some("중지했습니다".into());
                break;
            }
            let key = sqls_analyze::chunk::unit_key(&u.owner, &u.name, &u.unit_type);
            let (src, spec) = match source::fetch(s, u).await {
                Ok(x) => x,
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
pub fn analysis_result(st: State<'_, AppState>, id: u64, rebuild: bool) -> R<Option<Integrated>> {
    let store = store_for(&st, id)?;
    if rebuild {
        let overview = store.read_integrated::<Integrated>("integrated.json").and_then(|g| g.overview);
        let (g, _) = integrate::write_all(&store, overview).map_err(|e| ErrView::msg("io", e.to_string()))?;
        return Ok(Some(g));
    }
    Ok(store.read_integrated("integrated.json"))
}

#[derive(Serialize)]
pub struct UnitDetail {
    unit: UnitResult,
    chunks: Vec<ChunkResult>,
}

#[tauri::command]
pub fn analysis_unit(st: State<'_, AppState>, id: u64, key: String) -> R<UnitDetail> {
    let store = store_for(&st, id)?;
    let unit = store.read_unit(&key).ok_or_else(|| ErrView::msg("invalid", format!("{key} 의 분석 결과가 없습니다")))?;
    let chunks = store.chunks_of(&unit);
    Ok(UnitDetail { unit, chunks })
}

/// 결과 폴더 경로
#[tauri::command]
pub fn analysis_dir(st: State<'_, AppState>, id: u64) -> R<String> {
    Ok(store_for(&st, id)?.root().display().to_string())
}

/// 품질 평가 (저장된 조각 결과만 읽는다 — 모델을 부르지 않는다). Markdown 과 숫자.
#[tauri::command]
pub fn analysis_eval(st: State<'_, AppState>, id: u64) -> R<serde_json::Value> {
    let store = store_for(&st, id)?;
    let r = sqls_analyze::eval::evaluate(&store, sqls_analyze::llm::Lang::Ko);
    let md = sqls_analyze::eval::markdown(&r, None);
    let _ = store.write_text("integrated/eval.md", &md);
    Ok(serde_json::json!({ "report": r, "markdown": md }))
}
