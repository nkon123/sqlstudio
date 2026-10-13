//! SQLStudio 데스크톱 앱 — Tauri 백엔드.
//!
//! 화면(WebView)은 그리기만 한다. DB·LLM·파일 작업은 전부 여기서 하고, 무거운 일은
//! 세션 스레드(sqls-core)나 async 작업으로 보낸다. 명령 처리기는 절대 블로킹하지 않는다.

mod ai;
mod analyze;
mod complete;
mod db;
mod debug;
mod export;
mod files;
mod git;
mod history;
mod import;
mod monitor;
mod state;

use state::AppState;

/// 포터블이면 WebView 데이터 폴더 (모든 창이 같은 곳을 써야 한다)
pub(crate) fn webview_dir() -> Option<std::path::PathBuf> {
    sqls_core::config::portable_data_dir().map(|d| d.join("webview"))
}

pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("SQLSTUDIO_LOG")
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    let state = AppState::load();

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(state)
        .invoke_handler(tauri::generate_handler![
            db::app_info,
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
            db::describe,
            db::describe_fast,
            db::get_ddl,
            db::object_sections,
            db::object_section,
            db::get_mcp_settings,
            complete::complete,
            complete::completion_status,
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
            analyze::analysis_start,
            analyze::analysis_cancel,
            analyze::analysis_result,
            analyze::analysis_result_for,
            analyze::open_dfd_window,
            analyze::analysis_unit,
            analyze::analysis_dir,
            analyze::analysis_eval,
            analyze::analysis_sets,
            analyze::analysis_set_units,
            analyze::analysis_export,
            analyze::analysis_peek,
            analyze::analysis_import,
            analyze::analysis_remove_set,
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
            ai::ai_ask,
            ai::ai_cancel,
            ai::ai_guides_info,
            ai::ai_guides_edit,
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
        ])
        .on_window_event(|window, event| {
            // 창을 닫으면 열린 세션을 모두 닫는다 (커밋 안 한 변경은 롤백)
            if let tauri::WindowEvent::Destroyed = event {
                use tauri::Manager;
                if let Some(st) = window.try_state::<AppState>() {
                    st.close_all();
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
