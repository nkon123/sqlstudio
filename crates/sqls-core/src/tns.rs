//! tnsnames.ora — 접속 별칭 목록 (Toad 처럼 별칭을 골라 접속하게).
//!
//! 접속 자체는 Oracle Client 가 별칭을 푼다. 여기서는 화면에 보일 목록을 만든다:
//! 어느 파일을 쓰는지 찾고, 별칭과 호스트·포트·서비스를 읽는다 (IFILE 포함).

use std::path::{Path, PathBuf};

use serde::Serialize;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct TnsEntry {
    pub alias: String,
    pub host: Option<String>,
    pub port: Option<String>,
    /// SERVICE_NAME, 없으면 SID
    pub service: Option<String>,
}

/// tnsnames.ora 가 있는 폴더를 찾는다. Oracle Client 가 찾는 순서와 같게:
/// 설정의 tns_admin → TNS_ADMIN → ORACLE_HOME\network\admin → Instant Client 폴더\network\admin (설정 또는 PATH 의 oci.dll 위치)
pub fn locate(configured: Option<&Path>, client_lib_dir: Option<&Path>) -> Option<PathBuf> {
    let mut cands: Vec<PathBuf> = Vec::new();
    if let Some(d) = configured {
        cands.push(d.to_path_buf());
    }
    if let Some(d) = std::env::var_os("TNS_ADMIN") {
        cands.push(PathBuf::from(d));
    }
    if let Some(h) = std::env::var_os("ORACLE_HOME") {
        cands.push(PathBuf::from(h).join("network").join("admin"));
    }
    if let Some(d) = client_lib_dir {
        cands.push(d.join("network").join("admin"));
    }
    if let Some(path) = std::env::var_os("PATH") {
        for d in std::env::split_paths(&path) {
            if d.join("oci.dll").is_file() || d.join("libclntsh.so").is_file() {
                cands.push(d.join("network").join("admin"));
            }
        }
    }
    cands.into_iter().find(|d| d.join("tnsnames.ora").is_file())
}

/// 폴더의 tnsnames.ora 를 읽는다 (IFILE 은 세 단계까지 따라간다)
pub fn load(dir: &Path) -> std::io::Result<Vec<TnsEntry>> {
    let mut out = Vec::new();
    read_file(&dir.join("tnsnames.ora"), 0, &mut out)?;
    out.sort_by(|a, b| a.alias.cmp(&b.alias));
    out.dedup_by(|a, b| a.alias == b.alias);
    Ok(out)
}

fn read_file(path: &Path, depth: usize, out: &mut Vec<TnsEntry>) -> std::io::Result<()> {
    let bytes = std::fs::read(path)?;
    let text = String::from_utf8_lossy(&bytes);
    for item in parse_items(&text) {
        match item {
            Item::Entry(e) => out.extend(e),
            Item::Include(f) if depth < 3 => {
                let p = PathBuf::from(&f);
                let p = if p.is_absolute() { p } else { path.parent().unwrap_or(Path::new(".")).join(p) };
                // 못 읽는 IFILE 은 건너뛴다 (목록일 뿐이다)
                let _ = read_file(&p, depth + 1, out);
            }
            Item::Include(_) => {}
        }
    }
    Ok(())
}

enum Item {
    Entry(Vec<TnsEntry>),
    Include(String),
}

/// 맨 바깥(괄호 밖)의 `이름[, 이름…] = (…)` 와 `IFILE = 경로` 를 읽는다
fn parse_items(text: &str) -> Vec<Item> {
    // 주석(#…줄 끝) 지우기
    let clean: String = text.lines().map(|l| l.split('#').next().unwrap_or("")).collect::<Vec<_>>().join("\n");
    let mut items = Vec::new();
    let chars: Vec<char> = clean.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        // 이름 부분: '=' 까지
        let start = i;
        while i < chars.len() && chars[i] != '=' && chars[i] != '(' {
            i += 1;
        }
        if i >= chars.len() {
            break;
        }
        if chars[i] == '(' {
            // 이름 없이 시작한 괄호 — 짝까지 건너뛴다
            i = skip_group(&chars, i);
            continue;
        }
        let names: Vec<String> = chars[start..i].iter().collect::<String>().split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
        i += 1; // '='
        while i < chars.len() && chars[i].is_whitespace() {
            i += 1;
        }
        if i < chars.len() && chars[i] == '(' {
            let end = skip_group(&chars, i);
            let body: String = chars[i..end].iter().collect();
            i = end;
            let up = body.to_uppercase();
            let get = |key: &str| value_of(&body, &up, key);
            let service = get("SERVICE_NAME").or_else(|| get("SID"));
            let (host, port) = (get("HOST"), get("PORT"));
            items.push(Item::Entry(
                names.into_iter().map(|alias| TnsEntry { alias: alias.to_uppercase(), host: host.clone(), port: port.clone(), service: service.clone() }).collect(),
            ));
        } else {
            // 값이 괄호가 아니다 — IFILE = 경로 같은 것. 줄 끝까지
            let s = i;
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            let val: String = chars[s..i].iter().collect::<String>().trim().trim_matches('"').to_string();
            if names.len() == 1 && names[0].eq_ignore_ascii_case("IFILE") && !val.is_empty() {
                items.push(Item::Include(val));
            }
        }
    }
    items
}

/// `(` 에서 시작해 짝이 맞는 `)` 다음 위치
fn skip_group(chars: &[char], start: usize) -> usize {
    let mut depth = 0i32;
    let mut i = start;
    while i < chars.len() {
        match chars[i] {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return i + 1;
                }
            }
            _ => {}
        }
        i += 1;
    }
    chars.len()
}

/// 본문에서 처음 나오는 `(KEY = 값)` 의 값
fn value_of(body: &str, upper: &str, key: &str) -> Option<String> {
    let mut from = 0;
    while let Some(p) = upper[from..].find(key) {
        let at = from + p;
        from = at + key.len();
        // 앞이 '(' (공백 허용) 이고 뒤가 '=' 인 것만 — "SERVICE_NAME" 안의 "NAME" 같은 오인 방지
        let before = upper[..at].trim_end();
        let after = upper[from..].trim_start();
        if before.ends_with('(') && after.starts_with('=') {
            // 원래 글자(대소문자)로 — 대문자로 바꾸며 길이가 달라졌으면(ASCII 아님) 대문자 그대로
            let src = if body.len() == upper.len() { body } else { upper };
            let rest = &src[src.len() - after.len() + 1..];
            let v: String = rest.trim_start().chars().take_while(|c| *c != ')').collect();
            let v = v.trim().to_string();
            if !v.is_empty() {
                return Some(v);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
# 운영
ERP_PROD, ERPP =
  (DESCRIPTION =
    (ADDRESS_LIST =
      (ADDRESS = (PROTOCOL = TCP)(HOST = db-prod.corp.local)(PORT = 1521))
    )
    (CONNECT_DATA = (SERVER = DEDICATED) (SERVICE_NAME = ERPPRD))
  )

erp_dev=(DESCRIPTION=(ADDRESS=(PROTOCOL=TCP)(HOST=10.1.2.3)(PORT=1522))(CONNECT_DATA=(SID=ERPDEV)))

IFILE = extra.ora
"#;

    #[test]
    fn parses_aliases_hosts_services() {
        let items = parse_items(SAMPLE);
        let entries: Vec<TnsEntry> = items.iter().filter_map(|i| if let Item::Entry(e) = i { Some(e.clone()) } else { None }).flatten().collect();
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0], TnsEntry { alias: "ERP_PROD".into(), host: Some("db-prod.corp.local".into()), port: Some("1521".into()), service: Some("ERPPRD".into()) });
        assert_eq!(entries[1].alias, "ERPP");
        assert_eq!(entries[2], TnsEntry { alias: "ERP_DEV".into(), host: Some("10.1.2.3".into()), port: Some("1522".into()), service: Some("ERPDEV".into()) });
        assert!(items.iter().any(|i| matches!(i, Item::Include(f) if f == "extra.ora")));
    }

    #[test]
    fn follows_ifile_and_locates() {
        let dir = std::env::temp_dir().join(format!("sqls-tns-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("tnsnames.ora"), SAMPLE).unwrap();
        std::fs::write(dir.join("extra.ora"), "HR = (DESCRIPTION=(ADDRESS=(HOST=h)(PORT=1))(CONNECT_DATA=(SERVICE_NAME=hr)))").unwrap();
        let all = load(&dir).unwrap();
        assert_eq!(all.iter().map(|e| e.alias.as_str()).collect::<Vec<_>>(), vec!["ERPP", "ERP_DEV", "ERP_PROD", "HR"]);
        assert_eq!(locate(Some(&dir), None).as_deref(), Some(dir.as_path()));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
