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
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};

use crate::store::Store;

pub const EXT: &str = "sqla";
const KIND: &str = "sqlstudio-analysis";
const FORMAT: u32 = 1;
/// 풀어 놓을 전체 크기 상한 — 깨진·악의적인 파일이 디스크를 채우지 않게
const MAX_TOTAL: u64 = 4 << 30;

/// 가져오기·합치기 진행 — (단계, 한 것, 전체). 전체가 0 이면 개수를 모르는 단계
pub type OnStep<'a> = &'a dyn Fn(Step, usize, usize);

/// 가져오기·합치기의 단계
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Step {
    /// 파일을 임시 폴더에 푸는 중 (항목 수)
    Unzip,
    /// 단위마다 있던 결과와 견주어 합치는 중 (단위 수)
    Merge,
    /// 통합 결과(호출 관계·CRUD·흐름)를 다시 만드는 중
    Integrate,
    /// 마무리 (바꿔 끼우기·기록)
    Finish,
}

fn no_step(_: Step, _: usize, _: usize) {}
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
    import_with(file, dest, overwrite, &no_step)
}

/// [`import`] — 푸는 진행을 알린다
pub fn import_with(file: &Path, dest: &Path, overwrite: bool, on: OnStep) -> io::Result<Manifest> {
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
        let n = z.len();
        for i in 0..n {
            on(Step::Unzip, i, n);
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
        on(Step::Unzip, n, n);
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

// ── 합치기: 같은 곳에 여러 번 가져와도 단위마다 하나만 남는다 ─────────────

/// 가져온 기록 (결과 폴더의 imports.json — 내보내기에는 들어가지 않는다)
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ImportLog {
    pub profile: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub db_user: Option<String>,
    pub exported_at: u64,
    #[serde(default)]
    pub exported_by: String,
    pub imported_at: u64,
    #[serde(default)]
    pub from: String,
    pub added: usize,
    pub updated: usize,
    pub kept: usize,
}

const IMPORT_LOG: &str = "imports.json";

pub fn import_log(dir: &Path) -> Vec<ImportLog> {
    fs::read_to_string(dir.join(IMPORT_LOG)).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

fn push_log(dir: &Path, l: ImportLog) -> io::Result<()> {
    let mut v = import_log(dir);
    v.push(l);
    let keep = v.len().saturating_sub(100);
    fs::write(dir.join(IMPORT_LOG), serde_json::to_vec_pretty(&v[keep..]).map_err(io::Error::other)?)
}

/// 합친 결과
#[derive(Debug, Clone, Default, Serialize)]
pub struct MergeReport {
    /// 없던 단위
    pub added: usize,
    /// 있던 것보다 나중에 분석한 것이라 바꾼 단위
    pub updated: usize,
    /// 있던 것이 같거나 더 나중 것이라 그대로 둔 단위
    pub kept: usize,
    /// 흐름도 보고서 (없던 것·나중에 고친 것)
    pub reports: usize,
    /// 있던 결과에 합쳤으면 true, 빈 곳에 새로 풀었으면 false
    pub merged: bool,
}

fn copy_tree(from: &Path, to: &Path) -> io::Result<()> {
    fs::create_dir_all(to)?;
    for e in fs::read_dir(from)? {
        let p = e?.path();
        let t = to.join(p.file_name().unwrap_or_default());
        if p.is_dir() {
            copy_tree(&p, &t)?;
        } else if p.extension().is_none_or(|x| x != "tmp") {
            fs::copy(&p, &t)?;
        }
    }
    Ok(())
}

/// 폴더 `to` 를 `from` 의 사본으로 바꿔 끼운다 (다 복사한 뒤에 바꾸므로 도중에 실패해도 있던 것이 남는다)
fn replace_tree(from: &Path, to: &Path) -> io::Result<()> {
    let parent = to.parent().ok_or_else(|| err("폴더가 잘못되었습니다"))?;
    let leaf = to.file_name().unwrap_or_default().to_string_lossy().into_owned();
    let new = parent.join(format!(".new-{leaf}"));
    let _ = fs::remove_dir_all(&new);
    if let Err(e) = copy_tree(from, &new) {
        let _ = fs::remove_dir_all(&new);
        return Err(e);
    }
    if to.exists() {
        let old = parent.join(format!(".old-{leaf}"));
        let _ = fs::remove_dir_all(&old);
        fs::rename(to, &old)?;
        if let Err(e) = fs::rename(&new, to) {
            let _ = fs::rename(&old, to);
            return Err(e);
        }
        let _ = fs::remove_dir_all(&old);
        Ok(())
    } else {
        fs::rename(&new, to)
    }
}

fn updated_at(p: &Path) -> u64 {
    fs::read_to_string(p).ok().and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()).and_then(|v| v["updated_at"].as_u64()).unwrap_or(0)
}

/// 결과 폴더 `from` 을 `into` 에 합친다 — 단위마다 더 나중에 분석한 것을 남기고, 통합 결과를 다시 만든다.
/// 같은 것을 몇 번 합쳐도 결과는 같다 (같은 시각이면 있던 것을 둔다). `from` 은 그대로 둔다.
pub fn merge_dir(from: &Path, into: &Path) -> io::Result<MergeReport> {
    merge_dir_with(from, into, &no_step)
}

/// [`merge_dir`] — 단위마다 진행을 알린다
pub fn merge_dir_with(from: &Path, into: &Path, on: OnStep) -> io::Result<MergeReport> {
    merge_dir_ctl(from, into, on, &AtomicBool::new(false), false)
}

/// [`merge_dir_with`] — `stop` 이 서면 단위 사이에서 멈춘다 (합친 단위는 그대로 남고, 다시 돌리면 남은 것만 합친다).
/// `force_integrate` 면 새로 합친 단위가 없어도 통합 결과를 다시 만든다 (이어서 할 때 — 지난번에 합치고 통합 전에 멈췄을 수 있다)
pub fn merge_dir_ctl(from: &Path, into: &Path, on: OnStep, stop: &AtomicBool, force_integrate: bool) -> io::Result<MergeReport> {
    let src = Store::open(from)?;
    let dst = Store::open(into)?;
    let mut r = MergeReport { merged: true, ..Default::default() };
    let mut taken = std::collections::HashSet::new();
    on(Step::Merge, 0, 0);
    let units = src.units();
    let n = units.len();
    for (i, u) in units.into_iter().enumerate() {
        if stop.load(Ordering::Relaxed) {
            return Err(stopped());
        }
        on(Step::Merge, i, n);
        let have = dst.read_unit(&u.key);
        if have.as_ref().is_some_and(|h| h.analyzed_at >= u.analyzed_at) {
            r.kept += 1;
            continue;
        }
        replace_tree(&src.unit_dir(&u.key), &dst.unit_dir(&u.key))?;
        if have.is_some() {
            r.updated += 1;
        } else {
            r.added += 1;
        }
        taken.insert(u.key);
    }
    // 사전 대조의 모델 추정: 가져온 단위의 것은 그쪽 것으로
    if !taken.is_empty() {
        use crate::deps::Guess;
        let mut g: Vec<Guess> = dst.read_integrated("supplement.json").unwrap_or_default();
        g.retain(|x| !taken.contains(&x.unit));
        g.extend(src.read_integrated::<Vec<Guess>>("supplement.json").unwrap_or_default().into_iter().filter(|x| taken.contains(&x.unit)));
        if !g.is_empty() {
            dst.write_integrated("supplement.json", &g)?;
        }
    }
    // 흐름도 보고서: 없던 것은 넣고, 같은 것이면 나중에 고친 것
    if let Ok(rd) = fs::read_dir(from.join("integrated").join("reports")) {
        let to_dir = into.join("integrated").join("reports");
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().is_none_or(|x| x != "json") {
                continue;
            }
            let t = to_dir.join(e.file_name());
            if !t.exists() || updated_at(&p) > updated_at(&t) {
                fs::create_dir_all(&to_dir)?;
                fs::copy(&p, &t)?;
                r.reports += 1;
            }
        }
    }
    let had = dst.read_integrated::<crate::integrate::Integrated>("integrated.json");
    if r.added + r.updated > 0 || had.is_none() || force_integrate {
        on(Step::Integrate, 0, 0);
        // 전체 요약(모델이 쓴 것)은 있던 것을, 없으면 가져온 것을
        let overview = had.and_then(|g| g.overview).or_else(|| src.read_integrated::<crate::integrate::Integrated>("integrated.json").and_then(|g| g.overview));
        crate::integrate::write_all(&dst, overview)?;
    }
    Ok(r)
}

/// 묶음을 결과 폴더 `dest` 로 가져온다. 비어 있으면 그대로 풀고, 결과가 있으면 합친다 (덮어쓰지 않는다).
/// `mark` 면 '가져온 결과' 표시(imported.json)를 남긴다 — 접속 프로필의 결과 폴더로 가져올 때는 남기지 않는다
/// (남기면 그 접속의 결과가 '가져온 것' 으로 보이고, 이름 바꾼 접속의 결과 이어 붙이기에서도 빠진다).
pub fn import_to(file: &Path, dest: &Path, mark: bool) -> io::Result<(Manifest, MergeReport)> {
    import_to_with(file, dest, mark, &no_step)
}

/// [`import_to`] — 푸는·합치는·통합을 다시 만드는 진행을 알린다
pub fn import_to_with(file: &Path, dest: &Path, mark: bool, on: OnStep) -> io::Result<(Manifest, MergeReport)> {
    import_resumable(file, dest, mark, "", on, &AtomicBool::new(false))
}

// ── 이어서 가져오기 ─────────────────────────────────────
// 같은 파일(경로·크기·수정 시각)을 같은 곳으로 가져오면 늘 같은 임시 폴더(.import-<id>)를 쓰고, 옆에 기록(.import-<id>.json)을
// 남긴다. 멈추거나(중지·앱 종료·PC 꺼짐) 실패해도 지우지 않으므로, 다시 가져오면 다 푼 항목은 건너뛰고 합친 단위는 그대로 둔다.

/// 중단된 가져오기 하나 (결과 폴더들 옆의 `.import-<id>.json`)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pending {
    pub id: String,
    pub file: String,
    pub size: u64,
    pub modified: u64,
    pub dest: String,
    /// 화면이 고른 가져올 곳 (접속 이름 또는 따로 둘 이름)
    #[serde(default)]
    pub target: String,
    pub mark: bool,
    pub profile: String,
    pub step: Step,
    pub done: usize,
    pub total: usize,
    pub started_at: u64,
    pub updated_at: u64,
    /// 원본 파일이 그대로 있어 이어서 할 수 있는지 (읽을 때 채운다)
    #[serde(default)]
    pub resumable: bool,
}

fn file_sig(file: &Path) -> io::Result<(u64, u64)> {
    let md = fs::metadata(file)?;
    let modified = md.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs()).unwrap_or(0);
    Ok((md.len(), modified))
}

/// 파일·크기·수정 시각·가져올 곳으로 정하는 아이디 (FNV-1a — 실행마다 같아야 한다)
fn pending_id(file: &Path, size: u64, modified: u64, dest: &Path) -> String {
    let key = format!("{}|{size}|{modified}|{}", file.display(), dest.display()).to_lowercase();
    let mut h: u64 = 0xcbf29ce484222325;
    for b in key.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{h:016x}")
}

fn stopped() -> io::Error {
    io::Error::new(io::ErrorKind::Interrupted, "중지했습니다 — 다시 가져오면 이어서 합니다")
}

/// 묶음을 `stage` 에 푼다. 이미 다 푼 항목(크기가 같은 파일)은 건너뛴다 — 항목은 .part 로 쓰고 이름을 바꾸므로 쓰다 만 파일은 남지 않는다
fn unzip_resumable(file: &Path, stage: &Path, on: OnStep, stop: &AtomicBool) -> io::Result<()> {
    let mut z = zip::ZipArchive::new(fs::File::open(file)?).map_err(io::Error::other)?;
    let n = z.len();
    if n > MAX_ENTRIES {
        return Err(err("항목이 너무 많습니다"));
    }
    let mut total = 0u64;
    for i in 0..n {
        if stop.load(Ordering::Relaxed) {
            return Err(stopped());
        }
        on(Step::Unzip, i, n);
        let mut e = z.by_index(i).map_err(io::Error::other)?;
        if e.is_dir() {
            continue;
        }
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
        if fs::metadata(&to).is_ok_and(|m| m.len() == e.size()) {
            continue;
        }
        if let Some(d) = to.parent() {
            fs::create_dir_all(d)?;
        }
        let part = to.with_extension(format!("{}part", to.extension().map(|x| format!("{}.", x.to_string_lossy())).unwrap_or_default()));
        let mut f = fs::File::create(&part)?;
        io::copy(&mut (&mut e).take(MAX_TOTAL), &mut f)?;
        drop(f);
        fs::rename(&part, &to)?;
    }
    on(Step::Unzip, n, n);
    fs::create_dir_all(stage.join("units"))?;
    Ok(())
}

/// 묶음을 결과 폴더 `dest` 로 가져온다 — 비어 있으면 풀어서 그대로, 결과가 있으면 합친다.
/// 멈추거나 실패하면 풀어 둔 것과 기록을 남겨, 같은 파일을 같은 곳으로 다시 가져오면 이어서 한다. `target` 은 기록에 남길 화면의 이름
pub fn import_resumable(file: &Path, dest: &Path, mark: bool, target: &str, on: OnStep, stop: &AtomicBool) -> io::Result<(Manifest, MergeReport)> {
    let mut m = peek(file)?;
    let parent = dest.parent().ok_or_else(|| err("가져올 폴더가 잘못되었습니다"))?;
    fs::create_dir_all(parent)?;
    let (size, modified) = file_sig(file)?;
    let id = pending_id(file, size, modified, dest);
    let stage = parent.join(format!(".import-{id}"));
    let note = parent.join(format!(".import-{id}.json"));
    let resumed = note.is_file();
    let started_at = fs::read_to_string(&note).ok().and_then(|s| serde_json::from_str::<Pending>(&s).ok()).map(|p| p.started_at).unwrap_or_else(crate::store::now);
    // 진행을 화면에 알리면서 기록에도 남긴다 (몇 초에 한 번)
    let last_write = std::cell::Cell::new(0u64);
    let record = |step: Step, done: usize, total: usize| {
        on(step, done, total);
        let now = crate::store::now();
        if now == last_write.get() && done != total {
            return;
        }
        last_write.set(now);
        let p = Pending {
            id: id.clone(), file: file.display().to_string(), size, modified, dest: dest.display().to_string(), target: target.to_string(),
            mark, profile: m.profile.clone(), step, done, total, started_at, updated_at: now, resumable: true,
        };
        let _ = fs::write(&note, serde_json::to_vec_pretty(&p).unwrap_or_default());
    };
    record(Step::Unzip, 0, 0);
    unzip_resumable(file, &stage, &record, stop)?;
    m.imported_at = Some(crate::store::now());
    m.imported_from = Some(file.display().to_string());
    fs::write(stage.join("imported.json"), serde_json::to_vec_pretty(&m).map_err(io::Error::other)?)?;

    let r = if crate::has_results(dest) {
        // 합치기는 단위마다 바꿔 끼우고, 이미 합친 단위는 다시 해도 그대로 — 이어서 할 때는 통합을 꼭 다시 만든다
        let r = merge_dir_ctl(&stage, dest, &record, stop, resumed)?;
        record(Step::Finish, 0, 0);
        let _ = fs::remove_dir_all(&stage);
        r
    } else {
        record(Step::Finish, 0, 0);
        if !mark {
            let _ = fs::remove_file(stage.join("imported.json"));
        }
        // Store::open 이 만들어 둔 빈 폴더는 바꿔 끼운다
        if dest.exists() {
            let old = parent.join(format!(".old-{id}"));
            let _ = fs::remove_dir_all(&old);
            fs::rename(dest, &old)?;
            if let Err(e) = fs::rename(&stage, dest) {
                let _ = fs::rename(&old, dest);
                return Err(e);
            }
            let _ = fs::remove_dir_all(&old);
        } else {
            fs::rename(&stage, dest)?;
        }
        MergeReport { added: Store::open(dest)?.units().len(), ..Default::default() }
    };
    let _ = fs::remove_file(&note);
    let m2 = m.clone();
    push_log(dest, ImportLog {
        profile: m2.profile,
        db_user: m2.db_user,
        exported_at: m2.exported_at,
        exported_by: m2.exported_by,
        imported_at: crate::store::now(),
        from: file.display().to_string(),
        added: r.added,
        updated: r.updated,
        kept: r.kept,
    })?;
    Ok((m, r))
}

/// 중단된 가져오기들 (`root` 는 결과 폴더들이 있는 곳). 원본 파일이 바뀌었거나 없어졌으면 resumable = false
pub fn pending_imports(root: &Path) -> Vec<Pending> {
    let Ok(rd) = fs::read_dir(root) else { return Vec::new() };
    let mut out: Vec<Pending> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.file_name().is_some_and(|n| n.to_string_lossy().starts_with(".import-")) && p.extension().is_some_and(|x| x == "json"))
        .filter_map(|p| fs::read_to_string(&p).ok().and_then(|s| serde_json::from_str::<Pending>(&s).ok()))
        .map(|mut p| {
            p.resumable = file_sig(Path::new(&p.file)).is_ok_and(|(s, m)| s == p.size && m == p.modified);
            p
        })
        .collect();
    out.sort_by_key(|p| std::cmp::Reverse(p.updated_at));
    out
}

/// 중단된 가져오기를 버린다 (풀어 둔 것과 기록)
pub fn discard_pending(root: &Path, id: &str) -> io::Result<()> {
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(err("잘못된 아이디"));
    }
    let _ = fs::remove_dir_all(root.join(format!(".import-{id}")));
    match fs::remove_file(root.join(format!(".import-{id}.json"))) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
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
    /// 이 폴더로 가져온 기록 (오래된 것부터)
    pub imports: Vec<ImportLog>,
}

/// 통합 결과를 만든 때 — 파일 맨 앞만 읽는다 (첫 필드다). 통합 결과는 수백 MB 일 수 있어 통째로 읽으면 목록이 느리다
fn generated_at(p: &Path) -> Option<u64> {
    let mut head = [0u8; 256];
    let n = fs::File::open(p).ok()?.read(&mut head).ok()?;
    let s = String::from_utf8_lossy(&head[..n]);
    let rest = &s[s.find("\"generated_at\"")? + "\"generated_at\"".len()..];
    let digits: String = rest.trim_start_matches(|c: char| c == ':' || c.is_whitespace()).chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

/// 결과 폴더를 지운다 — 이름만 바로 바꾸고(.trash-…) 실제로 지우는 것은 뒤에서 (파일 수만 개라 몇 초 걸린다).
/// 지우다 만 것은 다음에 [`purge_trash`] 가 마저 지운다
pub fn remove_set_dir(dir: &Path) -> io::Result<()> {
    let parent = dir.parent().ok_or_else(|| err("폴더가 잘못되었습니다"))?;
    let leaf = dir.file_name().unwrap_or_default().to_string_lossy().into_owned();
    let trash = parent.join(format!(".trash-{}-{leaf}", crate::store::now()));
    fs::rename(dir, &trash)?;
    std::thread::spawn(move || {
        let _ = fs::remove_dir_all(&trash);
    });
    Ok(())
}

/// 지우다 만 결과 폴더(.trash-…)를 뒤에서 지운다
pub fn purge_trash(root: &Path) {
    let Ok(rd) = fs::read_dir(root) else { return };
    let trash: Vec<PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| p.is_dir() && p.file_name().is_some_and(|n| n.to_string_lossy().starts_with(".trash-"))).collect();
    if !trash.is_empty() {
        std::thread::spawn(move || {
            for t in trash {
                let _ = fs::remove_dir_all(t);
            }
        });
    }
}

/// 결과 폴더들 (`analysis/` 아래)
pub fn list_sets(base: &Path) -> Vec<SetInfo> {
    let Ok(rd) = fs::read_dir(base) else { return Vec::new() };
    let mut out: Vec<SetInfo> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir() && p.join("units").is_dir() && !p.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.')))
        .map(|p| {
            let units = fs::read_dir(p.join("units")).map(|r| r.flatten().filter(|e| !e.file_name().to_string_lossy().starts_with('.') && e.path().join("unit.json").is_file()).count()).unwrap_or(0);
            let generated_at = generated_at(&p.join("integrated").join("integrated.json"));
            let imported = fs::read_to_string(p.join("imported.json")).ok().and_then(|s| serde_json::from_str(&s).ok());
            let imports = import_log(&p);
            SetInfo { name: p.file_name().unwrap().to_string_lossy().into_owned(), dir: p.display().to_string(), units, generated_at, imported, imports }
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

    async fn analyze(store: &Store, name: &str, at: u64) {
        let src = UnitSource { owner: "APP".into(), name: name.into(), unit_type: "PROCEDURE".into(), text: format!("PROCEDURE {name} IS\nBEGIN\n  UPDATE t_{name} SET a = 1;\nEND {name};") };
        analyze_unit(store, &src, None, None, &Options::default(), Arc::new(|_: Event| {}), Arc::new(AtomicBool::new(false))).await.unwrap();
        let key = crate::chunk::unit_key("APP", name, "PROCEDURE");
        let mut u = store.read_unit(&key).unwrap();
        u.analyzed_at = at;
        store.write_unit(&u).unwrap();
        crate::integrate::write_all(store, None).unwrap();
    }

    #[tokio::test]
    async fn unit_index_stays_fresh() {
        let base = std::env::temp_dir().join(format!("sqls-bundle-index-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let s = Store::open(base.join("S")).unwrap();
        analyze(&s, "P", 100).await;
        let at = |s: &Store, n: &str| s.briefs().iter().find(|b| b.name == n).map(|b| b.analyzed_at);
        assert_eq!(at(&s, "P"), Some(100));
        assert!(base.join("S/integrated/units-index.json").is_file(), "색인을 남긴다");
        // 파일 시각의 해상도를 넘기게 잠깐 쉰다
        let tick = || std::thread::sleep(std::time::Duration::from_millis(1100));
        tick();
        analyze(&s, "Q", 100).await; // 새 단위
        assert_eq!(at(&s, "Q"), Some(100));
        tick();
        analyze(&s, "P", 150).await; // 고쳐 쓴 단위
        assert_eq!(at(&s, "P"), Some(150));
        // 다른 결과를 합쳐 바꿔 끼운 단위
        let o = Store::open(base.join("O")).unwrap();
        analyze(&o, "P", 300).await;
        tick();
        merge_dir(&base.join("O"), &base.join("S")).unwrap();
        assert_eq!(at(&s, "P"), Some(300));
        assert_eq!(s.briefs().len(), 2);
        fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn generated_at_from_head_and_trash() {
        let base = std::env::temp_dir().join(format!("sqls-bundle-head-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(base.join("X/integrated")).unwrap();
        fs::write(base.join("X/integrated/integrated.json"), format!("{{\n  \"generated_at\": 1761000000,\n  \"units\": 3,\n  \"x\": \"{}\"\n}}", "가".repeat(100_000))).unwrap();
        assert_eq!(generated_at(&base.join("X/integrated/integrated.json")), Some(1761000000));
        assert_eq!(generated_at(&base.join("없음.json")), None);
        // 지우기: 바로 사라지고(이름 바꿈), 뒤에서 지운다
        remove_set_dir(&base.join("X")).unwrap();
        assert!(!base.join("X").exists());
        purge_trash(&base);
        for _ in 0..50 {
            if !fs::read_dir(&base).unwrap().flatten().any(|e| e.file_name().to_string_lossy().starts_with(".trash-")) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        assert!(!fs::read_dir(&base).unwrap().flatten().any(|e| e.file_name().to_string_lossy().starts_with(".trash-")), "뒤에서 다 지운다");
        fs::remove_dir_all(&base).unwrap();
    }

    #[tokio::test]
    async fn import_stops_and_resumes() {
        let base = std::env::temp_dir().join(format!("sqls-bundle-resume-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let a = Store::open(base.join("A")).unwrap();
        for (n, at) in [("P", 100), ("Q", 100), ("R", 100)] {
            analyze(&a, n, at).await;
        }
        let fa = base.join("a.sqla");
        export(&a, &fa, Manifest::new("QA", "0")).unwrap();
        let root = base.join("sets");
        let qa = root.join("QA");

        // 1) 푸는 도중에 중지 → 기록이 남고, 다시 하면 끝까지
        let stop = AtomicBool::new(false);
        let on = |s: Step, d: usize, _t: usize| if s == Step::Unzip && d >= 2 { stop.store(true, Ordering::Relaxed) };
        let e = import_resumable(&fa, &qa, false, "QA", &on, &stop).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::Interrupted);
        let p = pending_imports(&root);
        assert_eq!(p.len(), 1, "{p:?}");
        assert!(p[0].resumable && p[0].step == Step::Unzip && p[0].target == "QA", "{p:?}");
        let (_, r) = import_resumable(&fa, &qa, false, "QA", &no_step, &AtomicBool::new(false)).unwrap();
        assert_eq!(r.added, 3);
        assert!(pending_imports(&root).is_empty());
        assert_eq!(Store::open(&qa).unwrap().units().len(), 3);
        assert!(!fs::read_dir(&root).unwrap().flatten().any(|e| e.file_name().to_string_lossy().starts_with('.')), "임시 폴더·기록을 남기지 않는다");

        // 2) 있던 결과에 합치는 도중에 중지 → 다시 하면 남은 단위만, 통합은 꼭 다시
        let b = Store::open(base.join("B")).unwrap();
        for (n, at) in [("P", 200), ("Q", 200), ("R", 200)] {
            analyze(&b, n, at).await;
        }
        let fb = base.join("b.sqla");
        export(&b, &fb, Manifest::new("QA", "0")).unwrap();
        let stop = AtomicBool::new(false);
        let on = |s: Step, d: usize, _t: usize| if s == Step::Merge && d >= 1 { stop.store(true, Ordering::Relaxed) };
        assert_eq!(import_resumable(&fb, &qa, false, "QA", &on, &stop).unwrap_err().kind(), io::ErrorKind::Interrupted);
        let p = pending_imports(&root);
        assert_eq!((p.len(), p[0].step), (1, Step::Merge), "{p:?}");
        // 반쯤 합친 상태 — 통합 결과를 지워 두면, 이어서 할 때 다시 만드는지 보인다
        let _ = fs::remove_file(qa.join("integrated").join("integrated.json"));
        let (_, r) = import_resumable(&fb, &qa, false, "QA", &no_step, &AtomicBool::new(false)).unwrap();
        assert_eq!(r.added + r.updated + r.kept, 3, "{r:?}");
        let s = Store::open(&qa).unwrap();
        assert!(s.units().iter().all(|u| u.analyzed_at == 200), "모든 단위가 새 것으로");
        assert!(s.read_integrated::<crate::integrate::Integrated>("integrated.json").is_some(), "통합을 다시 만든다");
        assert!(pending_imports(&root).is_empty());

        // 3) 버리기 · 원본이 바뀌면 이어서 할 수 없다
        let stop = AtomicBool::new(true);
        let _ = import_resumable(&fa, &root.join("NEW"), true, "NEW", &no_step, &stop);
        let p = pending_imports(&root);
        assert_eq!(p.len(), 1);
        fs::write(&fa, b"changed").unwrap();
        assert!(!pending_imports(&root)[0].resumable, "원본 파일이 바뀌었다");
        discard_pending(&root, &p[0].id).unwrap();
        assert!(pending_imports(&root).is_empty());
        assert!(!fs::read_dir(&root).unwrap().flatten().any(|e| e.file_name().to_string_lossy().starts_with(".import-")));
        fs::remove_dir_all(&base).unwrap();
    }

    #[tokio::test]
    async fn import_reports_steps() {
        let base = std::env::temp_dir().join(format!("sqls-bundle-steps-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let a = Store::open(base.join("A")).unwrap();
        analyze(&a, "P", 100).await;
        let fa = base.join("a.sqla");
        export(&a, &fa, Manifest::new("QA", "0")).unwrap();
        let b = Store::open(base.join("B")).unwrap();
        analyze(&b, "R", 200).await;
        let fb = base.join("b.sqla");
        export(&b, &fb, Manifest::new("QA", "0")).unwrap();
        let seen = std::sync::Mutex::new(Vec::<(Step, usize, usize)>::new());
        let on = |s: Step, d: usize, t: usize| seen.lock().unwrap().push((s, d, t));
        let steps = |v: &[(Step, usize, usize)]| {
            let mut out: Vec<Step> = Vec::new();
            for (s, _, _) in v {
                if out.last() != Some(s) {
                    out.push(*s);
                }
            }
            out
        };
        // 빈 곳: 풀고 끝
        let qa = base.join("QA");
        import_to_with(&fa, &qa, false, &on).unwrap();
        let v = std::mem::take(&mut *seen.lock().unwrap());
        assert_eq!(steps(&v), vec![Step::Unzip, Step::Finish], "{v:?}");
        let (_, d, t) = v.iter().rev().find(|x| x.0 == Step::Unzip).unwrap();
        assert!(t > &0 && d == t, "푸는 항목 수가 끝까지: {v:?}");
        // 결과가 있는 곳: 풀고 · 합치고 · 통합을 다시 만들고 · 끝
        import_to_with(&fb, &qa, false, &on).unwrap();
        let v = std::mem::take(&mut *seen.lock().unwrap());
        assert_eq!(steps(&v), vec![Step::Unzip, Step::Merge, Step::Integrate, Step::Finish], "{v:?}");
        assert!(v.iter().any(|x| x.0 == Step::Merge && x.2 == 1), "합칠 단위 수: {v:?}");
        fs::remove_dir_all(&base).unwrap();
    }

    #[tokio::test]
    async fn import_merges_into_existing() {
        let base = std::env::temp_dir().join(format!("sqls-bundle-merge-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        // 보낸 쪽: P(100), Q(100)
        let a = Store::open(base.join("A")).unwrap();
        analyze(&a, "P", 100).await;
        analyze(&a, "Q", 100).await;
        let fa = base.join("a.sqla");
        export(&a, &fa, Manifest::new("QA", "0")).unwrap();

        // 접속 프로필 폴더로 (빈 곳) — 가져온 표시를 남기지 않는다: 그 접속의 결과로 보인다
        let qa = base.join("QA");
        Store::open(&qa).unwrap();
        let (_, r) = import_to(&fa, &qa, false).unwrap();
        assert_eq!((r.added, r.merged), (2, false));
        assert!(!qa.join("imported.json").exists());
        // 같은 파일을 또: 그대로 (같은 시각이면 있던 것)
        let (_, r) = import_to(&fa, &qa, false).unwrap();
        assert_eq!((r.added, r.updated, r.kept, r.merged), (0, 0, 2, true));
        assert_eq!(Store::open(&qa).unwrap().units().len(), 2);

        // 이쪽에서 Q 를 더 나중에(300) 다시 분석했고, 보낸 쪽은 P 를 200 에 다시, R 을 새로
        let here = Store::open(&qa).unwrap();
        analyze(&here, "Q", 300).await;
        analyze(&a, "P", 200).await;
        analyze(&a, "R", 200).await;
        let fb = base.join("b.sqla");
        export(&a, &fb, Manifest::new("QA", "0")).unwrap();
        let (_, r) = import_to(&fb, &qa, false).unwrap();
        assert_eq!((r.added, r.updated, r.kept), (1, 1, 1), "R 새로, P 바꿈, Q 는 이쪽이 더 나중");
        let at = |n: &str| here.read_unit(&crate::chunk::unit_key("APP", n, "PROCEDURE")).unwrap().analyzed_at;
        assert_eq!((at("P"), at("Q"), at("R")), (200, 300, 200));
        // 통합 결과도 다시 만든다
        let g: crate::integrate::Integrated = here.read_integrated("integrated.json").unwrap();
        assert_eq!(g.units, 3);
        assert_eq!(import_log(&qa).len(), 3);
        assert!(!fs::read_dir(&base).unwrap().flatten().any(|e| e.file_name().to_string_lossy().starts_with('.')), "임시 폴더를 남기지 않는다");
        assert!(!fs::read_dir(qa.join("units")).unwrap().flatten().any(|e| e.file_name().to_string_lossy().starts_with('.')));

        // 따로 둔 결과(가져옴 표시)를 접속 결과에 합치기
        let set = base.join("QA (가져옴)");
        import_to(&fa, &set, true).unwrap();
        assert!(set.join("imported.json").exists());
        let r = merge_dir(&set, &qa).unwrap();
        assert_eq!((r.added, r.updated, r.kept), (0, 0, 2), "옛 파일의 P·Q 는 이쪽이 더 나중");
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
