//! SQL 파일 읽기/쓰기와 MCP 설정 조각.
//!
//! 사내 SQL 파일은 CP949(EUC-KR) 인 경우가 많다. UTF-8 로 못 읽으면 CP949 로 읽고,
//! 저장할 때는 원래 인코딩으로 되돌린다 — 저장하다 한글이 깨지면 안 된다.

use serde::Serialize;
use sqls_core::config::{config_path, password_env_name};
use tauri::State;

use crate::state::{AppState, ErrView};

type R<T> = Result<T, ErrView>;

#[derive(Serialize)]
pub struct SqlFile {
    text: String,
    /// "utf-8" | "utf-8-bom" | "cp949"
    encoding: &'static str,
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
    // 임시 파일에 쓰고 바꿔치기 — 저장 중에 죽어도 원본이 남는다
    let tmp = format!("{path}.sqlstudio-tmp");
    std::fs::write(&tmp, bytes).map_err(|e| ErrView::msg("io", format!("{tmp}: {e}")))?;
    std::fs::rename(&tmp, &path).map_err(|e| ErrView::msg("io", format!("{path}: {e}")))?;
    Ok(())
}

/// Claude Desktop / Claude Code / Cursor 에 붙여 넣을 MCP 설정
#[tauri::command]
pub fn mcp_snippet(st: State<'_, AppState>) -> String {
    let exe = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join(if cfg!(windows) { "sqlstudio-mcp.exe" } else { "sqlstudio-mcp" })))
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "sqlstudio-mcp".into());
    let cfg = st.cfg.read().unwrap();
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
