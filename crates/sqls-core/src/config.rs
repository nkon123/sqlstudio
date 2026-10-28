//! 접속 프로필 — 앱과 MCP 서버가 같은 파일을 본다.
//!
//! 위치: `%APPDATA%\sqlstudio\config.toml` (Windows), `~/.config/sqlstudio/config.toml`
//! 포터블은 실행 파일 옆 `data\`. 설정 화면에서 다른 폴더를 지정할 수 있다 ([`set_data_dir`]).
//! 환경변수 `SQLSTUDIO_CONFIG` 로 바꿀 수 있다.
//!
//! 비밀번호는 파일에 쓰지 않는다. 순서대로 찾는다:
//! 1. 환경변수 `SQLSTUDIO_PW_<프로필 이름>` (영숫자 외 문자는 `_`, 대문자)
//! 2. OS 자격 증명 저장소 (앱 접속 창에서 "비밀번호 저장" 을 고른 경우) — [`crate::secret`]
//! 3. (앱) 접속 창에서 입력 — 메모리에만 둔다

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
    /// 접속 그룹 — 목록 순서가 화면 순서. 접속의 `group` 에 쓴 그룹은 여기 없어도 읽을 때 채운다
    #[serde(default, rename = "group")]
    pub groups: Vec<Group>,
    #[serde(default)]
    pub mcp: McpConfig,
    #[serde(default, rename = "llm")]
    pub llm_providers: Vec<toml::Value>,
    #[serde(default)]
    pub history: HistoryConfig,
    #[serde(default)]
    pub ai: AiConfig,
}

/// AI 질문 (앱)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiConfig {
    /// 문법 안내 카드를 AI 에게도 고르게 할지: "off" | "auto"(낱말로 약할 때만) | "always"
    #[serde(default = "default_guide_select")]
    pub guide_select: String,
    /// 카드 고르기 질의의 상한(초) — 넘으면 낱말로 고른 것만 쓴다
    #[serde(default = "default_guide_select_timeout")]
    pub guide_select_timeout_secs: u64,
    /// SQL 생성에서 테이블을 모를 때 질문 낱말로 찾아 AI 에게 고르게 할지: "auto" | "off"
    #[serde(default = "default_auto")]
    pub table_pick: String,
    /// 내 쿼리 예시 — 서버 파싱을 통과하고 에디터에 넣은 AI 의 SQL 을 쌓아 비슷한 질문에 예시로 (로컬 모델만)
    #[serde(default = "default_true")]
    pub examples: bool,
}

impl Default for AiConfig {
    fn default() -> Self {
        Self {
            guide_select: default_guide_select(),
            guide_select_timeout_secs: default_guide_select_timeout(),
            table_pick: default_auto(),
            examples: true,
        }
    }
}

fn default_auto() -> String {
    "auto".into()
}

fn default_guide_select() -> String {
    "auto".into()
}
fn default_guide_select_timeout() -> u64 {
    10
}

/// SQL 실행 이력 (앱) — 이 PC 의 설정 폴더 history\ 에 남긴다
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// 남길 달 수 (0 이면 지우지 않음)
    #[serde(default = "default_keep_months")]
    pub keep_months: u32,
}

impl Default for HistoryConfig {
    fn default() -> Self {
        Self { enabled: true, keep_months: default_keep_months() }
    }
}

fn default_true() -> bool {
    true
}
fn default_keep_months() -> u32 {
    12
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OracleConfig {
    /// Instant Client 폴더. 비우면 PATH 에서 찾는다. 상대 경로는 실행 파일 폴더 기준 ([`resolve_path`])
    pub client_lib_dir: Option<PathBuf>,
    /// tnsnames.ora·sqlnet.ora 가 있는 폴더 (TNS_ADMIN 과 같음). 비우면 Oracle Client 기본 위치. 상대 경로는 실행 파일 폴더 기준
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tns_admin: Option<PathBuf>,
}

impl OracleConfig {
    /// 실제로 쓸 Instant Client 폴더 (상대 경로를 풀어서)
    pub fn client_dir(&self) -> Option<PathBuf> {
        self.client_lib_dir.as_deref().map(resolve_path)
    }
    /// 실제로 쓸 TNS 폴더 (상대 경로를 풀어서)
    pub fn tns_dir(&self) -> Option<PathBuf> {
        self.tns_admin.as_deref().map(resolve_path)
    }
}

/// 설정에 적힌 경로 — 상대 경로면 실행 파일 폴더 기준으로 푼다 (포터블 폴더째 옮겨도 그대로 쓰게).
/// `.\instantclient_19_25`, `..\oracle\network\admin` 처럼 쓴다
pub fn resolve_path(p: &std::path::Path) -> PathBuf {
    if p.is_absolute() || p.as_os_str().is_empty() {
        return p.to_path_buf();
    }
    match exe_dir() {
        Some(d) => normalize(&d.join(p)),
        None => p.to_path_buf(),
    }
}

/// `.`·`..` 를 접어 보기 좋은 경로로 (디스크에 묻지 않는다)
fn normalize(p: &std::path::Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// 저장할 경로 — 실행 파일 폴더 안이면 그 기준 상대 경로로 (대소문자·구분자 차이는 무시). 밖이면 그대로
pub fn relative_to_app(p: &std::path::Path) -> PathBuf {
    match exe_dir() {
        Some(d) => relative_in(p, &d),
        None => p.to_path_buf(),
    }
}

fn relative_in(p: &std::path::Path, base: &std::path::Path) -> PathBuf {
    let key = |c: std::path::Component| c.as_os_str().to_string_lossy().to_lowercase();
    let pc: Vec<_> = p.components().collect();
    let bc: Vec<_> = base.components().collect();
    if !p.is_absolute() || pc.len() < bc.len() || !bc.iter().zip(&pc).all(|(a, b)| key(*a) == key(*b)) {
        return p.to_path_buf();
    }
    let rest: PathBuf = pc[bc.len()..].iter().collect();
    if rest.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        rest
    }
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
    /// 운영 DB — 데이터·구조를 바꾸는 문장과 COMMIT 을 실행 전에 항상 확인한다 (서버 쪽에서 막는다)
    #[serde(default)]
    pub production: bool,
    /// 속한 그룹 — `ERP` 또는 `ERP/운영` (두 단계까지). 없으면 그룹 없음
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
}

/// 접속 그룹. `path` 는 `ERP` 또는 `ERP/운영` (두 단계까지)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Group {
    pub path: String,
    /// 이 그룹 접속의 기본 색 (접속에 색이 없을 때)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
}

/// 그룹 경로의 구분자
pub const GROUP_SEP: char = '/';

/// 그룹 경로를 다듬는다 — 칸마다 앞뒤 공백을 지우고 빈 칸은 뺀다. 세 단계 이상이거나 비면 None
pub fn normalize_group(path: &str) -> Option<String> {
    let parts: Vec<&str> = path.split(GROUP_SEP).map(str::trim).filter(|x| !x.is_empty()).collect();
    if parts.is_empty() || parts.len() > 2 {
        return None;
    }
    Some(parts.join("/"))
}

/// 상위 그룹 경로 (`ERP/운영` → `ERP`). 맨 위면 None
pub fn group_parent(path: &str) -> Option<&str> {
    path.rsplit_once(GROUP_SEP).map(|(a, _)| a)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpConfig {
    /// MCP 로 노출할 프로필 이름. 비어 있으면 아무것도 노출하지 않는다 (안전한 기본값).
    #[serde(default)]
    pub allowed_connections: Vec<String>,
    /// 쿼리 하나의 상한(초)
    #[serde(default = "default_mcp_timeout")]
    pub call_timeout_secs: u64,
    /// explain_plan 툴을 켤지. AI 가 쓴 SQL 을 파서에 넘기므로(실행은 하지 않음) 기본은 끈다.
    #[serde(default)]
    pub allow_explain: bool,
}

fn default_mcp_timeout() -> u64 {
    60
}

impl Default for McpConfig {
    fn default() -> Self {
        Self {
            allowed_connections: Vec::new(),
            call_timeout_secs: default_mcp_timeout(),
            allow_explain: false,
        }
    }
}

pub fn config_path() -> PathBuf {
    if let Ok(p) = std::env::var("SQLSTUDIO_CONFIG") {
        return PathBuf::from(p);
    }
    data_dir().join("config.toml")
}

/// 포터블 표시 파일 — 실행 파일 옆에 있으면 설정·이력·분석 결과를 그 옆 `data\` 에 둔다.
/// 파일 안에 `data_dir=<폴더>` 줄이 있으면 그 폴더를 쓴다 (상대 경로는 실행 파일 폴더 기준) — 버전마다 다른 폴더에 풀어도 설정을 같이 쓰게
pub const PORTABLE_MARKER: &str = "portable.txt";
/// 설치판의 데이터 폴더 지정 파일 — `%APPDATA%\sqlstudio\location.txt` 에 `data_dir=<폴더>`
pub const LOCATION_FILE: &str = "location.txt";
const DATA_DIR_KEY: &str = "data_dir";

fn exe_dir() -> Option<PathBuf> {
    std::env::current_exe().ok()?.parent().map(|p| p.to_path_buf())
}

fn is_portable() -> bool {
    exe_dir().is_some_and(|d| d.join(PORTABLE_MARKER).is_file())
}

/// 설치판 기본 폴더 (`%APPDATA%\sqlstudio`)
fn installed_dir() -> PathBuf {
    dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("sqlstudio")
}

/// 데이터 폴더를 적어 두는 파일 — 포터블은 `portable.txt`, 설치판은 `%APPDATA%\sqlstudio\location.txt`
fn location_file() -> PathBuf {
    match exe_dir().filter(|_| is_portable()) {
        Some(d) => d.join(PORTABLE_MARKER),
        None => installed_dir().join(LOCATION_FILE),
    }
}

/// `data_dir=` 를 지정하지 않았을 때의 데이터 폴더 (포터블: 실행 파일 옆 `data`, 설치판: `%APPDATA%\sqlstudio`)
pub fn default_data_dir() -> PathBuf {
    match exe_dir().filter(|_| is_portable()) {
        Some(d) => d.join("data"),
        None => installed_dir(),
    }
}

/// 설정·실행 이력·분석 결과를 두는 폴더 (`SQLSTUDIO_CONFIG` 는 따로 — [`config_path`])
pub fn data_dir() -> PathBuf {
    let f = location_file();
    let base = f.parent().map(|p| p.to_path_buf()).unwrap_or_default();
    std::fs::read_to_string(&f).ok().and_then(|t| data_dir_line(&t).map(|v| base.join(v))).unwrap_or_else(default_data_dir)
}

/// 포터블로 풀어 쓰는 중이면 데이터 폴더 (`data_dir=` 로 지정한 곳, 없으면 실행 파일 옆 `data`). 아니면 None
pub fn portable_data_dir() -> Option<PathBuf> {
    is_portable().then(data_dir)
}

#[cfg(test)]
fn portable_data_dir_in(dir: &std::path::Path) -> Option<PathBuf> {
    let marker = std::fs::read_to_string(dir.join(PORTABLE_MARKER)).ok()?;
    Some(match data_dir_line(&marker) {
        Some(p) => dir.join(p),
        None => dir.join("data"),
    })
}

/// `data_dir=...` 줄의 값 (따옴표는 벗긴다)
fn data_dir_line(marker: &str) -> Option<&str> {
    marker.lines().find_map(|l| {
        let (k, v) = l.trim().trim_start_matches('\u{feff}').split_once('=')?;
        let v = v.trim().trim_matches('"');
        (k.trim().eq_ignore_ascii_case(DATA_DIR_KEY) && !v.is_empty()).then_some(v)
    })
}

/// 데이터 폴더 지정 — `portable.txt`(포터블) 또는 `location.txt`(설치판) 의 `data_dir=` 줄을 바꾼다.
/// None 이면 줄을 지워 기본 폴더로 돌린다. 다시 시작해야 적용된다
pub fn set_data_dir(dir: Option<&std::path::Path>) -> Result<()> {
    let f = location_file();
    let err = |e: std::io::Error| Error::Config(format!("{}: {e}", f.display()));
    let old = match std::fs::read_to_string(&f) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => "SQLStudio 데이터 폴더 위치 — 설정 > 정보 > 데이터 폴더 에서 바꿉니다\n".into(),
        Err(e) => return Err(err(e)),
    };
    let text = with_data_dir(&old, dir);
    // 설치판에서 기본으로 돌리면 지정 파일을 지운다
    if dir.is_none() && !is_portable() {
        return match std::fs::remove_file(&f) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(err(e)),
            _ => Ok(()),
        };
    }
    if let Some(p) = f.parent() {
        std::fs::create_dir_all(p).map_err(err)?;
    }
    std::fs::write(&f, text).map_err(err)
}

fn with_data_dir(marker: &str, dir: Option<&std::path::Path>) -> String {
    let nl = if marker.contains("\r\n") { "\r\n" } else { "\n" };
    let mut lines: Vec<&str> = marker.lines().filter(|l| data_dir_line(l).is_none()).collect();
    while lines.last().is_some_and(|l| l.trim().is_empty()) {
        lines.pop();
    }
    let mut out = lines.join(nl);
    if let Some(d) = dir {
        if !out.is_empty() {
            out.push_str(&format!("{nl}{nl}"));
        }
        out.push_str(&format!("{DATA_DIR_KEY}={}", d.display()));
    }
    out.push_str(nl);
    out
}

/// 실행 파일 옆에 풀어 둔 Instant Client (`instantclient_19_25` 처럼 oci.dll 이 든 폴더). 설정에 경로가 없을 때 쓴다
pub fn bundled_client_dir() -> Option<PathBuf> {
    client_dir_in(&exe_dir()?)
}

fn client_dir_in(dir: &std::path::Path) -> Option<PathBuf> {
    let lib = if cfg!(windows) { "oci.dll" } else { "libclntsh.so" };
    let mut found: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir() && p.file_name().is_some_and(|n| n.to_string_lossy().to_lowercase().starts_with("instantclient")) && p.join(lib).is_file())
        .collect();
    // 여러 개면 이름이 큰 것 (새 버전)
    found.sort();
    found.pop()
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
        let mut cfg = cfg;
        for p in &cfg.connections {
            if let Some(g) = &p.group {
                if normalize_group(g).is_none() {
                    return Err(format!("접속 '{}' 의 그룹 '{g}' — 그룹은 두 단계(예: ERP/운영)까지입니다", p.name));
                }
            }
        }
        for g in &cfg.groups {
            if normalize_group(&g.path).is_none() {
                return Err(format!("그룹 '{}' — 그룹은 두 단계(예: ERP/운영)까지입니다", g.path));
            }
        }
        cfg.tidy_groups();
        Ok(cfg)
    }

    /// 그룹 목록을 맞춘다 — 경로를 다듬고, 접속이 쓰는 그룹과 그 상위 그룹을 채우고, 겹친 것은 지운다
    pub fn tidy_groups(&mut self) {
        for p in &mut self.connections {
            p.group = p.group.as_deref().and_then(normalize_group);
        }
        let mut out: Vec<Group> = Vec::new();
        let add = |out: &mut Vec<Group>, path: &str, color: Option<String>| {
            let mut path = path.to_string();
            if let Some((parent, leaf)) = path.clone().split_once(GROUP_SEP) {
                // 상위 그룹 이름의 대소문자는 먼저 나온 것을 따른다
                match out.iter().find(|g| g.path.eq_ignore_ascii_case(parent)) {
                    Some(g) => path = format!("{}{GROUP_SEP}{leaf}", g.path),
                    None => out.push(Group { path: parent.to_string(), color: None }),
                }
            }
            match out.iter_mut().find(|g| g.path.eq_ignore_ascii_case(&path)) {
                Some(g) => {
                    if g.color.is_none() {
                        g.color = color;
                    }
                }
                None => out.push(Group { path, color }),
            }
        };
        for g in std::mem::take(&mut self.groups) {
            if let Some(path) = normalize_group(&g.path) {
                add(&mut out, &path, g.color);
            }
        }
        for p in &self.connections {
            if let Some(g) = &p.group {
                add(&mut out, g, None);
            }
        }
        // 접속의 그룹 이름 대소문자는 그룹 목록을 따른다
        for p in &mut self.connections {
            if let Some(g) = &p.group {
                if let Some(x) = out.iter().find(|x| x.path.eq_ignore_ascii_case(g)) {
                    p.group = Some(x.path.clone());
                }
            }
        }
        self.groups = out;
    }

    /// 접속의 표시 색 — 접속에 색이 없으면 그룹, 그 위 그룹 순으로
    pub fn color_of(&self, p: &Profile) -> Option<String> {
        if p.color.is_some() {
            return p.color.clone();
        }
        let mut path = p.group.as_deref();
        while let Some(g) = path {
            if let Some(c) = self.groups.iter().find(|x| x.path.eq_ignore_ascii_case(g)).and_then(|x| x.color.clone()) {
                return Some(c);
            }
            path = group_parent(g);
        }
        None
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

    /// 환경변수 → OS 자격 증명 저장소 순으로 찾은 비밀번호 (MCP 서버·분석 CLI 가 쓴다)
    pub fn stored_password(&self) -> Option<String> {
        self.password_from_env().or_else(|| crate::secret::get(&crate::secret::db_account(&self.name)))
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

    #[test]
    fn portable_marker_and_bundled_client() {
        let dir = std::env::temp_dir().join(format!("sqls-portable-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("instantclient_19_25")).unwrap();
        std::fs::create_dir_all(dir.join("instantclient_old")).unwrap(); // oci 라이브러리가 없으면 아니다
        assert_eq!(portable_data_dir_in(&dir), None);
        std::fs::write(dir.join(PORTABLE_MARKER), "").unwrap();
        assert_eq!(portable_data_dir_in(&dir), Some(dir.join("data")));
        std::fs::write(dir.join(PORTABLE_MARKER), "\u{feff}설명\r\n# data_dir=예시\r\ndata_dir = ..\\SQLStudio-data\r\n").unwrap();
        assert_eq!(portable_data_dir_in(&dir), Some(dir.join("..\\SQLStudio-data")));
        assert_eq!(client_dir_in(&dir), None);
        let lib = if cfg!(windows) { "oci.dll" } else { "libclntsh.so" };
        std::fs::write(dir.join("instantclient_19_25").join(lib), "").unwrap();
        assert_eq!(client_dir_in(&dir), Some(dir.join("instantclient_19_25")));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    #[cfg(windows)]
    fn relative_paths() {
        let base = std::path::Path::new(r"C:\Tools\SQLStudio");
        // 안이면 상대로 (대소문자 무시), 밖이면 그대로, 같은 폴더면 "."
        assert_eq!(relative_in(std::path::Path::new(r"c:\tools\sqlstudio\instantclient_19_25"), base), PathBuf::from("instantclient_19_25"));
        assert_eq!(relative_in(std::path::Path::new(r"C:\Tools\SQLStudio\ic\network\admin"), base), PathBuf::from(r"ic\network\admin"));
        assert_eq!(relative_in(std::path::Path::new(r"D:\oracle\admin"), base), PathBuf::from(r"D:\oracle\admin"));
        assert_eq!(relative_in(std::path::Path::new(r"C:\Tools\SQLStudio2\x"), base), PathBuf::from(r"C:\Tools\SQLStudio2\x"));
        assert_eq!(relative_in(base, base), PathBuf::from("."));
        assert_eq!(relative_in(std::path::Path::new("already_relative"), base), PathBuf::from("already_relative"));
        // 상대 경로는 실행 파일 폴더 기준으로 풀린다
        let exe = exe_dir().unwrap();
        assert_eq!(resolve_path(std::path::Path::new(r".\ic")), exe.join("ic"));
        assert_eq!(resolve_path(std::path::Path::new(r"..\oracle\admin")), exe.parent().unwrap().join("oracle").join("admin"));
        assert_eq!(resolve_path(std::path::Path::new(r"D:\abs")), PathBuf::from(r"D:\abs"));
        let o = OracleConfig { client_lib_dir: Some("instantclient_19_25".into()), tns_admin: None };
        assert_eq!(o.client_dir(), Some(exe.join("instantclient_19_25")));
        assert_eq!(o.tns_dir(), None);
    }

    #[test]
    fn data_dir_line_set_and_clear() {
        let base = "SQLStudio 포터블\r\n# data_dir=D:\\예시\r\n\r\n";
        let set = with_data_dir(base, Some(std::path::Path::new("D:\\SQLStudio-data")));
        assert_eq!(set, "SQLStudio 포터블\r\n# data_dir=D:\\예시\r\n\r\ndata_dir=D:\\SQLStudio-data\r\n");
        assert_eq!(data_dir_line(&set), Some("D:\\SQLStudio-data"));
        let again = with_data_dir(&set, Some(std::path::Path::new("E:\\x")));
        assert_eq!(data_dir_line(&again), Some("E:\\x"));
        assert_eq!(again.matches("data_dir=E").count(), 1);
        let clear = with_data_dir(&again, None);
        assert_eq!(data_dir_line(&clear), None);
        assert_eq!(clear, "SQLStudio 포터블\r\n# data_dir=D:\\예시\r\n");
        assert_eq!(data_dir_line("data_dir=\"C:\\a b\""), Some("C:\\a b"));
    }

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
        assert!(!c.mcp.allow_explain);
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
    fn example_file_parses() {
        let c = Config::parse(include_str!("../../../config.example.toml")).unwrap();
        assert_eq!(c.connections.len(), 2);
        assert_eq!(c.llm_providers.len(), 3);
        assert!(!c.mcp.allow_explain);
    }

    #[test]
    fn env_name() {
        assert_eq!(password_env_name("erp-dev 1"), "SQLSTUDIO_PW_ERP_DEV_1");
    }

    #[test]
    fn groups() {
        let c = Config::parse(
            r##"
[[group]]
path = "MES"
color = "#ff0000"

[[connection]]
name = "a"
user = "u"
connect_string = "x"
group = " erp / 운영 "

[[connection]]
name = "b"
user = "u"
connect_string = "x"
group = "mes/dev"
"##,
        )
        .unwrap();
        // 접속이 쓰는 그룹과 상위 그룹이 채워지고, 대소문자는 그룹 목록을 따른다
        let paths: Vec<&str> = c.groups.iter().map(|g| g.path.as_str()).collect();
        assert_eq!(paths, ["MES", "erp", "erp/운영", "MES/dev"]);
        assert_eq!(c.connections[0].group.as_deref(), Some("erp/운영"));
        assert_eq!(c.connections[1].group.as_deref(), Some("MES/dev"));
        // 색은 접속 → 그룹 → 상위 그룹
        assert_eq!(c.color_of(&c.connections[1]).as_deref(), Some("#ff0000"));
        assert_eq!(c.color_of(&c.connections[0]), None);
        assert!(Config::parse("[[group]]\npath = \"a/b/c\"").is_err());
        assert_eq!(normalize_group("a//b"), Some("a/b".into()));
        assert_eq!(group_parent("a/b"), Some("a"));
    }
}
