//! DB 명령 — 접속, 실행, 스크립트, 사전 조회.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sqls_core::config::{config_path, Profile};
use sqls_core::sql::{self, StmtKind};
use sqls_core::{meta, ExecOptions, ExecOutcome, ExecResult, RowPage};
use tauri::ipc::Channel;
use tauri::State;

use crate::state::{AppState, ErrView, OpenSession};

type R<T> = Result<T, ErrView>;

#[derive(Serialize)]
pub struct AppInfo {
    version: &'static str,
    config_path: String,
    config_error: Option<String>,
}

#[tauri::command]
pub fn app_info(st: State<'_, AppState>) -> AppInfo {
    AppInfo {
        version: env!("CARGO_PKG_VERSION"),
        config_path: config_path().display().to_string(),
        config_error: st.config_error.clone(),
    }
}

#[derive(Serialize)]
pub struct ProfileView {
    #[serde(flatten)]
    profile: Profile,
    /// 환경변수에 비밀번호가 있어 묻지 않고 붙을 수 있는지
    has_env_password: bool,
    /// OS 자격 증명 저장소에 비밀번호가 있는지
    has_saved_password: bool,
}

#[tauri::command]
pub fn list_profiles(st: State<'_, AppState>) -> Vec<ProfileView> {
    st.cfg
        .read()
        .unwrap()
        .connections
        .iter()
        .map(|p| ProfileView {
            has_env_password: p.password_from_env().is_some(),
            has_saved_password: sqls_core::secret::get(&sqls_core::secret::db_account(&p.name)).is_some(),
            profile: p.clone(),
        })
        .collect()
}

#[tauri::command]
pub fn save_profile(st: State<'_, AppState>, profile: Profile, original_name: Option<String>) -> R<()> {
    if profile.name.trim().is_empty() || profile.user.trim().is_empty() || profile.connect_string.trim().is_empty() {
        return Err(ErrView::msg("invalid", "이름, 사용자, 접속 문자열은 비울 수 없습니다"));
    }
    {
        let mut cfg = st.cfg.write().unwrap();
        let key = original_name.unwrap_or_else(|| profile.name.clone());
        if !key.eq_ignore_ascii_case(&profile.name) && cfg.profile(&profile.name).is_some() {
            return Err(ErrView::msg("invalid", format!("같은 이름의 접속이 있습니다: {}", profile.name)));
        }
        match cfg.connections.iter_mut().find(|p| p.name.eq_ignore_ascii_case(&key)) {
            Some(p) => *p = profile,
            None => cfg.connections.push(profile),
        }
    }
    st.save_config()
}

#[tauri::command]
pub fn delete_profile(st: State<'_, AppState>, name: String) -> R<()> {
    {
        let mut cfg = st.cfg.write().unwrap();
        cfg.connections.retain(|p| !p.name.eq_ignore_ascii_case(&name));
        cfg.mcp.allowed_connections.retain(|n| !n.eq_ignore_ascii_case(&name));
    }
    st.save_config()
}

#[derive(Serialize)]
pub struct Connected {
    id: u64,
    user: String,
    connect_string: String,
    server_version: String,
    read_only: bool,
    color: Option<String>,
}

#[tauri::command]
pub async fn connect(
    st: State<'_, AppState>,
    app: tauri::AppHandle,
    profile: String,
    password: Option<String>,
    // 입력한 비밀번호를 OS 자격 증명 저장소에 둘지
    remember: Option<bool>,
) -> R<Connected> {
    let p = st
        .cfg
        .read()
        .unwrap()
        .profile(&profile)
        .cloned()
        .ok_or_else(|| ErrView::msg("invalid", format!("접속 프로필이 없습니다: {profile}")))?;
    let key = p.name.to_uppercase();
    let typed = password.filter(|s| !s.is_empty());
    let pw = typed
        .clone()
        .or_else(|| st.passwords.read().unwrap().get(&key).cloned())
        .or_else(|| p.stored_password())
        .ok_or_else(|| ErrView::msg("password_required", "비밀번호를 입력하세요"))?;
    let account = sqls_core::secret::db_account(&p.name);
    let session = match sqls_core::Session::connect(p.to_spec(pw.clone())).await {
        Ok(s) => s,
        Err(e) => {
            // 기억해 둔 비밀번호가 틀렸으면 (ORA-01017) 잊는다 — 저장소의 것도
            if matches!(e, sqls_core::Error::Db { code: 1017, .. }) {
                st.passwords.write().unwrap().remove(&key);
                if typed.is_none() {
                    sqls_core::secret::delete(&account);
                }
            }
            return Err(e.into());
        }
    };
    st.passwords.write().unwrap().insert(key, pw.clone());
    // 접속에 성공한 비밀번호만 저장한다
    if typed.is_some() && remember == Some(true) {
        if let Err(e) = sqls_core::secret::set(&account, &pw) {
            tracing::warn!("{}: {e}", p.name);
        }
    }
    // 자동완성 캐시는 프로필마다 하나 — 처음 접속이면 백그라운드로 읽기 시작한다
    crate::complete::ensure_hub(&st, &app, &p, &pw);
    let id = st.next_id();
    let info = session.info().clone();
    st.sessions.lock().unwrap().insert(
        id,
        OpenSession {
            session,
            profile: p.name.clone(),
            txn_pending: Arc::new(AtomicBool::new(false)),
            stop_script: Arc::new(AtomicBool::new(false)),
        },
    );
    Ok(Connected {
        id,
        user: info.user,
        connect_string: info.connect_string,
        server_version: info.server_version,
        read_only: info.read_only,
        color: p.color.clone(),
    })
}

#[tauri::command]
pub fn disconnect(st: State<'_, AppState>, id: u64) {
    if let Some(s) = st.sessions.lock().unwrap().remove(&id) {
        s.session.close();
    }
}

#[derive(Deserialize)]
pub struct ExecArgs {
    id: u64,
    sql: String,
    #[serde(default)]
    binds: Vec<(String, Option<String>)>,
    /// 사람이 확인 창에서 "실행" 을 눌렀는지
    #[serde(default)]
    confirmed: bool,
    #[serde(default)]
    page_size: Option<usize>,
}

#[derive(Serialize)]
pub struct ExecView {
    #[serde(flatten)]
    result: ExecResult,
    txn_pending: bool,
}

fn track_txn(kind: StmtKind, outcome: &ExecOutcome, flag: &AtomicBool) {
    match kind {
        StmtKind::Dml if matches!(outcome, ExecOutcome::Affected { .. }) => flag.store(true, Ordering::Relaxed),
        // DDL 은 앞뒤로 암묵적 커밋을 한다
        StmtKind::Ddl | StmtKind::PlsqlUnit => flag.store(false, Ordering::Relaxed),
        // PL/SQL 블록 안에서 DML 을 했을 수 있다 — 안전하게 "진행 중" 으로 본다
        StmtKind::Plsql => flag.store(true, Ordering::Relaxed),
        _ => {}
    }
}

fn opts(page: Option<usize>, binds: Vec<(String, Option<String>)>) -> ExecOptions {
    let page = page.unwrap_or(500).clamp(1, 100_000);
    ExecOptions {
        first_page: page,
        fetch_array_size: page.min(1000) as u32,
        binds,
        ..Default::default()
    }
}

#[tauri::command]
pub async fn execute(st: State<'_, AppState>, args: ExecArgs) -> R<ExecView> {
    let (s, txn, _) = st.session(args.id)?;
    if !args.confirmed {
        if let Some(why) = sql::needs_confirmation(&args.sql) {
            return Err(ErrView::msg("confirm_required", why));
        }
    }
    let r = s.execute(&args.sql, opts(args.page_size, args.binds)).await?;
    track_txn(r.kind, &r.outcome, &txn);
    if matches!(r.kind, StmtKind::Ddl | StmtKind::PlsqlUnit) {
        crate::complete::after_ddl(&st, args.id);
    }
    let txn_pending = txn.load(Ordering::Relaxed);
    Ok(ExecView { result: r, txn_pending })
}

/// 스크립트 실행 진행 — 문장마다 하나씩 화면으로 흘려보낸다
#[derive(Clone, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum ScriptEvent {
    Started { index: usize, total: usize, line: usize, kind: StmtKind, preview: String },
    Finished { index: usize, elapsed_ms: u64, summary: String, output: Vec<String> },
    Failed { index: usize, line: usize, error: ErrView },
    /// 마지막 SELECT 의 결과 (그리드에 띄운다)
    Rows { index: usize, page: RowPage },
}

#[derive(Serialize)]
pub struct ScriptSummary {
    total: usize,
    ok: usize,
    failed: usize,
    stopped: bool,
    txn_pending: bool,
}

#[tauri::command]
pub async fn execute_script(
    st: State<'_, AppState>,
    id: u64,
    script: String,
    stop_on_error: bool,
    confirmed: bool,
    on_event: Channel<ScriptEvent>,
) -> R<ScriptSummary> {
    let (s, txn, stop) = st.session(id)?;
    let stmts = sql::split_script(&script);
    if !confirmed {
        let risky: Vec<String> = stmts
            .iter()
            .filter_map(|x| sql::needs_confirmation(&x.text).map(|why| format!("{}행: {why}", x.line)))
            .collect();
        if !risky.is_empty() {
            return Err(ErrView::msg("confirm_required", risky.join("\n")));
        }
    }
    stop.store(false, Ordering::Relaxed);
    let total = stmts.len();
    let (mut ok, mut failed, mut stopped) = (0, 0, false);
    for (index, stmt) in stmts.iter().enumerate() {
        if stop.load(Ordering::Relaxed) {
            stopped = true;
            break;
        }
        let preview: String = stmt.text.lines().next().unwrap_or("").chars().take(120).collect();
        let _ = on_event.send(ScriptEvent::Started { index, total, line: stmt.line, kind: stmt.kind, preview });
        match s.execute(&stmt.text, opts(Some(200), vec![])).await {
            Ok(r) => {
                ok += 1;
                track_txn(r.kind, &r.outcome, &txn);
                if matches!(r.kind, StmtKind::Ddl | StmtKind::PlsqlUnit) {
                    crate::complete::after_ddl(&st, id);
                }
                let summary = match &r.outcome {
                    ExecOutcome::Rows(p) => {
                        format!("{}행{}", p.rows.len(), if p.has_more { "+" } else { "" })
                    }
                    ExecOutcome::Affected { rows } => format!("{rows}행 처리"),
                    ExecOutcome::Done => "완료".into(),
                    ExecOutcome::Skipped { reason } => reason.clone(),
                };
                let _ = on_event.send(ScriptEvent::Finished {
                    index,
                    elapsed_ms: r.elapsed_ms,
                    summary,
                    output: r.output.clone(),
                });
                if let ExecOutcome::Rows(page) = r.outcome {
                    let _ = on_event.send(ScriptEvent::Rows { index, page });
                }
            }
            Err(e) => {
                failed += 1;
                let lost = e.is_connection_lost();
                let cancelled = matches!(e, sqls_core::Error::Cancelled);
                let _ = on_event.send(ScriptEvent::Failed { index, line: stmt.line, error: e.into() });
                if stop_on_error || lost || cancelled {
                    stopped = index + 1 < total;
                    break;
                }
            }
        }
    }
    Ok(ScriptSummary { total, ok, failed, stopped, txn_pending: txn.load(Ordering::Relaxed) })
}

#[tauri::command]
pub async fn fetch_more(st: State<'_, AppState>, id: u64, rows: usize) -> R<RowPage> {
    let (s, _, _) = st.session(id)?;
    Ok(s.fetch_more(rows.clamp(1, 100_000)).await?)
}

#[tauri::command]
pub fn cancel(st: State<'_, AppState>, id: u64) -> R<()> {
    let (s, _, stop) = st.session(id)?;
    stop.store(true, Ordering::Relaxed);
    Ok(s.cancel()?)
}

/// 취소가 서버에 닿지 않을 때: 세션을 버리고 같은 프로필로 새로 붙을 수 있게 한다.
#[tauri::command]
pub fn abandon(st: State<'_, AppState>, id: u64) {
    if let Some(s) = st.sessions.lock().unwrap().remove(&id) {
        s.session.abandon();
    }
}

#[tauri::command]
pub async fn commit(st: State<'_, AppState>, id: u64) -> R<()> {
    let (s, txn, _) = st.session(id)?;
    s.commit().await?;
    txn.store(false, Ordering::Relaxed);
    Ok(())
}

#[tauri::command]
pub async fn rollback(st: State<'_, AppState>, id: u64) -> R<()> {
    let (s, txn, _) = st.session(id)?;
    s.rollback().await?;
    txn.store(false, Ordering::Relaxed);
    Ok(())
}

#[tauri::command]
pub async fn explain(st: State<'_, AppState>, id: u64, sql: String) -> R<Vec<String>> {
    let (s, _, _) = st.session(id)?;
    Ok(s.explain(&sql).await?)
}

/// 커서 위치의 문장과 그 바인드 변수 (Ctrl+Enter 직전에 부른다)
#[derive(Serialize)]
pub struct Analyzed {
    statement: Option<sql::Statement>,
    binds: Vec<String>,
    confirm: Option<&'static str>,
}

#[tauri::command]
pub fn analyze_sql(text: String, cursor: Option<usize>, selection: Option<String>) -> Analyzed {
    let stmt = match selection.filter(|s| !s.trim().is_empty()) {
        Some(sel) => sql::split_script(&sel).into_iter().next(),
        None => sql::statement_at(&text, cursor.unwrap_or(0).min(text.len())),
    };
    let binds = stmt.as_ref().map(|s| sql::bind_names(&s.text)).unwrap_or_default();
    let confirm = stmt.as_ref().and_then(|s| sql::needs_confirmation(&s.text));
    Analyzed { statement: stmt, binds, confirm }
}

#[tauri::command]
pub async fn list_schemas(st: State<'_, AppState>, id: u64) -> R<Vec<String>> {
    let (s, _, _) = st.session(id)?;
    Ok(meta::list_schemas(&s).await?)
}

#[tauri::command]
pub async fn list_objects(
    st: State<'_, AppState>,
    id: u64,
    owner: Option<String>,
    object_type: Option<String>,
    name_like: Option<String>,
) -> R<Vec<meta::ObjectEntry>> {
    let (s, _, _) = st.session(id)?;
    Ok(meta::list_objects(&s, owner.as_deref(), object_type.as_deref(), name_like.as_deref(), 5000).await?)
}

#[tauri::command]
pub async fn describe(st: State<'_, AppState>, id: u64, name: String) -> R<meta::TableDesc> {
    let (s, _, _) = st.session(id)?;
    Ok(meta::describe(&s, &name).await?)
}

#[tauri::command]
pub async fn get_ddl(st: State<'_, AppState>, id: u64, object_type: String, name: String) -> R<String> {
    let (s, _, _) = st.session(id)?;
    Ok(meta::get_ddl(&s, &object_type, &name).await?)
}

#[derive(Serialize)]
pub struct McpSettings {
    allowed_connections: Vec<String>,
    call_timeout_secs: u64,
    allow_explain: bool,
}

#[tauri::command]
pub fn get_mcp_settings(st: State<'_, AppState>) -> McpSettings {
    let c = st.cfg.read().unwrap();
    McpSettings {
        allowed_connections: c.mcp.allowed_connections.clone(),
        call_timeout_secs: c.mcp.call_timeout_secs,
        allow_explain: c.mcp.allow_explain,
    }
}

#[tauri::command]
pub fn set_mcp_settings(
    st: State<'_, AppState>,
    allowed_connections: Vec<String>,
    call_timeout_secs: u64,
    allow_explain: bool,
) -> R<()> {
    {
        let mut c = st.cfg.write().unwrap();
        for n in &allowed_connections {
            if c.profile(n).is_none() {
                return Err(ErrView::msg("invalid", format!("접속 프로필이 없습니다: {n}")));
            }
        }
        c.mcp.allowed_connections = allowed_connections;
        c.mcp.allow_explain = allow_explain;
        c.mcp.call_timeout_secs = call_timeout_secs.clamp(1, 3600);
    }
    st.save_config()
}

/// 저장해 둔 비밀번호를 지운다 (설정 화면)
#[tauri::command]
pub fn forget_password(st: State<'_, AppState>, profile: String) {
    sqls_core::secret::delete(&sqls_core::secret::db_account(&profile));
    st.passwords.write().unwrap().remove(&profile.to_uppercase());
}

// ─────────────────────────────────────────────────────────────
// 결과 그리드 편집
// ─────────────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct EditableView {
    /// ROWID 를 붙인 SELECT — 화면이 이것으로 다시 조회한다
    sql: String,
    table: String,
    /// 테이블에 실제로 있는 열 (식·별칭 열은 편집하지 못한다)
    columns: Vec<String>,
}

fn require_writable(st: &AppState, id: u64) -> R<()> {
    let profile = st.sessions.lock().unwrap().get(&id).map(|s| s.profile.clone()).unwrap_or_default();
    let ro = st.cfg.read().unwrap().profile(&profile).map(|p| p.read_only).unwrap_or(true);
    if ro {
        return Err(ErrView::msg("read_only", "읽기 전용 프로필에서는 결과를 편집할 수 없습니다"));
    }
    Ok(())
}

/// 이 SELECT 를 편집할 수 있는지 — 되면 ROWID 를 붙인 문장과 테이블의 열
#[tauri::command]
pub async fn grid_editable(st: State<'_, AppState>, id: u64, sql: String) -> R<EditableView> {
    require_writable(&st, id)?;
    let e = sqls_core::edit::editable(&sql).map_err(|m| ErrView::msg("invalid", m))?;
    let (s, _, _) = st.session(id)?;
    let d = sqls_core::meta::describe(&s, &e.table).await?;
    if d.object_type != "TABLE" {
        return Err(ErrView::msg("invalid", format!("{} 는 {} 입니다 — 테이블만 편집할 수 있습니다", e.table, d.object_type)));
    }
    Ok(EditableView { sql: e.sql, table: format!("{}.{}", d.owner, d.name), columns: d.columns.into_iter().map(|c| c.name).collect() })
}

#[derive(Serialize)]
pub struct ApplyView {
    /// 문장마다 (읽기용 문장, 바뀐 행 수)
    done: Vec<(String, u64)>,
    txn_pending: bool,
}

/// 편집 적용. `dry_run` 이면 문장만 만들어 돌려준다 (화면이 보여 주고 확인을 받는다).
/// 한 문장이라도 1행이 아닌 행을 바꾸면 거기서 멈춘다 (다른 사람이 그 행을 바꿨거나 지웠다).
#[tauri::command]
pub async fn grid_apply(
    st: State<'_, AppState>,
    id: u64,
    table: String,
    edits: Vec<sqls_core::edit::RowEdit>,
    deletes: Vec<String>,
    dry_run: bool,
) -> R<ApplyView> {
    require_writable(&st, id)?;
    let changes = sqls_core::edit::changes(&table, &edits, &deletes).map_err(|m| ErrView::msg("invalid", m))?;
    let (s, txn, _) = st.session(id)?;
    if dry_run {
        return Ok(ApplyView { done: changes.into_iter().map(|c| (c.preview, 0)).collect(), txn_pending: txn.load(Ordering::Relaxed) });
    }
    let mut done = Vec::new();
    for c in changes {
        let r = s.execute(&c.sql, opts(None, c.binds.clone())).await?;
        let n = match r.outcome {
            ExecOutcome::Affected { rows } => rows,
            _ => 0,
        };
        txn.store(true, Ordering::Relaxed);
        if n != 1 {
            return Err(ErrView::msg(
                "invalid",
                format!("{}\n→ {n}행이 바뀌었습니다 (1행이어야 합니다). 다른 세션이 그 행을 바꿨거나 지웠을 수 있습니다. 앞의 변경은 커밋 전이니 롤백할 수 있습니다.", c.preview),
            ));
        }
        done.push((c.preview, n));
    }
    Ok(ApplyView { done, txn_pending: txn.load(Ordering::Relaxed) })
}
