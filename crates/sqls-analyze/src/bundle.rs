//! 분석 결과 묶음 (.sqla) — 분석할 수 없는 곳(DB 에 닿지 않는 PC, 모델이 없는 PC)에서도 결과를 보게.
//!
//! ```text
//! 분석한 PC: 결과 폴더(units/·integrated/) → zip 하나 (+ manifest.json)
//! 보는 PC  : zip → analysis/<이름>/ 에 풀고 imported.json 을 남긴다 → 접속 없이 요약·호출 관계·흐름도·테이블
//! ```
//! 묶음에는 PL/SQL 소스(조각 코드)와 SQL 문이 그대로 들어 있다 — 회사 코드다. 화면이 내보낼 때 알린다.
//! 풀 때는 이름이 안전한 항목(units/·integrated/ 아래, 상위 폴더로 나가지 않는 것)만 받는다.

use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::store::Store;

pub const EXT: &str = "sqla";
const KIND: &str = "sqlstudio-analysis";
const FORMAT: u32 = 1;
/// 풀어 놓을 전체 크기 상한 — 깨진·악의적인 파일이 디스크를 채우지 않게
const MAX_TOTAL: u64 = 4 << 30;
const MAX_ENTRIES: usize = 500_000;

/// 묶음의 머리 (manifest.json, 가져온 폴더에는 imported.json)
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Manifest {
    pub kind: String,
    pub format: u32,
    /// 분석한 접속 프로필
    pub profile: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub db_user: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server: Option<String>,
    pub exported_at: u64,
    #[serde(default)]
    pub exported_by: String,
    pub app_version: String,
    pub units: usize,
    #[serde(default)]
    pub files: usize,
    /// 가져온 때 (가져온 폴더에서만)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub imported_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub imported_from: Option<String>,
}

impl Manifest {
    pub fn new(profile: &str, app_version: &str) -> Self {
        Self {
            kind: KIND.into(),
            format: FORMAT,
            profile: profile.into(),
            exported_at: crate::store::now(),
            exported_by: std::env::var("USERNAME").or_else(|_| std::env::var("USER")).unwrap_or_default(),
            app_version: app_version.into(),
            ..Default::default()
        }
    }
}

fn err(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.into())
}

/// 결과 폴더 안의 파일들 (units/·integrated/ 아래, 임시 파일 제외)
fn files_of(root: &Path) -> io::Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    let mut stack: Vec<PathBuf> = ["units", "integrated"].iter().map(|d| root.join(d)).filter(|p| p.is_dir()).collect();
    while let Some(d) = stack.pop() {
        for e in fs::read_dir(&d)? {
            let p = e?.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_none_or(|x| x != "tmp") {
                out.push(p);
            }
        }
    }
    out.sort();
    Ok(out)
}

/// 결과 폴더를 묶음 하나로. 돌려주는 것은 머리 (단위·파일 수를 채운 것)
pub fn export(store: &Store, out: &Path, mut m: Manifest) -> io::Result<Manifest> {
    let root = store.root();
    let files = files_of(root)?;
    m.units = store.units().len();
    if m.units == 0 {
        return Err(err("내보낼 분석 결과가 없습니다"));
    }
    m.files = files.len();
    let tmp = out.with_extension("part");
    {
        let f = fs::File::create(&tmp)?;
        let mut z = zip::ZipWriter::new(io::BufWriter::new(f));
        let opt = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated).large_file(false);
        z.start_file("manifest.json", opt).map_err(io::Error::other)?;
        z.write_all(&serde_json::to_vec_pretty(&m).map_err(io::Error::other)?)?;
        for p in &files {
            let rel = p.strip_prefix(root).map_err(io::Error::other)?;
            let name = rel.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect::<Vec<_>>().join("/");
            z.start_file(name, opt).map_err(io::Error::other)?;
            z.write_all(&fs::read(p)?)?;
        }
        z.finish().map_err(io::Error::other)?.flush()?;
    }
    fs::rename(&tmp, out)?;
    Ok(m)
}

/// 묶음의 머리만 읽는다 (가져오기 전에 보여 주려고)
pub fn peek(file: &Path) -> io::Result<Manifest> {
    let mut z = zip::ZipArchive::new(fs::File::open(file)?).map_err(|e| err(format!("분석 결과 파일이 아닙니다: {e}")))?;
    let mut s = String::new();
    z.by_name("manifest.json").map_err(|_| err("분석 결과 파일이 아닙니다 (manifest.json 없음)"))?.read_to_string(&mut s)?;
    let m: Manifest = serde_json::from_str(&s).map_err(|e| err(format!("manifest.json 을 읽을 수 없습니다: {e}")))?;
    if m.kind != KIND {
        return Err(err("SQLStudio 분석 결과 파일이 아닙니다"));
    }
    if m.format > FORMAT {
        return Err(err(format!("더 새 버전에서 만든 파일입니다 (형식 {}) — SQLStudio 를 올리세요", m.format)));
    }
    Ok(m)
}

/// 묶음을 `dest` 폴더에 푼다. `overwrite` 가 아니면 이미 있는 폴더에는 풀지 않는다.
/// 다른 폴더에 다 푼 뒤 바꿔 끼우므로, 도중에 실패해도 있던 결과가 깨지지 않는다.
pub fn import(file: &Path, dest: &Path, overwrite: bool) -> io::Result<Manifest> {
    let mut m = peek(file)?;
    if dest.exists() && !overwrite {
        return Err(io::Error::new(io::ErrorKind::AlreadyExists, format!("이미 있습니다: {}", dest.display())));
    }
    let mut z = zip::ZipArchive::new(fs::File::open(file)?).map_err(io::Error::other)?;
    if z.len() > MAX_ENTRIES {
        return Err(err("항목이 너무 많습니다"));
    }
    let parent = dest.parent().ok_or_else(|| err("가져올 폴더가 잘못되었습니다"))?;
    fs::create_dir_all(parent)?;
    let stage = parent.join(format!(".import-{}-{}", std::process::id(), crate::store::now()));
    let _ = fs::remove_dir_all(&stage);
    let r = (|| -> io::Result<()> {
        let mut total = 0u64;
        for i in 0..z.len() {
            let mut e = z.by_index(i).map_err(io::Error::other)?;
            if e.is_dir() {
                continue;
            }
            // 안전한 이름만: 상대 경로, 상위로 나가지 않음, 결과 폴더 안
            let Some(rel) = e.enclosed_name() else { return Err(err(format!("잘못된 항목 이름: {}", e.name()))) };
            let top = rel.components().next().map(|c| c.as_os_str().to_string_lossy().into_owned()).unwrap_or_default();
            if rel == Path::new("manifest.json") {
                continue;
            }
            if top != "units" && top != "integrated" {
                return Err(err(format!("모르는 항목: {}", e.name())));
            }
            total += e.size();
            if total > MAX_TOTAL {
                return Err(err("풀면 너무 큽니다"));
            }
            let to = stage.join(&rel);
            if let Some(d) = to.parent() {
                fs::create_dir_all(d)?;
            }
            let mut f = fs::File::create(&to)?;
            io::copy(&mut (&mut e).take(MAX_TOTAL), &mut f)?;
        }
        fs::create_dir_all(stage.join("units"))?;
        m.imported_at = Some(crate::store::now());
        m.imported_from = Some(file.display().to_string());
        fs::write(stage.join("imported.json"), serde_json::to_vec_pretty(&m).map_err(io::Error::other)?)?;
        Ok(())
    })();
    if let Err(e) = r {
        let _ = fs::remove_dir_all(&stage);
        return Err(e);
    }
    if dest.exists() {
        let old = parent.join(format!(".old-{}-{}", std::process::id(), crate::store::now()));
        fs::rename(dest, &old)?;
        if let Err(e) = fs::rename(&stage, dest) {
            let _ = fs::rename(&old, dest);
            return Err(e);
        }
        let _ = fs::remove_dir_all(&old);
    } else {
        fs::rename(&stage, dest)?;
    }
    Ok(m)
}

/// 이 PC 에 있는 결과 하나
#[derive(Debug, Clone, Serialize)]
pub struct SetInfo {
    /// 폴더 이름 (= 접속 프로필 이름, 가져온 것은 가져올 때 정한 이름)
    pub name: String,
    pub dir: String,
    pub units: usize,
    /// 통합 결과를 만든 때
    pub generated_at: Option<u64>,
    /// 가져온 것이면 그 머리
    pub imported: Option<Manifest>,
}

/// 결과 폴더들 (`analysis/` 아래)
pub fn list_sets(base: &Path) -> Vec<SetInfo> {
    let Ok(rd) = fs::read_dir(base) else { return Vec::new() };
    let mut out: Vec<SetInfo> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir() && p.join("units").is_dir() && !p.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.')))
        .map(|p| {
            let units = fs::read_dir(p.join("units")).map(|r| r.flatten().filter(|e| e.path().join("unit.json").is_file()).count()).unwrap_or(0);
            let generated_at = fs::read_to_string(p.join("integrated").join("integrated.json"))
                .ok()
                .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
                .and_then(|v| v["generated_at"].as_u64());
            let imported = fs::read_to_string(p.join("imported.json")).ok().and_then(|s| serde_json::from_str(&s).ok());
            SetInfo { name: p.file_name().unwrap().to_string_lossy().into_owned(), dir: p.display().to_string(), units, generated_at, imported }
        })
        .filter(|s| s.units > 0)
        .collect();
    out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunk::UnitSource;
    use crate::run::{analyze_unit, Event, Options};
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;

    #[tokio::test]
    async fn export_import_roundtrip() {
        let base = std::env::temp_dir().join(format!("sqls-bundle-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let store = Store::open(base.join("DEV")).unwrap();
        let src = UnitSource { owner: "APP".into(), name: "P".into(), unit_type: "PROCEDURE".into(), text: "PROCEDURE p IS\nBEGIN\n  UPDATE t SET a = 1;\nEND p;".into() };
        analyze_unit(&store, &src, None, None, &Options::default(), Arc::new(|_: Event| {}), Arc::new(AtomicBool::new(false))).await.unwrap();
        crate::integrate::write_all(&store, None).unwrap();

        let file = base.join("dev.sqla");
        let m = export(&store, &file, Manifest::new("DEV", "0.0.0")).unwrap();
        assert_eq!(m.units, 1);
        assert!(m.files >= 3, "{m:?}");
        assert_eq!(peek(&file).unwrap().profile, "DEV");

        let dest = base.join("DEV (가져옴)");
        let got = import(&file, &dest, false).unwrap();
        assert!(got.imported_at.is_some());
        let again = Store::open(&dest).unwrap();
        assert_eq!(again.units().len(), 1);
        let g: crate::integrate::Integrated = again.read_integrated("integrated.json").unwrap();
        assert!(g.tables.iter().any(|t| t.table == "APP.T"));
        // 이미 있으면 묻는다, 덮어쓰기는 바꿔 끼운다
        assert_eq!(import(&file, &dest, false).unwrap_err().kind(), io::ErrorKind::AlreadyExists);
        import(&file, &dest, true).unwrap();
        let sets = list_sets(&base);
        assert_eq!(sets.iter().map(|s| (s.name.as_str(), s.imported.is_some())).collect::<Vec<_>>(), vec![("DEV", false), ("DEV (가져옴)", true)]);
        assert!(!fs::read_dir(&base).unwrap().flatten().any(|e| e.file_name().to_string_lossy().starts_with('.')), "임시 폴더를 남기지 않는다");
        fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn rejects_unsafe_entries() {
        let base = std::env::temp_dir().join(format!("sqls-bundle-bad-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        let file = base.join("bad.sqla");
        {
            let mut z = zip::ZipWriter::new(fs::File::create(&file).unwrap());
            let opt = zip::write::SimpleFileOptions::default();
            z.start_file("manifest.json", opt).unwrap();
            z.write_all(&serde_json::to_vec(&Manifest::new("X", "0")).unwrap()).unwrap();
            z.start_file("../evil.txt", opt).unwrap();
            z.write_all(b"x").unwrap();
            z.finish().unwrap();
        }
        assert!(import(&file, &base.join("X"), false).is_err());
        assert!(!base.join("evil.txt").exists() && !base.parent().unwrap().join("evil.txt").exists());
        assert!(!base.join("X").exists(), "실패하면 아무것도 남기지 않는다");
        fs::write(base.join("not.sqla"), b"hello").unwrap();
        assert!(peek(&base.join("not.sqla")).is_err());
        fs::remove_dir_all(&base).unwrap();
    }
}
