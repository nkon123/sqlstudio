//! 접속 간 데이터 복사 — 원본 탭 접속의 테이블에서 읽어 대상 탭 접속의 테이블에 넣는다 (QA → DEV 등).
//!
//! - 원본은 따로 읽는 커서로 읽는다 — 원본 탭의 결과 그리드는 그대로 둔다. SELECT 만 열 수 있다.
//! - 대상에는 1000행씩 array DML. 다음 묶음을 읽는 것과 지금 묶음을 넣는 것을 함께 돌린다 (두 DB 가 따로 일한다).
//! - 커밋하지 않는다 — 대상 탭에 '커밋 전' 이 켜지고, 거기서 커밋·롤백한다.
//! - 운영 DB 가 대상이면 확인을 한 번 더 받는다. 읽기 전용 프로필은 대상이 될 수 없다. 멈춤은 대상 탭의 '중지' 신호.

use std::sync::atomic::Ordering;
use std::time::Instant;

use serde::{Deserialize, Serialize};
use sqls_core::copydata::{self as cd, ColPlan, Mode};
use sqls_core::meta;
use tauri::ipc::Channel;
use tauri::State;

use crate::state::{AppState, ErrView};

type R<T> = Result<T, ErrView>;

const CHUNK: usize = 1000;
const MAX_ERRORS: usize = 200;

#[derive(Debug, Clone, Deserialize)]
pub struct CopyArgs {
    src_id: u64,
    src_table: String,
    dst_id: u64,
    dst_table: String,
    /// 원본(지우고 넣기면 대상에도)에 붙이는 조건 — WHERE 없이
    #[serde(default)]
    cond: Option<String>,
    mode: Mode,
    /// MERGE 키 (비우면 대상의 기본 키)
    #[serde(default)]
    keys: Vec<String>,
    /// 옮길 열 (비우면 옮길 수 있는 열 전부)
    #[serde(default)]
    columns: Vec<String>,
    #[serde(default)]
    skip_errors: bool,
    /// 이만큼 넣을 때마다 대상에서 커밋 (0·없음이면 커밋하지 않는다) — 큰 복사가 중간에 끊겨도 앞부분이 남고,
    /// 대상의 UNDO 가 모자라 실패하지 않게
    #[serde(default)]
    commit_every: Option<u64>,
    #[serde(default)]
    confirmed: bool,
}

#[derive(Debug, Serialize)]
pub struct CopyPlan {
    src: String,
    dst: String,
    columns: Vec<ColPlan>,
    /// 대상의 기본 키
    primary_key: Vec<String>,
    keys: Vec<String>,
    src_rows: u64,
    /// 지우고 넣기면 대상에서 지울 행 수
    dst_rows: Option<u64>,
    select_sql: String,
    write_sql: String,
    delete_sql: Option<String>,
    warnings: Vec<String>,
    production: bool,
}

struct Prepared {
    plan: CopyPlan,
}

async fn count(s: &sqls_core::Session, sql: &str) -> R<u64> {
    let p = s.query(sql, vec![], 1).await?;
    Ok(p.rows.first().and_then(|r| r.first().cloned().flatten()).and_then(|v| v.parse().ok()).unwrap_or(0))
}

async fn prepare(st: &AppState, a: &CopyArgs) -> R<Prepared> {
    let (src, _, _) = st.session(a.src_id)?;
    let (dst, _, _) = st.session(a.dst_id)?;
    if dst.info().read_only {
        return Err(ErrView::msg("read_only", "대상 접속이 읽기 전용 프로필입니다 — 다른 접속을 대상으로 고르세요"));
    }
    let sd = meta::describe(&src, a.src_table.trim()).await?;
    let dd = meta::describe(&dst, a.dst_table.trim()).await?;
    if dd.object_type != "TABLE" {
        return Err(ErrView::msg("invalid", format!("대상 {}.{} 은 테이블이 아닙니다 ({})", dd.owner, dd.name, dd.object_type)));
    }
    let same_db = a.src_id == a.dst_id || (src.info().connect_string == dst.info().connect_string);
    if same_db && sd.owner == dd.owner && sd.name == dd.name {
        return Err(ErrView::msg("invalid", "원본과 대상이 같은 테이블입니다"));
    }
    let columns = cd::plan_columns(&sd.columns, &dd.columns);
    let cols = cd::copied(&columns, &a.columns);
    if cols.is_empty() {
        return Err(ErrView::msg("invalid", "옮길 열이 없습니다 — 두 테이블에 이름이 같은 열이 있어야 합니다"));
    }
    let cond = a.cond.as_deref();
    let keys: Vec<String> = if a.keys.is_empty() { dd.primary_key.clone() } else { a.keys.iter().map(|k| k.to_uppercase()).collect() };
    let mut warnings = Vec::new();
    let write_sql = match a.mode {
        Mode::Merge => {
            if keys.is_empty() {
                return Err(ErrView::msg("invalid", "MERGE 할 키가 없습니다 — 대상에 기본 키가 없으면 키 열을 고르세요"));
            }
            if let Some(k) = keys.iter().find(|k| !cols.iter().any(|c| &c.name == *k)) {
                return Err(ErrView::msg("invalid", format!("키 열 {k} 을 옮기지 않습니다 — 옮길 열에 넣으세요")));
            }
            cd::merge_sql(&dd.owner, &dd.name, &cols, &keys)
        }
        _ => cd::insert_sql(&dd.owner, &dd.name, &cols),
    };
    // 열마다의 알림은 열 표에 — 여기에는 넣을 때 실패할 것만
    for c in columns.iter().filter(|c| c.status == "dst_only" && c.note.is_some()) {
        warnings.push(format!("{}: {}", c.name, c.note.as_deref().unwrap_or("")));
    }
    if a.mode == Mode::Replace && cond.map(str::trim).unwrap_or("").is_empty() {
        warnings.push("조건이 없어 대상 테이블의 행을 모두 지우고 넣습니다".into());
    }
    let select_sql = cd::select_sql(&sd.owner, &sd.name, &cols, cond);
    // 조건이 틀렸으면 여기서 알게 된다 (건수를 세면서)
    let src_rows = count(&src, &cd::count_sql(&sd.owner, &sd.name, cond)).await?;
    let delete_sql = (a.mode == Mode::Replace).then(|| cd::delete_sql(&dd.owner, &dd.name, cond));
    let dst_rows = match a.mode {
        Mode::Replace => Some(count(&dst, &cd::count_sql(&dd.owner, &dd.name, cond)).await?),
        _ => None,
    };
    let plan = CopyPlan {
        src: format!("{}.{}", sd.owner, sd.name),
        dst: format!("{}.{}", dd.owner, dd.name),
        columns: columns.clone(),
        primary_key: dd.primary_key.clone(),
        keys: if a.mode == Mode::Merge { keys } else { vec![] },
        src_rows,
        dst_rows,
        select_sql,
        write_sql,
        delete_sql,
        warnings,
        production: crate::db::is_production(st, a.dst_id),
    };
    Ok(Prepared { plan })
}

/// 미리 보기 — 열 맞춤, 건수, 실행할 SQL. 아무것도 바꾸지 않는다
#[tauri::command]
pub async fn copy_plan(st: State<'_, AppState>, args: CopyArgs) -> R<CopyPlan> {
    Ok(prepare(&st, &args).await?.plan)
}

#[derive(Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CopyEvent {
    Progress { read: u64, written: u64, failed: u64, total: u64 },
}

#[derive(Serialize)]
pub struct CopySummary {
    read: u64,
    written: u64,
    failed: u64,
    deleted: u64,
    stopped: bool,
    errors: Vec<(u64, String)>,
    txn_pending: bool,
    /// 중간 커밋으로 확정된 행 수 (커밋 간격을 정했을 때)
    committed: u64,
    elapsed_ms: u64,
}

/// 복사 — 미리 보기와 같은 계획을 다시 세워 (화면이 보낸 SQL 을 믿지 않는다) 실행한다
#[tauri::command]
pub async fn copy_run(st: State<'_, AppState>, args: CopyArgs, on_event: Channel<CopyEvent>) -> R<CopySummary> {
    let p = prepare(&st, &args).await?;
    if p.plan.production && !args.confirmed {
        return Err(ErrView::msg("confirm_required", format!("대상이 운영 DB 입니다 — {} 에 데이터를 넣습니다.", p.plan.dst)));
    }
    let (src, _, _) = st.session(args.src_id)?;
    let (dst, txn, stop) = st.session(args.dst_id)?;
    stop.store(false, Ordering::Relaxed);
    let started = Instant::now();
    let total = p.plan.src_rows;
    let every = args.commit_every.unwrap_or(0);
    tracing::info!(
        "데이터 복사 시작: {} → {} ({}행, {:?}{})",
        p.plan.src, p.plan.dst, total, args.mode,
        if every > 0 { format!(", {every}행마다 커밋") } else { String::new() }
    );
    let mut deleted = 0u64;
    if let Some(del) = &p.plan.delete_sql {
        let r = dst.execute(del, sqls_core::session::ExecOptions::default()).await?;
        if let sqls_core::session::ExecOutcome::Affected { rows } = r.outcome {
            deleted = rows;
        }
        txn.store(true, Ordering::Relaxed);
    }
    src.open_side(&p.plan.select_sql, vec![]).await?;
    let (mut read, mut written, mut failed, mut stopped) = (0u64, 0u64, 0u64, false);
    // 마지막 커밋 때까지 넣은 행
    let mut committed = 0u64;
    let mut errors: Vec<(u64, String)> = Vec::new();
    let mut next = src.fetch_side(CHUNK).await;
    loop {
        let (rows, more) = match next {
            Ok(x) => x,
            Err(e) => {
                let ev: ErrView = e.into();
                errors.push((read + 1, format!("원본을 읽다가 실패: {}", ev.message)));
                stopped = true;
                break;
            }
        };
        if rows.is_empty() {
            break;
        }
        if stop.load(Ordering::Relaxed) {
            stopped = true;
            break;
        }
        let base = read;
        let n = rows.len() as u64;
        read += n;
        // 지금 묶음을 넣는 동안 다음 묶음을 읽는다
        let write = dst.batch(&p.plan.write_sql, rows, args.skip_errors);
        let (w, fetched) = if more {
            let (w, f) = tokio::join!(write, src.fetch_side(CHUNK));
            (w, Some(f))
        } else {
            (write.await, None)
        };
        match w {
            Ok(r) => {
                written += r.inserted;
                failed += r.errors.len() as u64;
                for (off, msg) in r.errors {
                    if errors.len() < MAX_ERRORS {
                        errors.push((base + off as u64 + 1, msg));
                    }
                }
            }
            Err(e) => {
                // 멈춤 모드: 이 묶음은 통째로 실패 (앞 묶음은 들어가 있다 — 롤백하면 된다)
                let ev: ErrView = e.into();
                errors.push((base + 1, format!("{}행부터의 묶음({n}행)에서 실패: {}", base + 1, ev.message)));
                failed += n;
                stopped = true;
                break;
            }
        }
        if written > 0 {
            txn.store(true, Ordering::Relaxed);
        }
        if every > 0 && written - committed >= every {
            if let Err(e) = dst.commit().await {
                let ev: ErrView = e.into();
                errors.push((read, format!("{written}행까지 넣고 커밋하다가 실패: {}", ev.message)));
                stopped = true;
                break;
            }
            committed = written;
            txn.store(false, Ordering::Relaxed);
        }
        let _ = on_event.send(CopyEvent::Progress { read, written, failed, total: total.max(read) });
        match fetched {
            Some(f) => next = f,
            None => break,
        }
    }
    src.close_side();
    let elapsed_ms = started.elapsed().as_millis() as u64;
    // 큰 복사가 중간에 멈추면 무엇 때문인지 나중에 로그에서 본다
    if stopped || failed > 0 {
        tracing::warn!(
            "데이터 복사 {}: 읽음 {read} · 넣음 {written} · 실패 {failed} · 커밋 {committed} · {}초 — {}",
            if stopped { "멈춤" } else { "끝(실패 있음)" }, elapsed_ms / 1000,
            errors.iter().take(3).map(|(n, m)| format!("{n}행: {m}")).collect::<Vec<_>>().join(" / ")
        );
    } else {
        tracing::info!("데이터 복사 끝: 넣음 {written} · 커밋 {committed} · {}초", elapsed_ms / 1000);
    }
    Ok(CopySummary { read, written, failed, deleted, stopped, errors, txn_pending: txn.load(Ordering::Relaxed), committed, elapsed_ms })
}
