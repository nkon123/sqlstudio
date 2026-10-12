//! 프로젝트(사내 Git) — PC 에 설치된 git 명령을 대신 실행한다.
//!
//! - 인증은 git 이 한다: HTTPS 는 Windows 자격 증명 관리자(Git Credential Manager) 창, SSH 는 기존 키.
//!   앱은 비밀번호·토큰을 받지도 저장하지도 않는다. 터미널 질문은 막는다(GIT_TERMINAL_PROMPT=0) — 창 없는 프로세스가 멈추지 않게.
//! - 사내 인증서·프록시는 git 설정을 그대로 쓴다.
//! - 셸을 거치지 않고 인자를 따로 넘긴다. 경로는 `--` 뒤에 둔다. '-' 로 시작하는 주소·브랜치 이름은 거절한다 (옵션으로 읽히지 않게).
//! - 풀은 기본 빨리 감기만, 다시 쌓기(rebase)는 사용자가 고를 때만. 충돌이 나면 되돌리고 알린다. 강제 푸시는 없다.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde::Serialize;

use crate::state::ErrView;

type R<T> = Result<T, ErrView>;

const LOCAL: Duration = Duration::from_secs(60);
const NETWORK: Duration = Duration::from_secs(15 * 60);

struct Out {
    ok: bool,
    stdout: Vec<u8>,
    stderr: String,
}

impl Out {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.stdout).trim_end().to_string()
    }
    /// git 의 오류 문구 (마지막 몇 줄)
    fn err(&self) -> String {
        let s = self.stderr.trim();
        let s = if s.is_empty() { String::from_utf8_lossy(&self.stdout).trim().to_string() } else { s.to_string() };
        let lines: Vec<&str> = s.lines().filter(|l| !l.trim().is_empty()).collect();
        lines[lines.len().saturating_sub(6)..].join("\n")
    }
}

async fn git(dir: Option<&Path>, args: &[&str], timeout: Duration) -> R<Out> {
    let mut c = tokio::process::Command::new("git");
    c.args(["-c", "core.quotepath=false", "-c", "color.ui=false", "-c", "protocol.ext.allow=never", "-c", "core.longpaths=true"]);
    if let Some(d) = dir {
        c.arg("-C").arg(d);
    }
    c.args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        // 오류 문구를 영어로 받아 종류를 가린다 (화면에는 우리 말로 바꿔 보인다)
        .env("LC_ALL", "C")
        .env("LANG", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW — 콘솔 창이 깜빡이지 않게
    let child = c.spawn().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            ErrView::msg("git_missing", "git 이 설치되어 있지 않습니다. Git for Windows 를 설치하세요 (https://git-scm.com).")
        } else {
            ErrView::msg("git", format!("git 을 실행하지 못했습니다: {e}"))
        }
    })?;
    let out = tokio::time::timeout(timeout, child.wait_with_output())
        .await
        .map_err(|_| ErrView::msg("git", format!("git {} 이 {}초 안에 끝나지 않아 멈췄습니다", args.first().unwrap_or(&""), timeout.as_secs())))?
        .map_err(|e| ErrView::msg("git", e.to_string()))?;
    Ok(Out { ok: out.status.success(), stdout: out.stdout, stderr: String::from_utf8_lossy(&out.stderr).into_owned() })
}

/// 성공해야 하는 명령
async fn run(root: &Path, args: &[&str], timeout: Duration) -> R<Out> {
    let o = git(Some(root), args, timeout).await?;
    if !o.ok {
        return Err(ErrView::msg("git", o.err()));
    }
    Ok(o)
}

fn no_dash(what: &str, s: &str) -> R<()> {
    if s.trim().is_empty() || s.starts_with('-') {
        return Err(ErrView::msg("invalid", format!("{what} 이(가) 올바르지 않습니다: {s}")));
    }
    Ok(())
}

fn root_of(root: &str) -> R<PathBuf> {
    let p = PathBuf::from(root);
    if !p.is_dir() {
        return Err(ErrView::msg("invalid", format!("폴더가 없습니다: {root}")));
    }
    Ok(p)
}

// ── 열기·복제 ─────────────────────────────────────

#[tauri::command]
pub async fn git_version() -> R<String> {
    let o = git(None, &["--version"], LOCAL).await?;
    Ok(o.text().trim_start_matches("git version ").to_string())
}

#[derive(Serialize)]
pub struct RepoInfo {
    root: String,
    name: String,
    remote: Option<String>,
}

/// 폴더가 속한 저장소의 맨 위
#[tauri::command]
pub async fn git_open(path: String) -> R<RepoInfo> {
    let dir = root_of(&path)?;
    let o = git(Some(&dir), &["rev-parse", "--show-toplevel"], LOCAL).await?;
    if !o.ok {
        return Err(ErrView::msg("not_repo", format!("Git 저장소가 아닙니다: {path}")));
    }
    let root = o.text();
    let remote = git(Some(Path::new(&root)), &["remote", "get-url", "origin"], LOCAL).await.ok().filter(|o| o.ok).map(|o| strip_userinfo(&o.text()));
    let name = root.rsplit(['/', '\\']).next().unwrap_or(&root).to_string();
    Ok(RepoInfo { root, name, remote })
}

/// 화면에 보이는 주소에서 "사용자:토큰@" 을 지운다
fn strip_userinfo(url: &str) -> String {
    if let Some((scheme, rest)) = url.split_once("://") {
        if let Some((_, host)) = rest.split_once('@').filter(|(u, _)| !u.contains('/')) {
            return format!("{scheme}://{host}");
        }
    }
    url.to_string()
}

#[tauri::command]
pub async fn git_clone(url: String, dir: String) -> R<RepoInfo> {
    let url = url.trim().to_string();
    no_dash("저장소 주소", &url)?;
    let target = PathBuf::from(dir.trim());
    if target.exists() && std::fs::read_dir(&target).map(|mut d| d.next().is_some()).unwrap_or(true) {
        return Err(ErrView::msg("invalid", format!("비어 있지 않은 폴더입니다: {}", target.display())));
    }
    let t = target.to_string_lossy().to_string();
    let o = git(None, &["clone", "--", &url, &t], NETWORK).await?;
    if !o.ok {
        return Err(ErrView::msg(classify(&o.stderr), o.err()));
    }
    git_open(t).await
}

// ── 상태 ─────────────────────────────────────────

#[derive(Serialize, Debug, PartialEq)]
pub struct FileState {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub orig: Option<String>,
    /// 화면용 한 글자: M 수정 · A 추가 · D 삭제 · R 이름 바뀜 · ? 새 파일(추적 안 함) · U 충돌
    pub code: char,
}

#[derive(Serialize, Debug, Default, PartialEq)]
pub struct Status {
    pub branch: Option<String>,
    pub upstream: Option<String>,
    pub ahead: u32,
    pub behind: u32,
    pub files: Vec<FileState>,
}

fn code_of(xy: &str) -> char {
    let (x, y) = (xy.chars().next().unwrap_or('.'), xy.chars().nth(1).unwrap_or('.'));
    for c in [y, x] {
        match c {
            'D' => return 'D',
            'A' => return 'A',
            'R' | 'C' => return 'R',
            _ => {}
        }
    }
    'M'
}

/// `git status --porcelain=v2 --branch -z` 읽기
pub fn parse_status(raw: &[u8]) -> Status {
    let s = String::from_utf8_lossy(raw);
    let mut st = Status::default();
    let mut it = s.split('\0');
    while let Some(rec) = it.next() {
        if rec.is_empty() {
            continue;
        }
        if let Some(h) = rec.strip_prefix("# ") {
            if let Some(b) = h.strip_prefix("branch.head ") {
                st.branch = (b != "(detached)").then(|| b.to_string());
            } else if let Some(u) = h.strip_prefix("branch.upstream ") {
                st.upstream = Some(u.to_string());
            } else if let Some(ab) = h.strip_prefix("branch.ab ") {
                for p in ab.split(' ') {
                    if let Some(a) = p.strip_prefix('+') {
                        st.ahead = a.parse().unwrap_or(0);
                    } else if let Some(b) = p.strip_prefix('-') {
                        st.behind = b.parse().unwrap_or(0);
                    }
                }
            }
            continue;
        }
        let kind = rec.as_bytes()[0];
        match kind {
            b'1' => {
                let f: Vec<&str> = rec.splitn(9, ' ').collect();
                if f.len() == 9 {
                    st.files.push(FileState { path: f[8].to_string(), orig: None, code: code_of(f[1]) });
                }
            }
            b'2' => {
                let f: Vec<&str> = rec.splitn(10, ' ').collect();
                let orig = it.next().map(|x| x.to_string());
                if f.len() == 10 {
                    st.files.push(FileState { path: f[9].to_string(), orig, code: 'R' });
                }
            }
            b'u' => {
                let f: Vec<&str> = rec.splitn(11, ' ').collect();
                if f.len() == 11 {
                    st.files.push(FileState { path: f[10].to_string(), orig: None, code: 'U' });
                }
            }
            b'?' => st.files.push(FileState { path: rec[2..].to_string(), orig: None, code: '?' }),
            _ => {}
        }
    }
    st
}

#[tauri::command]
pub async fn git_status(root: String) -> R<Status> {
    let r = root_of(&root)?;
    let o = run(&r, &["status", "--porcelain=v2", "--branch", "-z", "--untracked-files=all"], LOCAL).await?;
    Ok(parse_status(&o.stdout))
}

/// 트리에 보일 파일 (추적 중 + 무시하지 않는 새 파일)
#[tauri::command]
pub async fn git_files(root: String) -> R<Vec<String>> {
    let r = root_of(&root)?;
    let o = run(&r, &["ls-files", "-z", "--cached", "--others", "--exclude-standard"], LOCAL).await?;
    let mut v: Vec<String> = String::from_utf8_lossy(&o.stdout).split('\0').filter(|x| !x.is_empty()).map(|x| x.to_string()).collect();
    v.sort();
    v.dedup();
    Ok(v)
}

#[derive(Serialize)]
pub struct Diff {
    /// 마지막 커밋의 내용 (새 파일이면 없음)
    old: Option<String>,
    /// 디스크의 내용 (지운 파일이면 없음)
    new: Option<String>,
    encoding: String,
    binary: bool,
}

fn is_binary(b: &[u8]) -> bool {
    b[..b.len().min(8000)].contains(&0)
}

#[tauri::command]
pub async fn git_diff(root: String, path: String) -> R<Diff> {
    let r = root_of(&root)?;
    no_dash("경로", &path)?;
    // --filters: 체크아웃할 때처럼 줄 끝(autocrlf)·필터를 적용한 내용 — 디스크 파일과 같은 기준으로 비교한다
    let head = git(Some(&r), &["cat-file", "--filters", &format!("HEAD:{path}")], LOCAL).await?;
    let old_bytes = head.ok.then_some(head.stdout);
    let new_bytes = std::fs::read(r.join(&path)).ok();
    let binary = old_bytes.as_deref().is_some_and(is_binary) || new_bytes.as_deref().is_some_and(is_binary);
    if binary {
        return Ok(Diff { old: None, new: None, encoding: String::new(), binary });
    }
    let new = new_bytes.as_deref().map(crate::files::decode);
    let old = old_bytes.as_deref().map(crate::files::decode);
    let encoding = new.as_ref().or(old.as_ref()).map(|f| f.encoding.to_string()).unwrap_or_else(|| "utf-8".into());
    Ok(Diff { old: old.map(|f| f.text), new: new.map(|f| f.text), encoding, binary })
}

// ── 커밋·되돌리기 ─────────────────────────────────

/// 고른 파일만 커밋한다 (다른 데서 스테이징해 둔 것은 섞이지 않는다). 새 커밋의 짧은 해시.
#[tauri::command]
pub async fn git_commit(root: String, paths: Vec<String>, message: String) -> R<String> {
    let r = root_of(&root)?;
    if message.trim().is_empty() {
        return Err(ErrView::msg("invalid", "커밋 메시지를 쓰세요"));
    }
    if paths.is_empty() {
        return Err(ErrView::msg("invalid", "커밋할 파일을 고르세요"));
    }
    let mut add = vec!["add", "-A", "--"];
    add.extend(paths.iter().map(|s| s.as_str()));
    run(&r, &add, LOCAL).await?;
    let mut commit = vec!["commit", "-m", message.trim(), "--"];
    commit.extend(paths.iter().map(|s| s.as_str()));
    let o = git(Some(&r), &commit, LOCAL).await?;
    if !o.ok {
        let e = o.err();
        if e.contains("tell me who you are") || e.contains("user.email") || e.contains("user.name") {
            return Err(ErrView::msg("identity", "커밋 작성자(이름·이메일)가 설정되어 있지 않습니다"));
        }
        return Err(ErrView::msg("git", e));
    }
    Ok(run(&r, &["rev-parse", "--short", "HEAD"], LOCAL).await?.text())
}

#[derive(Serialize)]
pub struct Identity {
    name: String,
    email: String,
}

#[tauri::command]
pub async fn git_identity(root: String) -> R<Identity> {
    let r = root_of(&root)?;
    let get = |k: &'static str| {
        let r = r.clone();
        async move { git(Some(&r), &["config", k], LOCAL).await.map(|o| if o.ok { o.text() } else { String::new() }) }
    };
    Ok(Identity { name: get("user.name").await?, email: get("user.email").await? })
}

/// global 이면 이 PC 의 모든 저장소, 아니면 이 저장소만
#[tauri::command]
pub async fn git_set_identity(root: String, name: String, email: String, global: bool) -> R<()> {
    let r = root_of(&root)?;
    if name.trim().is_empty() || !email.contains('@') {
        return Err(ErrView::msg("invalid", "이름과 이메일을 쓰세요"));
    }
    let scope = if global { "--global" } else { "--local" };
    run(&r, &["config", scope, "user.name", name.trim()], LOCAL).await?;
    run(&r, &["config", scope, "user.email", email.trim()], LOCAL).await?;
    Ok(())
}

/// 변경 취소. 마지막 커밋에 있던 파일은 그 내용으로, 없던 파일(새 파일)은 지운다.
#[tauri::command]
pub async fn git_discard(root: String, paths: Vec<String>) -> R<()> {
    let r = root_of(&root)?;
    for p in &paths {
        no_dash("경로", p)?;
        let in_head = git(Some(&r), &["cat-file", "-e", &format!("HEAD:{p}")], LOCAL).await?.ok;
        if in_head {
            run(&r, &["restore", "--source=HEAD", "--staged", "--worktree", "--", p], LOCAL).await?;
        } else {
            run(&r, &["rm", "--cached", "--quiet", "--ignore-unmatch", "--", p], LOCAL).await?;
            let f = r.join(p);
            if f.is_file() {
                std::fs::remove_file(&f).map_err(|e| ErrView::msg("io", format!("{}: {e}", f.display())))?;
            }
        }
    }
    Ok(())
}

// ── 브랜치 ───────────────────────────────────────

#[derive(Serialize)]
pub struct Branches {
    current: Option<String>,
    local: Vec<String>,
    remote: Vec<String>,
}

#[tauri::command]
pub async fn git_branches(root: String) -> R<Branches> {
    let r = root_of(&root)?;
    let cur = git(Some(&r), &["symbolic-ref", "--quiet", "--short", "HEAD"], LOCAL).await?;
    let refs = |pat: &'static str| {
        let r = r.clone();
        async move {
            let o = run(&r, &["for-each-ref", "--format=%(refname:short)", pat], LOCAL).await?;
            Ok::<Vec<String>, ErrView>(o.text().lines().filter(|l| !l.is_empty()).map(|l| l.to_string()).collect())
        }
    };
    let local = refs("refs/heads").await?;
    // 원격 브랜치는 로컬에 같은 이름이 없는 것만 ("origin/HEAD" 같은 가리킴은 뺀다)
    let remote = refs("refs/remotes")
        .await?
        .into_iter()
        .filter(|b| b.contains('/') && !b.ends_with("/HEAD"))
        .filter(|b| !local.iter().any(|l| b.split_once('/').map(|x| x.1) == Some(l.as_str())))
        .collect();
    Ok(Branches { current: cur.ok.then(|| cur.text()), local, remote })
}

/// 브랜치 옮기기. create 면 새로 만들어 옮긴다. "origin/x" 면 같은 이름의 로컬 브랜치를 만들어 따라간다.
#[tauri::command]
pub async fn git_switch(root: String, name: String, create: bool) -> R<()> {
    let r = root_of(&root)?;
    let name = name.trim().to_string();
    no_dash("브랜치 이름", &name)?;
    let ok = git(Some(&r), &["check-ref-format", "--branch", &name], LOCAL).await?.ok;
    if !ok {
        return Err(ErrView::msg("invalid", format!("브랜치 이름으로 쓸 수 없습니다: {name}")));
    }
    let remote_branch = git(Some(&r), &["show-ref", "--verify", "--quiet", &format!("refs/remotes/{name}")], LOCAL).await?.ok;
    let o = if create {
        git(Some(&r), &["switch", "-c", &name], LOCAL).await?
    } else if remote_branch {
        git(Some(&r), &["switch", "--track", &name], LOCAL).await?
    } else {
        git(Some(&r), &["switch", &name], LOCAL).await?
    };
    if !o.ok {
        let e = o.err();
        if e.contains("would be overwritten") {
            return Err(ErrView::msg("dirty", format!("커밋하지 않은 변경이 있어 브랜치를 옮길 수 없습니다. 먼저 커밋하거나 되돌리세요.\n\n{e}")));
        }
        return Err(ErrView::msg("git", e));
    }
    Ok(())
}

// ── 서버와 주고받기 ──────────────────────────────

/// 오류 종류 — 화면이 안내를 고른다
fn classify(stderr: &str) -> &'static str {
    let e = stderr.to_ascii_lowercase();
    if e.contains("authentication failed") || e.contains("could not read username") || e.contains("permission denied") || e.contains("403") || e.contains("401") {
        "auth"
    } else if e.contains("ssl certificate") || e.contains("certificate problem") || e.contains("unable to get local issuer") {
        "cert"
    } else if e.contains("could not resolve host") || e.contains("failed to connect") || e.contains("connection timed out") || e.contains("couldn't connect") {
        "network"
    } else if e.contains("[rejected]") || e.contains("non-fast-forward") || e.contains("fetch first") {
        "rejected"
    } else if e.contains("not possible to fast-forward") || e.contains("diverging branches") || e.contains("have diverged") {
        "diverged"
    } else if e.contains("no tracking information") || e.contains("has no upstream") {
        "no_upstream"
    } else if e.contains("would be overwritten") {
        "dirty"
    } else if e.contains("conflict") {
        "conflict"
    } else {
        "git"
    }
}

async fn net(root: &Path, args: &[&str]) -> R<Out> {
    let o = git(Some(root), args, NETWORK).await?;
    if !o.ok {
        return Err(ErrView::msg(classify(&o.stderr), o.err()));
    }
    Ok(o)
}

#[tauri::command]
pub async fn git_fetch(root: String) -> R<()> {
    let r = root_of(&root)?;
    net(&r, &["fetch", "--prune"]).await?;
    Ok(())
}

/// 풀. 기본은 빨리 감기만 — 갈라졌으면 "diverged" 로 알리고, 사용자가 고르면 rebase 로 다시 부른다.
/// rebase 중 충돌이 나면 원래대로 되돌리고 "conflict" 로 알린다.
#[tauri::command]
pub async fn git_pull(root: String, rebase: bool) -> R<String> {
    let r = root_of(&root)?;
    let before = run(&r, &["rev-parse", "HEAD"], LOCAL).await.map(|o| o.text()).unwrap_or_default();
    if rebase {
        let o = git(Some(&r), &["pull", "--rebase", "--autostash"], NETWORK).await?;
        if !o.ok {
            let kind = classify(&o.stderr);
            let in_rebase = r.join(".git").join("rebase-merge").exists() || r.join(".git").join("rebase-apply").exists();
            if in_rebase {
                let _ = git(Some(&r), &["rebase", "--abort"], LOCAL).await;
                return Err(ErrView::msg("conflict", format!("내 커밋을 서버 커밋 위로 다시 쌓다가 충돌이 났습니다. 풀 하기 전 상태로 되돌렸습니다.\n충돌 해결은 다른 Git 도구에서 하세요.\n\n{}", o.err())));
            }
            return Err(ErrView::msg(kind, o.err()));
        }
    } else {
        net(&r, &["pull", "--ff-only"]).await?;
    }
    let after = run(&r, &["rev-parse", "HEAD"], LOCAL).await.map(|o| o.text()).unwrap_or_default();
    if before == after {
        return Ok("이미 최신입니다".into());
    }
    let n = run(&r, &["rev-list", "--count", &format!("{before}..{after}")], LOCAL).await.map(|o| o.text()).unwrap_or_default();
    Ok(format!("커밋 {n}개를 받았습니다"))
}

#[derive(Serialize)]
pub struct Commit {
    hash: String,
    subject: String,
}

/// 아직 올리지 않은 커밋 (어느 원격에도 없는 것)
#[tauri::command]
pub async fn git_outgoing(root: String) -> R<Vec<Commit>> {
    let r = root_of(&root)?;
    let o = git(Some(&r), &["log", "HEAD", "--not", "--remotes", "--format=%h%x1f%s", "-n", "50"], LOCAL).await?;
    if !o.ok {
        return Ok(Vec::new()); // 커밋이 하나도 없는 새 저장소
    }
    Ok(o.text().lines().filter_map(|l| l.split_once('\x1f')).map(|(h, s)| Commit { hash: h.into(), subject: s.into() }).collect())
}

/// 푸시. 따라가는 원격 브랜치가 없으면 origin 에 같은 이름으로 만든다. 강제 푸시는 없다.
#[tauri::command]
pub async fn git_push(root: String) -> R<String> {
    let r = root_of(&root)?;
    let up = git(Some(&r), &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"], LOCAL).await?;
    if up.ok {
        net(&r, &["push"]).await?;
        Ok(format!("{} 에 올렸습니다", up.text()))
    } else {
        net(&r, &["push", "-u", "origin", "HEAD"]).await?;
        Ok("origin 에 새 브랜치로 올렸습니다".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_v2() {
        let raw = "# branch.oid 1234\0# branch.head feature/x\0# branch.upstream origin/feature/x\0# branch.ab +2 -1\0\
1 .M N... 100644 100644 100644 aaa aaa packages/ORDER PKG.pkb\0\
1 A. N... 000000 100644 100644 000 bbb new.sql\0\
1 .D N... 100644 100644 000000 ccc ccc gone.sql\0\
2 R. N... 100644 100644 100644 ddd ddd R100 renamed.sql\0old.sql\0\
u UU N... 100644 100644 100644 100644 e1 e2 e3 conflict.sql\0\
? 한글 파일.sql\0";
        let s = parse_status(raw.as_bytes());
        assert_eq!(s.branch.as_deref(), Some("feature/x"));
        assert_eq!(s.upstream.as_deref(), Some("origin/feature/x"));
        assert_eq!((s.ahead, s.behind), (2, 1));
        let v: Vec<(&str, char)> = s.files.iter().map(|f| (f.path.as_str(), f.code)).collect();
        assert_eq!(v, vec![("packages/ORDER PKG.pkb", 'M'), ("new.sql", 'A'), ("gone.sql", 'D'), ("renamed.sql", 'R'), ("conflict.sql", 'U'), ("한글 파일.sql", '?')]);
        assert_eq!(s.files[3].orig.as_deref(), Some("old.sql"));
        assert_eq!(parse_status(b"# branch.head (detached)\0").branch, None);
    }

    #[test]
    fn userinfo_hidden() {
        assert_eq!(strip_userinfo("https://kim:tok123@git.corp.local/erp/a.git"), "https://git.corp.local/erp/a.git");
        assert_eq!(strip_userinfo("git@git.corp.local:erp/a.git"), "git@git.corp.local:erp/a.git");
        assert_eq!(strip_userinfo("https://git.corp.local/a@b/c.git"), "https://git.corp.local/a@b/c.git");
    }

    #[test]
    fn error_kinds() {
        assert_eq!(classify(" ! [rejected]        main -> main (fetch first)"), "rejected");
        assert_eq!(classify("fatal: Not possible to fast-forward, aborting."), "diverged");
        assert_eq!(classify("fatal: Authentication failed for 'https://git.corp.local/'"), "auth");
        assert_eq!(classify("SSL certificate problem: unable to get local issuer certificate"), "cert");
    }

    /// 실제 git 으로: 저장소를 만들고 → 커밋 → 원격(bare)에 푸시 → 다른 복제본에서 풀
    #[test]
    fn roundtrip_with_real_git() {
        if std::process::Command::new("git").arg("--version").output().is_err() {
            return;
        }
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(async {
            let base = std::env::temp_dir().join(format!("sqls-git-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&base);
            std::fs::create_dir_all(&base).unwrap();
            let s = |p: &Path| p.to_string_lossy().to_string();
            let remote = base.join("remote.git");
            git(None, &["init", "--bare", "-b", "main", &s(&remote)], LOCAL).await.unwrap();
            let a = git_clone(s(&remote), s(&base.join("a"))).await.unwrap();
            git_set_identity(a.root.clone(), "테스트".into(), "t@corp.local".into(), false).await.unwrap();
            git(Some(Path::new(&a.root)), &["symbolic-ref", "HEAD", "refs/heads/main"], LOCAL).await.unwrap();
            std::fs::write(Path::new(&a.root).join("p.sql"), "SELECT 1 FROM dual;\n").unwrap();
            std::fs::write(Path::new(&a.root).join("skip.sql"), "x\n").unwrap();
            let st = git_status(a.root.clone()).await.unwrap();
            assert_eq!(st.files.len(), 2);
            git_commit(a.root.clone(), vec!["p.sql".into()], "첫 커밋".into()).await.unwrap();
            let st = git_status(a.root.clone()).await.unwrap();
            assert_eq!(st.files.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(), vec!["skip.sql"], "고른 파일만 커밋");
            assert_eq!(git_outgoing(a.root.clone()).await.unwrap().len(), 1);
            git_push(a.root.clone()).await.unwrap();
            assert!(git_outgoing(a.root.clone()).await.unwrap().is_empty());

            let b = git_clone(s(&remote), s(&base.join("b"))).await.unwrap();
            git_set_identity(b.root.clone(), "b".into(), "b@corp.local".into(), false).await.unwrap();
            std::fs::write(Path::new(&a.root).join("p.sql"), "SELECT 2 FROM dual;\n").unwrap();
            let d = git_diff(a.root.clone(), "p.sql".into()).await.unwrap();
            // autocrlf 인 PC 에서는 체크아웃한 파일이 CRLF — 비교 기준을 디스크와 맞췄는지 본다
            assert_eq!(d.old.map(|t| t.replace("\r\n", "\n")).as_deref(), Some("SELECT 1 FROM dual;\n"));
            assert_eq!(d.new.map(|t| t.replace("\r\n", "\n")).as_deref(), Some("SELECT 2 FROM dual;\n"));
            git_commit(a.root.clone(), vec!["p.sql".into()], "둘째".into()).await.unwrap();
            git_push(a.root.clone()).await.unwrap();
            assert_eq!(git_pull(b.root.clone(), false).await.unwrap(), "커밋 1개를 받았습니다");
            assert_eq!(std::fs::read_to_string(Path::new(&b.root).join("p.sql")).unwrap().replace("\r\n", "\n"), "SELECT 2 FROM dual;\n");

            // 되돌리기: 고친 파일은 원래대로, 새 파일은 지운다
            std::fs::write(Path::new(&b.root).join("p.sql"), "bad\n").unwrap();
            std::fs::write(Path::new(&b.root).join("new.sql"), "n\n").unwrap();
            git_discard(b.root.clone(), vec!["p.sql".into(), "new.sql".into()]).await.unwrap();
            assert!(git_status(b.root.clone()).await.unwrap().files.is_empty());

            // 갈라진 경우: 푸시 거절 → 빨리 감기 풀 실패 → rebase 풀 → 푸시
            std::fs::write(Path::new(&a.root).join("a.sql"), "a\n").unwrap();
            git_commit(a.root.clone(), vec!["a.sql".into()], "a".into()).await.unwrap();
            git_push(a.root.clone()).await.unwrap();
            std::fs::write(Path::new(&b.root).join("b.sql"), "b\n").unwrap();
            git_commit(b.root.clone(), vec!["b.sql".into()], "b".into()).await.unwrap();
            assert_eq!(git_push(b.root.clone()).await.unwrap_err().kind, "rejected");
            assert_eq!(git_pull(b.root.clone(), false).await.unwrap_err().kind, "diverged");
            git_pull(b.root.clone(), true).await.unwrap();
            git_push(b.root.clone()).await.unwrap();

            // 브랜치
            git_switch(b.root.clone(), "feature/x".into(), true).await.unwrap();
            let br = git_branches(b.root.clone()).await.unwrap();
            assert_eq!(br.current.as_deref(), Some("feature/x"));
            assert!(br.local.contains(&"main".to_string()));
            assert!(git_switch(b.root.clone(), "-bad".into(), true).await.is_err());
            let _ = std::fs::remove_dir_all(&base);
        });
    }
}
