//! 스키마 비교 화면의 명령.
//!
//! - 양쪽 모두 이 비교만을 위한 읽기 전용 세션을 따로 연다 — 작업 탭·자동완성 세션을 막지 않는다. 끝나면 닫는다.
//! - 두 스키마를 동시에 읽는다. 진행은 "schema-compare" 이벤트로.
//! - 결과(양쪽 구조·다른 소스)는 메모리에 두고, 화면이 닫거나 새로 비교하면 버린다.
//! - 스크립트는 실행하지 않고 문자열로 돌려준다 (화면이 새 탭으로 연다).

use sqls_core::sync::{MutexExt, RwLockExt};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use serde::Serialize;
use sqls_core::schemacmp::{self, Item, Kinds, Snapshot, Status};
use sqls_core::{ConnectSpec, Session};
use tauri::{AppHandle, Emitter, State};

use crate::state::{AppState, ErrView};

type R<T> = Result<T, ErrView>;

struct Run {
    cancel: Arc<AtomicBool>,
    /// 일이 끝났다 (성공·실패 모두)
    finished: AtomicBool,
    done: Mutex<Option<Done>>,
}

struct Done {
    a: Snapshot,
    b: Snapshot,
    items: Vec<Item>,
    /// "KIND NAME" → 소스 (원본·대상)
    src_code: BTreeMap<String, String>,
    tgt_code: BTreeMap<String, String>,
    header: String,
}

fn runs() -> &'static Mutex<HashMap<u64, Arc<Run>>> {
    static RUNS: OnceLock<Mutex<HashMap<u64, Arc<Run>>>> = OnceLock::new();
    RUNS.get_or_init(|| Mutex::new(HashMap::new()))
}

static NEXT: AtomicU64 = AtomicU64::new(1);

#[derive(Serialize, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Progress {
    Phase { run: u64, side: &'static str, phase: String },
    Done { run: u64, items: Vec<Item>, src: String, tgt: String, ms: u64, counts: (usize, usize, usize) },
    Failed { run: u64, error: String },
}

fn compare_spec(p: &sqls_core::config::Profile, pw: String) -> ConnectSpec {
    let mut spec = p.to_spec(pw);
    spec.read_only = true;
    spec.dbms_output = false;
    spec.module = "SQLStudio-Compare".into();
    // 큰 스키마의 사전 조회도 넉넉히, 그래도 끝없이 매달리지 않게
    spec.call_timeout = Some(Duration::from_secs(300));
    spec
}

async fn open(spec: ConnectSpec) -> sqls_core::Result<Session> {
    match tokio::time::timeout(Duration::from_secs(60), Session::connect(spec)).await {
        Ok(r) => r,
        Err(_) => Err(sqls_core::Error::ConnectionLost("60초 안에 서버가 응답하지 않습니다".into())),
    }
}

/// 비교 시작. 대상 비밀번호를 모르면 "password_required" — 화면이 물어 password 로 다시 부른다
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn schema_compare_start(
    app: AppHandle,
    st: State<'_, AppState>,
    id: u64,
    src_schema: Option<String>,
    target: String,
    tgt_schema: Option<String>,
    kinds: Kinds,
    name_like: Option<String>,
    password: Option<String>,
) -> R<u64> {
    let src_profile = st.sessions.lk().get(&id).map(|s| s.profile.clone()).ok_or_else(|| ErrView::msg("session_closed", "원본 탭이 접속되어 있지 않습니다"))?;
    let (sp, tp) = {
        let cfg = st.cfg.rd();
        (
            cfg.profile(&src_profile).cloned().ok_or_else(|| ErrView::msg("invalid", format!("접속 프로필이 없습니다: {src_profile}")))?,
            cfg.profile(&target).cloned().ok_or_else(|| ErrView::msg("invalid", format!("접속 프로필이 없습니다: {target}")))?,
        )
    };
    let pw_of = |p: &sqls_core::config::Profile| st.passwords.rd().get(&p.name.to_uppercase()).cloned().or_else(|| p.stored_password());
    let spw = pw_of(&sp).ok_or_else(|| ErrView::msg("invalid", "원본 비밀번호를 모릅니다 — 탭을 다시 접속하세요"))?;
    let typed = password.filter(|p| !p.is_empty());
    let tpw = typed.clone().or_else(|| pw_of(&tp)).ok_or_else(|| ErrView::msg("password_required", format!("{} 비밀번호를 입력하세요", tp.name)))?;

    // 접속은 여기서 (비밀번호가 틀리면 바로 알린다). 양쪽 동시에
    let (a, b) = tokio::join!(open(compare_spec(&sp, spw)), open(compare_spec(&tp, tpw.clone())));
    let a = a.map_err(ErrView::from)?;
    let b = match b {
        Ok(b) => b,
        Err(e) => {
            a.close();
            if matches!(e, sqls_core::Error::Db { code: 1017, .. }) {
                return Err(ErrView::msg("password_required", format!("{} 비밀번호가 틀렸습니다 (ORA-01017)", tp.name)));
            }
            return Err(e.into());
        }
    };
    if typed.is_some() {
        st.passwords.wr().insert(tp.name.to_uppercase(), tpw);
    }
    let src_owner = src_schema.filter(|s| !s.trim().is_empty()).unwrap_or_else(|| a.info().user.clone()).to_uppercase();
    let tgt_owner = tgt_schema.filter(|s| !s.trim().is_empty()).unwrap_or_else(|| b.info().user.clone()).to_uppercase();
    let src_label = format!("{} · {src_owner}", sp.name);
    let tgt_label = format!("{} · {tgt_owner}", tp.name);

    let run = NEXT.fetch_add(1, Ordering::Relaxed);
    let cancel = Arc::new(AtomicBool::new(false));
    let r = Arc::new(Run { cancel: cancel.clone(), finished: AtomicBool::new(false), done: Mutex::new(None) });
    {
        let mut m = runs().lk();
        m.clear(); // 앞의 결과는 버린다 (메모리)
        m.insert(run, r.clone());
    }
    tracing::info!("스키마 비교 #{run}: {src_label} → {tgt_label}");
    // 멈추면 세션을 버린다 — 매달린 사전 조회도 바로 풀린다
    {
        let (wa, wb, w) = (a.clone(), b.clone(), r.clone());
        tauri::async_runtime::spawn(async move {
            while !w.finished.load(Ordering::Relaxed) {
                if w.cancel.load(Ordering::Relaxed) {
                    wa.abandon();
                    wb.abandon();
                    return;
                }
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        });
    }
    tauri::async_runtime::spawn(async move {
        let emit = |p: Progress| {
            let _ = app.emit("schema-compare", p);
        };
        let started = std::time::Instant::now();
        let like = name_like.as_deref();
        let pa = |phase: &str| emit(Progress::Phase { run, side: "src", phase: phase.into() });
        let pb = |phase: &str| emit(Progress::Phase { run, side: "tgt", phase: phase.into() });
        let result: sqls_core::Result<Done> = async {
            let (sa, sb) = tokio::join!(schemacmp::snapshot(&a, &src_owner, &kinds, like, &pa), schemacmp::snapshot(&b, &tgt_owner, &kinds, like, &pb));
            let (sa, sb) = (sa?, sb?);
            if cancel.load(Ordering::Relaxed) {
                return Err(sqls_core::Error::Cancelled);
            }
            let items = schemacmp::compare(&sa, &sb);
            // 다른 소스만 읽는다 (스크립트·비교 화면용)
            let code = |i: &&Item| schemacmp::CODE_TYPES.contains(&i.kind.as_str());
            let src_keys: Vec<String> = items.iter().filter(code).filter(|i| i.status != Status::OnlyTarget).map(|i| i.key()).collect();
            let tgt_keys: Vec<String> = items.iter().filter(code).filter(|i| i.status != Status::OnlySource).map(|i| i.key()).collect();
            emit(Progress::Phase { run, side: "both", phase: format!("다른 소스 {}개 읽는 중", src_keys.len().max(tgt_keys.len())) });
            let (ca, cb) = tokio::join!(schemacmp::sources(&a, &src_owner, &src_keys), schemacmp::sources(&b, &tgt_owner, &tgt_keys));
            let header = format!("SQLStudio 스키마 비교 — 원본 {src_label} → 대상 {tgt_label}");
            Ok(Done { a: sa, b: sb, items, src_code: ca?, tgt_code: cb?, header })
        }
        .await;
        a.close();
        b.close();
        r.finished.store(true, Ordering::Relaxed);
        match result {
            Ok(d) => {
                let counts = (
                    d.items.iter().filter(|i| i.status == Status::OnlySource).count(),
                    d.items.iter().filter(|i| i.status == Status::Changed).count(),
                    d.items.iter().filter(|i| i.status == Status::OnlyTarget).count(),
                );
                let ms = started.elapsed().as_millis() as u64;
                tracing::info!("스키마 비교 #{run}: {}ms, 대상에 없음 {} · 다름 {} · 대상에만 {}", ms, counts.0, counts.1, counts.2);
                let items = d.items.clone();
                *r.done.lk() = Some(d);
                emit(Progress::Done { run, items, src: src_label, tgt: tgt_label, ms, counts });
            }
            Err(e) => {
                let msg = if matches!(e, sqls_core::Error::Cancelled) || cancel.load(Ordering::Relaxed) { "멈췄습니다".to_string() } else { e.to_string() };
                tracing::warn!("스키마 비교 #{run} 실패: {msg}");
                emit(Progress::Failed { run, error: msg });
            }
        }
    });
    Ok(run)
}

#[tauri::command]
pub fn schema_compare_cancel(run: u64) {
    if let Some(r) = runs().lk().get(&run) {
        r.cancel.store(true, Ordering::Relaxed);
    }
}

#[derive(Serialize)]
pub struct Detail {
    details: Vec<String>,
    /// 소스·뷰면 양쪽 본문
    src: Option<String>,
    tgt: Option<String>,
}

fn with_done<T>(run: u64, f: impl FnOnce(&Done) -> T) -> R<T> {
    let r = runs().lk().get(&run).cloned().ok_or_else(|| ErrView::msg("invalid", "비교 결과가 없습니다 — 다시 비교하세요"))?;
    let d = r.done.lk();
    let d = d.as_ref().ok_or_else(|| ErrView::msg("invalid", "비교가 아직 끝나지 않았습니다"))?;
    Ok(f(d))
}

/// 항목 하나의 자세한 차이 (소스·뷰는 양쪽 본문)
#[tauri::command]
pub fn schema_compare_detail(run: u64, key: String) -> R<Detail> {
    with_done(run, |d| {
        let it = d.items.iter().find(|i| i.key() == key);
        let (kind, name) = schemacmp::split_key(&key);
        let (src, tgt) = if kind == "VIEW" {
            (d.a.views.get(name).cloned(), d.b.views.get(name).cloned())
        } else {
            (d.src_code.get(&key).cloned(), d.tgt_code.get(&key).cloned())
        };
        Detail { details: it.map(|i| i.details.clone()).unwrap_or_default(), src, tgt }
    })
}

/// 고른 항목으로 동기화 스크립트 (실행하지 않는다)
#[tauri::command]
pub fn schema_compare_script(run: u64, keys: Vec<String>) -> R<String> {
    with_done(run, |d| {
        let keys: BTreeSet<String> = keys.into_iter().collect();
        schemacmp::script(&d.a, &d.b, &d.items, &keys, &d.src_code, &d.header)
    })
}

/// 화면을 닫으면 결과를 버린다
#[tauri::command]
pub fn schema_compare_close(run: u64) {
    if let Some(r) = runs().lk().remove(&run) {
        r.cancel.store(true, Ordering::Relaxed);
    }
}
