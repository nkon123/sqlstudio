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
    /// 외부면 지금 주소로 전송 동의를 받았는지
    remote_ok: bool,
    has_key: bool,
}

fn view(st: &AppState, p: &ProviderConfig) -> ProviderView {
    let has_key = match p.kind {
        sqls_llm::ProviderKind::Ollama => true,
        _ => {
            st.api_keys.read().unwrap().contains_key(&p.name)
                || sqls_core::secret::get(&sqls_core::secret::llm_account(&p.name)).is_some()
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
    ProviderView { remote: p.is_remote(), remote_ok: p.remote_ok(), has_key, cfg: p.clone() }
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
        let mut provider = provider;
        // 동의 기록은 화면이 보낸 값을 믿지 않는다 — 원래 기록을 주소가 같을 때만 이어 간다
        provider.remote_ack = list.iter().find(|p| p.name == key).and_then(|p| p.remote_ack.clone()).filter(|a| *a == provider.base_url());
        match list.iter_mut().find(|p| p.name == key) {
            Some(p) => *p = provider,
            None => list.push(provider),
        }
    }
    st.save_config()
}

/// 공급자 삭제 — 저장된 API 키(실행 중 기억·OS 자격 증명)도 함께 지운다
#[tauri::command]
pub fn delete_provider(st: State<'_, AppState>, name: String) -> R<()> {
    {
        let mut list = st.providers.write().unwrap();
        let before = list.len();
        list.retain(|p| p.name != name);
        if list.len() == before {
            return Err(ErrView::msg("invalid", format!("AI 공급자가 없습니다: {name}")));
        }
    }
    st.api_keys.write().unwrap().remove(&name);
    sqls_core::secret::delete(&sqls_core::secret::llm_account(&name));
    st.save_config()
}

/// 모델 목록 — 아직 저장하지 않은 설정(주소·종류)으로 서버에 묻는다 (Ollama 는 /api/tags).
/// 같은 이름으로 저장된 API 키는 저장된 설정과 같은 서버(종류·주소)로 물을 때만 쓴다 —
/// 주소 칸을 고치는 중에(치는 도중 자동으로 묻는다) 저장된 키가 다른 서버로 나가지 않게
#[tauri::command]
pub async fn ai_list_models(st: State<'_, AppState>, provider: ProviderConfig) -> R<Vec<String>> {
    let mut p = provider;
    let same_server = st
        .providers
        .read()
        .unwrap()
        .iter()
        .any(|s| s.name == p.name && s.kind == p.kind && s.base_url().trim_end_matches('/') == p.base_url().trim_end_matches('/'));
    if p.api_key.is_none() && same_server {
        if let Some(k) = st.api_keys.read().unwrap().get(&p.name) {
            p.api_key = Some(k.clone());
        } else if let Some(k) = sqls_core::secret::get(&sqls_core::secret::llm_account(&p.name)) {
            p.api_key = Some(k);
        }
    }
    let mut models = st.llm.list_models(&p).await?;
    models.sort();
    Ok(models)
}

/// 외부 전송 동의 — 확인 창에서 "계속"을 눌렀을 때. 지금 주소로 기록하고 설정 파일에 남긴다.
#[tauri::command]
pub fn ai_ack_remote(st: State<'_, AppState>, provider: String) -> R<()> {
    {
        let mut list = st.providers.write().unwrap();
        let p = list.iter_mut().find(|p| p.name == provider).ok_or_else(|| ErrView::msg("invalid", format!("AI 공급자가 없습니다: {provider}")))?;
        p.remote_ack = Some(p.base_url());
    }
    st.save_config()
}

/// API 키는 파일에 쓰지 않는다. 이 실행 동안 기억하고, `remember` 면 OS 자격 증명 저장소에도 둔다.
#[tauri::command]
pub fn set_api_key(st: State<'_, AppState>, provider: String, key: String, remember: Option<bool>) -> R<()> {
    let account = sqls_core::secret::llm_account(&provider);
    let mut keys = st.api_keys.write().unwrap();
    if key.trim().is_empty() {
        keys.remove(&provider);
        sqls_core::secret::delete(&account);
    } else {
        keys.insert(provider, key.trim().to_string());
        if remember == Some(true) {
            sqls_core::secret::set(&account, key.trim()).map_err(|e| ErrView::msg("invalid", e))?;
        }
    }
    Ok(())
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
    } else if let Some(k) = sqls_core::secret::get(&sqls_core::secret::llm_account(name)) {
        p.api_key = Some(k);
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
    Context { tables: Vec<String>, plan: bool, remote: bool, guides: Vec<String> },
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
    // 외부 전송 동의는 여기서 확인한다 (화면 확인만으로는 우회될 수 있다)
    if !p.remote_ok() {
        return Err(ErrView::msg("remote_consent", format!("'{}' ({}) 은 외부 서버입니다. 전송 동의가 필요합니다.", p.name, p.base_url())));
    }
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
    // Oracle 문법 안내: 질문·SQL·오류와 관련된 카드만 (기본 + 설정 폴더의 oracle-guides.md)
    let text = [ctx.question.as_deref(), ctx.sql.as_deref(), ctx.error.as_deref()].into_iter().flatten().collect::<Vec<_>>().join("\n");
    let all = sqls_llm::guides::load(Some(&guides_file()));
    let major = ctx.server_version.as_deref().and_then(prompts::major_version);
    // 낱말로 고르고, 설정에 따라 AI 에게도 고르게 한다 (자동: 낱말로 약할 때만). 실패·시간 초과면 낱말만
    let sel = st.cfg.read().unwrap().ai.clone();
    let route = match sel.guide_select.as_str() {
        "always" => true,
        "off" => false,
        _ => !sqls_llm::guides::keyword_strong(&all, args.task, &text, major),
    };
    let mut ai_ids: Vec<String> = Vec::new();
    if route {
        if let Some(req) = sqls_llm::guides::route_request(&all, args.task, &text, major) {
            status("참고할 문법 카드를 고르는 중…");
            let llm = st.llm.clone();
            // 생각이 긴 모델도 끝나게 넉넉히 (모델 문맥 크기는 그대로 — Ollama 가 모델을 다시 올리지 않게)
            let mut rp = p.clone();
            rp.max_tokens = rp.max_tokens.min(1024);
            let limit = std::time::Duration::from_secs(sel.guide_select_timeout_secs.max(1));
            let task = tokio::spawn(async move {
                let mut req = req;
                let first = tokio::time::timeout(limit, llm.chat(&rp, &req, |_| {})).await;
                match first {
                    // 형식 지정을 못 받는 서버
                    Ok(Err(sqls_llm::Error::Api { status: 400, .. })) => {
                        req.json_schema = None;
                        tokio::time::timeout(limit, llm.chat(&rp, &req, |_| {})).await
                    }
                    r => r,
                }
            });
            st.ai_tasks.lock().unwrap().insert(args.request_id, task.abort_handle());
            let joined = task.await;
            st.ai_tasks.lock().unwrap().remove(&args.request_id);
            match joined {
                Ok(Ok(Ok(r))) => ai_ids = sqls_llm::guides::parse_route(&r.text, &all),
                Ok(Ok(Err(e))) => tracing::warn!("문법 카드 고르기 실패 — 낱말로 고른 것만 씁니다: {e}"),
                Ok(Err(_)) => tracing::warn!("문법 카드 고르기가 {}초를 넘겨 낱말로 고른 것만 씁니다", limit.as_secs()),
                Err(e) if e.is_cancelled() => return Err(ErrView::msg("cancelled", "AI 요청을 취소했습니다")),
                Err(e) => tracing::warn!("문법 카드 고르기 작업 오류: {e}"),
            }
        }
    }
    let picked = sqls_llm::guides::pick_with(&all, args.task, &text, major, &ai_ids);
    let cards: Vec<&sqls_llm::guides::Guide> = picked.iter().map(|(g, _)| *g).collect();
    ctx.guides = sqls_llm::guides::render(&cards);
    let _ = on_event.send(AiEvent::Context {
        tables: used_tables,
        plan: ctx.plan.is_some(),
        remote: p.is_remote(),
        guides: picked
            .iter()
            .map(|(g, why)| match why {
                sqls_llm::guides::Why::Ai => format!("{} (AI 선택)", g.title),
                sqls_llm::guides::Why::Both => format!("{} (낱말+AI)", g.title),
                _ => g.title.clone(),
            })
            .collect(),
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

/// 사용자 문법 안내 파일 (설정 폴더의 oracle-guides.md)
fn guides_file() -> std::path::PathBuf {
    sqls_core::config::config_path().parent().map(|p| p.join("oracle-guides.md")).unwrap_or_else(|| "oracle-guides.md".into())
}

#[derive(Serialize)]
pub struct GuidesInfo {
    path: String,
    exists: bool,
    builtin: usize,
    custom: usize,
}

/// 문법 안내 상태 (설정 > AI)
#[tauri::command]
pub fn ai_guides_info() -> GuidesInfo {
    let path = guides_file();
    let custom = std::fs::read_to_string(&path).map(|t| sqls_llm::guides::parse(&t).len()).unwrap_or(0);
    GuidesInfo { exists: path.is_file(), path: path.display().to_string(), builtin: sqls_llm::guides::load(None).len(), custom }
}

/// 사용자 문법 안내 파일을 (없으면 예시로 만들어) 경로를 돌려준다 — 화면이 편집기로 연다
#[tauri::command]
pub fn ai_guides_edit() -> R<String> {
    let path = guides_file();
    if !path.is_file() {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        std::fs::write(&path, sqls_llm::guides::user_template()).map_err(|e| ErrView::msg("io", format!("{}: {e}", path.display())))?;
    }
    Ok(path.display().to_string())
}

/// AI 로 문법 카드 고르기 설정 (off · auto · always)
#[tauri::command]
pub fn ai_guide_select(st: State<'_, AppState>) -> String {
    st.cfg.read().unwrap().ai.guide_select.clone()
}

#[tauri::command]
pub fn set_ai_guide_select(st: State<'_, AppState>, mode: String) -> R<()> {
    if !matches!(mode.as_str(), "off" | "auto" | "always") {
        return Err(ErrView::msg("invalid", format!("알 수 없는 값: {mode}")));
    }
    st.cfg.write().unwrap().ai.guide_select = mode;
    st.save_config()
}

#[derive(Serialize)]
pub struct GuideView {
    id: String,
    title: String,
    keys: Vec<String>,
    /// "12-" · "-11" · 빈칸
    version: String,
    body: String,
    /// builtin · custom(회사 규칙에만) · override(기본을 회사 규칙이 바꿈)
    source: &'static str,
}

/// 문법 안내 카드 전체 — 실제로 쓰는 것 (회사 규칙이 바꾼 것은 바뀐 내용으로)
#[tauri::command]
pub fn ai_guides_list() -> Vec<GuideView> {
    let builtin = sqls_llm::guides::builtin_cards();
    sqls_llm::guides::load(Some(&guides_file()))
        .into_iter()
        .map(|g| {
            let source = match builtin.iter().find(|b| b.id == g.id) {
                None => "custom",
                Some(b) if *b == g => "builtin",
                Some(_) => "override",
            };
            let version = match (g.min, g.max) {
                (None, None) => String::new(),
                (a, b) => format!("{}-{}", a.map(|x| x.to_string()).unwrap_or_default(), b.map(|x| x.to_string()).unwrap_or_default()),
            };
            GuideView { id: g.id, title: g.title, keys: g.keys, version, body: g.body, source }
        })
        .collect()
}

/// 카드 고치기 — 기본 카드면 원문을 회사 규칙 파일 끝에 옮겨 둔다 (같은 아이디라 그쪽이 쓰인다). 파일 경로를 돌려준다
#[tauri::command]
pub fn ai_guides_customize(id: String) -> R<String> {
    let path = ai_guides_edit()?;
    let io = |e: std::io::Error| ErrView::msg("io", format!("{}: {e}", path));
    let text = std::fs::read_to_string(&path).map_err(io)?;
    if sqls_llm::guides::parse(&text).iter().any(|g| g.id == id) {
        return Ok(path);
    }
    let block = sqls_llm::guides::builtin_block(&id).ok_or_else(|| ErrView::msg("config", format!("카드 '{id}' 가 없습니다")))?;
    let sep = if text.ends_with('\n') { "\n" } else { "\n\n" };
    std::fs::write(&path, format!("{text}{sep}# 기본 카드 '{id}' 를 옮겨 왔다 — 고치면 이 내용을 쓴다. 지우면 기본으로 돌아간다\n{block}")).map_err(io)?;
    Ok(path)
}

#[tauri::command]
pub fn ai_cancel(st: State<'_, AppState>, request_id: u64) {
    if let Some(h) = st.ai_tasks.lock().unwrap().remove(&request_id) {
        h.abort();
    }
}
