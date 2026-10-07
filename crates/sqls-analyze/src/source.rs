//! 분석할 소스 모으기 — DB 의 사전(ALL_SOURCE) 또는 .sql/.pkb/.pks 파일.
//!
//! DB 에서는 사전 조회만 한다 (읽기 전용 세션이면 충분하다).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use sqls_core::session::Session;

use crate::chunk::UnitSource;

/// 분석 대상이 되는 형식 (본문이 있는 것)
pub const BODY_TYPES: &[&str] = &["PACKAGE BODY", "PROCEDURE", "FUNCTION", "TRIGGER", "TYPE BODY"];

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct UnitRef {
    pub owner: String,
    pub name: String,
    pub unit_type: String,
}

/// 스키마의 단위 목록. `name_like` 는 LIKE 패턴 (대문자로 바꾼다)
pub async fn list_units(s: &Session, owner: &str, types: &[String], name_like: Option<&str>) -> sqls_core::Result<Vec<UnitRef>> {
    let types: Vec<String> = if types.is_empty() { BODY_TYPES.iter().map(|t| t.to_string()).collect() } else { types.iter().map(|t| t.to_uppercase().replace('_', " ")).collect() };
    // 11g 의 ALL_OBJECTS 는 권한을 줄마다 따져서 권한이 많은 계정(SYSTEM 등)에서 1분 넘게 걸린다.
    // DBA_OBJECTS 를 볼 수 있으면(SELECT_CATALOG_ROLE) 그쪽이 수십 배 빠르다.
    let sql = |view: &str| {
        format!(
            "SELECT owner, object_name, object_type FROM {view} \
             WHERE owner = :o AND (:pat IS NULL OR object_name LIKE :pat ESCAPE '\\') \
               AND object_type IN ('PACKAGE BODY','PROCEDURE','FUNCTION','TRIGGER','TYPE BODY','PACKAGE','TYPE') \
             ORDER BY object_name, object_type"
        )
    };
    let binds = || vec![("O".to_string(), Some(owner.to_uppercase())), ("PAT".to_string(), name_like.map(|p| p.to_uppercase()))];
    let page = match s.query(&sql("dba_objects"), binds(), 1_000_000).await {
        Ok(p) => p,
        Err(_) => s.query(&sql("all_objects"), binds(), 1_000_000).await?,
    };
    Ok(page
        .rows
        .iter()
        .map(|r| UnitRef {
            owner: r[0].clone().unwrap_or_default(),
            name: r[1].clone().unwrap_or_default(),
            unit_type: r[2].clone().unwrap_or_default(),
        })
        .filter(|u| types.contains(&u.unit_type))
        .collect())
}

/// 단위 소스 + (패키지·형식 본문이면) 명세 소스
pub async fn fetch(s: &Session, u: &UnitRef) -> sqls_core::Result<(UnitSource, Option<String>)> {
    let lines = sqls_core::debug::source(s, &u.owner, &u.name, &u.unit_type).await?;
    let text = lines.join("\n");
    let spec_type = match u.unit_type.as_str() {
        "PACKAGE BODY" => Some("PACKAGE"),
        "TYPE BODY" => Some("TYPE"),
        _ => None,
    };
    let spec = match spec_type {
        Some(t) => {
            let l = sqls_core::debug::source(s, &u.owner, &u.name, t).await?;
            (!l.is_empty()).then(|| l.join("\n"))
        }
        None => None,
    };
    Ok((UnitSource { owner: u.owner.clone(), name: u.name.clone(), unit_type: u.unit_type.clone(), text }, spec))
}

/// UTF-8 (BOM) 이 아니면 CP949 로 읽는다 — 사내 SQL 파일은 대개 둘 중 하나다
pub fn decode(bytes: &[u8]) -> String {
    let b = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    match std::str::from_utf8(b) {
        Ok(s) => s.to_string(),
        Err(_) => encoding_rs::EUC_KR.decode(b).0.into_owned(),
    }
}

/// 파일·폴더에서 CREATE 문을 찾아 단위로. 명세와 본문이 다른 파일에 있어도 이름으로 짝짓는다.
/// `default_owner` 는 소스에 스키마가 없을 때 쓴다.
pub fn from_files(paths: &[PathBuf], default_owner: &str) -> std::io::Result<Vec<(UnitSource, Option<String>)>> {
    let mut files = Vec::new();
    for p in paths {
        collect(p, &mut files)?;
    }
    files.sort();
    let mut units: Vec<UnitSource> = Vec::new();
    for f in &files {
        let text = decode(&std::fs::read(f)?);
        for st in sqls_core::sql::split_script(&text) {
            if st.kind != sqls_core::sql::StmtKind::PlsqlUnit {
                continue;
            }
            let (s, _) = crate::plsql::structure(&st.text);
            if s.name.is_empty() || s.unit_type.is_empty() {
                continue;
            }
            // ALL_SOURCE 와 같게: CREATE [OR REPLACE] 를 떼고 단위 머리부터 1행
            let body = strip_create(&st.text);
            units.push(UnitSource {
                owner: s.owner.clone().unwrap_or_else(|| default_owner.to_uppercase()),
                name: s.name.clone(),
                unit_type: s.unit_type.clone(),
                text: body,
            });
        }
    }
    // 같은 단위가 여러 번 나오면 뒤의 것
    let mut by_key: BTreeMap<String, UnitSource> = BTreeMap::new();
    for u in units {
        by_key.insert(u.key(), u);
    }
    let specs: BTreeMap<String, String> = by_key
        .values()
        .filter(|u| u.unit_type == "PACKAGE" || u.unit_type == "TYPE")
        .map(|u| (format!("{}.{}.{}", u.owner, u.name, u.unit_type), u.text.clone()))
        .collect();
    Ok(by_key
        .into_values()
        .filter(|u| BODY_TYPES.contains(&u.unit_type.as_str()))
        .map(|u| {
            let spec_type = if u.unit_type == "PACKAGE BODY" { "PACKAGE" } else { "TYPE" };
            let spec = specs.get(&format!("{}.{}.{}", u.owner, u.name, spec_type)).cloned();
            (u, spec)
        })
        .collect())
}

fn collect(p: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    if p.is_dir() {
        for e in std::fs::read_dir(p)? {
            collect(&e?.path(), out)?;
        }
    } else if p
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "sql" | "pkb" | "pks" | "pkg" | "prc" | "fnc" | "trg" | "tps" | "tpb" | "plsql"))
    {
        out.push(p.to_path_buf());
    }
    Ok(())
}

/// "CREATE OR REPLACE EDITIONABLE PACKAGE BODY s.x" → "PACKAGE BODY s.x" (같은 줄 위치를 지킨다)
fn strip_create(text: &str) -> String {
    let t = text.trim_start();
    let up = t.to_uppercase();
    if !up.starts_with("CREATE") {
        return t.to_string();
    }
    let mut rest = &t[6..];
    for w in ["OR", "REPLACE", "EDITIONABLE", "NONEDITIONABLE"] {
        let r = rest.trim_start_matches([' ', '\t']);
        if r.len() >= w.len() && r[..w.len()].eq_ignore_ascii_case(w) && r[w.len()..].starts_with([' ', '\t', '\n', '\r']) {
            rest = &r[w.len()..];
        }
    }
    rest.trim_start_matches([' ', '\t']).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_prefix() {
        assert_eq!(strip_create("CREATE OR REPLACE PACKAGE BODY a.x IS"), "PACKAGE BODY a.x IS");
        assert_eq!(strip_create("create or replace editionable procedure p as"), "procedure p as");
        assert_eq!(strip_create("PACKAGE x IS"), "PACKAGE x IS");
    }

    #[test]
    fn files_pair_spec_and_body() {
        let dir = std::env::temp_dir().join(format!("sqls-analyze-src-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("a.pks"), "CREATE OR REPLACE PACKAGE app.pk IS\n  -- 하나\n  PROCEDURE one;\nEND;\n/\n").unwrap();
        // CP949 로 쓴 본문
        let (body, _, _) = encoding_rs::EUC_KR.encode("CREATE OR REPLACE PACKAGE BODY app.pk IS\n  PROCEDURE one IS BEGIN NULL; END; -- 한글\nEND;\n/\nCREATE PROCEDURE solo AS BEGIN NULL; END;\n/\n");
        std::fs::write(dir.join("sub").join("b.pkb"), body).unwrap();
        let units = from_files(&[dir.clone()], "scott").unwrap();
        let keys: Vec<String> = units.iter().map(|(u, _)| u.key()).collect();
        assert_eq!(keys, vec!["APP.PK.PACKAGE_BODY", "SCOTT.SOLO.PROCEDURE"]);
        assert!(units[0].1.as_deref().unwrap().contains("하나"));
        assert!(units[0].0.text.contains("한글"));
        assert!(units[0].0.text.starts_with("PACKAGE BODY"));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
