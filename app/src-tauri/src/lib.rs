//! SQLStudio 데스크톱 앱 — Tauri 백엔드.
//!
//! 화면(WebView)은 그리기만 한다. DB·LLM·파일 작업은 전부 여기서 하고, 무거운 일은
//! 세션 스레드(sqls-core)나 async 작업으로 보낸다. 명령 처리기는 절대 블로킹하지 않는다.

mod ai;
mod db;
mod files;
mod state;

use state::AppState;

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
            db::get_ddl,
            db::get_mcp_settings,
            db::set_mcp_settings,
            ai::list_providers,
            ai::save_provider,
            ai::set_api_key,
            ai::test_provider,
            ai::ai_ask,
            ai::ai_cancel,
            files::read_sql_file,
            files::write_sql_file,
            files::mcp_snippet,
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
        .run(tauri::generate_context!())
        .expect("SQLStudio 를 시작할 수 없습니다");
}
