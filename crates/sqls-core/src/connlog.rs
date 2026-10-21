//! 접속 기록 — 접속·실패·끊김·다시 접속을 한 줄 JSON 으로 남기고, 사람이 읽을 몇 줄 요약을 만든다.
//!
//! 로그를 밖으로 보낼 수 없는 사내 환경을 전제로 한다: 요약만 읽어 주면 원인을 가를 수 있게
//! (쉬는 동안 끊기는지, 실행 중 끊기는지, 서버가 죽이는지, 접속이 거부되는지).
//! 앱이 [`init`] 하지 않으면(CLI·MCP) 아무것도 쓰지 않는다.

use crate::sync::MutexExt;
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::error::Error;

static SINK: OnceLock<Mutex<PathBuf>> = OnceLock::new();
/// 넘으면 .1 로 밀어 둔다 (둘만)
const MAX_BYTES: u64 = 2 << 20;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Ev {
    /// 유닉스 초
    pub t: u64,
    /// connect_ok · connect_fail · lost · reopen · abandon · recover_ok · recover_fail · meta_ok · meta_fail
    pub kind: String,
    /// 작업 탭 · 사전 조회 · 분석 … (module)
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub who: String,
    /// user@접속 문자열
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub target: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sid: Option<i64>,
    /// 끊김을 안 길: keepalive (쉬는 동안 ping) · call (실행하다)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub via: Option<String>,
    /// 끊기기 전에 쉰 초 (마지막 사용 → 끊김을 안 때)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle: Option<u64>,
    /// 걸린 ms (접속, 끊긴 호출)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ms: Option<u64>,
    /// ORA-03113 · DPI-1010 …
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub msg: String,
}

pub fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// 앱이 시작할 때 한 번 — 이 파일에 쌓는다
pub fn init(path: PathBuf) {
    if let Some(d) = path.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let _ = SINK.set(Mutex::new(path));
}

pub fn path() -> Option<PathBuf> {
    SINK.get().map(|m| m.lk().clone())
}

pub fn record(mut e: Ev) {
    let Some(sink) = SINK.get() else { return };
    if e.t == 0 {
        e.t = now();
    }
    e.msg = e.msg.chars().take(300).collect();
    let p = sink.lk();
    if std::fs::metadata(&*p).map(|m| m.len() > MAX_BYTES).unwrap_or(false) {
        let _ = std::fs::rename(&*p, p.with_extension("1.jsonl"));
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&*p) {
        let _ = writeln!(f, "{}", serde_json::to_string(&e).unwrap_or_default());
    }
}

/// 오류에서 ORA-·DPI-·TNS- 번호
pub fn code_of(e: &Error) -> Option<String> {
    if let Error::Db { code, .. } = e {
        if *code > 0 {
            return Some(format!("ORA-{code:05}"));
        }
    }
    code_in(&e.to_string())
}

pub fn code_in(s: &str) -> Option<String> {
    for p in ["ORA-", "DPI-", "TNS-"] {
        if let Some(i) = s.find(p) {
            let n: String = s[i + 4..].chars().take_while(|c| c.is_ascii_digit()).collect();
            if !n.is_empty() {
                return Some(format!("{p}{n}"));
            }
        }
    }
    None
}

/// 파일에서 since 이후 (회전한 앞 파일까지)
pub fn read(path: &Path, since: u64) -> Vec<Ev> {
    let mut out = Vec::new();
    for p in [path.with_extension("1.jsonl"), path.to_path_buf()] {
        let Ok(text) = std::fs::read_to_string(&p) else { continue };
        out.extend(text.lines().filter_map(|l| serde_json::from_str::<Ev>(l).ok()).filter(|e| e.t >= since));
    }
    out.sort_by_key(|e| e.t);
    out
}

fn mmss(secs: u64) -> String {
    if secs >= 3600 {
        format!("{}시간 {}분", secs / 3600, secs % 3600 / 60)
    } else if secs >= 60 {
        format!("{}분", secs / 60)
    } else {
        format!("{secs}초")
    }
}

fn counts(codes: impl Iterator<Item = String>) -> String {
    let mut m: BTreeMap<String, usize> = BTreeMap::new();
    for c in codes {
        *m.entry(c).or_default() += 1;
    }
    let mut v: Vec<(String, usize)> = m.into_iter().collect();
    v.sort_by(|a, b| b.1.cmp(&a.1));
    v.iter().map(|(c, n)| format!("{c} ×{n}")).collect::<Vec<_>>().join(", ")
}

/// 오류 번호가 뜻하는 것 (접속 실패·끊김에 흔한 것)
pub fn meaning(code: &str) -> Option<&'static str> {
    Some(match code {
        "ORA-03113" | "ORA-03114" | "ORA-03135" | "DPI-1080" | "DPI-1010" | "ORA-12547" | "ORA-12570" | "ORA-12571" => "통신 끊김 (네트워크 장비·서버 쪽에서 끊음)",
        "ORA-00028" => "세션이 죽임을 당함 (DBA·다른 사용자가 KILL SESSION)",
        "ORA-02396" => "DB 프로파일의 IDLE_TIME 초과 (DB 가 쉬는 세션을 끊음)",
        "ORA-01012" => "로그온되어 있지 않음 (세션이 이미 끊김)",
        "ORA-03156" | "DPI-1067" => "응답 시간 초과",
        "ORA-12170" | "TNS-12535" => "접속 시간 초과 (서버에 닿지 않음 — 네트워크·방화벽)",
        "ORA-12541" => "리스너 없음 (주소·포트 확인)",
        "ORA-12514" => "서비스 이름을 리스너가 모름",
        "ORA-12154" => "TNS 별칭을 찾지 못함 (tnsnames.ora)",
        "ORA-12516" | "ORA-12519" | "ORA-12520" => "리스너가 받을 자리가 없음 (DB 세션·프로세스 한도)",
        "ORA-00018" | "ORA-00020" => "DB 세션·프로세스 한도 초과",
        "ORA-02391" => "이 사용자의 동시 세션 수 한도 (SESSIONS_PER_USER)",
        "ORA-01017" => "아이디·비밀번호 틀림",
        "ORA-28000" => "계정 잠김",
        "ORA-28001" => "비밀번호 만료",
        "DPI-1047" => "Oracle Client(64비트) 를 찾지 못함",
        _ => return None,
    })
}

/// 주소·계정을 가린다 ("ERP_READ@dbhost:1521/ORCL" → "접속1")
fn masker(evs: &[Ev]) -> BTreeMap<String, String> {
    let mut m = BTreeMap::new();
    for e in evs {
        if !e.target.is_empty() && !m.contains_key(&e.target) {
            let n = m.len() + 1;
            m.insert(e.target.clone(), format!("접속{n}"));
        }
    }
    m
}

/// 사람이 읽을 요약 — 말로 전해도 원인을 가를 수 있게
pub fn summarize(evs: &[Ev], since: u64, until: u64, mask: bool, app_version: &str) -> String {
    let names = masker(evs);
    let name = |t: &str| if mask { names.get(t).cloned().unwrap_or_default() } else { t.to_string() };
    let day = |t: u64| {
        // UTC 날짜 (시간대 없이 대략) — 기간 표시용
        let d = t / 86400;
        let (y, m, dd) = civil(d as i64);
        format!("{y}-{m:02}-{dd:02}")
    };
    let mut s = format!("SQLStudio {app_version} 접속 진단 ({} ~ {})\n", day(since), day(until));
    let ok: Vec<&Ev> = evs.iter().filter(|e| e.kind == "connect_ok").collect();
    let fail: Vec<&Ev> = evs.iter().filter(|e| e.kind == "connect_fail").collect();
    let lost: Vec<&Ev> = evs.iter().filter(|e| e.kind == "lost").collect();
    let rec_ok = evs.iter().filter(|e| e.kind == "recover_ok" || e.kind == "meta_ok").count();
    let rec_fail = evs.iter().filter(|e| e.kind == "recover_fail" || e.kind == "meta_fail").count();
    if evs.is_empty() {
        s.push_str("- 기록이 없습니다 (이 기간에 접속하지 않았거나, 0.5.3 이전 버전)\n");
        return s;
    }
    s.push_str(&format!("- 접속 {}회, 실패 {}회", ok.len(), fail.len()));
    if !fail.is_empty() {
        s.push_str(&format!(" ({})", counts(fail.iter().map(|e| e.code.clone().unwrap_or_else(|| "번호 없음".into())))));
    }
    if !ok.is_empty() {
        let mut ms: Vec<u64> = ok.iter().filter_map(|e| e.ms).collect();
        ms.sort();
        if let Some(mid) = ms.get(ms.len() / 2) {
            s.push_str(&format!(" · 접속에 걸린 시간 보통 {:.1}초", *mid as f64 / 1000.0));
            if let Some(max) = ms.last().filter(|m| **m > 10_000) {
                s.push_str(&format!(", 가장 오래 {:.0}초", *max as f64 / 1000.0));
            }
        }
    }
    s.push('\n');
    let idle_lost: Vec<&Ev> = lost.iter().copied().filter(|e| e.via.as_deref() == Some("keepalive")).collect();
    let call_lost: Vec<&Ev> = lost.iter().copied().filter(|e| e.via.as_deref() != Some("keepalive")).collect();
    if lost.is_empty() {
        s.push_str("- 끊김 없음\n");
    } else {
        s.push_str(&format!("- 끊김 {}회: 쉬는 동안 {}회, 쓰다가 {}회\n", lost.len(), idle_lost.len(), call_lost.len()));
        let idles: Vec<u64> = lost.iter().filter_map(|e| e.idle).collect();
        if !idles.is_empty() {
            let (mn, mx) = (*idles.iter().min().unwrap(), *idles.iter().max().unwrap());
            s.push_str(&format!("  끊기기 전에 쉰 시간: {} ~ {}\n", mmss(mn), mmss(mx)));
        }
        let long_calls: Vec<u64> = call_lost.iter().filter_map(|e| e.ms).filter(|m| *m > 30_000).collect();
        if !long_calls.is_empty() {
            s.push_str(&format!("  실행 중(30초 넘게 돌던 중)에 끊김 {}회\n", long_calls.len()));
        }
        let codes = counts(lost.iter().filter_map(|e| e.code.clone()));
        if !codes.is_empty() {
            s.push_str(&format!("  오류: {codes}\n"));
        }
        // 어느 접속에서
        let mut per: BTreeMap<String, usize> = BTreeMap::new();
        for e in &lost {
            *per.entry(format!("{} {}", name(&e.target), e.who)).or_default() += 1;
        }
        if per.len() > 1 {
            s.push_str(&format!("  어디서: {}\n", per.iter().map(|(k, n)| format!("{k} {n}회")).collect::<Vec<_>>().join(", ")));
        }
    }
    let panics = evs.iter().filter(|e| e.kind == "panic").count();
    if panics > 0 {
        s.push_str(&format!("- 앱 내부 오류(패닉) {panics}회 — 세션 스레드에서 났으면 그 세션이 닫혀 끊김처럼 보입니다. 로그(sqlstudio.log)의 '패닉' 줄을 확인하세요\n"));
    }
    if rec_ok + rec_fail > 0 {
        s.push_str(&format!("- 다시 접속: {}회 중 {}회 성공\n", rec_ok + rec_fail, rec_ok));
    }
    // 짐작
    let mut guess: Vec<String> = Vec::new();
    let all_codes: Vec<String> = evs.iter().filter_map(|e| e.code.clone()).collect();
    let has = |c: &str| all_codes.iter().any(|x| x == c);
    if has("ORA-02396") {
        guess.push("DB 가 오래 쉬는 세션을 끊습니다 (프로파일 IDLE_TIME). DBA 에게 IDLE_TIME 을 물어보세요.".into());
    }
    if has("ORA-00028") {
        guess.push("누군가 세션을 KILL 했습니다 (DBA 작업·세션 정리 배치).".into());
    }
    if ["ORA-12516", "ORA-12519", "ORA-12520", "ORA-00018", "ORA-00020", "ORA-02391"].iter().any(|c| has(c)) {
        guess.push("세션 한도에 걸립니다. 탭을 줄이거나 DBA 에게 SESSIONS_PER_USER·PROCESSES 를 물어보세요.".into());
    }
    if has("ORA-12170") || has("TNS-12535") {
        guess.push("서버에 닿지 않는 때가 있습니다 (VPN·망 전환·방화벽).".into());
    }
    if idle_lost.len() >= 2 && idle_lost.len() * 2 >= lost.len() && !has("ORA-02396") && !has("ORA-00028") {
        guess.push("주로 쉬는 동안 끊깁니다 — 방화벽·네트워크 장비의 유휴 끊기, 또는 VPN·절전으로 망이 바뀐 경우.".into());
    }
    if call_lost.len() >= 2 && call_lost.len() > idle_lost.len() {
        guess.push("쓰다가 끊깁니다 — 서버 쪽 문제(재시작·오류)나 불안정한 망. 같은 SQL 에서만이면 그 SQL 이 서버 오류를 냅니다.".into());
    }
    for c in all_codes.iter().collect::<std::collections::BTreeSet<_>>() {
        if let Some(m) = meaning(c) {
            if !guess.iter().any(|g| g.contains(c.as_str())) {
                s.push_str(&format!("  · {c}: {m}\n"));
            }
        }
    }
    if !guess.is_empty() {
        s.push_str("- 짐작:\n");
        for g in guess {
            s.push_str(&format!("  {g}\n"));
        }
    }
    s
}

/// 1970-01-01 부터 날 수 → (년, 월, 일)
fn civil(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(kind: &str, via: Option<&str>, idle: Option<u64>, code: Option<&str>) -> Ev {
        Ev { t: 1_790_000_000, kind: kind.into(), who: "작업 탭".into(), target: "APP@db:1521/ORCL".into(), via: via.map(String::from), idle, code: code.map(String::from), ms: Some(800), ..Default::default() }
    }

    #[test]
    fn summary_reads_like_a_report() {
        let evs = vec![
            ev("connect_ok", None, None, None),
            ev("connect_ok", None, None, None),
            ev("lost", Some("keepalive"), Some(1900), Some("DPI-1010")),
            ev("lost", Some("keepalive"), Some(3500), Some("ORA-03113")),
            ev("lost", Some("call"), Some(10), Some("ORA-03113")),
            ev("recover_ok", None, None, None),
            ev("recover_ok", None, None, None),
            ev("recover_fail", None, None, None),
            ev("connect_fail", None, None, Some("ORA-12170")),
        ];
        let s = summarize(&evs, 1_789_400_000, 1_790_000_000, true, "0.5.3");
        assert!(s.contains("접속 2회, 실패 1회 (ORA-12170 ×1)"), "{s}");
        assert!(s.contains("끊김 3회: 쉬는 동안 2회, 쓰다가 1회"), "{s}");
        assert!(s.contains("쉰 시간: 10초 ~ 58분"), "{s}");
        assert!(s.contains("ORA-03113 ×2"), "{s}");
        assert!(s.contains("3회 중 2회 성공"), "{s}");
        assert!(s.contains("주로 쉬는 동안 끊깁니다"), "{s}");
        assert!(s.contains("서버에 닿지 않는"), "{s}");
        assert!(!s.contains("db:1521"), "주소를 가린다: {s}");
        assert_eq!(code_in("DPI-1010: not connected"), Some("DPI-1010".into()));
        assert_eq!(code_in("오류 ORA-12170: TNS:Connect timeout"), Some("ORA-12170".into()));
        assert_eq!(civil(20_000), (2024, 10, 4));
    }
}
