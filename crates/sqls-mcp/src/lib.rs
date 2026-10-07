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
            let pw = p.password_from_env().ok_or_else(|| {
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
            tool_router: {
                let mut r = Self::tool_router();
                if !cfg.mcp.allow_explain {
                    r.remove_route("explain_plan");
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
                    "server_version": s.map(|s| s.info().server_version.clone()),
                })
            })
            .collect();
        to_json(json!({
            "connections": list,
            "read_only": true,
            "executes_sql": false,
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
                 use analysis_overview, analysis_unit, table_usage and subprogram_relations — they read a stored analysis, not the database.",
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
