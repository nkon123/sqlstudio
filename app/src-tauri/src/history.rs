//! SQL 실행 이력 — 실행한 문장을 이 PC 에 남기고 찾아 다시 쓴다 (Toad 의 SQL History, F8).
//!
//! - 위치: 설정 파일 옆 history\YYYY-MM.jsonl (한 줄에 한 문장). 이 PC 밖으로 나가지 않는다.
//! - 실행을 늦추지 않게 기록은 별도 스레드가 한다 (채널로 넘기고 바로 돌아온다).
//! - 바인드 변수 값은 남기지 않는다. 설정에서 끄거나 지울 수 있다.
//! - keep_months 보다 오래된 달 파일은 시작할 때 지운다.

use sqls_core::sync::MutexExt;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

/// 문장 하나의 상한 — 큰 스크립트 붙여 넣기로 파일이 부풀지 않게
const MAX_SQL: usize = 100_000;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    /// 실행 시각 (유닉스 초)
    pub t: u64,
    pub profile: String,
    pub user: String,
    pub sql: String,
    pub kind: String,
    pub ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rows: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct Row {
    #[serde(flatten)]
    pub entry: Entry,
    /// 같은 문장을 실행한 횟수 (찾은 범위 안에서)
    pub count: u32,
}

pub struct History {
    dir: PathBuf,
    enabled: AtomicBool,
    tx: Mutex<Option<Sender<Entry>>>,
}

fn month_of(t: u64) -> String {
    // 유닉스 초 → YYYY-MM (UTC 기준 달 — 파일 나누기용이라 충분하다)
    let days = (t / 86_400) as i64;
    let (y, m, _) = civil_from_days(days);
    format!("{y:04}-{m:02}")
}

/// 1970-01-01 부터의 날 수 → (년, 월, 일)  (Howard Hinnant 의 알고리즘)
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

pub fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

impl History {
    pub fn new(dir: PathBuf, enabled: bool, keep_months: u32) -> Self {
        prune(&dir, keep_months);
        let (tx, rx) = channel::<Entry>();
        let wdir = dir.clone();
        std::thread::Builder::new()
            .name("sql-history".into())
            .spawn(move || {
                for e in rx {
                    if let Err(err) = append(&wdir, &e) {
                        tracing::warn!("실행 이력 기록 실패: {err}");
                    }
                }
            })
            .ok();
        Self { dir, enabled: AtomicBool::new(enabled), tx: Mutex::new(Some(tx)) }
    }

    pub fn enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    pub fn set_enabled(&self, on: bool) {
        self.enabled.store(on, Ordering::Relaxed);
    }

    /// 기록 — 바로 돌아온다
    pub fn record(&self, mut e: Entry) {
        if !self.enabled() || e.sql.trim().is_empty() {
            return;
        }
        if e.sql.len() > MAX_SQL {
            let mut cut = MAX_SQL;
            while !e.sql.is_char_boundary(cut) {
                cut -= 1;
            }
            e.sql.truncate(cut);
        }
        if let Some(tx) = self.tx.lk().as_ref() {
            let _ = tx.send(e);
        }
    }

    /// 최근 것부터 찾는다. 같은 문장(공백 차이 무시)은 가장 최근 하나로 묶고 횟수를 센다 (찾은 범위 안에서 —
    /// limit 개를 채우면 거기서 멈춘다. 그래서 오래된 달 파일은 필요할 때만 읽는다).
    /// 줄을 JSON 으로 풀기 전에 원문 줄에서 낱말을 먼저 본다 — 맞지 않는 줄은 풀지 않는다.
    pub fn search(&self, query: &str, profile: Option<&str>, limit: usize) -> Vec<Row> {
        let words: Vec<String> = query.split_whitespace().map(|w| w.to_uppercase()).collect();
        let mut out: Vec<Row> = Vec::new();
        let mut index: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        'files: for file in month_files(&self.dir).into_iter().rev() {
            let Ok(text) = std::fs::read_to_string(&file) else { continue };
            for l in text.lines().rev() {
                if out.len() >= limit {
                    break 'files;
                }
                if !words.iter().all(|w| maybe_contains(l, w)) {
                    continue;
                }
                let Ok(e) = serde_json::from_str::<Entry>(l) else { continue };
                if profile.is_some_and(|p| !e.profile.eq_ignore_ascii_case(p)) {
                    continue;
                }
                let up = e.sql.to_uppercase();
                if !words.iter().all(|w| up.contains(w.as_str())) {
                    continue;
                }
                let key = up.split_whitespace().collect::<Vec<_>>().join(" ");
                match index.get(&key) {
                    Some(&i) => out[i].count += 1,
                    None => {
                        if out.len() >= limit {
                            continue;
                        }
                        index.insert(key, out.len());
                        out.push(Row { entry: e, count: 1 });
                    }
                }
            }
        }
        out
    }

    /// 모두 지운다 (진행 중인 기록은 다음 줄부터 새 파일에)
    pub fn clear(&self) -> std::io::Result<()> {
        for f in month_files(&self.dir) {
            std::fs::remove_file(f)?;
        }
        Ok(())
    }
}

/// 원문 JSON 줄에 낱말(대문자로 바꾼 것)이 있을 수 있는지 — 거짓이면 확실히 없다, 참이면 풀어서 다시 본다.
/// ASCII 낱말은 대소문자 무시로, 대소문자가 없는 글자(한글 등)는 그대로 찾는다.
/// JSON 이 바꿔 적는 글자(" \)가 든 낱말이나 대소문자가 있는 비ASCII 글자는 판단하지 않는다 (참)
fn maybe_contains(line: &str, word_upper: &str) -> bool {
    if word_upper.contains(['"', '\\']) {
        return true;
    }
    if word_upper.is_ascii() {
        let (h, n) = (line.as_bytes(), word_upper.as_bytes());
        let Some(&first) = n.first() else { return true };
        // 첫 글자(대·소문자)가 나오는 곳만 맞춰 본다
        let (a, b) = (first.to_ascii_uppercase(), first.to_ascii_lowercase());
        let mut i = 0;
        while let Some(p) = memchr::memchr2(a, b, &h[i..]) {
            let s = i + p;
            if h.len() - s < n.len() {
                return false;
            }
            if h[s..s + n.len()].eq_ignore_ascii_case(n) {
                return true;
            }
            i = s + 1;
        }
        return false;
    }
    if word_upper.chars().all(|c| !c.is_alphabetic() || (c.is_uppercase() == c.is_lowercase())) {
        return line.contains(word_upper);
    }
    true
}

fn append(dir: &Path, e: &Entry) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(dir.join(format!("{}.jsonl", month_of(e.t))))?;
    let mut line = serde_json::to_string(e).map_err(std::io::Error::other)?;
    line.push('\n');
    f.write_all(line.as_bytes())
}

/// YYYY-MM.jsonl 파일들 (오래된 것부터)
fn month_files(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|d| {
            d.filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.extension().is_some_and(|x| x == "jsonl") && p.file_stem().and_then(|s| s.to_str()).is_some_and(|s| s.len() == 7 && s.as_bytes()[4] == b'-'))
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

fn prune(dir: &Path, keep_months: u32) {
    if keep_months == 0 {
        return;
    }
    let files = month_files(dir);
    if files.len() > keep_months as usize {
        for f in &files[..files.len() - keep_months as usize] {
            let _ = std::fs::remove_file(f);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn months() {
        assert_eq!(month_of(0), "1970-01");
        assert_eq!(month_of(1_791_763_200), "2026-10"); // 2026-10-12
        assert_eq!(month_of(951_782_400), "2000-02"); // 2000-02-29
    }

    #[test]
    fn record_search_dedupe_prune() {
        let dir = std::env::temp_dir().join(format!("sqls-hist-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        // 오래된 달 파일 3개 → keep 2 로 열면 1개 지움
        std::fs::create_dir_all(&dir).unwrap();
        for m in ["2020-01", "2020-02", "2020-03"] {
            std::fs::write(dir.join(format!("{m}.jsonl")), "").unwrap();
        }
        let h = History::new(dir.clone(), true, 2);
        assert!(!dir.join("2020-01.jsonl").exists());
        let e = |sql: &str, p: &str, t: u64| Entry { t, profile: p.into(), user: "APP".into(), sql: sql.into(), kind: "query".into(), ms: 3, rows: Some(1), error: None };
        h.record(e("select * from orders", "DEV", 1_791_763_200));
        h.record(e("SELECT *  FROM orders", "DEV", 1_791_763_260));
        h.record(e("update stock set qty = 0 where id = 1", "PROD", 1_791_763_300));
        h.record(e("   ", "DEV", 1_791_763_300));
        // 기록은 다른 스레드 — 파일에 닿을 때까지 잠깐 기다린다
        for _ in 0..50 {
            if h.search("", None, 10).len() == 2 && h.search("", None, 10).iter().map(|r| r.count).sum::<u32>() == 3 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let all = h.search("", None, 10);
        assert_eq!(all.len(), 2, "공백만 다른 문장은 하나로");
        assert_eq!(all[0].entry.profile, "PROD", "최근 것부터");
        assert_eq!(all[1].count, 2);
        assert_eq!(all[1].entry.sql, "SELECT *  FROM orders", "묶을 때는 가장 최근 원문");
        assert_eq!(h.search("orders", Some("dev"), 10).len(), 1);
        assert!(h.search("stock qty", None, 10)[0].entry.sql.starts_with("update"));
        assert!(h.search("orders", Some("PROD"), 10).is_empty());
        h.set_enabled(false);
        h.record(e("select 1 from dual", "DEV", 1_791_763_400));
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert!(h.search("dual", None, 10).is_empty(), "끄면 남기지 않는다");
        h.clear().unwrap();
        assert!(h.search("", None, 10).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn raw_line_prefilter() {
        let l = r#"{"t":1,"profile":"DEV","user":"APP","sql":"select * from Orders where 상태 = '취소'\n and x = \"Q\"","kind":"query","ms":1}"#;
        assert!(maybe_contains(l, "ORDERS"), "대소문자 무시");
        assert!(maybe_contains(l, "취소"), "한글 그대로");
        assert!(!maybe_contains(l, "STOCK"), "없는 낱말");
        assert!(!maybe_contains(l, "배송"), "없는 한글");
        assert!(maybe_contains(l, "\"Q\""), "JSON 이 바꿔 적는 글자가 든 낱말은 풀어서 본다");
        assert!(!maybe_contains("ab", "ABC"), "끝에서 모자람");
        assert!(maybe_contains(l, "'취소'"), "따옴표·한글 섞임");
    }

    /// 많이 쓴 사람의 이력(6달 × 4만 줄)에서 찾는 시간 — cargo test --release -p sqlstudio history_bench -- --ignored --nocapture
    #[test]
    #[ignore]
    fn history_bench() {
        let dir = std::env::temp_dir().join(format!("sqls-hist-bench-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let base = now() - 150 * 86_400;
        for m in 0..6u64 {
            let mut s = String::new();
            for i in 0..40_000u64 {
                let t = base + m * 25 * 86_400 + i * 50;
                let e = Entry { t, profile: ["ERP 운영", "ERP 개발", "MES"][(i % 3) as usize].into(), user: "APP".into(),
                    sql: format!("SELECT o.order_id, o.cust_id, o.status, SUM(i.qty * i.price) amt\n  FROM orders o JOIN order_items i ON i.order_id = o.order_id\n WHERE o.order_dt >= TRUNC(SYSDATE) - {}\n   AND o.status = '{}'\n GROUP BY o.order_id, o.cust_id, o.status", i % 977, ["접수", "취소", "배송"][(i % 3) as usize]),
                    kind: "query".into(), ms: 12, rows: Some(3), error: None };
                s.push_str(&serde_json::to_string(&e).unwrap());
                s.push('\n');
            }
            std::fs::write(dir.join(format!("{}.jsonl", month_of(base + m * 25 * 86_400 + 20_000 * 50))), s).unwrap();
        }
        let h = History::new(dir.clone(), true, 120);
        for (q, p) in [("", None), ("", Some("MES")), ("취소 - 976", None), ("nothing_matches_this", None)] {
            let t = std::time::Instant::now();
            let r = h.search(q, p, 200);
            println!("찾기 {q:?} {p:?}: {}개 · {:?}", r.len(), t.elapsed());
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
