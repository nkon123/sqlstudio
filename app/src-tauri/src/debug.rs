//! PL/SQL 디버거 명령. 세션 두 개(대상·제어)는 sqls_core::debug 가 연다.
//! 이 탭의 작업 세션은 소스·사전 조회에만 쓴다 (디버그 중에도 비어 있다).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use sqls_core::debug::{self, Debugger, Entry, Frame, Step, Stop, VarValue};
use sqls_core::Session;
use tauri::State;

use crate::state::{AppState, ErrView};

type R<T> = Result<T, ErrView>;

const MAX_AUTO_VARS: usize = 40;

pub struct DebugRun {
    dbg: tokio::sync::Mutex<Option<Debugger>>,
    /// 디버거를 잡고 기다리는 중에도 멈출 수 있게
    target: Session,
    /// 소스·사전 조회용 (탭의 작업 세션)
    meta: Session,
    sources: Mutex<HashMap<(String, String, String), Arc<Vec<String>>>>,
    block_lines: Vec<String>,
}

impl DebugRun {
    async fn source(&self, owner: &str, name: &str, unit_type: &str) -> R<Arc<Vec<String>>> {
        if unit_type == "ANONYMOUS BLOCK" || name.is_empty() {
            return Ok(Arc::new(self.block_lines.clone()));
        }
        let key = (owner.to_string(), name.to_string(), unit_type.to_string());
        if let Some(s) = self.sources.lock().unwrap().get(&key) {
            return Ok(s.clone());
        }
        let src = Arc::new(debug::source(&self.meta, owner, name, unit_type).await?);
        self.sources.lock().unwrap().insert(key, src.clone());
        Ok(src)
    }
}

#[derive(Serialize)]
pub struct Prepared {
    /// 중단점을 놓을 소스 (PACKAGE 면 본문)
    source: Vec<String>,
    source_type: String,
    debug_info: Option<bool>,
    compile_sql: Option<String>,
    entries: Vec<Entry>,
}

/// 단위를 디버그 화면으로 연다: 소스, 디버그 정보 여부, 호출 블록 후보
#[tauri::command]
pub async fn debug_prepare(st: State<'_, AppState>, id: u64, owner: String, name: String, unit_type: String) -> R<Prepared> {
    let (s, _, _) = st.session(id)?;
    // 실행 코드는 본문에 있다
    let source_type = match unit_type.as_str() {
        "PACKAGE" => "PACKAGE BODY".to_string(),
        "TYPE" => "TYPE BODY".to_string(),
        t => t.to_string(),
    };
    let source = debug::source(&s, &owner, &name, &source_type).await?;
    if source.is_empty() {
        return Err(ErrView::msg("invalid", format!("{owner}.{name} ({source_type}) 의 소스를 읽을 수 없습니다 (권한 또는 본문 없음)")));
    }
    let debug_info = debug::has_debug_info(&s, &owner, &name, &source_type).await;
    let entries = if matches!(unit_type.as_str(), "PACKAGE" | "PACKAGE BODY" | "PROCEDURE" | "FUNCTION") {
        let base = if unit_type == "PACKAGE BODY" { "PACKAGE" } else { unit_type.as_str() };
        debug::entries(&s, &owner, &name, base).await.unwrap_or_default()
    } else {
        Vec::new()
    };
    Ok(Prepared {
        compile_sql: debug::compile_debug_sql(&owner, &name, &source_type),
        source,
        source_type,
        debug_info,
        entries,
    })
}

/// `ALTER ... COMPILE DEBUG` — 사람이 확인 창에서 누른 뒤에만 부른다
#[tauri::command]
pub async fn debug_compile(st: State<'_, AppState>, id: u64, owner: String, name: String, unit_type: String) -> R<()> {
    let (s, _, _) = st.session(id)?;
    let sql = debug::compile_debug_sql(&owner, &name, &unit_type)
        .ok_or_else(|| ErrView::msg("invalid", format!("{unit_type} 은(는) 디버그 컴파일 대상이 아닙니다")))?;
    s.execute(&sql, Default::default()).await?;
    let errs = sqls_core::meta::compile_errors(&s, &unit_type, &format!("\"{owner}\".\"{name}\"")).await.unwrap_or_default();
    if !errs.is_empty() {
        let msg = errs.iter().take(10).map(|e| format!("{}:{} {}", e.line, e.position, e.text)).collect::<Vec<_>>().join("\n");
        return Err(ErrView::msg("compile", format!("컴파일 오류가 있습니다:\n{msg}")));
    }
    crate::complete::after_ddl(&st, id);
    Ok(())
}

#[derive(Deserialize)]
pub struct BpReq {
    owner: String,
    name: String,
    unit_type: String,
    line: u32,
}

#[derive(Serialize)]
pub struct BpResult {
    owner: String,
    name: String,
    line: u32,
    id: Option<i64>,
    error: Option<String>,
}

#[derive(Serialize)]
pub struct Snapshot {
    stop: Stop,
    stack: Vec<Frame>,
    vars: Vec<VarValue>,
    /// 끝났으면: 대상 블록의 결과
    #[serde(skip_serializing_if = "Option::is_none")]
    finished: Option<Finished>,
}

#[derive(Serialize, Clone)]
pub struct Finished {
    output: Vec<String>,
    error: Option<String>,
}

#[derive(Serialize)]
pub struct Started {
    did: u64,
    snapshot: Snapshot,
    breakpoints: Vec<BpResult>,
}

fn run(st: &AppState, did: u64) -> R<Arc<DebugRun>> {
    st.debuggers
        .lock()
        .unwrap()
        .get(&did)
        .cloned()
        .ok_or_else(|| ErrView::msg("invalid", "디버그 세션이 끝났습니다"))
}

/// 멈춘 곳의 스택과 지역 변수 (자동)
async fn snapshot(r: &DebugRun, d: &mut Debugger, stop: Stop) -> R<Snapshot> {
    if stop.terminated {
        let finished = d.result().map(|x| match x {
            Ok(e) => Finished { output: e.output, error: None },
            Err(e) => Finished { output: vec![], error: Some(e) },
        });
        return Ok(Snapshot { stop, stack: vec![], vars: vec![], finished: finished.or(Some(Finished { output: vec![], error: None })) });
    }
    let stack = d.backtrace().await.unwrap_or_default();
    let src = r.source(&stop.owner, &stop.name, &stop.unit_type).await.unwrap_or_default();
    let mut vars = Vec::new();
    for n in debug::locals_at(&src, stop.line as usize).into_iter().take(MAX_AUTO_VARS) {
        if let Ok(v) = d.get_value(&n, 0).await {
            // 범위 밖 이름(소스 추정이 틀린 것)은 숨긴다
            if v.error.as_deref().map(|e| e.starts_with("그런 변수가 없습니다")).unwrap_or(false) {
                continue;
            }
            vars.push(v);
        }
    }
    Ok(Snapshot { stop, stack, vars, finished: None })
}

#[tauri::command]
pub async fn debug_start(
    st: State<'_, AppState>,
    id: u64,
    block: String,
    binds: Vec<(String, Option<String>)>,
    breakpoints: Vec<BpReq>,
    #[allow(unused_variables)] break_on_exception: bool,
) -> R<Started> {
    let (meta, _, _) = st.session(id)?;
    let profile = st.sessions.lock().unwrap().get(&id).map(|s| s.profile.clone()).unwrap_or_default();
    let p = st
        .cfg
        .read()
        .unwrap()
        .profile(&profile)
        .cloned()
        .ok_or_else(|| ErrView::msg("invalid", "접속 프로필을 찾을 수 없습니다"))?;
    let pw = st
        .passwords
        .read()
        .unwrap()
        .get(&p.name.to_uppercase())
        .cloned()
        .or_else(|| p.password_from_env())
        .ok_or_else(|| ErrView::msg("password_required", "다시 접속한 뒤 디버그하세요"))?;
    if p.read_only {
        return Err(ErrView::msg(
            "read_only",
            "읽기 전용 프로필에서는 디버그할 수 없습니다 — 디버그는 블록을 실제로 실행합니다",
        ));
    }

    let (mut d, first) = Debugger::start(p.to_spec(pw), &block, binds).await?;
    let mut results = Vec::new();
    for b in breakpoints {
        let r = d.set_breakpoint(&b.owner, &b.name, &b.unit_type, b.line).await;
        results.push(BpResult {
            owner: b.owner,
            name: b.name,
            line: b.line,
            id: r.as_ref().ok().copied(),
            error: r.err().map(|e| e.to_string()),
        });
    }
    let target = d.target_handle();
    let run = Arc::new(DebugRun {
        dbg: tokio::sync::Mutex::new(None),
        target,
        meta,
        sources: Mutex::new(HashMap::new()),
        block_lines: block.lines().map(String::from).collect(),
    });
    let snap = snapshot(&run, &mut d, first).await?;
    *run.dbg.lock().await = Some(d);
    let did = st.next_id();
    st.debuggers.lock().unwrap().insert(did, run);
    Ok(Started { did, snapshot: snap, breakpoints: results })
}

#[tauri::command]
pub async fn debug_step(st: State<'_, AppState>, did: u64, step: Step, break_on_exception: bool) -> R<Snapshot> {
    let r = run(&st, did)?;
    let mut g = r.dbg.lock().await;
    let d = g.as_mut().ok_or_else(|| ErrView::msg("invalid", "디버그 세션이 끝났습니다"))?;
    let stop = d.step(step, break_on_exception).await?;
    snapshot(&r, d, stop).await
}

/// 대상이 오래 돌 때 (긴 루프) 멈추게 한다 — 대상 호출은 ORA-01013 으로 끝난다
#[tauri::command]
pub fn debug_interrupt(st: State<'_, AppState>, did: u64) -> R<()> {
    Ok(run(&st, did)?.target.cancel()?)
}

#[tauri::command]
pub async fn debug_breakpoint(
    st: State<'_, AppState>,
    did: u64,
    owner: String,
    name: String,
    unit_type: String,
    line: u32,
) -> R<i64> {
    let r = run(&st, did)?;
    let g = r.dbg.lock().await;
    let d = g.as_ref().ok_or_else(|| ErrView::msg("invalid", "디버그 세션이 끝났습니다"))?;
    Ok(d.set_breakpoint(&owner, &name, &unit_type, line).await?)
}

#[tauri::command]
pub async fn debug_clear_breakpoint(st: State<'_, AppState>, did: u64, bp: i64) -> R<()> {
    let r = run(&st, did)?;
    let g = r.dbg.lock().await;
    if let Some(d) = g.as_ref() {
        d.delete_breakpoint(bp).await?;
    }
    Ok(())
}

#[tauri::command]
pub async fn debug_eval(st: State<'_, AppState>, did: u64, name: String, frame: u32) -> R<VarValue> {
    let r = run(&st, did)?;
    let g = r.dbg.lock().await;
    let d = g.as_ref().ok_or_else(|| ErrView::msg("invalid", "디버그 세션이 끝났습니다"))?;
    Ok(d.get_value(&name, frame).await?)
}

/// 호출 스택의 다른 단계를 골랐을 때: 그 단계의 지역 변수
#[tauri::command]
pub async fn debug_frame_vars(st: State<'_, AppState>, did: u64, frame: Frame) -> R<Vec<VarValue>> {
    let r = run(&st, did)?;
    let src = r.source(&frame.owner, &frame.name, &frame.unit_type).await?;
    let g = r.dbg.lock().await;
    let d = g.as_ref().ok_or_else(|| ErrView::msg("invalid", "디버그 세션이 끝났습니다"))?;
    let mut vars = Vec::new();
    for n in debug::locals_at(&src, frame.line as usize).into_iter().take(MAX_AUTO_VARS) {
        let v = d.get_value(&n, frame.depth).await?;
        if !v.error.as_deref().map(|e| e.starts_with("그런 변수가 없습니다")).unwrap_or(false) {
            vars.push(v);
        }
    }
    Ok(vars)
}

#[tauri::command]
pub async fn debug_set(st: State<'_, AppState>, did: u64, frame: u32, assignment: String) -> R<()> {
    let r = run(&st, did)?;
    let g = r.dbg.lock().await;
    let d = g.as_ref().ok_or_else(|| ErrView::msg("invalid", "디버그 세션이 끝났습니다"))?;
    Ok(d.set_value(frame, &assignment).await?)
}

#[tauri::command]
pub async fn debug_source(st: State<'_, AppState>, did: u64, owner: String, name: String, unit_type: String) -> R<Vec<String>> {
    Ok(run(&st, did)?.source(&owner, &name, &unit_type).await?.to_vec())
}

/// 끝낸다. 대상 세션의 변경은 `commit` 이 아니면 롤백.
#[tauri::command]
pub async fn debug_finish(st: State<'_, AppState>, did: u64, commit: bool) -> R<Option<Finished>> {
    let Some(r) = st.debuggers.lock().unwrap().remove(&did) else { return Ok(None) };
    let d = r.dbg.lock().await.take();
    let Some(d) = d else { return Ok(None) };
    Ok(d.finish(commit).await.map(|x| match x {
        Ok(e) => Finished { output: e.output, error: None },
        Err(e) => Finished { output: vec![], error: Some(e.to_string()) },
    }))
}
