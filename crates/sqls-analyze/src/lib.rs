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

pub mod bundle;
pub mod chunk;
pub mod deps;
pub mod eval;
pub mod facts;
pub mod flow;
pub mod integrate;
pub mod llm;
pub mod plsql;
pub mod proc;
pub mod run;
pub mod source;
pub mod store;
pub mod xmlquery;

/// 접속 프로필의 기본 결과 폴더 — 앱과 CLI 가 같은 곳을 쓴다 (설정 파일 옆 `analysis/<프로필>`)
pub fn default_dir(profile: &str) -> std::path::PathBuf {
    // 이전과 같은 규칙 (바꾸면 있던 결과 폴더를 못 찾는다)
    analysis_root().join(profile.replace(['/', '\\', ':', '*', '?', '"', '<', '>', '|'], "_"))
}

/// 모든 결과 폴더가 있는 곳 (설정 파일 옆 `analysis`)
pub fn analysis_root() -> std::path::PathBuf {
    let base = sqls_core::config::config_path().parent().map(|p| p.to_path_buf()).unwrap_or_else(|| std::path::PathBuf::from("."));
    base.join("analysis")
}

/// 가져온 결과의 폴더 이름 (경로 문자·앞뒤 점·공백 정리)
pub fn safe_set_name(name: &str) -> String {
    let s: String = name.replace(['/', '\\', ':', '*', '?', '"', '<', '>', '|'], "_");
    let s = s.trim().trim_matches('.').to_string();
    if s.is_empty() { "_".into() } else { s }
}

fn has_results(dir: &std::path::Path) -> bool {
    dir.join("integrated").is_dir() || std::fs::read_dir(dir.join("units")).map(|mut d| d.next().is_some()).unwrap_or(false)
}

/// 접속 이름을 바꾸면 그 이름의 결과 폴더가 없어 '분석한 게 없다'로 보인다 — 옛 이름으로 남은 결과를 이어 붙인다.
/// 이 접속의 결과가 없고, 어느 접속의 것도 아닌(가져온 것이 아닌) 결과 폴더가 **딱 하나**일 때만 (여럿이면 어느 것인지 몰라 건드리지 않는다).
/// 이어 붙였으면 옛 폴더 이름
pub fn adopt_renamed(profile: &str, profiles: &[String]) -> Option<String> {
    adopt_renamed_in(&analysis_root(), profile, profiles)
}

pub fn adopt_renamed_in(root: &std::path::Path, profile: &str, profiles: &[String]) -> Option<String> {
    let target = root.join(profile.replace(['/', '\\', ':', '*', '?', '"', '<', '>', '|'], "_"));
    if has_results(&target) {
        return None;
    }
    let owned: Vec<std::ffi::OsString> = profiles
        .iter()
        .map(|p| std::ffi::OsString::from(p.replace(['/', '\\', ':', '*', '?', '"', '<', '>', '|'], "_")))
        .collect();
    let orphans: Vec<std::path::PathBuf> = std::fs::read_dir(root)
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_dir() && p != &target && !owned.iter().any(|o| p.file_name() == Some(o.as_os_str())))
        .filter(|p| !p.join("imported.json").exists() && has_results(p))
        .collect();
    let [from] = orphans.as_slice() else { return None };
    // Store::open 이 만들어 둔 빈 폴더가 있으면 치운다 (결과가 없는 것만)
    if target.exists() {
        let _ = std::fs::remove_dir(target.join("units"));
        let _ = std::fs::remove_dir(&target);
    }
    std::fs::rename(from, &target).ok()?;
    Some(from.file_name()?.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests;
