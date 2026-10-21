//! SQLStudio 데스크톱 앱 — Tauri 백엔드.
//!
//! 화면(WebView)은 그리기만 한다. DB·LLM·파일 작업은 전부 여기서 하고, 무거운 일은
//! 세션 스레드(sqls-core)나 async 작업으로 보낸다. 명령 처리기는 절대 블로킹하지 않는다.

mod ai;
mod analyze;
mod complete;
mod copydata;
mod db;
mod debug;
mod export;
mod files;
mod git;
mod history;
mod import;
mod monitor;
mod schemacmp;
mod state;

use state::AppState;

/// 이 창이 닫힐 때 열린 세션을 모두 닫는지 — 메인 창만
pub(crate) fn closes_sessions(label: &str) -> bool {
    label == "main"
}

#[cfg(test)]
mod tests {
    #[test]
    fn only_main_window_closes_sessions() {
        assert!(super::closes_sessions("main"));
        assert!(!super::closes_sessions("dfd-1"));
        assert!(!super::closes_sessions("dfd-27"));
    }
}

/// 포터블이면 WebView 데이터 폴더 (모든 창이 같은 곳을 써야 한다)
pub(crate) fn webview_dir() -> Option<std::path::PathBuf> {
    sqls_core::config::portable_data_dir().map(|d| d.join("webview"))
}

/// 로그 폴더 (설정 파일 옆 logs)
pub(crate) fn log_dir() -> std::path::PathBuf {
    sqls_core::config::config_path().parent().map(|p| p.join("logs")).unwrap_or_else(|| "logs".into())
}

/// 로그 파일 — 접속·끊김·다시 접속을 사후에 볼 수 있게. 5MB 를 넘으면 .1 로 밀어 둔다 (둘만 남김)
fn open_log_file() -> Option<std::fs::File> {
    let dir = log_dir();
    std::fs::create_dir_all(&dir).ok()?;
    let p = dir.join("sqlstudio.log");
    if std::fs::metadata(&p).map(|m| m.len() > 5 << 20).unwrap_or(false) {
        let _ = std::fs::rename(&p, dir.join("sqlstudio.1.log"));
    }
    std::fs::OpenOptions::new().create(true).append(true).open(p).ok()
}

/// 데이터 폴더를 옮긴 뒤 남은 WebView 데이터 복사 표시 — 안에 이전 webview 폴더 경로 (db::set_data_dir)
pub(crate) const WEBVIEW_FROM: &str = ".webview-from";

/// 이전 데이터 폴더의 WebView 데이터를 새 폴더로. 이전 프로세스가 아직 잡고 있을 수 있어 잠깐 기다리며 다시 해 본다
fn take_pending_webview() {
    let Some(to) = webview_dir() else { return };
    let Some(mark) = to.parent().map(|d| d.join(WEBVIEW_FROM)) else { return };
    let Ok(from) = std::fs::read_to_string(&mark) else { return };
    if !to.exists() {
        for _ in 0..10 {
            let _ = std::fs::remove_dir_all(&to);
            match std::fs::create_dir_all(&to).and_then(|_| db::copy_tree(std::path::Path::new(from.trim()), &to, &|_| true)) {
                Ok(()) => break,
                Err(e) => {
                    tracing::warn!("WebView 데이터 복사 다시 시도: {e}");
                    std::thread::sleep(std::time::Duration::from_millis(300));
                }
            }
        }
    }
    let _ = std::fs::remove_file(mark);
}

pub fn run() {
    use tracing_subscriber::fmt::writer::MakeWriterExt;
    let filter = || tracing_subscriber::EnvFilter::try_from_env("SQLSTUDIO_LOG").unwrap_or_else(|_| "info".into());
    match open_log_file() {
        Some(f) => tracing_subscriber::fmt()
            .with_env_filter(filter())
            .with_ansi(false)
            .with_writer(std::io::stdout.and(std::sync::Mutex::new(f)))
            .init(),
        None => tracing_subscriber::fmt().with_env_filter(filter()).init(),
    }
    tracing::info!("SQLStudio {} 시작 — 설정 {}", env!("CARGO_PKG_VERSION"), sqls_core::config::config_path().display());
    // 접속·끊김·다시 접속 기록 (설정 > 정보 > 접속 진단 이 요약한다)
    sqls_core::connlog::init(log_dir().join("connection.jsonl"));
    // 예상 못 한 오류(패닉)도 남긴다 — 세션 작업 스레드에서 나면 그 세션이 닫혀 '끊김' 으로 보인다
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let thread = std::thread::current().name().unwrap_or("?").to_string();
        tracing::error!("패닉 (스레드 {thread}): {info}");
        sqls_core::connlog::record(sqls_core::connlog::Ev { kind: "panic".into(), who: thread, msg: info.to_string(), ..Default::default() });
        default_hook(info);
    }));

    take_pending_webview();
    let state = AppState::load();

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(state)
        .invoke_handler(tauri::generate_handler![
            db::app_info,
            db::column_usage,
            db::object_deps,
            db::open_log_dir,
            db::data_dir_info,
            db::set_data_dir,
            db::restart_app,
            db::client_log,
            db::conn_event,
            db::conn_report,
            db::list_profiles,
            db::save_profile,
            db::delete_profile,
            db::connect,
            db::disconnect,
            db::tns_list,
            db::history_search,
            db::history_clear,
            db::history_settings,
            db::set_history_enabled,
            db::set_tns_admin,
            db::execute,
            db::execute_script,
            db::fetch_more,
            db::cancel,
            db::abandon,
            db::commit,
            db::rollback,
            db::explain,
            db::analyze_sql,
            db::list_schemas,
            db::list_objects,
            db::describe, db::count_rows, db::tablespace_usage, db::table_sizes,
            db::describe_fast,
            db::get_ddl,
            db::object_sections,
            db::object_section,
            db::get_mcp_settings,
            complete::complete,
            complete::completion_status,
            complete::column_notes,
            complete::refresh_completion,
            debug::debug_prepare,
            debug::debug_compile,
            debug::debug_start,
            debug::debug_step,
            debug::debug_interrupt,
            debug::debug_breakpoint,
            debug::debug_clear_breakpoint,
            debug::debug_eval,
            debug::debug_frame_vars,
            debug::debug_set,
            debug::debug_source,
            debug::debug_finish,
            analyze::analysis_list,
            analyze::analysis_start, analyze::analysis_code_list, analyze::analysis_code_start,
            analyze::analysis_cancel,
            analyze::analysis_result,
            analyze::analysis_result_for,
            analyze::open_dfd_window,
            analyze::analysis_unit,
            analyze::column_source,
            analyze::analysis_dir,
            analyze::analysis_eval,
            analyze::analysis_sets,
            analyze::analysis_set_units,
            analyze::analysis_export,
            analyze::analysis_peek,
            analyze::analysis_import,
            analyze::analysis_merge_set,
            analyze::analysis_remove_set,
            schemacmp::schema_compare_start,
            schemacmp::schema_compare_cancel,
            schemacmp::schema_compare_detail,
            schemacmp::schema_compare_script,
            schemacmp::schema_compare_close,
            analyze::dfd_reports,
            analyze::dfd_report_get,
            analyze::dfd_report_save,
            analyze::dfd_report_delete,
            db::forget_password,
            db::grid_editable,
            db::grid_apply,
            monitor::monitor_snapshot,
            monitor::monitor_detail,
            monitor::monitor_kill,
            db::set_mcp_settings,
            ai::list_providers,
            ai::save_provider,
            ai::set_api_key,
            ai::ai_ack_remote,
            ai::test_provider,
            ai::delete_provider,
            ai::ai_list_models,
            ai::ai_ask,
            ai::ai_cancel,
            ai::ai_guides_info,
            ai::ai_guides_edit,
            ai::ai_guides_list,
            ai::ai_guides_customize,
            ai::ai_guide_select,
            ai::set_ai_guide_select,
            files::read_sql_file,
            files::write_sql_file,
            files::write_binary_file,
            git::git_version,
            git::git_open,
            git::git_clone,
            git::git_status,
            git::git_files,
            git::git_diff,
            git::git_commit,
            git::git_identity,
            git::git_set_identity,
            git::git_discard,
            git::git_branches,
            git::git_switch,
            git::git_fetch,
            git::git_pull,
            git::git_outgoing,
            git::git_push,
            files::mcp_snippet,
            export::export_xlsx,
            import::import_read,
            import::import_run,
            copydata::copy_plan,
            copydata::copy_run,
        ])
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::Destroyed = event {
                use tauri::Manager;
                tracing::info!("창 닫힘: {}", window.label());
                // 메인 창을 닫을 때만 열린 세션을 모두 닫는다 (커밋 안 한 변경은 롤백).
                // 흐름도 같은 보조 창을 닫는다고 작업 탭의 접속까지 닫으면 안 된다
                if closes_sessions(window.label()) {
                    if let Some(st) = window.try_state::<AppState>() {
                        st.close_all();
                    }
                }
            }
        })
        // 메인 창은 여기서 만든다 — 포터블이면 WebView 데이터(화면 설정·바인드 이력)도 실행 파일 옆 data 에 두도록
        .setup(|app| {
            let cfg = app.config().app.windows.iter().find(|w| w.label == "main").cloned().ok_or("main 창 설정이 없습니다")?;
            let mut b = tauri::WebviewWindowBuilder::from_config(app.handle(), &cfg)?;
            if let Some(dir) = webview_dir() {
                b = b.data_directory(dir);
            }
            b.build()?;
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("SQLStudio 를 시작할 수 없습니다");
}
