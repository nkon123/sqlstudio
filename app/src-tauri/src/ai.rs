//! AI 명령 — 공급자 관리, 스트리밍 질의, 취소.
//!
//! 문맥(스키마, 실행계획)은 여기서 모은다. 화면이 조합하면 공급자마다 형식이 달라지고,
//! 무엇이 밖으로 나가는지 한 곳에서 볼 수 없다.

use sqls_core::sync::{MutexExt, RwLockExt};
use serde::{Deserialize, Serialize};
use sqls_core::meta;
use sqls_llm::harness;
use sqls_llm::prompts::{self, Context, Task};
use sqls_llm::{ChatRequest, ChatResponse, Message, ProviderConfig};
use tauri::ipc::Channel;
use tauri::State;

use crate::state::{AppState, ErrView};

type R<T> = Result<T, ErrView>;

/// 스키마 문맥에 넣을 테이블 수 상한 — 로컬 모델에서 토큰은 곧 시간이다 (넘치면 예산 맞추기가 더 줄인다)
const MAX_CONTEXT_TABLES: usize = 8;
/// SQL 생성에서 낱말로 찾아 AI 에게 보일 테이블 후보 수
const MAX_PICK_CANDIDATES: usize = 30;
/// 컬럼이 이보다 많은 테이블은 예산이 모자랄 때 관련 컬럼만 넣는다
const SLIM_COLUMNS: usize = 15;

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
            st.api_keys.rd().contains_key(&p.name)
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
    st.providers.rd().iter().map(|p| view(&st, p)).collect()
}

#[tauri::command]
pub fn save_provider(st: State<'_, AppState>, provider: ProviderConfig, original_name: Option<String>) -> R<()> {
    if provider.name.trim().is_empty() || provider.model.trim().is_empty() {
        return Err(ErrView::msg("invalid", "이름과 모델은 비울 수 없습니다"));
    }
    {
        let mut list = st.providers.wr();
        let key = original_name.unwrap_or_else(|| provider.name.clone());
        let mut provider = provider;
        // 동의 기록은 화면이 보낸 값을 믿지 않는다 — 원래 기록을 주소가 같을 때만 이어 간다
        provider.remote_ack = list.iter().find(|p| p.name == key).and_then(|p| p.remote_ack.clone()).filter(|a| *a == provider.base_url());
        // 모델 측정 결과는 화면에 칸이 없다 — 같은 모델이면 이어 간다
        if provider.profile.is_none() {
            provider.profile = list.iter().find(|p| p.name == key).and_then(|p| p.profile.clone()).filter(|m| m.model == provider.model);
        }
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
        let mut list = st.providers.wr();
        let before = list.len();
        list.retain(|p| p.name != name);
        if list.len() == before {
            return Err(ErrView::msg("invalid", format!("AI 공급자가 없습니다: {name}")));
        }
    }
    st.api_keys.wr().remove(&name);
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
        if let Some(k) = st.api_keys.rd().get(&p.name) {
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
        let mut list = st.providers.wr();
        let p = list.iter_mut().find(|p| p.name == provider).ok_or_else(|| ErrView::msg("invalid", format!("AI 공급자가 없습니다: {provider}")))?;
        p.remote_ack = Some(p.base_url());
    }
    st.save_config()
}

/// API 키는 파일에 쓰지 않는다. 이 실행 동안 기억하고, `remember` 면 OS 자격 증명 저장소에도 둔다.
#[tauri::command]
pub fn set_api_key(st: State<'_, AppState>, provider: String, key: String, remember: Option<bool>) -> R<()> {
    let account = sqls_core::secret::llm_account(&provider);
    let mut keys = st.api_keys.wr();
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
    if let Some(k) = st.api_keys.rd().get(name) {
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

/// 모델 측정 — 속도·형식 지정·생각 모드·문맥 크기를 재서 공급자 설정에 남긴다 (하네스가 단계를 고른다)
#[tauri::command]
pub async fn ai_probe(st: State<'_, AppState>, name: String) -> R<sqls_llm::ModelProfile> {
    let p = provider(&st, &name)?;
    if !p.remote_ok() {
        return Err(ErrView::msg("remote_consent", format!("'{}' ({}) 은 외부 서버입니다. 전송 동의가 필요합니다.", p.name, p.base_url())));
    }
    let prof = st.llm.probe(&p).await?;
    if let Some(x) = st.providers.wr().iter_mut().find(|x| x.name == name) {
        x.profile = Some(prof.clone());
    }
    st.save_config()?;
    Ok(prof)
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
    /// 밖으로 보낸 문맥 요약 (투명성) — 어림 토큰 수와 예산, 예산에 맞추느라 줄인 것.
    /// `picked`: 테이블을 하네스가 골랐으면 어떻게 (ai · keyword), `examples`: 넣은 내 쿼리 예시의 질문
    Context {
        tables: Vec<String>,
        plan: bool,
        remote: bool,
        guides: Vec<String>,
        tokens: usize,
        budget: usize,
        trimmed: Vec<String>,
        picked: Option<&'static str>,
        examples: Vec<String>,
    },
    Delta { text: String },
    /// 지금까지 보낸 답을 지운다 (다시 묻는 중 — 이어서 새 답이 온다)
    Reset,
}

/// 답의 SQL 을 확인한 결과
#[derive(Serialize, Default)]
pub struct Verify {
    /// ok: 서버에서 파싱까지 통과 · static: 앱 안의 검사만 (서버 확인을 못 함) · failed: 고치지 못함 · none: SQL 없음
    state: &'static str,
    /// static 이면 서버 확인을 못 한 까닭, failed 면 남은 문제
    note: Option<String>,
    /// 고쳐 달라고 다시 물은 횟수
    repairs: u32,
    /// 처음 답에서 걸린 문제 (고쳤으면 무엇을 고쳤는지 보인다)
    first_problem: Option<String>,
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
    verify: Verify,
    /// 하네스가 알릴 것 (문맥이 잘릴 수 있음, 생각을 끄고 다시 물음 …)
    warnings: Vec<String>,
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
    let local = harness::is_local_model(&p);
    // 모델 측정 결과로 단계를 고른다 (측정 전이면 4B 급 기본값)
    let plan = harness::plan(&p);
    let sel = st.cfg.rd().ai.clone();
    let session = args.session_id.and_then(|id| st.session(id).ok().map(|(s, _, _)| (id, s)));
    let mut descs: Vec<meta::TableDesc> = Vec::new();
    let mut tables_by: Option<&'static str> = None;
    {
        if let Some((_, s)) = &session {
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
            // SQL 생성인데 테이블을 모르면 — 질문 낱말로 딕셔너리에서 후보를 찾고 고르게 한다 (고르기와 쓰기를 나눈다)
            if names.is_empty() && args.task == Task::Generate && sel.table_pick != "off" {
                if let Some(q) = ctx.question.clone() {
                    status("쓸 테이블을 찾는 중…");
                    let words = harness::focus_words(&q);
                    match tokio::time::timeout(std::time::Duration::from_secs(10), meta::find_tables(s, &words, MAX_PICK_CANDIDATES)).await {
                        Ok(Ok(hits)) if !hits.is_empty() => {
                            let (chosen, by) = pick_tables(&st, args.request_id, &p, &plan, &sel, &q, &hits, &status).await?;
                            names = chosen;
                            tables_by = Some(by);
                        }
                        Ok(Ok(_)) => {}
                        Ok(Err(e)) => tracing::warn!("테이블 후보 찾기 실패: {e}"),
                        Err(_) => tracing::warn!("테이블 후보 찾기가 10초를 넘겨 건너뜁니다"),
                    }
                }
            }
            if !names.is_empty() {
                status("스키마 정보를 읽는 중…");
            }
            let take = if tables_by.is_some() { plan.tables } else { MAX_CONTEXT_TABLES };
            for n in names.into_iter().take(take) {
                // 없는 이름(별칭, CTE 이름)은 조용히 건너뛴다
                if let Ok(d) = meta::describe(s, &n).await {
                    descs.push(d);
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
    // 낱말로 고르고, 설정에 따라 AI 에게도 고르게 한다 (자동: 낱말로 약할 때만 — 느린 모델이면 건너뜀). 실패·시간 초과면 낱말만
    let route = match sel.guide_select.as_str() {
        "always" => true,
        "off" => false,
        _ => plan.routing && !sqls_llm::guides::keyword_strong(&all, args.task, &text, major),
    };
    let mut ai_ids: Vec<String> = Vec::new();
    if route {
        if let Some(mut req) = sqls_llm::guides::route_request(&all, args.task, &text, major) {
            status("참고할 문법 카드를 고르는 중…");
            if !plan.json {
                req.json_schema = None;
            }
            let limit = std::time::Duration::from_secs(sel.guide_select_timeout_secs.max(1));
            match side_chat(&st, args.request_id, &side_provider(&p, &plan), req, limit).await? {
                Ok(t) => ai_ids = sqls_llm::guides::parse_route(&t, &all),
                Err(why) => tracing::warn!("문법 카드 고르기 {why} — 낱말로 고른 것만 씁니다"),
            }
        }
    }
    // 내 쿼리 예시 — 로컬 모델에서만 (사내 SQL 을 밖으로 보내지 않는다)
    if local && sel.examples && matches!(args.task, Task::Generate | Task::Chat) {
        if let (Some((_, s)), Some(q)) = (&session, &ctx.question) {
            ctx.examples = examples().search(&example_db(s), q, 2).into_iter().map(|e| (e.q, e.sql)).collect();
        }
    }
    let picked = sqls_llm::guides::pick_with(&all, args.task, &text, major, &ai_ids);
    let mut cards: Vec<&sqls_llm::guides::Guide> = picked.iter().map(|(g, _)| *g).collect();

    // ── 예산 맞추기 ──────────────────────────────────
    let budget = harness::context_budget(&p);
    let words = harness::focus_words(&text);
    let fit = fit_context(&mut ctx, &descs, &mut cards, &args.history, budget, &words);
    let _ = on_event.send(AiEvent::Context {
        tables: fit.tables.clone(),
        plan: ctx.plan.is_some(),
        remote: p.is_remote(),
        guides: picked
            .iter()
            .filter(|(g, _)| cards.iter().any(|c| c.id == g.id))
            .map(|(g, why)| match why {
                sqls_llm::guides::Why::Ai => format!("{} (AI 선택)", g.title),
                sqls_llm::guides::Why::Both => format!("{} (낱말+AI)", g.title),
                _ => g.title.clone(),
            })
            .collect(),
        tokens: fit.tokens,
        budget,
        trimmed: fit.trimmed.clone(),
        picked: tables_by,
        examples: ctx.examples.iter().map(|(q, _)| q.clone()).collect(),
    });
    let mut warnings = Vec::new();
    if p.kind == sqls_llm::ProviderKind::Ollama {
        let ctx_len = p.num_ctx.unwrap_or(16384) as usize;
        if fit.tokens + 512 > ctx_len {
            warnings.push(format!("문맥(약 {} 토큰)이 num_ctx({ctx_len})를 넘어 앞부분이 잘릴 수 있습니다 — 공급자 설정에서 num_ctx 를 늘리세요", fit.tokens));
        }
    }

    let req = prompts::build(args.task, &ctx, &fit.history);
    let user_message = req.messages.last().map(|m| m.content.clone()).unwrap_or_default();

    // 작은 로컬 모델: SQL 을 쓰거나 설명할 때는 생각을 끈다 (빠르고, 되풀이에 빠지지 않는다 — 틀리면 검사가 잡는다).
    // 튜닝·오류 수정은 모델 기본값. 사용자가 생각·temperature 를 정했으면 그대로
    let mut mp = p.clone();
    if local && p.think.is_none() && plan.think && matches!(args.task, Task::Generate | Task::Explain | Task::Chat) {
        mp.think = Some("off".into());
    }
    if local && p.temperature.is_none() {
        mp.temperature = Some(if mp.think.as_deref() == Some("off") { 0.2 } else { 0.6 });
    }

    // ── 질의 ─────────────────────────────────────────
    status(if p.is_remote() { "응답을 기다리는 중…" } else { "로컬 모델이 생각하는 중… (첫 응답은 모델 로딩으로 오래 걸릴 수 있습니다)" });
    let mut resp = match stream_chat(&st, args.request_id, &mp, &req, &on_event).await? {
        Err(sqls_llm::Error::Repetition) if mp.think.as_deref() != Some("off") || !plan.think => {
            let _ = on_event.send(AiEvent::Reset);
            status("같은 말을 되풀이해 끊었습니다 — 생각을 끈 채로 다시 묻는 중…");
            warnings.push("생각하다가 같은 말을 되풀이해, 생각을 끄고 다시 물었습니다".into());
            mp.think = plan.think.then(|| "off".into());
            if local && p.temperature.is_none() {
                mp.temperature = Some(0.2);
            }
            stream_chat(&st, args.request_id, &mp, &req, &on_event).await??
        }
        r => r?,
    };

    // ── 검증·수리 ────────────────────────────────────
    // 답의 SQL 을 앱 안에서(버전 문법) → 서버에서(파싱만) 확인하고, 걸리면 오류와 전체 컬럼을 주고 다시 묻는다
    let mut verify = Verify { state: "none", ..Default::default() };
    if args.task != Task::Explain {
        let mut msgs = req.messages.clone();
        let mut known: Vec<meta::TableDesc> = descs.clone();
        while let Some(sql) = prompts::extract_sql(&resp.text) {
            status("답의 SQL 을 확인하는 중…");
            match check_sql(&st, session.as_ref(), &sql, major).await {
                Check::Problem(problem) if verify.repairs < plan.repairs => {
                    verify.first_problem.get_or_insert_with(|| problem.clone());
                    let schema = repair_schema(session.as_ref(), &sql, &mut known).await;
                    msgs.push(Message::assistant(harness::strip_think(&resp.text)));
                    msgs.push(Message::user(prompts::repair_message(&problem, &schema)));
                    let rreq = ChatRequest { system: req.system.clone(), messages: msgs.clone(), json_schema: None };
                    verify.repairs += 1;
                    let _ = on_event.send(AiEvent::Reset);
                    status(&format!("확인에서 걸렸습니다 ({}) — 고쳐 달라고 다시 묻는 중 ({}/{})", first_line(&problem), verify.repairs, plan.repairs));
                    let mut rp = mp.clone();
                    if local && p.think.is_none() {
                        rp.think = plan.think.then(|| "off".into());
                        rp.temperature = p.temperature.or(Some(0.2));
                    }
                    match stream_chat(&st, args.request_id, &rp, &rreq, &on_event).await? {
                        Ok(r) => resp = r,
                        Err(e) => {
                            // 고치는 답을 못 받았다 — 앞의 답을 다시 보이고 문제를 알린다
                            let _ = on_event.send(AiEvent::Reset);
                            let _ = on_event.send(AiEvent::Delta { text: resp.text.clone() });
                            verify.state = "failed";
                            verify.note = Some(format!("{problem}\n(고쳐 달라는 요청이 실패: {e})"));
                            break;
                        }
                    }
                }
                Check::Problem(problem) => {
                    verify.state = "failed";
                    verify.note = Some(problem);
                    break;
                }
                Check::Static(why) => {
                    verify.state = "static";
                    verify.note = Some(why);
                    break;
                }
                Check::Ok => {
                    verify.state = "ok";
                    break;
                }
            }
        }
    }

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
        verify,
        warnings,
    })
}

/// 질의 하나를 스트리밍으로 — 취소할 수 있게 별도 작업으로 돌린다.
/// 바깥 Err 는 취소·작업 오류, 안쪽 Err 는 LLM 오류 (부르는 쪽이 되풀이 등을 골라 다룬다)
async fn stream_chat(
    st: &AppState,
    request_id: u64,
    p: &ProviderConfig,
    req: &ChatRequest,
    on_event: &Channel<AiEvent>,
) -> R<sqls_llm::Result<ChatResponse>> {
    let llm = st.llm.clone();
    let ch = on_event.clone();
    let (p, req) = (p.clone(), req.clone());
    let task = tokio::spawn(async move {
        llm.chat(&p, &req, move |d| {
            let _ = ch.send(AiEvent::Delta { text: d.to_string() });
        })
        .await
    });
    st.ai_tasks.lk().insert(request_id, task.abort_handle());
    let joined = task.await;
    st.ai_tasks.lk().remove(&request_id);
    match joined {
        Ok(r) => Ok(r),
        Err(e) if e.is_cancelled() => Err(ErrView::msg("cancelled", "AI 요청을 취소했습니다")),
        Err(e) => Err(ErrView::msg("llm", format!("AI 작업이 비정상 종료되었습니다: {e}"))),
    }
}

/// 중간 단계(문법 카드·테이블 고르기)에 쓸 설정 — 짧게, 작은 로컬 모델은 생각을 끄고 낮은 temperature 로.
/// 모델 문맥 크기는 그대로 둔다 (Ollama 가 모델을 다시 올리지 않게)
fn side_provider(p: &ProviderConfig, plan: &harness::Plan) -> ProviderConfig {
    let mut rp = p.clone();
    rp.max_tokens = rp.max_tokens.min(1024);
    if harness::is_local_model(p) && p.think.is_none() {
        rp.think = plan.think.then(|| "off".into());
        rp.temperature = p.temperature.or(Some(0.2));
    }
    rp
}

/// 형식으로 묶은 짧은 질의 — 형식 지정을 못 받는 서버(400)면 형식 없이 다시. 안쪽 Err 는 왜 못 했는지 (부르는 쪽이 낱말로 대신한다)
async fn side_chat(st: &AppState, request_id: u64, p: &ProviderConfig, req: ChatRequest, limit: std::time::Duration) -> R<Result<String, String>> {
    let llm = st.llm.clone();
    let p = p.clone();
    let task = tokio::spawn(async move {
        let mut req = req;
        match tokio::time::timeout(limit, llm.chat(&p, &req, |_| {})).await {
            Ok(Err(sqls_llm::Error::Api { status: 400, .. })) if req.json_schema.is_some() => {
                req.json_schema = None;
                tokio::time::timeout(limit, llm.chat(&p, &req, |_| {})).await
            }
            r => r,
        }
    });
    st.ai_tasks.lk().insert(request_id, task.abort_handle());
    let joined = task.await;
    st.ai_tasks.lk().remove(&request_id);
    Ok(match joined {
        Ok(Ok(Ok(r))) => Ok(r.text),
        Ok(Ok(Err(e))) => Err(format!("실패: {e}")),
        Ok(Err(_)) => Err(format!("{}초 넘김", limit.as_secs())),
        Err(e) if e.is_cancelled() => return Err(ErrView::msg("cancelled", "AI 요청을 취소했습니다")),
        Err(e) => Err(format!("작업 오류: {e}")),
    })
}

/// 쓸 테이블 고르기 — 후보가 몇 개뿐이면 그대로, 많으면 AI 에게 enum 으로 고르게 한다.
/// 느린 모델이거나 고르기가 실패하면 점수 높은 것 몇 개
#[allow(clippy::too_many_arguments)]
async fn pick_tables(
    st: &AppState,
    request_id: u64,
    p: &ProviderConfig,
    plan: &harness::Plan,
    sel: &sqls_core::config::AiConfig,
    question: &str,
    hits: &[meta::TableHit],
    status: &impl Fn(&str),
) -> R<(Vec<String>, &'static str)> {
    let top = |n: usize| hits.iter().take(n).map(|h| h.name.clone()).collect::<Vec<_>>();
    if hits.len() <= plan.tables {
        return Ok((top(plan.tables), "keyword"));
    }
    if !plan.routing {
        return Ok((top(plan.tables.min(3)), "keyword"));
    }
    let cands: Vec<(String, String)> = hits.iter().map(|h| (h.name.clone(), h.comment.clone())).collect();
    if let Some(mut req) = prompts::table_pick_request(question, &cands, plan.tables) {
        status(&format!("쓸 테이블을 고르는 중… (후보 {}개)", cands.len()));
        if !plan.json {
            req.json_schema = None;
        }
        let limit = std::time::Duration::from_secs(sel.guide_select_timeout_secs.max(1));
        match side_chat(st, request_id, &side_provider(p, plan), req, limit).await? {
            Ok(t) => {
                let got = prompts::parse_pick(&t, &cands, plan.tables);
                if !got.is_empty() {
                    return Ok((got, "ai"));
                }
            }
            Err(why) => tracing::warn!("테이블 고르기 {why} — 낱말 점수로 고릅니다"),
        }
    }
    Ok((top(plan.tables.min(3)), "keyword"))
}

/// 내 쿼리 예시 파일 (데이터 폴더)
fn examples() -> harness::ExampleStore {
    harness::ExampleStore::new(sqls_core::config::data_dir().join("ai-examples.jsonl"))
}

/// 예시를 나눌 접속 구분 — 같은 사용자·같은 DB 의 것만 쓴다
fn example_db(s: &sqls_core::session::Session) -> String {
    format!("{}@{}", s.info().user, s.info().connect_string)
}

/// AI 가 만든 SQL 을 사용자가 에디터에 넣었을 때 — 서버 파싱을 통과한 것만 화면이 보낸다
#[tauri::command]
pub fn ai_remember_example(st: State<'_, AppState>, session_id: u64, question: String, sql: String) -> R<()> {
    if !st.cfg.rd().ai.examples || question.trim().is_empty() || sql.trim().is_empty() {
        return Ok(());
    }
    let (s, _, _) = st.session(session_id)?;
    examples()
        .add(harness::Example { db: example_db(&s), q: question.trim().to_string(), sql: sql.trim().to_string() })
        .map_err(|e| ErrView::msg("io", format!("{}: {e}", examples().path().display())))
}

#[derive(Serialize)]
pub struct HarnessOpts {
    /// SQL 생성에서 테이블을 하네스가 고를지: "auto" | "off"
    table_pick: String,
    /// 내 쿼리 예시를 쌓고 쓸지
    examples: bool,
    examples_count: usize,
    examples_path: String,
}

#[tauri::command]
pub fn ai_harness_opts(st: State<'_, AppState>) -> HarnessOpts {
    let ai = st.cfg.rd().ai.clone();
    let ex = examples();
    HarnessOpts { table_pick: ai.table_pick, examples: ai.examples, examples_count: ex.load().len(), examples_path: ex.path().display().to_string() }
}

#[tauri::command]
pub fn set_ai_harness_opts(st: State<'_, AppState>, table_pick: Option<String>, examples: Option<bool>) -> R<()> {
    {
        let mut cfg = st.cfg.wr();
        if let Some(t) = table_pick {
            if !matches!(t.as_str(), "auto" | "off") {
                return Err(ErrView::msg("invalid", format!("알 수 없는 값: {t}")));
            }
            cfg.ai.table_pick = t;
        }
        if let Some(e) = examples {
            cfg.ai.examples = e;
        }
    }
    st.save_config()
}

#[tauri::command]
pub fn ai_examples_clear() -> R<()> {
    examples().clear().map_err(|e| ErrView::msg("io", e.to_string()))
}

fn first_line(s: &str) -> String {
    let l = s.lines().next().unwrap_or("").trim();
    match l.char_indices().nth(80) {
        Some((i, _)) => format!("{}…", &l[..i]),
        None => l.to_string(),
    }
}

struct Fit {
    history: Vec<Message>,
    tables: Vec<String>,
    tokens: usize,
    trimmed: Vec<String>,
}

/// 문맥을 예산 안으로 — 스키마(전체) · 문법 카드 · 대화 기록을 넣어 보고, 넘치면
/// ① 컬럼 많은 테이블은 관련 컬럼만 ② 내 쿼리 예시 ③ 문법 카드를 1장까지 ④ 뒤쪽 테이블을 1개까지 순서로 줄인다.
/// 대화 기록은 남은 예산 안에서 (최근 2번은 원문, 그 앞은 SQL 만)
fn fit_context(
    ctx: &mut Context,
    descs: &[meta::TableDesc],
    cards: &mut Vec<&sqls_llm::guides::Guide>,
    history: &[Message],
    budget: usize,
    words: &[String],
) -> Fit {
    let keep_col = |c: &meta::ColumnDesc| {
        words.iter().any(|w| {
            if w.is_ascii() {
                c.name.eq_ignore_ascii_case(w)
            } else {
                c.comment.as_deref().is_some_and(|cm| cm.contains(w.as_str()))
            }
        })
    };
    let mut slim = false;
    let mut n_tables = descs.len();
    let mut trimmed = Vec::new();
    let measure = |ctx: &mut Context, slim: bool, n: usize, cards: &[&sqls_llm::guides::Guide]| {
        ctx.schema = descs[..n]
            .iter()
            .map(|d| if slim && d.columns.len() > SLIM_COLUMNS { meta::schema_brief_some(d, &keep_col) } else { meta::schema_brief(d) })
            .collect();
        ctx.guides = sqls_llm::guides::render(cards);
        harness::estimate_tokens(&prompts::system_prompt(ctx)) + harness::estimate_tokens(&prompts::user_message(Task::Chat, ctx))
    };
    // 대화 기록 몫으로 4분의 1 은 남겨 둔다
    let room = budget * 3 / 4;
    let mut tokens = measure(ctx, slim, n_tables, cards);
    if tokens > room && descs.iter().any(|d| d.columns.len() > SLIM_COLUMNS) {
        slim = true;
        tokens = measure(ctx, slim, n_tables, cards);
        trimmed.push("컬럼 많은 테이블은 관련 컬럼만".to_string());
    }
    let before = ctx.examples.len();
    while tokens > room && !ctx.examples.is_empty() {
        ctx.examples.pop();
        tokens = measure(ctx, slim, n_tables, cards);
    }
    if ctx.examples.len() < before {
        trimmed.push(format!("내 쿼리 예시 {}개 뺌", before - ctx.examples.len()));
    }
    let before = cards.len();
    while tokens > room && cards.len() > 1 {
        cards.pop();
        tokens = measure(ctx, slim, n_tables, cards);
    }
    if cards.len() < before {
        trimmed.push(format!("문법 안내 {}장 뺌", before - cards.len()));
    }
    let before = n_tables;
    while tokens > room && n_tables > 1 {
        n_tables -= 1;
        tokens = measure(ctx, slim, n_tables, cards);
    }
    if n_tables < before {
        trimmed.push(format!("테이블 {}개 뺌 ({})", before - n_tables, descs[n_tables..].iter().map(|d| d.name.as_str()).collect::<Vec<_>>().join(", ")));
    }
    let hist = harness::trim_history(history, budget.saturating_sub(tokens), 2);
    if hist.len() < history.len() {
        trimmed.push(format!("대화 기록 앞쪽 {}개 뺌", history.len() - hist.len()));
    } else if hist.iter().zip(history).any(|(a, b)| a.content.len() != b.content.len()) {
        trimmed.push("지난 대화는 요약해서".to_string());
    }
    tokens += hist.iter().map(|m| harness::estimate_tokens(&m.content)).sum::<usize>();
    Fit { history: hist, tables: descs[..n_tables].iter().map(|d| format!("{}.{}", d.owner, d.name)).collect(), tokens, trimmed }
}

enum Check {
    /// 서버에서 파싱까지 통과
    Ok,
    /// 앱 안의 검사만 통과 — 서버 확인을 못 한 까닭
    Static(String),
    /// 고쳐야 할 문제
    Problem(String),
}

/// 환경 탓인 오류 (모델이 고칠 수 없다) — 권한, 접속, 취소
fn env_error(msg: &str) -> bool {
    ["ORA-01031", "ORA-01039", "ORA-03113", "ORA-03114", "ORA-03135", "ORA-01013", "ORA-00054", "ORA-04068", "DPI-", "ORA-06550"]
        .iter()
        .any(|c| msg.contains(c))
}

/// 답의 SQL 확인 — 버전 문법(앱 안) → 서버 파싱(DBMS_SQL.PARSE, 실행하지 않음).
/// 운영 DB 이거나, 세션이 바쁘거나, SELECT·DML 이 아니면 서버 확인은 건너뛴다
async fn check_sql(st: &AppState, session: Option<&(u64, sqls_core::session::Session)>, sql: &str, major: Option<u32>) -> Check {
    let stmts = sqls_core::sql::split_script(sql);
    let many = stmts.len() > 1;
    let mut parsed = 0;
    let mut why: Option<String> = None;
    for (i, s) in stmts.iter().take(5).enumerate() {
        let at = if many { format!("{}번째 문장: ", i + 1) } else { String::new() };
        let lint = harness::lint_version(&s.text, major);
        if !lint.is_empty() {
            return Check::Problem(format!("{at}{}", lint.join("\n")));
        }
        if !matches!(s.kind, sqls_core::sql::StmtKind::Query | sqls_core::sql::StmtKind::Dml) {
            why.get_or_insert_with(|| format!("{} 은 서버에서 확인하지 않습니다", s.kind.label()));
            continue;
        }
        let Some((id, sess)) = session else {
            why.get_or_insert_with(|| "접속하지 않아 서버 확인은 하지 않았습니다".into());
            continue;
        };
        if crate::db::is_production(st, *id) {
            why.get_or_insert_with(|| "운영 DB 라 서버 확인은 하지 않았습니다".into());
            continue;
        }
        // 실행 중인 문장 뒤에 줄 서지 않는다
        if sess.is_busy() {
            why.get_or_insert_with(|| "세션이 실행 중이라 서버 확인은 하지 않았습니다".into());
            continue;
        }
        match tokio::time::timeout(std::time::Duration::from_secs(10), sess.parse_only(&s.text)).await {
            Ok(Ok(())) => parsed += 1,
            Ok(Err(e)) => {
                // PL/SQL 로 감싸 부른 흔적(ORA-06512: at line …)은 뺀다
                let msg: String = e.to_string().lines().filter(|l| !l.contains("ORA-06512")).collect::<Vec<_>>().join("\n");
                if env_error(&msg) {
                    why.get_or_insert_with(|| format!("서버 확인을 못 했습니다: {}", first_line(&msg)));
                } else {
                    return Check::Problem(format!("{at}{msg}"));
                }
            }
            Err(_) => {
                why.get_or_insert_with(|| "서버 확인이 10초를 넘겨 건너뛰었습니다".into());
            }
        }
    }
    match why {
        None if parsed > 0 => Check::Ok,
        None => Check::Static("확인할 문장이 없습니다".into()),
        Some(w) => Check::Static(w),
    }
}

/// 고칠 때 줄 스키마 — 걸린 SQL 에 나온 테이블의 전체 컬럼. 없는 테이블은 없다고 적는다
async fn repair_schema(session: Option<&(u64, sqls_core::session::Session)>, sql: &str, known: &mut Vec<meta::TableDesc>) -> Vec<String> {
    let mut out = Vec::new();
    for n in meta::referenced_tables(sql).into_iter().take(MAX_CONTEXT_TABLES) {
        let leaf = n.rsplit('.').next().unwrap_or(&n).to_string();
        let found = known.iter().position(|d| d.name.eq_ignore_ascii_case(&leaf));
        let found = match (found, session) {
            (Some(i), _) => Some(i),
            (None, Some((_, s))) => match meta::describe(s, &n).await {
                Ok(d) => {
                    known.push(d);
                    Some(known.len() - 1)
                }
                Err(_) => {
                    out.push(format!("-- {n}: 이런 테이블·뷰는 없다 (또는 권한이 없다)\n"));
                    None
                }
            },
            (None, None) => None,
        };
        if let Some(i) = found {
            out.push(meta::schema_brief(&known[i]));
        }
    }
    out
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
    st.cfg.rd().ai.guide_select.clone()
}

#[tauri::command]
pub fn set_ai_guide_select(st: State<'_, AppState>, mode: String) -> R<()> {
    if !matches!(mode.as_str(), "off" | "auto" | "always") {
        return Err(ErrView::msg("invalid", format!("알 수 없는 값: {mode}")));
    }
    st.cfg.wr().ai.guide_select = mode;
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
    if let Some(h) = st.ai_tasks.lk().remove(&request_id) {
        h.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(name: &str, cols: usize) -> meta::TableDesc {
        meta::TableDesc {
            owner: "APP".into(),
            name: name.into(),
            object_type: "TABLE".into(),
            comment: None,
            num_rows: None,
            last_analyzed: None,
            columns: (0..cols)
                .map(|i| meta::ColumnDesc {
                    name: if i == 7 { "ORDER_DT".into() } else { format!("COL_{i:03}") },
                    data_type: "VARCHAR2(100)".into(),
                    nullable: true,
                    default: None,
                    comment: Some(if i == 9 { "고객번호".into() } else { format!("설명 {i}") }),
                })
                .collect(),
            primary_key: vec!["COL_000".into()],
            indexes: vec![],
            partial: false,
        }
    }

    #[test]
    fn words_for_columns() {
        assert_eq!(harness::focus_words("order_dt 로 최근 7 고객별 합계"), vec!["ORDER_DT", "최근", "고객", "합계"]);
    }

    #[test]
    fn fit_slims_then_drops() {
        let descs = vec![table("ORDERS", 120), table("ITEMS", 120)];
        let words = harness::focus_words("ORDER_DT 기준 고객별 건수");
        // 넉넉하면 그대로
        let mut ctx = Context::default();
        let f = fit_context(&mut ctx, &descs, &mut vec![], &[], 60000, &words);
        assert!(f.trimmed.is_empty() && f.tables.len() == 2 && ctx.schema[0].contains("COL_119"));
        // 빠듯하면 관련 컬럼만 (PK, 이름이 맞는 것, 주석이 맞는 것)
        let mut ctx = Context::default();
        let f = fit_context(&mut ctx, &descs, &mut vec![], &[], 1200, &words);
        assert_eq!(f.tables.len(), 2, "{:?}", f.trimmed);
        let s = &ctx.schema[0];
        assert!(s.contains("COL_000") && s.contains("ORDER_DT") && s.contains("COL_009") && !s.contains("COL_050"), "{s}");
        assert!(s.contains("외 117개"));
        assert!(f.tokens <= 1200);
        // 아주 빠듯하면 뒤쪽 테이블을 뺀다
        let mut ctx = Context::default();
        let f = fit_context(&mut ctx, &descs, &mut vec![], &[], 200, &words);
        assert_eq!(f.tables, vec!["APP.ORDERS"]);
    }
}
