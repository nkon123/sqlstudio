//! SQLStudio MCP 서버 — AI 에이전트(Claude, Cursor, 로컬 에이전트)에 Oracle 을 읽기 전용으로 연다.
//!
//! 안전 원칙:
//! - **AI 가 SQL 을 직접 실행하지 않는다.** 임의 SQL 을 받아 실행하는 툴은 없다.
//!   툴은 전부 앱이 정해 둔 사전 조회(바인드 변수만 AI 입력)이고, 데이터 행을 돌려주지 않는다.
//!   SQL 은 AI 가 제안하고, 사람이 에디터에서 읽고 실행한다.
//! - `explain_plan` 은 AI 가 쓴 SQL 을 파서에 넘기므로(실행은 안 함) 기본으로 꺼 두고,
//!   `[mcp] allow_explain = true` 로만 켠다.
//! - `config.toml` 의 `[mcp].allowed_connections` 에 적은 프로필만 보인다. 기본은 아무것도 없음.
//! - 프로필 설정과 상관없이 **항상 읽기 전용**이다. 앱 쪽 판정(SELECT/WITH, FOR UPDATE 제외)에
//!   더해 서버에서 `SET TRANSACTION READ ONLY` 로 막는다.
//! - 쿼리마다 시간 상한이 있다. 넘으면 취소하고, 취소가 안 먹히면 세션을 버리고 새로 연다.
//! - 결과 행 수에 상한이 있다 (토큰 = 시간·비용).
//! - stdout 은 프로토콜 전용이다. 로그는 stderr 로만 쓴다.
//! - `analysis_*` / `table_usage` / `subprogram_relations` 툴은 DB 에 붙지 않는다 — PL/SQL 분석 결과(JSON)만 읽는다.
//!   흐름도 툴(`flow_reports` / `flow_report` / `data_lineage` / `subprogram_sql` / `column_writers`)도 같다.
//! - **예외 하나: `run_check`** (데이터 검증 하네스). AI 가 쓴 SELECT 하나를 실제로 돌리고 위반 행 표본을 돌려준다.
//!   `[mcp] allow_checks = true` 일 때만 생기고, 운영(production) 프로필은 늘 거절한다.
//!   SELECT/WITH 한 문장(FOR UPDATE 없음)만 받고, 세션은 그대로 읽기 전용이며, 모든 호출을 기록한다.
//!   읽기 전용 트랜잭션도 자율 트랜잭션(PRAGMA AUTONOMOUS_TRANSACTION) 함수 안의 쓰기는 막지 못한다 — 그래서 운영은 늘 뺀다.

use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use std::path::PathBuf;
use std::time::Duration;

mod analysis;

use rmcp::handler::server::{router::tool::ToolRouter, wrapper::Parameters};
use rmcp::model::{Implementation, ServerCapabilities, ServerConfig};
use rmcp::{schemars, tool, tool_handler, tool_router, ServerHandler};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::Mutex;

use sqls_core::config::{password_env_name, Config, McpConfig, Profile};
use sqls_core::{meta, Error, Session};

/// 접속을 여는 방법 — 테스트에서 바꿔 끼운다.
pub type Connector = Arc<
    dyn Fn(Profile) -> std::pin::Pin<Box<dyn Future<Output = sqls_core::Result<Session>> + Send>>
        + Send
        + Sync,
>;

pub fn oracle_connector() -> Connector {
    Arc::new(|p: Profile| {
        Box::pin(async move {
            let pw = p.stored_password().ok_or_else(|| {
                Error::Config(format!(
                    "'{}' 의 비밀번호가 없습니다. MCP 설정의 env 에 {} 를 넣으세요",
                    p.name,
                    password_env_name(&p.name)
                ))
            })?;
            let mut spec = p.to_spec(pw);
            spec.read_only = true; // MCP 는 무조건 읽기 전용
            spec.dbms_output = false;
            spec.module = "SQLStudio-MCP".into();
            Session::connect(spec).await
        })
    })
}

#[derive(Clone)]
pub struct SqlStudioMcp {
    profiles: Arc<Vec<Profile>>,
    limits: McpConfig,
    sessions: Arc<Mutex<HashMap<String, Session>>>,
    connector: Connector,
    /// PL/SQL 분석 결과 폴더의 부모 (None 이면 설정 파일 옆 analysis/<프로필>)
    analysis_root: Option<PathBuf>,
    analysis_cache: analysis::Cache,
    /// run_check 기록 (JSON 한 줄씩). None 이면 stderr 에만
    check_log: Option<PathBuf>,
    tool_router: ToolRouter<Self>,
}

// ─────────────────────────────────────────────────────────────
// 툴 인자
// ─────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ConnArg {
    /// Connection profile name (see list_connections)
    pub connection: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SqlArgs {
    /// Connection profile name
    pub connection: String,
    /// The SQL statement to analyze (SELECT, INSERT, UPDATE, DELETE, MERGE). It is NOT executed.
    pub sql: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct DescribeArgs {
    /// Connection profile name
    pub connection: String,
    /// Table or view name, optionally schema-qualified (OWNER.NAME). Synonyms are resolved.
    pub name: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ListObjectsArgs {
    /// Connection profile name
    pub connection: String,
    /// Schema (owner). Defaults to the connected user.
    #[serde(default)]
    pub owner: Option<String>,
    /// TABLE, VIEW, PACKAGE, PROCEDURE, FUNCTION, SEQUENCE, SYNONYM, TRIGGER, INDEX ...
    #[serde(default)]
    pub object_type: Option<String>,
    /// LIKE pattern on the object name, case-insensitive, e.g. "ORD%"
    #[serde(default)]
    pub name_like: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct DdlArgs {
    /// Connection profile name
    pub connection: String,
    /// TABLE, VIEW, INDEX, SEQUENCE, PACKAGE, PACKAGE BODY, PROCEDURE, FUNCTION, TRIGGER, TYPE
    pub object_type: String,
    /// Object name, optionally OWNER.NAME
    pub name: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct NameArgs {
    /// Connection profile name
    pub connection: String,
    /// Name, optionally OWNER-qualified: a package/procedure (ORDER_PKG) or a subprogram (ORDER_PKG.CLOSE_ORDER)
    pub name: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct TableArgs {
    /// Connection profile name
    pub connection: String,
    /// Table name, optionally OWNER.TABLE
    pub table: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct FindingsArgs {
    /// Connection profile name
    pub connection: String,
    /// Filter by kind (예외 삼킴, 동적 SQL, 순환 호출, 모델 지적, 구조). Omit for all.
    #[serde(default)]
    pub kind: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ReportArgs {
    /// Connection profile name
    pub connection: String,
    /// Report id (from flow_reports), or its exact name/title
    pub id: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct LineageArgs {
    /// Connection profile name
    pub connection: String,
    /// Table name, optionally OWNER.TABLE
    pub table: String,
    /// "up" = where the table's data comes from (writers and what they read), "down" = where it goes. Default up.
    #[serde(default)]
    pub direction: Option<String>,
    /// Table steps to follow (one subprogram + one table = 1). Default 2, max 10.
    #[serde(default)]
    pub depth: Option<usize>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ColumnArgs {
    /// Connection profile name
    pub connection: String,
    /// Table name, optionally OWNER.TABLE
    pub table: String,
    /// Column name
    pub column: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct CheckArgs {
    /// Connection profile name (never a production profile)
    pub connection: String,
    /// One SELECT (or WITH … SELECT) that returns the rows VIOLATING the rule — 0 rows means the check passes.
    /// No FOR UPDATE, no DML/DDL/PL-SQL, no trailing semicolon needed.
    pub sql: String,
    /// What the check verifies, e.g. "ORDER_LOG.AMOUNT = sum of ORDER_ITEMS for closed orders (ORDER_PKG.CLOSE_ORDER line 120)"
    pub purpose: String,
}

// ─────────────────────────────────────────────────────────────
// 서버
// ─────────────────────────────────────────────────────────────

impl SqlStudioMcp {
    pub fn new(cfg: &Config, connector: Connector) -> Self {
        let allowed: Vec<Profile> = cfg
            .connections
            .iter()
            .filter(|p| {
                cfg.mcp
                    .allowed_connections
                    .iter()
                    .any(|a| a.eq_ignore_ascii_case(&p.name))
            })
            .cloned()
            .collect();
        Self {
            profiles: Arc::new(allowed),
            limits: cfg.mcp.clone(),
            sessions: Arc::new(Mutex::new(HashMap::new())),
            connector,
            analysis_root: None,
            analysis_cache: analysis::Cache::default(),
            check_log: None,
            tool_router: {
                let mut r = Self::tool_router();
                if !cfg.mcp.allow_explain {
                    r.remove_route("explain_plan");
                }
                if !cfg.mcp.allow_checks {
                    r.remove_route("run_check");
                }
                r
            },
        }
    }

    /// 분석 결과 폴더를 바꾼다 (시험용, 또는 결과를 다른 곳에 둔 경우)
    pub fn with_analysis_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.analysis_root = Some(root.into());
        self
    }

    /// run_check 마다 한 줄씩 남길 파일 (보낸 SQL·목적·결과 수·오류)
    pub fn with_check_log(mut self, path: impl Into<PathBuf>) -> Self {
        self.check_log = Some(path.into());
        self
    }

    fn log_check(&self, rec: serde_json::Value) {
        tracing::info!("run_check {rec}");
        let Some(p) = &self.check_log else { return };
        use std::io::Write;
        let r = std::fs::OpenOptions::new().create(true).append(true).open(p).and_then(|mut f| writeln!(f, "{rec}"));
        if let Err(e) = r {
            tracing::warn!("run_check 기록을 쓸 수 없습니다 ({}): {e}", p.display());
        }
    }

    fn analysis_dir(&self, conn: &str) -> Result<PathBuf, String> {
        let p = self.profile(conn)?;
        Ok(match &self.analysis_root {
            Some(r) => r.join(&p.name),
            None => sqls_analyze::default_dir(&p.name),
        })
    }

    fn integrated(&self, conn: &str) -> Result<(PathBuf, std::sync::Arc<sqls_analyze::integrate::Integrated>), String> {
        let dir = self.analysis_dir(conn)?;
        let g = self.analysis_cache.integrated(&dir)?;
        Ok((dir, g))
    }

    fn profile(&self, name: &str) -> Result<Profile, String> {
        self.profiles
            .iter()
            .find(|p| p.name.eq_ignore_ascii_case(name))
            .cloned()
            .ok_or_else(|| {
                let names: Vec<_> = self.profiles.iter().map(|p| p.name.as_str()).collect();
                format!("Unknown connection '{name}'. Available: {}", names.join(", "))
            })
    }

    async fn session(&self, name: &str) -> Result<Session, String> {
        let p = self.profile(name)?;
        let key = p.name.to_uppercase();
        // 접속은 느릴 수 있다 — 잠금을 들고 기다리지만, 같은 프로필에 두 번 붙지 않게 하는 편이 낫다
        let mut map = self.sessions.lock().await;
        if let Some(s) = map.get(&key) {
            if !s.is_abandoned() {
                return Ok(s.clone());
            }
        }
        let s = (self.connector)(p).await.map_err(fmt_err)?;
        map.insert(key, s.clone());
        Ok(s)
    }

    async fn forget(&self, name: &str) {
        self.sessions.lock().await.remove(&name.to_uppercase());
    }

    /// 세션을 얻어 작업을 하고, 시간 상한·끊긴 접속을 처리한다.
    async fn run<T, F, Fut>(&self, conn: &str, f: F) -> Result<T, String>
    where
        F: Fn(Session) -> Fut,
        Fut: Future<Output = sqls_core::Result<T>>,
    {
        let limit = Duration::from_secs(self.limits.call_timeout_secs.max(1));
        for attempt in 0..2 {
            let s = self.session(conn).await?;
            match tokio::time::timeout(limit, f(s.clone())).await {
                Ok(Ok(v)) => return Ok(v),
                // 접속이 끊겼으면 한 번만 새로 붙어서 다시 한다 (읽기 전용이라 재시도해도 안전하다)
                Ok(Err(e)) if e.is_connection_lost() && attempt == 0 => {
                    tracing::warn!("{conn}: 접속이 끊겨 다시 연다 ({e})");
                    s.abandon();
                    self.forget(conn).await;
                }
                Ok(Err(e)) => return Err(fmt_err(e)),
                Err(_) => {
                    let _ = s.cancel();
                    self.forget(conn).await;
                    // 취소가 서버에 닿지 않을 수도 있다 — 잠시 뒤 세션을 버린다
                    tokio::spawn(async move {
                        tokio::time::sleep(Duration::from_secs(5)).await;
                        s.abandon();
                    });
                    return Err(format!(
                        "Query exceeded the {}s time limit and was cancelled. Narrow the query (add WHERE conditions, ROWNUM limits) or check the plan with explain_plan.",
                        limit.as_secs()
                    ));
                }
            }
        }
        Err("connection lost twice".into())
    }
}

fn fmt_err(e: Error) -> String {
    let i = e.info();
    match (i.ora_code, i.offset) {
        (Some(_), Some(off)) if off > 0 => format!("{} (at character offset {off})", i.message),
        _ => i.message,
    }
}

fn to_json(v: impl serde::Serialize) -> String {
    serde_json::to_string(&v).unwrap_or_else(|e| format!("{{\"error\":\"{e}\"}}"))
}

#[tool_router]
impl SqlStudioMcp {
    #[tool(
        description = "List the Oracle connection profiles this server can query. Call this first.",
        annotations(read_only_hint = true)
    )]
    async fn list_connections(&self) -> String {
        let open = self.sessions.lock().await;
        let list: Vec<_> = self
            .profiles
            .iter()
            .map(|p| {
                let s = open.get(&p.name.to_uppercase());
                json!({
                    "name": p.name,
                    "user": p.user.to_uppercase(),
                    "connect_string": p.connect_string,
                    "connected": s.is_some(),
                    "production": p.production,
                    "run_check": self.limits.allow_checks && !p.production,
                    "server_version": s.map(|s| s.info().server_version.clone()),
                })
            })
            .collect();
        to_json(json!({
            "connections": list,
            "read_only": true,
            "executes_sql": self.limits.allow_checks,
            "call_timeout_secs": self.limits.call_timeout_secs,
        }))
    }

    #[tool(
        description = "Show the Oracle execution plan (EXPLAIN PLAN + DBMS_XPLAN.DISPLAY) for a statement. The statement is parsed but NOT executed and no rows are returned. Use this to tune SQL you propose to the user.",
        annotations(read_only_hint = true)
    )]
    async fn explain_plan(&self, Parameters(a): Parameters<SqlArgs>) -> Result<String, String> {
        let sql = a.sql.trim().trim_end_matches(';').trim().to_string();
        let lines = self
            .run(&a.connection, |s| {
                let sql = sql.clone();
                async move { s.explain(&sql).await }
            })
            .await?;
        Ok(lines.join("\n"))
    }

    #[tool(
        description = "Describe a table or view: columns with types, NOT NULL, comments, primary key, indexes, row count statistics.",
        annotations(read_only_hint = true)
    )]
    async fn describe_table(&self, Parameters(a): Parameters<DescribeArgs>) -> Result<String, String> {
        let d = self
            .run(&a.connection, |s| {
                let name = a.name.clone();
                async move { meta::describe(&s, &name).await }
            })
            .await?;
        Ok(meta::schema_brief(&d))
    }

    #[tool(
        description = "List database objects (tables, views, packages, ...) in a schema, optionally filtered by type and a LIKE pattern.",
        annotations(read_only_hint = true)
    )]
    async fn list_objects(&self, Parameters(a): Parameters<ListObjectsArgs>) -> Result<String, String> {
        let limit = 1000;
        let objs = self
            .run(&a.connection, |s| {
                let (o, t, n) = (a.owner.clone(), a.object_type.clone(), a.name_like.clone());
                async move { meta::list_objects(&s, o.as_deref(), t.as_deref(), n.as_deref(), limit).await }
            })
            .await?;
        let truncated = objs.len() >= limit;
        Ok(to_json(json!({
            "objects": objs.iter().map(|o| json!([o.name, o.object_type, o.status])).collect::<Vec<_>>(),
            "format": ["name", "object_type", "status"],
            "truncated": truncated,
        })))
    }

    #[tool(
        description = "Get the DDL (CREATE statement) or PL/SQL source of an object.",
        annotations(read_only_hint = true)
    )]
    async fn get_ddl(&self, Parameters(a): Parameters<DdlArgs>) -> Result<String, String> {
        self.run(&a.connection, |s| {
            let (t, n) = (a.object_type.clone(), a.name.clone());
            async move { meta::get_ddl(&s, &t, &n).await }
        })
        .await
    }

    #[tool(
        description = "Overview of the stored PL/SQL analysis for a connection (no database access): analyzed packages/procedures with summaries, entry points, cycles. Start here for questions about what the PL/SQL code does.",
        annotations(read_only_hint = true)
    )]
    async fn analysis_overview(&self, Parameters(a): Parameters<ConnArg>) -> Result<String, String> {
        let (dir, g) = self.integrated(&a.connection)?;
        Ok(to_json(analysis::overview(&dir, &g)))
    }

    #[tool(
        description = "Analysis of one package/procedure (no database access): purpose, business rules, each subprogram with tables (CRUD) and calls, long SQL/cursor summaries, cursor-to-DML data flows, findings.",
        annotations(read_only_hint = true)
    )]
    async fn analysis_unit(&self, Parameters(a): Parameters<NameArgs>) -> Result<String, String> {
        let (dir, g) = self.integrated(&a.connection)?;
        Ok(to_json(analysis::unit(&dir, &g, &a.name)?))
    }

    #[tool(
        description = "Who reads/inserts/updates/deletes a table, which tables feed it through cursors (data lineage), and which entry points are affected by changing it (no database access).",
        annotations(read_only_hint = true)
    )]
    async fn table_usage(&self, Parameters(a): Parameters<TableArgs>) -> Result<String, String> {
        let (_, g) = self.integrated(&a.connection)?;
        Ok(to_json(analysis::table_usage(&g, &a.table)?))
    }

    #[tool(
        description = "Callers, callees, tables, cursor flows and reachable COMMITs of a PL/SQL subprogram (no database access). Name like ORDER_PKG.CLOSE_ORDER or just CLOSE_ORDER.",
        annotations(read_only_hint = true)
    )]
    async fn subprogram_relations(&self, Parameters(a): Parameters<NameArgs>) -> Result<String, String> {
        let (_, g) = self.integrated(&a.connection)?;
        Ok(to_json(analysis::relations(&g, &a.name)?))
    }

    #[tool(
        description = "Things to review found by the PL/SQL analysis (no database access): swallowed exceptions, dynamic SQL, call cycles, model-reported risks.",
        annotations(read_only_hint = true)
    )]
    async fn analysis_findings(&self, Parameters(a): Parameters<FindingsArgs>) -> Result<String, String> {
        let (_, g) = self.integrated(&a.connection)?;
        Ok(to_json(analysis::findings(&g, a.kind.as_deref())))
    }

    #[tool(
        description = "List flow diagrams (data flow reports) a person saved in SQLStudio for this connection (no database access). They outline a business flow: which programs read/write which tables, with the person's notes. Start data validation here.",
        annotations(read_only_hint = true)
    )]
    async fn flow_reports(&self, Parameters(a): Parameters<ConnArg>) -> Result<String, String> {
        let (dir, _) = self.integrated(&a.connection)?;
        Ok(to_json(analysis::reports(&dir)))
    }

    #[tool(
        description = "One saved flow diagram (no database access): boxes (tables, programs with their table CRUD and line ranges, XML screens), edges (read/write/call/cursor flow) with the person's edge notes, sticky notes, and a Mermaid rendering. Follow it to walk the code in order.",
        annotations(read_only_hint = true)
    )]
    async fn flow_report(&self, Parameters(a): Parameters<ReportArgs>) -> Result<String, String> {
        let (dir, g) = self.integrated(&a.connection)?;
        Ok(to_json(analysis::report(&dir, &g, &a.id)?))
    }

    #[tool(
        description = "Data lineage of a table, step by step (no database access): direction up = which subprograms write it and which tables they read (with proven cursor→DML flows and lines); down = who reads it and what they write. Includes a Mermaid flowchart.",
        annotations(read_only_hint = true)
    )]
    async fn data_lineage(&self, Parameters(a): Parameters<LineageArgs>) -> Result<String, String> {
        let (_, g) = self.integrated(&a.connection)?;
        Ok(to_json(analysis::lineage(&g, &a.table, a.direction.as_deref().unwrap_or("up"), a.depth.unwrap_or(2))?))
    }

    #[tool(
        description = "Every SQL statement of a subprogram or package in source order (no database access): line range, kind, tables read/written, INTO variables, the cursor that feeds each DML, and the SQL text. Use it to follow the code flow statement by statement.",
        annotations(read_only_hint = true)
    )]
    async fn subprogram_sql(&self, Parameters(a): Parameters<NameArgs>) -> Result<String, String> {
        let dir = self.analysis_dir(&a.connection)?;
        let stmts = self.analysis_cache.statements(&dir)?;
        Ok(to_json(analysis::sql_of(&stmts, &a.name)?))
    }

    #[tool(
        description = "Statements that put a value into one column (INSERT column lists, UPDATE SET, MERGE), with the cursor query that supplies the row (no database access). Use it to derive how a column's value should be computed.",
        annotations(read_only_hint = true)
    )]
    async fn column_writers(&self, Parameters(a): Parameters<ColumnArgs>) -> Result<String, String> {
        let dir = self.analysis_dir(&a.connection)?;
        let stmts = self.analysis_cache.statements(&dir)?;
        Ok(to_json(analysis::column_writers(&stmts, &a.table, &a.column)?))
    }

    #[tool(
        description = "Run ONE data-validation SELECT on a non-production connection and return how many rows it found plus a small sample. Write the query so it returns the rows that VIOLATE the rule (0 rows = pass). Read-only session; DML, DDL, PL/SQL, FOR UPDATE and multiple statements are refused; production profiles are refused. Every call is logged.",
        annotations(read_only_hint = true)
    )]
    async fn run_check(&self, Parameters(a): Parameters<CheckArgs>) -> Result<String, String> {
        let p = self.profile(&a.connection)?;
        let sql = a.sql.trim().to_string();
        let refuse = |why: String| {
            self.log_check(json!({"at": sqls_analyze::store::now(), "connection": p.name, "purpose": a.purpose, "sql": sql, "refused": why}));
            Err(why)
        };
        if p.production {
            return refuse(format!("'{}' is a production profile — run_check never runs there. Give the SQL to the user instead.", p.name));
        }
        let stmts = sqls_core::sql::split_script(&sql);
        if stmts.len() != 1 {
            return refuse(format!("Send exactly one statement (got {}).", stmts.len()));
        }
        let text = stmts[0].text.clone();
        if !sqls_core::sql::is_read_only(&text) {
            return refuse(format!("Only SELECT / WITH … SELECT without FOR UPDATE can run (this is {:?}).", stmts[0].kind));
        }
        let max = self.limits.check_max_rows.clamp(1, 500);
        let t0 = std::time::Instant::now();
        let r = self
            .run(&a.connection, |s| {
                let text = text.clone();
                async move { s.query(&text, Vec::new(), max).await }
            })
            .await;
        let ms = t0.elapsed().as_millis() as u64;
        match r {
            Ok(page) => {
                let n = page.rows.len();
                self.log_check(json!({"at": sqls_analyze::store::now(), "connection": p.name, "purpose": a.purpose, "sql": text, "rows": n, "more": page.has_more, "ms": ms}));
                Ok(to_json(json!({
                    "passed": n == 0,
                    "violations": if page.has_more { format!("{n}+") } else { n.to_string() },
                    "columns": page.columns.iter().map(|c| c.name.clone()).collect::<Vec<_>>(),
                    "sample": page.rows,
                    "ms": ms,
                    "note": if page.has_more { format!("Only the first {n} rows are shown. Use COUNT(*) in a separate check for the total.") } else { String::new() },
                })))
            }
            Err(e) => {
                self.log_check(json!({"at": sqls_analyze::store::now(), "connection": p.name, "purpose": a.purpose, "sql": text, "error": e, "ms": ms}));
                Err(e)
            }
        }
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for SqlStudioMcp {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("sqlstudio", env!("CARGO_PKG_VERSION")))
            .with_instructions(
                "Schema information from Oracle databases configured in SQLStudio. \
                 This server never executes your SQL and never returns table data: \
                 learn the schema with list_connections, list_objects, describe_table and get_ddl, \
                 then write SQL and give it to the user, who reviews and runs it in SQLStudio. \
                 Always respect the server version: Oracle 11g lacks FETCH FIRST, IDENTITY, LATERAL and JSON functions. \
                 For questions about existing PL/SQL (what a package does, who writes a table, impact of a change), \
                 use analysis_overview, analysis_unit, table_usage and subprogram_relations — they read a stored analysis, not the database.                  To follow a data flow for validation: flow_reports / flow_report (diagrams a person saved), data_lineage (table up/down),                  subprogram_sql (statements in order) and column_writers (how a column gets its value).                  run_check exists only when the administrator enabled it: one SELECT returning violating rows, never on production.",
            )
    }
}

impl Drop for SqlStudioMcp {
    fn drop(&mut self) {
        // 마지막 사본이 사라질 때 세션을 닫는다
        if Arc::strong_count(&self.sessions) == 1 {
            if let Ok(map) = self.sessions.try_lock() {
                for s in map.values() {
                    s.close();
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
