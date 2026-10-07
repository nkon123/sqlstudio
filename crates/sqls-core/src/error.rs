//! 오류 타입. UI 와 MCP 가 같은 형태로 받는다.
//!
//! "결과가 없다" 와 "확인할 수 없다" 를 섞지 않는다. 실패는 항상 `Err` 로 간다.

use serde::Serialize;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// 서버가 돌려준 ORA- 오류. `offset` 은 문장 안의 바이트 위치 (에디터 표시용).
    /// `message` 는 서버 원문이다 (이미 "ORA-00904: ..." 로 시작한다).
    #[error("{}", display_db(*code, message))]
    Db { code: i32, offset: u32, message: String },

    /// 사용자가 취소했다 (ORA-01013)
    #[error("사용자가 실행을 취소했습니다")]
    Cancelled,

    /// 접속이 끊겼다 (ORA-03113/03114/03135, DPI-1080 등). 다시 접속해야 한다.
    #[error("DB 접속이 끊겼습니다: {0}")]
    ConnectionLost(String),

    /// 읽기 전용 세션에서 쓰기 문장을 막았다
    #[error("읽기 전용 접속에서는 실행할 수 없는 문장입니다 ({0})")]
    ReadOnly(String),

    /// 드라이버/클라이언트 문제 (Instant Client 없음 등)
    #[error("Oracle 드라이버 오류: {0}")]
    Driver(String),

    /// 세션 작업 스레드가 이미 끝났다
    #[error("세션이 닫혀 있습니다")]
    SessionClosed,

    #[error("설정 오류: {0}")]
    Config(String),

    #[error("{0}")]
    Invalid(String),
}

pub type Result<T> = std::result::Result<T, Error>;

fn display_db(code: i32, message: &str) -> String {
    if message.starts_with("ORA-") || message.starts_with("PLS-") {
        message.to_string()
    } else {
        format!("ORA-{code:05}: {message}")
    }
}

/// 접속이 끊긴 것으로 보는 ORA 코드
const LOST_CODES: &[i32] = &[3113, 3114, 3135, 28, 1012, 2396, 12570, 12571];

impl From<oracle::Error> for Error {
    fn from(e: oracle::Error) -> Self {
        if let Some(db) = e.db_error() {
            let code = db.code();
            if code == 1013 {
                return Error::Cancelled;
            }
            if LOST_CODES.contains(&code) {
                return Error::ConnectionLost(db.message().to_string());
            }
            return Error::Db {
                code,
                offset: db.offset(),
                message: db.message().to_string(),
            };
        }
        // DPI-1080: 접속이 끊김, DPI-1010: 접속되어 있지 않음
        if matches!(e.dpi_code(), Some(1080) | Some(1010)) {
            return Error::ConnectionLost(e.to_string());
        }
        Error::Driver(e.to_string())
    }
}

/// UI/MCP 로 보내는 직렬화 형태
#[derive(Debug, Serialize)]
pub struct ErrorInfo {
    pub kind: &'static str,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ora_code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub offset: Option<u32>,
}

impl Error {
    pub fn info(&self) -> ErrorInfo {
        let (kind, ora_code, offset) = match self {
            Error::Db { code, offset, .. } => ("db", Some(*code), Some(*offset)),
            Error::Cancelled => ("cancelled", Some(1013), None),
            Error::ConnectionLost(_) => ("connection_lost", None, None),
            Error::ReadOnly(_) => ("read_only", None, None),
            Error::Driver(_) => ("driver", None, None),
            Error::SessionClosed => ("session_closed", None, None),
            Error::Config(_) => ("config", None, None),
            Error::Invalid(_) => ("invalid", None, None),
        };
        ErrorInfo {
            kind,
            message: self.to_string(),
            ora_code,
            offset,
        }
    }

    pub fn is_connection_lost(&self) -> bool {
        matches!(self, Error::ConnectionLost(_) | Error::SessionClosed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ora_prefix_is_not_doubled() {
        let e = Error::Db { code: 904, offset: 7, message: "ORA-00904: \"X\": invalid identifier".into() };
        assert_eq!(e.to_string(), "ORA-00904: \"X\": invalid identifier");
        let e = Error::Db { code: 1, offset: 0, message: "unique constraint".into() };
        assert_eq!(e.to_string(), "ORA-00001: unique constraint");
    }
}
