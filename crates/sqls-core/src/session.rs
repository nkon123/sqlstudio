//! Oracle 세션 — 접속 하나에 전용 작업 스레드 하나.
//!
//! 왜 이렇게 하는가:
//! - OCI 호출은 블로킹이다. async 런타임이나 UI 스레드에서 부르면 화면이 멈춘다.
//! - 열린 커서(ResultSet)를 다음 "더 가져오기" 까지 들고 있어야 한다 (Toad 처럼
//!   처음 N 행만 보여 주고 스크롤하면 이어서 가져온다). 커서는 그 스레드가 소유한다.
//! - 작업 중 패닉이 나도 그 세션만 죽는다. 앱 전체는 살아 있다.
//!
//! 취소는 작업 큐를 거치지 않는다. 다른 스레드에서 `break_execution` 을 바로 부른다
//! (OCI 가 허용하는 유일한 동시 호출이다).

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use oracle::sql_type::{OracleType, ToSql};
use oracle::{Connection, Connector, Privilege, ResultSet, Row};
use serde::Serialize;
use tokio::sync::{oneshot, watch};

use crate::error::{Error, Result};
use crate::sql::{self, StmtKind};

// ─────────────────────────────────────────────────────────────
// 공개 타입
// ─────────────────────────────────────────────────────────────

/// 접속 정보. 비밀번호는 메모리에만 있고 직렬화되지 않는다.
#[derive(Clone)]
pub struct ConnectSpec {
    pub user: String,
    pub password: String,
    /// `host:port/service` 또는 TNS 별칭
    pub connect_string: String,
    pub as_sysdba: bool,
    /// 참이면 SELECT 만 허용하고 서버 쪽에서도 `SET TRANSACTION READ ONLY` 로 막는다.
    pub read_only: bool,
    /// 호출 하나의 상한 (Oracle Client 18 이상에서만 동작. 11.2 클라이언트는 무시)
    pub call_timeout: Option<Duration>,
    /// 참이면 DBMS_OUTPUT 을 켜고 PL/SQL 실행 뒤 읽어 온다
    pub dbms_output: bool,
    /// V$SESSION.MODULE 에 보일 이름 (DBA 가 누구 세션인지 알 수 있게)
    pub module: String,
}

impl std::fmt::Debug for ConnectSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConnectSpec")
            .field("user", &self.user)
            .field("password", &"***")
            .field("connect_string", &self.connect_string)
            .field("as_sysdba", &self.as_sysdba)
            .field("read_only", &self.read_only)
            .finish()
    }
}

impl ConnectSpec {
    pub fn new(user: &str, password: &str, connect_string: &str) -> Self {
        Self {
            user: user.into(),
            password: password.into(),
            connect_string: connect_string.into(),
            as_sysdba: false,
            read_only: false,
            call_timeout: None,
            dbms_output: true,
            module: "SQLStudio".into(),
        }
    }
}

/// 실행 옵션
#[derive(Debug, Clone)]
pub struct ExecOptions {
    /// 첫 페이지 행 수. 나머지는 [`Session::fetch_more`] 로 가져온다.
    pub first_page: usize,
    /// OCI 배열 fetch 크기 — 왕복 횟수를 줄인다. 행이 넓으면 낮춘다.
    pub fetch_array_size: u32,
    /// 바인드 값 (이름, 값). 이름은 `:` 없이. `None` 은 NULL.
    pub binds: Vec<(String, Option<String>)>,
    /// 셀 하나의 최대 글자 수 (CLOB 이 화면과 메모리를 잡아먹지 않게)
    pub max_cell_chars: usize,
}

impl Default for ExecOptions {
    fn default() -> Self {
        Self {
            first_page: 500,
            fetch_array_size: 500,
            binds: Vec::new(),
            max_cell_chars: 4000,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Column {
    pub name: String,
    pub type_name: String,
    pub nullable: bool,
}

/// 결과 한 페이지. 셀은 문자열로 보낸다 (NUMBER 정밀도를 잃지 않고, 화면에 바로 쓴다).
#[derive(Debug, Clone, Serialize)]
pub struct RowPage {
    pub columns: Vec<Column>,
    pub rows: Vec<Vec<Option<String>>>,
    /// 서버에 더 남아 있는지 (커서가 열려 있음)
    pub has_more: bool,
    /// 지금까지 가져온 누적 행 수
    pub fetched_total: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ExecOutcome {
    Rows(RowPage),
    Affected { rows: u64 },
    Done,
    /// SQL*Plus 명령 — 서버로 보내지 않았다
    Skipped { reason: String },
}

#[derive(Debug, Clone, Serialize)]
pub struct ExecResult {
    pub kind: StmtKind,
    pub outcome: ExecOutcome,
    pub elapsed_ms: u64,
    /// DBMS_OUTPUT 으로 나온 줄
    pub output: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionInfo {
    pub user: String,
    pub connect_string: String,
    pub server_version: String,
    pub read_only: bool,
}

// ─────────────────────────────────────────────────────────────
// 클라이언트 초기화 (프로세스당 한 번)
// ─────────────────────────────────────────────────────────────

static EXPLAIN_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

static CLIENT_INIT: OnceLock<std::result::Result<(), String>> = OnceLock::new();

/// Oracle Client 라이브러리 위치를 지정한다. 첫 접속 전에 한 번만 부른다.
/// 지정하지 않으면 PATH(Windows) / LD_LIBRARY_PATH 에서 찾는다.
///
/// 11g(11.2) 서버에는 Instant Client 가 반드시 있어야 한다. 19c 클라이언트를 권장한다
/// (11.2 서버에 붙고, call_timeout 도 지원한다).
pub fn init_client(lib_dir: Option<PathBuf>) -> Result<()> {
    let r = CLIENT_INIT.get_or_init(|| {
        let mut p = oracle::InitParams::new();
        if let Some(dir) = &lib_dir {
            p.oracle_client_lib_dir(dir.as_os_str())
                .map_err(|e| e.to_string())?;
        }
        p.init().map(|_| ()).map_err(|e| e.to_string())
    });
    r.clone().map_err(Error::Driver)
}

// ─────────────────────────────────────────────────────────────
// 세션 핸들 (async 쪽에서 쓰는 것)
// ─────────────────────────────────────────────────────────────

type Reply<T> = oneshot::Sender<Result<T>>;

enum Cmd {
    Exec { sql: String, opts: ExecOptions, reply: Reply<ExecResult> },
    FetchMore { rows: usize, reply: Reply<RowPage> },
    CloseCursor,
    /// 사전 조회 — 열린 커서를 건드리지 않는다
    Query { sql: String, binds: Vec<(String, Option<String>)>, max_rows: usize, reply: Reply<RowPage> },
    /// 실행계획 — EXPLAIN PLAN + DBMS_XPLAN.DISPLAY (11g 호환)
    Explain { sql: String, reply: Reply<Vec<String>> },
    Commit { reply: Reply<()> },
    Rollback { reply: Reply<()> },
    Ping { reply: Reply<()> },
    /// PL/SQL 호출 — OUT 바인드 값을 문자열로 돌려준다 (디버거·내부 도구용)
    Call { sql: String, ins: Vec<(String, Option<String>)>, outs: Vec<String>, reply: Reply<Vec<Option<String>>> },
    Shutdown,
}

/// 열린 Oracle 세션. `Clone` 해서 여러 곳에서 써도 된다 (작업은 순서대로 처리된다).
#[derive(Clone)]
pub struct Session {
    tx: mpsc::Sender<Cmd>,
    conn: Arc<Connection>,
    busy: Arc<AtomicBool>,
    info: Arc<SessionInfo>,
    /// 참이 되면 기다리던 호출이 모두 즉시 풀린다 ([`Session::abandon`])
    abandoned: Arc<watch::Sender<bool>>,
}

impl Session {
    /// 접속한다. 접속 자체도 블로킹이므로 별도 스레드에서 하고 기다린다.
    pub async fn connect(spec: ConnectSpec) -> Result<Session> {
        let (tx, rx) = oneshot::channel();
        thread::Builder::new()
            .name(format!("ora-{}", spec.user))
            .spawn(move || {
                let r = open(&spec);
                match r {
                    Ok((conn, info)) => {
                        let conn = Arc::new(conn);
                        let (ctx, crx) = mpsc::channel();
                        let busy = Arc::new(AtomicBool::new(false));
                        let (abandoned, abandoned_rx) = watch::channel(false);
                        let session = Session {
                            tx: ctx,
                            conn: conn.clone(),
                            busy: busy.clone(),
                            info: Arc::new(info),
                            abandoned: Arc::new(abandoned),
                        };
                        if tx.send(Ok(session)).is_err() {
                            let _ = conn.close();
                            return;
                        }
                        Worker::new(conn, spec, busy, abandoned_rx).run(crx);
                    }
                    Err(e) => {
                        let _ = tx.send(Err(e));
                    }
                }
            })
            .map_err(|e| Error::Driver(format!("세션 스레드를 만들 수 없습니다: {e}")))?;
        rx.await.map_err(|_| Error::SessionClosed)?
    }

    pub fn info(&self) -> &SessionInfo {
        &self.info
    }

    /// 지금 서버 호출 중인지 (UI 의 "실행 중" 표시)
    pub fn is_busy(&self) -> bool {
        self.busy.load(Ordering::Relaxed)
    }

    /// 실행 중인 호출을 취소한다. 작업 큐를 거치지 않고 바로 간다.
    ///
    /// OCI 취소는 TCP 긴급 데이터(OOB)로 간다. 방화벽·NAT·Docker 포트 매핑이 이것을
    /// 버리면 취소가 서버에 닿지 않는다. 그럴 때는 몇 초 기다렸다가 [`Session::abandon`].
    pub fn cancel(&self) -> Result<()> {
        if self.is_abandoned() {
            return Ok(());
        }
        self.conn.break_execution().map_err(Error::from)
    }

    /// 세션을 버린다. 기다리던 호출은 즉시 [`Error::SessionClosed`] 로 풀리고,
    /// 작업 스레드는 붙잡힌 서버 호출이 끝나는 대로 접속을 닫고 사라진다.
    /// 화면은 곧바로 새 세션을 열어 계속 일할 수 있다.
    ///
    /// 커밋하지 않은 변경은 롤백된다. 서버 쪽 SQL 은 끝날 때까지 돌 수 있다
    /// (DBA 권한이 있으면 ALTER SYSTEM KILL SESSION 으로 멈춘다).
    pub fn abandon(&self) {
        let _ = self.abandoned.send(true);
        let _ = self.conn.break_execution();
        let _ = self.tx.send(Cmd::Shutdown);
    }

    pub fn is_abandoned(&self) -> bool {
        *self.abandoned.borrow()
    }

    async fn call<T>(&self, make: impl FnOnce(Reply<T>) -> Cmd) -> Result<T> {
        let mut gone = self.abandoned.subscribe();
        if *gone.borrow() {
            return Err(Error::SessionClosed);
        }
        let (tx, rx) = oneshot::channel();
        self.tx.send(make(tx)).map_err(|_| Error::SessionClosed)?;
        tokio::select! {
            r = rx => r.map_err(|_| Error::SessionClosed)?,
            _ = gone.wait_for(|v| *v) => Err(Error::SessionClosed),
        }
    }

    /// 문장 하나를 실행한다. 쿼리면 첫 페이지를 돌려주고 커서를 열어 둔다.
    pub async fn execute(&self, sql: &str, opts: ExecOptions) -> Result<ExecResult> {
        let sql = sql.to_string();
        self.call(|reply| Cmd::Exec { sql, opts, reply }).await
    }

    /// 열린 커서에서 다음 행들을 가져온다.
    pub async fn fetch_more(&self, rows: usize) -> Result<RowPage> {
        self.call(|reply| Cmd::FetchMore { rows, reply }).await
    }

    pub fn close_cursor(&self) {
        let _ = self.tx.send(Cmd::CloseCursor);
    }

    /// 내부용 조회 (사전 조회 등). 열린 결과 커서를 건드리지 않는다.
    pub async fn query(
        &self,
        sql: &str,
        binds: Vec<(String, Option<String>)>,
        max_rows: usize,
    ) -> Result<RowPage> {
        let sql = sql.to_string();
        self.call(|reply| Cmd::Query { sql, binds, max_rows, reply }).await
    }

    /// 실행계획 (DBMS_XPLAN 텍스트 줄)
    pub async fn explain(&self, sql: &str) -> Result<Vec<String>> {
        let sql = sql.to_string();
        self.call(|reply| Cmd::Explain { sql, reply }).await
    }

    pub async fn commit(&self) -> Result<()> {
        self.call(|reply| Cmd::Commit { reply }).await
    }

    pub async fn rollback(&self) -> Result<()> {
        self.call(|reply| Cmd::Rollback { reply }).await
    }

    pub async fn ping(&self) -> Result<()> {
        self.call(|reply| Cmd::Ping { reply }).await
    }

    /// PL/SQL 블록을 실행하고 OUT 바인드(`outs` 이름 순서) 값을 문자열로 받는다.
    /// 숫자·날짜는 블록 안에서 문자열로 바뀐다. 읽기 전용 세션에서는 쓸 수 없다.
    pub async fn call_plsql(
        &self,
        sql: &str,
        ins: Vec<(String, Option<String>)>,
        outs: &[&str],
    ) -> Result<Vec<Option<String>>> {
        let sql = sql.to_string();
        let outs = outs.iter().map(|s| s.to_string()).collect();
        self.call(|reply| Cmd::Call { sql, ins, outs, reply }).await
    }

    /// 세션을 닫는다. 커밋하지 않은 변경은 롤백된다 (OCI 기본 동작).
    pub fn close(&self) {
        let _ = self.tx.send(Cmd::Shutdown);
    }
}

fn open(spec: &ConnectSpec) -> Result<(Connection, SessionInfo)> {
    init_client(None)?;
    let mut c = Connector::new(&spec.user, &spec.password, &spec.connect_string);
    if spec.as_sysdba {
        c.privilege(Privilege::Sysdba);
    }
    // 같은 SQL 을 반복 실행할 때 파싱을 줄인다
    c.stmt_cache_size(40);
    let conn = c.connect()?;
    // 실패해도 접속 자체는 쓸 수 있다 — 무시하되 기록한다
    if let Err(e) = conn.set_module(&spec.module) {
        tracing::debug!("set_module 실패: {e}");
    }
    if spec.call_timeout.is_some() {
        if let Err(e) = conn.set_call_timeout(spec.call_timeout) {
            tracing::warn!("call_timeout 미지원 (Oracle Client 18 미만?): {e}");
        }
    }
    if spec.dbms_output {
        conn.execute("BEGIN DBMS_OUTPUT.ENABLE(NULL); END;", &[])?;
    }
    let server_version = conn
        .server_version()
        .map(|(v, banner)| format!("{v} ({})", banner.lines().next().unwrap_or("")))
        .unwrap_or_default();
    Ok((
        conn,
        SessionInfo {
            user: spec.user.to_uppercase(),
            connect_string: spec.connect_string.clone(),
            server_version,
            read_only: spec.read_only,
        },
    ))
}

// ─────────────────────────────────────────────────────────────
// 작업 스레드
// ─────────────────────────────────────────────────────────────

struct OpenCursor {
    rs: ResultSet<'static, Row>,
    columns: Vec<Column>,
    fetched: u64,
    max_cell_chars: usize,
}

struct Worker {
    conn: Arc<Connection>,
    spec: ConnectSpec,
    busy: Arc<AtomicBool>,
    abandoned: watch::Receiver<bool>,
    cursor: Option<OpenCursor>,
}

impl Worker {
    fn new(
        conn: Arc<Connection>,
        spec: ConnectSpec,
        busy: Arc<AtomicBool>,
        abandoned: watch::Receiver<bool>,
    ) -> Self {
        Self { conn, spec, busy, abandoned, cursor: None }
    }

    fn run(mut self, rx: mpsc::Receiver<Cmd>) {
        while let Ok(cmd) = rx.recv() {
            if matches!(cmd, Cmd::Shutdown) || *self.abandoned.borrow() {
                break;
            }
            // 패닉이 나면 응답 채널이 떨어져 호출부는 SessionClosed 를 받는다.
            // 상태를 믿을 수 없으므로 세션을 끝낸다.
            self.busy.store(true, Ordering::Relaxed);
            let r = catch_unwind(AssertUnwindSafe(|| self.handle(cmd)));
            self.busy.store(false, Ordering::Relaxed);
            if r.is_err() {
                tracing::error!("세션 작업 중 패닉 — 세션을 닫는다");
                break;
            }
        }
        self.cursor = None;
        let _ = self.conn.close();
    }

    fn handle(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::Exec { sql, opts, reply } => {
                let _ = reply.send(self.exec(&sql, &opts));
            }
            Cmd::FetchMore { rows, reply } => {
                let _ = reply.send(self.fetch_more(rows));
            }
            Cmd::CloseCursor => self.cursor = None,
            Cmd::Query { sql, binds, max_rows, reply } => {
                let _ = reply.send(self.side_query(&sql, &binds, max_rows));
            }
            Cmd::Explain { sql, reply } => {
                let _ = reply.send(self.explain(&sql));
            }
            Cmd::Commit { reply } => {
                let _ = reply.send(self.conn.commit().map_err(Error::from));
            }
            Cmd::Rollback { reply } => {
                let _ = reply.send(self.conn.rollback().map_err(Error::from));
            }
            Cmd::Ping { reply } => {
                let _ = reply.send(self.conn.ping().map_err(Error::from));
            }
            Cmd::Call { sql, ins, outs, reply } => {
                let _ = reply.send(self.call_plsql(&sql, &ins, &outs));
            }
            Cmd::Shutdown => {}
        }
    }

    /// 읽기 전용 세션: 트랜잭션을 끝내고 READ ONLY 로 새로 연다.
    /// 앱 쪽 판정(sql::is_read_only)을 빠져나간 문장도 서버가 ORA-01456 으로 막는다.
    fn begin_read_only(&self) -> Result<()> {
        self.conn.rollback()?;
        self.conn.execute("SET TRANSACTION READ ONLY", &[])?;
        Ok(())
    }

    fn exec(&mut self, sql_text: &str, opts: &ExecOptions) -> Result<ExecResult> {
        // 새 실행은 이전 커서를 닫는다 (열린 커서가 쌓이면 ORA-01000)
        self.cursor = None;
        let started = Instant::now();
        let kind = sql::classify(sql_text);

        if kind == StmtKind::SqlPlus {
            return Ok(ExecResult {
                kind,
                outcome: ExecOutcome::Skipped {
                    reason: "SQL*Plus 명령은 서버로 보내지 않습니다".into(),
                },
                elapsed_ms: 0,
                output: Vec::new(),
            });
        }
        if self.spec.read_only {
            if !sql::is_read_only(sql_text) {
                return Err(Error::ReadOnly(kind.label().to_string()));
            }
            self.begin_read_only()?;
        }

        let mut stmt = self
            .conn
            .statement(sql_text)
            .fetch_array_size(opts.fetch_array_size.max(1))
            .prefetch_rows(opts.fetch_array_size.max(1))
            .build()?;
        bind_all(&mut stmt, &opts.binds)?;

        let outcome = if stmt.is_query() {
            let mut rs = stmt.into_result_set::<Row>(&[])?;
            let columns = columns_of(rs.column_info());
            let (rows, has_more) = take_rows(&mut rs, opts.first_page, opts.max_cell_chars)?;
            let fetched = rows.len() as u64;
            let page = RowPage {
                columns: columns.clone(),
                rows,
                has_more,
                fetched_total: fetched,
            };
            if has_more {
                self.cursor = Some(OpenCursor {
                    rs,
                    columns,
                    fetched,
                    max_cell_chars: opts.max_cell_chars,
                });
            }
            ExecOutcome::Rows(page)
        } else {
            stmt.execute(&[])?;
            if stmt.is_dml() {
                ExecOutcome::Affected { rows: stmt.row_count()? }
            } else {
                ExecOutcome::Done
            }
        };

        let output = if self.spec.dbms_output && matches!(kind, StmtKind::Plsql | StmtKind::Other) {
            self.read_dbms_output().unwrap_or_default()
        } else {
            Vec::new()
        };

        Ok(ExecResult {
            kind,
            outcome,
            elapsed_ms: started.elapsed().as_millis() as u64,
            output,
        })
    }

    fn fetch_more(&mut self, n: usize) -> Result<RowPage> {
        let cur = self
            .cursor
            .as_mut()
            .ok_or_else(|| Error::Invalid("열린 결과가 없습니다".into()))?;
        let (rows, has_more) = take_rows(&mut cur.rs, n, cur.max_cell_chars)?;
        cur.fetched += rows.len() as u64;
        let page = RowPage {
            columns: cur.columns.clone(),
            rows,
            has_more,
            fetched_total: cur.fetched,
        };
        if !has_more {
            self.cursor = None;
        }
        Ok(page)
    }

    fn side_query(
        &mut self,
        sql_text: &str,
        binds: &[(String, Option<String>)],
        max_rows: usize,
    ) -> Result<RowPage> {
        if !sql::is_read_only(sql_text) {
            return Err(Error::ReadOnly("내부 조회는 SELECT 만 허용".into()));
        }
        let arr = max_rows.clamp(1, 1000) as u32;
        let mut stmt = self.conn.statement(sql_text).fetch_array_size(arr).prefetch_rows(arr).build()?;
        bind_all(&mut stmt, binds)?;
        let mut rs = stmt.into_result_set::<Row>(&[])?;
        let columns = columns_of(rs.column_info());
        let (rows, has_more) = take_rows(&mut rs, max_rows, 1_000_000)?;
        let fetched_total = rows.len() as u64;
        Ok(RowPage { columns, rows, has_more, fetched_total })
    }

    fn explain(&mut self, sql_text: &str) -> Result<Vec<String>> {
        let kind = sql::classify(sql_text);
        if !matches!(kind, StmtKind::Query | StmtKind::Dml) {
            return Err(Error::Invalid(format!("실행계획을 볼 수 없는 문장입니다 ({kind:?})")));
        }
        // 결과 커서를 열어 둔 채로 트랜잭션을 끝내면 안 된다 — 보던 결과는 닫는다
        self.cursor = None;
        let id = format!(
            "SQLS{}_{}",
            std::process::id(),
            EXPLAIN_SEQ.fetch_add(1, Ordering::Relaxed)
        );
        // PLAN_TABLE 은 11g 에서 전역 임시 테이블이다. 끝나면 롤백해 지운다.
        // 읽기 전용 트랜잭션 안에서는 EXPLAIN PLAN 이 쓰기를 못 하므로 먼저 끝낸다.
        self.conn.rollback()?;
        let r = (|| -> Result<Vec<String>> {
            self.conn
                .execute(&format!("EXPLAIN PLAN SET STATEMENT_ID = '{id}' FOR {sql_text}"), &[])?;
            let rows = self.conn.query_as::<Option<String>>(
                "SELECT plan_table_output FROM TABLE(DBMS_XPLAN.DISPLAY('PLAN_TABLE', :1, 'TYPICAL'))",
                &[&id],
            )?;
            let mut out = Vec::new();
            for r in rows {
                out.push(r?.unwrap_or_default());
            }
            Ok(out)
        })();
        let _ = self.conn.rollback();
        r
    }

    fn call_plsql(&mut self, sql_text: &str, ins: &[(String, Option<String>)], outs: &[String]) -> Result<Vec<Option<String>>> {
        if self.spec.read_only {
            return Err(Error::ReadOnly("PL/SQL 호출".into()));
        }
        let mut stmt = self.conn.statement(sql_text).build()?;
        for (n, v) in ins {
            let v: &dyn ToSql = v;
            stmt.bind(n.as_str(), v)?;
        }
        // 4000 바이트를 넘는 VARCHAR2 바인드는 LONG 으로 바뀌어 숫자를 넣으면 PLS-00382 가 난다.
        // 긴 값이 필요한 OUT 은 이름 앞에 `l_` 를 붙인다 (LONG 으로 받는다 — 블록에서 문자열만 넣을 것).
        for n in outs {
            let ty = if n.starts_with("l_") { OracleType::Long } else { OracleType::Varchar2(4000) };
            stmt.bind(n.as_str(), &ty)?;
        }
        stmt.execute(&[])?;
        outs.iter().map(|n| stmt.bind_value::<_, Option<String>>(n.as_str()).map_err(Error::from)).collect()
    }

    /// DBMS_OUTPUT 버퍼를 비운다. 무한 루프를 막으려고 상한을 둔다.
    fn read_dbms_output(&self) -> Result<Vec<String>> {
        let mut stmt = self
            .conn
            .statement("BEGIN DBMS_OUTPUT.GET_LINE(:line, :status); END;")
            .build()?;
        stmt.bind("line", &OracleType::Varchar2(32767))?;
        stmt.bind("status", &OracleType::Int64)?;
        let mut lines = Vec::new();
        for _ in 0..100_000 {
            stmt.execute(&[])?;
            let status: i64 = stmt.bind_value("status")?;
            if status != 0 {
                break;
            }
            let line: Option<String> = stmt.bind_value("line")?;
            lines.push(line.unwrap_or_default());
        }
        Ok(lines)
    }
}

fn bind_all(stmt: &mut oracle::Statement, binds: &[(String, Option<String>)]) -> Result<()> {
    if binds.is_empty() {
        return Ok(());
    }
    let names: Vec<String> = stmt.bind_names().iter().map(|s| s.to_uppercase()).collect();
    for (name, value) in binds {
        let key = name.trim_start_matches(':').to_uppercase();
        if !names.contains(&key) {
            // 문장에 없는 바인드를 넘기면 ORA-01036 이 난다 — 미리 거른다
            continue;
        }
        let v: &dyn ToSql = value;
        stmt.bind(key.as_str(), v)?;
    }
    Ok(())
}

fn columns_of(info: &[oracle::ColumnInfo]) -> Vec<Column> {
    info.iter()
        .map(|c| Column {
            name: c.name().to_string(),
            type_name: c.oracle_type().to_string(),
            nullable: c.nullable(),
        })
        .collect()
}

/// 최대 `n` 행을 가져온다. (행들, 더 남았는지)
fn take_rows(
    rs: &mut ResultSet<'static, Row>,
    n: usize,
    max_cell_chars: usize,
) -> Result<(Vec<Vec<Option<String>>>, bool)> {
    let mut rows = Vec::with_capacity(n.min(10_000));
    while rows.len() < n {
        match rs.next() {
            Some(row) => rows.push(row_to_cells(&row?, max_cell_chars)),
            None => return Ok((rows, false)),
        }
    }
    // 정확히 n 행에서 끝났을 수도 있다. 한 행을 미리 보지 않고 "더 있을 수 있음" 으로 둔다.
    // (미리 보기를 하면 그 행을 버리거나 따로 들고 있어야 한다. 다음 fetch 가 빈 페이지를
    //  돌려주면 그때 닫는다.)
    Ok((rows, true))
}

fn row_to_cells(row: &Row, max_chars: usize) -> Vec<Option<String>> {
    row.sql_values()
        .iter()
        .map(|v| match v.get::<Option<String>>() {
            Ok(Some(s)) => Some(truncate_chars(s, max_chars)),
            Ok(None) => None,
            // BLOB, RAW 등 문자열로 바꿀 수 없는 형식 — 형식 이름만 보여 준다
            Err(_) => Some(format!("({})", v.oracle_type().map(|t| t.to_string()).unwrap_or_default())),
        })
        .collect()
}

fn truncate_chars(s: String, max: usize) -> String {
    if s.len() <= max {
        return s;
    }
    match s.char_indices().nth(max) {
        Some((idx, _)) => {
            let mut t = s[..idx].to_string();
            t.push('…');
            t
        }
        None => s,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_is_char_safe() {
        assert_eq!(truncate_chars("가나다라".into(), 2), "가나…");
        assert_eq!(truncate_chars("abc".into(), 5), "abc");
    }

    #[test]
    fn spec_debug_hides_password() {
        let s = ConnectSpec::new("scott", "tiger", "db:1521/orcl");
        assert!(!format!("{s:?}").contains("tiger"));
    }

    /// 실제 DB 가 있을 때만: SQLS_TEST_DSN=user/pass@host:1521/svc cargo test -- --ignored
    #[tokio::test]
    #[ignore]
    async fn live_roundtrip() {
        let dsn = std::env::var("SQLS_TEST_DSN").expect("SQLS_TEST_DSN");
        let (cred, cs) = dsn.split_once('@').unwrap();
        let (u, p) = cred.split_once('/').unwrap();
        let mut spec = ConnectSpec::new(u, p, cs);
        spec.read_only = true;
        let s = Session::connect(spec).await.unwrap();
        let r = s
            .execute(
                "select level n from dual connect by level <= 1200",
                ExecOptions { first_page: 500, ..Default::default() },
            )
            .await
            .unwrap();
        match r.outcome {
            ExecOutcome::Rows(p) => assert_eq!(p.rows.len(), 500),
            _ => panic!(),
        }
        let p2 = s.fetch_more(1000).await.unwrap();
        assert_eq!(p2.fetched_total, 1200);
        assert!(!p2.has_more);
        assert!(matches!(
            s.execute("delete from dual", Default::default()).await,
            Err(Error::ReadOnly(_))
        ));
        let plan = s.explain("select * from dual").await.unwrap();
        assert!(plan.iter().any(|l| l.contains("DUAL")));
    }
}
