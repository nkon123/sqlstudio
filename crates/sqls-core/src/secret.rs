//! 비밀번호·API 키를 OS 자격 증명 저장소에 둔다 — 설정 파일에는 쓰지 않는다.
//!
//! Windows: 자격 증명 관리자(일반 자격 증명, 대상 이름 "SQLStudio:db:<프로필>"), macOS: 키체인,
//! Linux: 커널 키링(로그아웃·재부팅하면 사라진다 — 개발용).
//! `SQLSTUDIO_NO_KEYRING=1` 이면 쓰지 않는다 (공용 PC, 시험).

const SERVICE: &str = "SQLStudio";

pub fn db_account(profile: &str) -> String {
    format!("db:{}", profile.to_uppercase())
}

pub fn llm_account(provider: &str) -> String {
    format!("llm:{provider}")
}

fn disabled() -> bool {
    std::env::var("SQLSTUDIO_NO_KEYRING").is_ok_and(|v| !v.is_empty() && v != "0")
}

fn entry(account: &str) -> Option<keyring::Entry> {
    if disabled() {
        return None;
    }
    keyring::Entry::new(SERVICE, account).ok()
}

/// 저장된 값. 없거나 저장소를 쓸 수 없으면 None.
pub fn get(account: &str) -> Option<String> {
    entry(account)?.get_password().ok().filter(|s| !s.is_empty())
}

/// 저장. 실패하면 사람이 읽을 이유를 돌려준다.
pub fn set(account: &str, secret: &str) -> Result<(), String> {
    let e = entry(account).ok_or_else(|| "자격 증명 저장소를 쓸 수 없습니다".to_string())?;
    match e.set_password(secret) {
        // Linux 커널 키링은 처음 만드는 항목에 NoEntry 를 돌려주고, 항목을 새로 만들어 다시 하면 된다
        Err(keyring::Error::NoEntry) => match entry(account) {
            Some(e2) => e2.set_password(secret),
            None => Err(keyring::Error::NoEntry),
        },
        r => r,
    }
    .map_err(|e| format!("자격 증명 저장 실패: {e}"))
}

/// 지운다 (없어도 성공)
pub fn delete(account: &str) {
    if let Some(e) = entry(account) {
        let _ = e.delete_credential();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 저장소를 쓸 수 없는 환경(CI 컨테이너 등)에서는 건너뛴다
    #[test]
    fn roundtrip() {
        let acc = format!("test:{}", std::process::id());
        if let Err(e) = set(&acc, "s3cret!") {
            // Windows 에서는 반드시 되어야 한다 (CI windows 잡)
            assert!(!cfg!(windows), "Windows 자격 증명 관리자에 저장하지 못했습니다: {e}");
            eprintln!("자격 증명 저장소를 쓸 수 없어 건너뜁니다: {e}");
            return;
        }
        assert_eq!(get(&acc).as_deref(), Some("s3cret!"));
        set(&acc, "changed").unwrap();
        assert_eq!(get(&acc).as_deref(), Some("changed"));
        delete(&acc);
        assert_eq!(get(&acc), None);
        delete(&acc); // 없어도 된다
    }
}
