//! AI 명령 — 공급자 관리, 스트리밍 질의, 취소.
//!
//! 문맥(스키마, 실행계획)은 여기서 모은다. 화면이 조합하면 공급자마다 형식이 달라지고,
//! 무엇이 밖으로 나가는지 한 곳에서 볼 수 없다.

use serde::{Deserialize, Serialize};
use sqls_core::meta;
use sqls_llm::prompts::{self, Context, Task};
use sqls_llm::{Message, ProviderConfig};
use tauri::ipc::Channel;
use tauri::State;

use crate::state::{AppState, ErrView};

type R<T> = Result<T, ErrView>;

/// 스키마 문맥에 넣을 테이블 수 상한 — 로컬 모델에서 토큰은 곧 시간이다
const MAX_CONTEXT_TABLES: usize = 8;

#[derive(Serialize)]
pub struct ProviderView {
    #[serde(flatten)]
    cfg: ProviderConfig,
    /// 데이터가 이 PC 밖으로 나가는지
    remote: bool,
    has_key: bool,
}

fn view(st: &AppState, p: &ProviderConfig) -> ProviderView {
    let has_key = match p.kind {
        sqls_llm::ProviderKind::Ollama => true,
        _ => {
            st.api_keys.read().unwrap().contains_key(&p.name)
                || p.api_key_env
                    .as_deref()
                    .or(match p.kind {
                        sqls_llm::ProviderKind::Anthropic => Some("ANTHROPIC_API_KEY"),
                        _ => Some("OPENAI_API_KEY"),
                    })
                    .and_then(|e| std::env::var(e).ok())
                    .is_some_and(|k| !k.is_empty())
        }
    };
    ProviderView { remote: p.is_remote(), has_key, cfg: p.clone() }
}

#[tauri::command]
pub fn list_providers(st: State<'_, AppState>) -> Vec<ProviderView> {
    st.providers.read().unwrap().iter().map(|p| view(&st, p)).collect()
}

#[tauri::command]
pub fn save_provider(st: State<'_, AppState>, provider: ProviderConfig, original_name: Option<String>) -> R<()> {
    if provider.name.trim().is_empty() || provider.model.trim().is_empty() {
        return Err(ErrView::msg("invalid", "이름과 모델은 비울 수 없습니다"));
    }
    {
        let mut list = st.providers.write().unwrap();
        let key = original_name.unwrap_or_else(|| provider.name.clone());
        match list.iter_mut().find(|p| p.name == key) {
            Some(p) => *p = provider,
            None => list.push(provider),
        }
    }
    st.save_config()
}

/// API 키는 파일에 쓰지 않는다. 이 실행 동안만 기억한다.
#[tauri::command]
pub fn set_api_key(st: State<'_, AppState>, provider: String, key: String) {
    let mut keys = st.api_keys.write().unwrap();
    if key.trim().is_empty() {
        keys.remove(&provider);
    } else {
        keys.insert(provider, key.trim().to_string());
    }
}

pub(crate) fn provider(st: &AppState, name: &str) -> R<ProviderConfig> {
    let mut p = st
        .providers
        .read()
        .unwrap()
        .iter()
        .find(|p| p.name == name)
        .cloned()
        .ok_or_else(|| ErrView::msg("invalid", format!("AI 공급자가 없습니다: {name}")))?;
    if let Some(k) = st.api_keys.read().unwrap().get(name) {
        p.api_key = Some(k.clone());
    }
    Ok(p)
}

/// 연결 테스트 — 모델 목록을 돌려준다
#[tauri::command]
pub async fn test_provider(st: State<'_, AppState>, name: String) -> R<Vec<String>> {
    let p = provider(&st, &name)?;
    Ok(st.llm.list_models(&p).await?)
}

#[derive(Deserialize)]
pub struct AskArgs {
    request_id: u64,
    provider: String,
    task: Task,
    /// 문맥을 가져올 세션 (없으면 스키마·계획 없이 묻는다)
    session_id: Option<u64>,
    sql: Option<String>,
    question: Option<String>,
    error: Option<String>,
    /// 사용자가 고른 테이블 (객체 탐색기에서). SQL 에 나온 테이블에 더한다.
    #[serde(default)]
    tables: Vec<String>,
    #[serde(default)]
    history: Vec<Message>,
}

#[derive(Serialize, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AiEvent {
    /// 무엇을 하는 중인지 (로컬 모델은 오래 걸린다 — 멈춘 것과 구분되게)
    Status { text: String },
    /// 밖으로 보낸 문맥 요약 (투명성)
    Context { tables: Vec<String>, plan: bool, remote: bool },
    Delta { text: String },
}

#[derive(Serialize)]
pub struct AiAnswer {
    text: String,
    sql: Option<String>,
    model: Option<String>,
    stop_reason: Option<String>,
    refused: bool,
    truncated: bool,
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    /// 이번 질문을 대화 기록에 넣을 때 쓸 사용자 메시지 원문
    user_message: String,
}

#[tauri::command]
pub async fn ai_ask(st: State<'_, AppState>, args: AskArgs, on_event: Channel<AiEvent>) -> R<AiAnswer> {
    let p = provider(&st, &args.provider)?;
    let status = |t: &str| {
        let _ = on_event.send(AiEvent::Status { text: t.into() });
    };

    // ── 문맥 모으기 ───────────────────────────────────
    let mut ctx = Context {
        sql: args.sql.clone().filter(|s| !s.trim().is_empty()),
        question: args.question.clone().filter(|s| !s.trim().is_empty()),
        error: args.error.clone(),
        ..Default::default()
    };
    let mut used_tables = Vec::new();
    if let Some(id) = args.session_id {
        if let Ok((s, _, _)) = st.session(id) {
            ctx.server_version = Some(s.info().server_version.clone());
            ctx.current_user = Some(s.info().user.clone());
            let mut names: Vec<String> = args.tables.clone();
            if let Some(sql) = &ctx.sql {
                for t in meta::referenced_tables(sql) {
                    if !names.iter().any(|n| n.eq_ignore_ascii_case(&t)) {
                        names.push(t);
                    }
                }
            }
            if !names.is_empty() {
                status("스키마 정보를 읽는 중…");
            }
            for n in names.into_iter().take(MAX_CONTEXT_TABLES) {
                // 없는 이름(별칭, CTE 이름)은 조용히 건너뛴다
                if let Ok(d) = meta::describe(&s, &n).await {
                    used_tables.push(format!("{}.{}", d.owner, d.name));
                    ctx.schema.push(meta::schema_brief(&d));
                }
            }
            if args.task == Task::Optimize {
                if let Some(sql) = &ctx.sql {
                    status("실행계획을 만드는 중…");
                    match s.explain(sql).await {
                        Ok(lines) => ctx.plan = Some(lines.join("\n")),
                        Err(e) => ctx.error.get_or_insert_with(|| format!("실행계획 실패: {e}")).push('\n'),
                    }
                }
            }
        }
    }
    let _ = on_event.send(AiEvent::Context {
        tables: used_tables,
        plan: ctx.plan.is_some(),
        remote: p.is_remote(),
    });

    let req = prompts::build(args.task, &ctx, &args.history);
    let user_message = req.messages.last().map(|m| m.content.clone()).unwrap_or_default();

    // ── 질의 (취소할 수 있게 별도 작업으로) ─────────────
    status(if p.is_remote() { "응답을 기다리는 중…" } else { "로컬 모델이 생각하는 중… (첫 응답은 모델 로딩으로 오래 걸릴 수 있습니다)" });
    let llm = st.llm.clone();
    let ch = on_event.clone();
    let task = tokio::spawn(async move {
        llm.chat(&p, &req, move |d| {
            let _ = ch.send(AiEvent::Delta { text: d.to_string() });
        })
        .await
    });
    st.ai_tasks.lock().unwrap().insert(args.request_id, task.abort_handle());
    let joined = task.await;
    st.ai_tasks.lock().unwrap().remove(&args.request_id);
    let resp = match joined {
        Ok(r) => r?,
        Err(e) if e.is_cancelled() => return Err(ErrView::msg("cancelled", "AI 요청을 취소했습니다")),
        Err(e) => return Err(ErrView::msg("llm", format!("AI 작업이 비정상 종료되었습니다: {e}"))),
    };
    Ok(AiAnswer {
        sql: prompts::extract_sql(&resp.text),
        refused: resp.refused(),
        truncated: resp.truncated(),
        text: resp.text,
        model: resp.model,
        stop_reason: resp.stop_reason,
        input_tokens: resp.input_tokens,
        output_tokens: resp.output_tokens,
        user_message,
    })
}

#[tauri::command]
pub fn ai_cancel(st: State<'_, AppState>, request_id: u64) {
    if let Some(h) = st.ai_tasks.lock().unwrap().remove(&request_id) {
        h.abort();
    }
}
