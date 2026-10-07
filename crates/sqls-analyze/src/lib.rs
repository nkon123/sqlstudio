//! PL/SQL 분석 — 작은 로컬 모델용.
//!
//! ```text
//! 소스 ─ plsql (렉서·구조) ─ chunk (조각) ─┬─ facts (정적 사실: 테이블 CRUD, 호출, 동적 SQL, COMMIT …)
//!                                          └─ llm   (조각별 의미: 요약·단계·규칙·위험)  → 조각마다 JSON
//!                                                                                      ↓
//!                                     integrate (호출 그래프, CRUD 행렬, 영향 범위, 보고서) ← 나중에, 모델 없이도
//! ```
//!
//! 모델은 소스를 읽기만 한다. 이 크레이트에는 DB 에 SQL 을 보내는 경로가 사전 조회(ALL_SOURCE 등) 말고는 없다.

pub mod chunk;
pub mod facts;
pub mod integrate;
pub mod llm;
pub mod plsql;
pub mod run;
pub mod source;
pub mod store;

/// 접속 프로필의 기본 결과 폴더 — 앱과 CLI 가 같은 곳을 쓴다 (설정 파일 옆 `analysis/<프로필>`)
pub fn default_dir(profile: &str) -> std::path::PathBuf {
    let base = sqls_core::config::config_path().parent().map(|p| p.to_path_buf()).unwrap_or_else(|| std::path::PathBuf::from("."));
    base.join("analysis").join(profile.replace(['/', '\\', ':', '*', '?', '"', '<', '>', '|'], "_"))
}

#[cfg(test)]
mod tests;
