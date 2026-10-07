//! 접속 프로필 — 앱과 MCP 서버가 같은 파일을 본다.
//!
//! 위치: `%APPDATA%\sqlstudio\config.toml` (Windows), `~/.config/sqlstudio/config.toml`
//! 환경변수 `SQLSTUDIO_CONFIG` 로 바꿀 수 있다.
//!
//! 비밀번호는 파일에 쓰지 않는다. 순서대로 찾는다:
//! 1. 환경변수 `SQLSTUDIO_PW_<프로필 이름>` (영숫자 외 문자는 `_`, 대문자)
//! 2. (앱) 접속 창에서 입력 — 메모리에만 둔다

use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::session::ConnectSpec;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub oracle: OracleConfig,
    #[serde(default, rename = "connection")]
    pub connections: Vec<Profile>,
    #[serde(default)]
    pub mcp: McpConfig,
    #[serde(default, rename = "llm")]
    pub llm_providers: Vec<toml::Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OracleConfig {
    /// Instant Client 폴더. 비우면 PATH 에서 찾는다.
    pub client_lib_dir: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Profile {
    pub name: String,
    pub user: String,
    /// `host:port/service` 또는 TNS 별칭
    pub connect_string: String,
    #[serde(default)]
    pub read_only: bool,
    #[serde(default)]
    pub as_sysdba: bool,
    /// 호출 하나의 상한(초). Oracle Client 18+ 에서만 동작.
    pub call_timeout_secs: Option<u64>,
    /// 화면에서 프로필을 구분하는 색 (운영=빨강 같은 식으로)
    pub color: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpConfig {
    /// MCP 로 노출할 프로필 이름. 비어 있으면 아무것도 노출하지 않는다 (안전한 기본값).
    #[serde(default)]
    pub allowed_connections: Vec<String>,
    /// 툴 결과 최대 행 수
    #[serde(default = "default_mcp_rows")]
    pub max_rows: usize,
    /// 쿼리 하나의 상한(초)
    #[serde(default = "default_mcp_timeout")]
    pub call_timeout_secs: u64,
}

fn default_mcp_rows() -> usize {
    200
}
fn default_mcp_timeout() -> u64 {
    60
}

impl Default for McpConfig {
    fn default() -> Self {
        Self {
            allowed_connections: Vec::new(),
            max_rows: default_mcp_rows(),
            call_timeout_secs: default_mcp_timeout(),
        }
    }
}

pub fn config_path() -> PathBuf {
    if let Ok(p) = std::env::var("SQLSTUDIO_CONFIG") {
        return PathBuf::from(p);
    }
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("sqlstudio")
        .join("config.toml")
}

impl Config {
    /// 파일이 없으면 빈 설정. 형식이 틀리면 오류 — 조용히 무시하면 "왜 안 되지" 로 헤맨다.
    pub fn load() -> Result<Config> {
        let path = config_path();
        match std::fs::read_to_string(&path) {
            Ok(text) => Self::parse(&text)
                .map_err(|e| Error::Config(format!("{}: {e}", path.display()))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
            Err(e) => Err(Error::Config(format!("{}: {e}", path.display()))),
        }
    }

    pub fn parse(text: &str) -> std::result::Result<Config, String> {
        let cfg: Config = toml::from_str(text).map_err(|e| e.to_string())?;
        let mut seen = std::collections::HashSet::new();
        for p in &cfg.connections {
            if !seen.insert(p.name.to_uppercase()) {
                return Err(format!("접속 이름이 중복됩니다: {}", p.name));
            }
        }
        for a in &cfg.mcp.allowed_connections {
            if !cfg.connections.iter().any(|p| p.name.eq_ignore_ascii_case(a)) {
                return Err(format!("mcp.allowed_connections 의 '{a}' 에 해당하는 [[connection]] 이 없습니다"));
            }
        }
        Ok(cfg)
    }

    pub fn save(&self) -> Result<()> {
        let path = config_path();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| Error::Config(e.to_string()))?;
        }
        let text = toml::to_string_pretty(self).map_err(|e| Error::Config(e.to_string()))?;
        // 쓰다가 죽어도 기존 파일이 깨지지 않게: 임시 파일에 쓰고 바꿔치기
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, text).map_err(|e| Error::Config(e.to_string()))?;
        std::fs::rename(&tmp, &path).map_err(|e| Error::Config(e.to_string()))?;
        Ok(())
    }

    pub fn profile(&self, name: &str) -> Option<&Profile> {
        self.connections.iter().find(|p| p.name.eq_ignore_ascii_case(name))
    }
}

/// 비밀번호 환경변수 이름: `SQLSTUDIO_PW_ERP_DEV`
pub fn password_env_name(profile: &str) -> String {
    let mut s = String::from("SQLSTUDIO_PW_");
    for c in profile.chars() {
        s.push(if c.is_ascii_alphanumeric() { c.to_ascii_uppercase() } else { '_' });
    }
    s
}

impl Profile {
    pub fn password_from_env(&self) -> Option<String> {
        std::env::var(password_env_name(&self.name)).ok().filter(|p| !p.is_empty())
    }

    pub fn to_spec(&self, password: String) -> ConnectSpec {
        let mut spec = ConnectSpec::new(&self.user, &password, &self.connect_string);
        spec.read_only = self.read_only;
        spec.as_sysdba = self.as_sysdba;
        spec.call_timeout = self.call_timeout_secs.map(Duration::from_secs);
        spec
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r##"
[oracle]
client_lib_dir = 'C:\oracle\instantclient_19_25'

[[connection]]
name = "ERP-DEV"
user = "erp_read"
connect_string = "dbhost:1521/ORCL"
read_only = true

[[connection]]
name = "ERP-PROD"
user = "erp"
connect_string = "PRODTNS"
color = "#d33"

[mcp]
allowed_connections = ["erp-dev"]
"##;

    #[test]
    fn parse_sample() {
        let c = Config::parse(SAMPLE).unwrap();
        assert_eq!(c.connections.len(), 2);
        assert!(c.profile("erp-dev").unwrap().read_only);
        assert_eq!(c.mcp.max_rows, 200);
        assert_eq!(c.mcp.allowed_connections, vec!["erp-dev"]);
    }

    #[test]
    fn rejects_unknown_mcp_profile() {
        let bad = SAMPLE.replace("[\"erp-dev\"]", "[\"nope\"]");
        assert!(Config::parse(&bad).unwrap_err().contains("nope"));
    }

    #[test]
    fn rejects_duplicate_names() {
        let bad = format!("{SAMPLE}\n[[connection]]\nname=\"erp-dev\"\nuser=\"x\"\nconnect_string=\"y\"\n");
        assert!(Config::parse(&bad).is_err());
    }

    #[test]
    fn env_name() {
        assert_eq!(password_env_name("erp-dev 1"), "SQLSTUDIO_PW_ERP_DEV_1");
    }
}
