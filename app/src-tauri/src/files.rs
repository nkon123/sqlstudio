//! SQL 파일 읽기/쓰기와 MCP 설정 조각.
//!
//! 사내 SQL 파일은 CP949(EUC-KR) 인 경우가 많다. UTF-8 로 못 읽으면 CP949 로 읽고,
//! 저장할 때는 원래 인코딩으로 되돌린다 — 저장하다 한글이 깨지면 안 된다.

use sqls_core::sync::RwLockExt;
use serde::Serialize;
use sqls_core::config::{config_path, password_env_name};
use tauri::State;

use crate::state::{AppState, ErrView};

type R<T> = Result<T, ErrView>;

#[derive(Serialize)]
pub struct SqlFile {
    pub(crate) text: String,
    /// "utf-8" | "utf-8-bom" | "cp949"
    pub(crate) encoding: &'static str,
}

pub fn decode(bytes: &[u8]) -> SqlFile {
    if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return SqlFile { text: String::from_utf8_lossy(rest).into_owned(), encoding: "utf-8-bom" };
    }
    match std::str::from_utf8(bytes) {
        Ok(s) => SqlFile { text: s.to_string(), encoding: "utf-8" },
        Err(_) => {
            // encoding_rs 의 EUC_KR 은 WHATWG 정의대로 windows-949(CP949) 다
            let (t, _, _) = encoding_rs::EUC_KR.decode(bytes);
            SqlFile { text: t.into_owned(), encoding: "cp949" }
        }
    }
}

pub fn encode(text: &str, encoding: &str) -> R<Vec<u8>> {
    match encoding {
        "cp949" => {
            let (b, _, had_errors) = encoding_rs::EUC_KR.encode(text);
            if had_errors {
                return Err(ErrView::msg(
                    "invalid",
                    "CP949 로 표현할 수 없는 글자가 있습니다. UTF-8 로 저장하세요.",
                ));
            }
            Ok(b.into_owned())
        }
        "utf-8-bom" => {
            let mut v = vec![0xEF, 0xBB, 0xBF];
            v.extend_from_slice(text.as_bytes());
            Ok(v)
        }
        _ => Ok(text.as_bytes().to_vec()),
    }
}

#[tauri::command]
pub fn read_sql_file(path: String) -> R<SqlFile> {
    let bytes = std::fs::read(&path).map_err(|e| ErrView::msg("io", format!("{path}: {e}")))?;
    Ok(decode(&bytes))
}

#[tauri::command]
pub fn write_sql_file(path: String, text: String, encoding: String) -> R<()> {
    let bytes = encode(&text, &encoding)?;
    // 임시 파일에 쓰고 바꿔치기 — 저장 중에 죽어도 원본이 남는다 (tabs.json 도 여기로)
    let tmp = sqls_core::config::temp_path(std::path::Path::new(&path));
    std::fs::write(&tmp, bytes).map_err(|e| ErrView::msg("io", format!("{}: {e}", tmp.display())))?;
    std::fs::rename(&tmp, &path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        ErrView::msg("io", format!("{path}: {e}"))
    })?;
    Ok(())
}

/// 탭 자동 저장 (tabs.json + tabs/<key>.sql). 문서 내용은 탭마다 따로 — 바뀐 탭만 다시 쓴다
/// (큰 소스를 여러 개 열어 두면 몇 초마다 통째로 쓰는 것이 무겁다)
fn tab_dir() -> std::path::PathBuf {
    config_path().parent().map(|p| p.to_path_buf()).unwrap_or_default()
}

fn tab_key_ok(k: &str) -> bool {
    !k.is_empty() && k.len() <= 64 && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn write_atomic(path: &std::path::Path, bytes: &[u8]) -> R<()> {
    let tmp = sqls_core::config::temp_path(path);
    std::fs::write(&tmp, bytes).map_err(|e| ErrView::msg("io", format!("{}: {e}", tmp.display())))?;
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        ErrView::msg("io", format!("{}: {e}", path.display()))
    })
}

/// `index` 는 tabs.json 본문(탭마다 "key"), `texts` 는 바뀐 탭의 내용. 목록에 없는 탭 파일은 지운다
#[tauri::command]
pub fn save_tab_session(index: String, texts: std::collections::HashMap<String, String>) -> R<()> {
    let dir = tab_dir();
    let sub = dir.join("tabs");
    std::fs::create_dir_all(&sub).map_err(|e| ErrView::msg("io", format!("{}: {e}", sub.display())))?;
    for (k, t) in &texts {
        if !tab_key_ok(k) {
            return Err(ErrView::msg("invalid", format!("탭 키가 올바르지 않습니다: {k}")));
        }
        write_atomic(&sub.join(format!("{k}.sql")), t.as_bytes())?;
    }
    // 내용을 다 쓴 뒤 목록 — 중간에 죽어도 목록이 없는 파일을 가리키지 않게
    write_atomic(&dir.join("tabs.json"), index.as_bytes())?;
    let keep: std::collections::HashSet<String> = tab_keys(&index).into_iter().collect();
    if let Ok(rd) = std::fs::read_dir(&sub) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if let Some(k) = name.strip_suffix(".sql") {
                if !keep.contains(k) {
                    let _ = std::fs::remove_file(e.path());
                }
            }
        }
    }
    Ok(())
}

fn tab_keys(index: &str) -> Vec<String> {
    let v: serde_json::Value = serde_json::from_str(index).unwrap_or_default();
    v.get("tabs")
        .and_then(|t| t.as_array())
        .map(|a| a.iter().filter_map(|t| t.get("key")?.as_str().map(str::to_string)).filter(|k| tab_key_ok(k)).collect())
        .unwrap_or_default()
}

#[derive(Serialize)]
pub struct TabSession {
    index: String,
    texts: std::collections::HashMap<String, String>,
}

/// 저장해 둔 탭 — 없으면 None. 예전 형식(내용이 tabs.json 안에)은 texts 가 비어 있다
#[tauri::command]
pub fn load_tab_session() -> Option<TabSession> {
    let dir = tab_dir();
    let index = std::fs::read_to_string(dir.join("tabs.json")).ok()?;
    let texts = tab_keys(&index)
        .into_iter()
        .filter_map(|k| std::fs::read_to_string(dir.join("tabs").join(format!("{k}.sql"))).ok().map(|t| (k, t)))
        .collect();
    Some(TabSession { index, texts })
}

/// 이미지 같은 바이너리 저장. 본문은 바이트 그대로 받고 (JSON 으로 바꾸면 몇 배로 커진다),
/// 경로는 "path" 헤더에 URI 인코딩해서 받는다 (헤더는 ASCII 만 된다).
#[tauri::command]
pub fn write_binary_file(request: tauri::ipc::Request<'_>) -> R<()> {
    let tauri::ipc::InvokeBody::Raw(bytes) = request.body() else {
        return Err(ErrView::msg("invalid", "바이트 본문이 아닙니다"));
    };
    let path = request
        .headers()
        .get("path")
        .and_then(|v| v.to_str().ok())
        .and_then(percent_decode)
        .ok_or_else(|| ErrView::msg("invalid", "저장 경로가 없습니다"))?;
    let tmp = format!("{path}.sqlstudio-tmp");
    std::fs::write(&tmp, bytes).map_err(|e| ErrView::msg("io", format!("{tmp}: {e}")))?;
    std::fs::rename(&tmp, &path).map_err(|e| ErrView::msg("io", format!("{path}: {e}")))?;
    Ok(())
}

/// encodeURIComponent 의 반대
fn percent_decode(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let hex = std::str::from_utf8(b.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok().filter(|p| !p.is_empty())
}

/// Claude Desktop / Claude Code / Cursor 에 붙여 넣을 MCP 설정
#[tauri::command]
pub fn mcp_snippet(st: State<'_, AppState>) -> String {
    let exe = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join(if cfg!(windows) { "sqlstudio-mcp.exe" } else { "sqlstudio-mcp" })))
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "sqlstudio-mcp".into());
    let cfg = st.cfg.rd();
    let mut env = serde_json::Map::new();
    for n in &cfg.mcp.allowed_connections {
        env.insert(password_env_name(n), serde_json::Value::String("<비밀번호>".into()));
    }
    let v = serde_json::json!({
        "mcpServers": {
            "sqlstudio": {
                "command": exe,
                "args": ["--config", config_path().display().to_string()],
                "env": env,
            }
        }
    });
    serde_json::to_string_pretty(&v).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_decode_korean_path() {
        assert_eq!(percent_decode("C%3A%5C%EB%B3%B4%EA%B3%A0%5Cdfd.png").as_deref(), Some(r"C:\보고\dfd.png"));
        assert_eq!(percent_decode("%E"), None);
        assert_eq!(percent_decode(""), None);
    }

    #[test]
    fn cp949_roundtrip() {
        let (bytes, _, _) = encoding_rs::EUC_KR.encode("-- 주문 조회\nselect * from 주문;");
        let f = decode(&bytes);
        assert_eq!(f.encoding, "cp949");
        assert!(f.text.contains("주문 조회"));
        assert_eq!(encode(&f.text, f.encoding).unwrap(), bytes.into_owned());
    }

    #[test]
    fn utf8_and_bom() {
        assert_eq!(decode("가".as_bytes()).encoding, "utf-8");
        let mut b = vec![0xEF, 0xBB, 0xBF];
        b.extend_from_slice("가".as_bytes());
        let f = decode(&b);
        assert_eq!((f.text.as_str(), f.encoding), ("가", "utf-8-bom"));
        assert_eq!(encode("가", "utf-8-bom").unwrap(), b);
    }

    #[test]
    fn cp949_rejects_unrepresentable() {
        assert!(encode("이모지 😀", "cp949").is_err());
    }
}

#[cfg(test)]
mod tab_session_tests {
    use super::*;

    #[test]
    fn keys_from_index() {
        let idx = r#"{"v":2,"tabs":[{"key":"mw3-ab_1"},{"key":"../x"},{"text":"옛 형식"},{"key":"zz"}]}"#;
        assert_eq!(tab_keys(idx), ["mw3-ab_1", "zz"], "경로를 벗어나는 키는 버린다");
        assert!(tab_keys("깨진 글").is_empty());
        assert!(!tab_key_ok("") && !tab_key_ok("a/b") && !tab_key_ok("a.sql"));
    }
}
