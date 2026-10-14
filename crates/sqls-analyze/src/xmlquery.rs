//! XML 쿼리 파일(Anyframe `<query id=""><statement><![CDATA[ SQL ]]></statement></query>`)을 분석용 PL/SQL 모양으로.
//!
//! 파일 하나 = 패키지 본문 하나(`XQ_<파일이름>`), `<query>` 하나 = 프로시저 하나(이름은 id).
//!   · 안의 Velocity 지시문(`#if(…)` `#else` `#end` `#foreach(…)` `#set(…)`, `##` 주석)은 걷고, 분기는 모두 남긴다 —
//!     읽고 쓰는 테이블이 실제 실행보다 넓게 잡힐 수 있지만 빠지지는 않는다
//!   · `:vo.name` 같은 이름 있는 바인드는 `:vo_name`, `?` 는 그대로
//!   · `$var` · `${var}` 는 SQL 조각이 들어가는 자리 — 테이블 자리(FROM·JOIN·INTO·UPDATE 바로 뒤)면 '동적 SQL' 로 표시
//!   · `{ call pkg.proc(?) }` 는 프로시저 호출로
//! 줄 번호는 원본과 같게 둔다 (`<query` 가 있는 줄에 문장을 두고 나머지 줄은 비운다).

use std::path::Path;

use crate::chunk::UnitSource;
use crate::proc::{Translated, INNER};

pub fn is_xml_file(p: &Path) -> bool {
    p.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("xml"))
}

/// 분석 단위의 형식 — 화면에는 "XML" 으로 보인다. `…BODY` 로 끝나니 패키지처럼 서브프로그램 묶음으로 다뤄진다
pub const UNIT_TYPE: &str = "XML BODY";

/// 파일 이름 → 단위 이름 (`order.xml` → `order`, 파일 이름 그대로)
pub fn unit_name(path: &Path) -> String {
    path.file_stem().and_then(|s| s.to_str()).unwrap_or("X").to_string()
}

/// 바로 앞 버전(0.5.10)이 쓰던 이름 (`XQ_ORDER`) — 남아 있는 옛 결과를 치울 때
pub fn legacy_name(path: &Path) -> String {
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("X");
    let s: String = stem.chars().map(|c| if c.is_alphanumeric() || c == '_' { c.to_ascii_uppercase() } else { '_' }).collect();
    format!("XQ_{s}")
}

/// `<query>` 가 하나도 없으면 None (쿼리 파일이 아닌 XML)
pub fn translate(text: &str, owner: &str, name: &str) -> Option<Translated> {
    let text = blank_comments(text);
    let lower = text.to_ascii_lowercase();
    let mut out = String::new();
    let mut at = 0usize;
    let mut count = 0usize;
    let mut from = 0usize;
    while let Some(s) = find_tag(&lower, "query", from) {
        let Some(gt) = tag_end(&text, s) else { break };
        let Some(close) = lower[gt..].find("</query").map(|p| gt + p) else { break };
        let end = lower[close..].find('>').map(|p| close + p + 1).unwrap_or(text.len());
        from = end;
        if text[..gt].ends_with("/>") {
            continue;
        }
        let Some(id) = attr(&text[s..gt], "id") else { continue };
        let Some(sql) = statement(&text, &lower, gt, close) else { continue };
        let (body, dynamic) = to_plsql(&sql);
        if body.is_empty() {
            continue;
        }
        let pname = proc_name(&id);
        out.push_str(&newlines(&text[at..s]));
        out.push_str(&format!(" PROCEDURE {pname} IS BEGIN {body}{} END {pname}; ", if dynamic { " EXECUTE IMMEDIATE :dynamic_sql;" } else { "" }));
        out.push_str(&newlines(&text[s..end]));
        at = end;
        count += 1;
    }
    if count == 0 {
        return None;
    }
    out.push_str(&newlines(&text[at..]));
    let body = format!("PACKAGE BODY {INNER} IS {} END {INNER};", out.trim_end_matches('\n'));
    Some(Translated { unit: UnitSource { owner: owner.to_uppercase(), name: name.to_string(), unit_type: UNIT_TYPE.into(), text: body }, functions: count, statements: count })
}

fn newlines(s: &str) -> String {
    "\n".repeat(s.matches('\n').count())
}

/// `<!-- … -->` 를 공백으로 (줄바꿈은 둔다) — 주석 처리된 쿼리를 읽지 않게
fn blank_comments(t: &str) -> String {
    let mut o = String::with_capacity(t.len());
    let mut rest = t;
    while let Some(p) = rest.find("<!--") {
        o.push_str(&rest[..p]);
        let after = &rest[p + 4..];
        let q = after.find("-->").map(|q| q + 3).unwrap_or(after.len());
        o.push_str(&newlines(&after[..q]));
        rest = &after[q..];
    }
    o.push_str(rest);
    o
}

/// `<name` 다음이 공백·`>`·`/` 인 곳 (`<queries`, `<queryservice` 는 아니다)
fn find_tag(lower: &str, name: &str, mut from: usize) -> Option<usize> {
    let pat = format!("<{name}");
    while let Some(p) = lower.get(from..)?.find(&pat) {
        let s = from + p;
        let next = lower.as_bytes().get(s + pat.len()).copied().unwrap_or(b'>');
        if next.is_ascii_whitespace() || next == b'>' || next == b'/' {
            return Some(s);
        }
        from = s + pat.len();
    }
    None
}

/// 시작 태그의 `>` 바로 다음 위치 (따옴표 안의 > 는 건너뛴다)
fn tag_end(t: &str, start: usize) -> Option<usize> {
    let b = t.as_bytes();
    let mut i = start;
    let mut q = 0u8;
    while i < b.len() {
        let c = b[i];
        if q != 0 {
            if c == q {
                q = 0;
            }
        } else if c == b'"' || c == b'\'' {
            q = c;
        } else if c == b'>' {
            return Some(i + 1);
        }
        i += 1;
    }
    None
}

fn attr(tag: &str, name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let mut from = 0;
    while let Some(p) = lower[from..].find(name) {
        let s = from + p;
        let before_ok = s == 0 || !lower.as_bytes()[s - 1].is_ascii_alphanumeric() && lower.as_bytes()[s - 1] != b'-';
        let rest = tag[s + name.len()..].trim_start();
        if before_ok && rest.starts_with('=') {
            let v = rest[1..].trim_start();
            let q = v.chars().next()?;
            if q == '"' || q == '\'' {
                let e = v[1..].find(q)?;
                return Some(v[1..1 + e].trim().to_string());
            }
        }
        from = s + name.len();
    }
    None
}

/// `<statement>` 의 SQL — CDATA 가 있으면 그 안, 없으면 글 그대로 (엔티티 풀기)
fn statement(text: &str, lower: &str, from: usize, to: usize) -> Option<String> {
    let s = find_tag(&lower[..to], "statement", from)?;
    let gt = tag_end(text, s)?;
    let e = lower[gt..to].find("</statement").map(|p| gt + p).unwrap_or(to);
    let inner = &text[gt..e];
    if inner.contains("<![CDATA[") {
        let mut sql = String::new();
        let mut rest = inner;
        while let Some(p) = rest.find("<![CDATA[") {
            let a = &rest[p + 9..];
            let q = a.find("]]>").unwrap_or(a.len());
            sql.push_str(&a[..q]);
            sql.push(' ');
            rest = &a[(q + 3).min(a.len())..];
        }
        Some(sql)
    } else {
        Some(inner.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'").replace("&amp;", "&"))
    }
}

fn proc_name(id: &str) -> String {
    let s: String = id.chars().map(|c| if c.is_alphanumeric() || c == '_' { c.to_ascii_uppercase() } else { '_' }).collect();
    if s.chars().next().is_some_and(|c| c.is_alphabetic() || c == '_') { s } else { format!("Q_{s}") }
}

/// SQL → PL/SQL 문장 하나. 두 번째는 '테이블 자리에 $변수' 가 있었는지
fn to_plsql(raw: &str) -> (String, bool) {
    let (v, dynamic) = strip_velocity(raw);
    let s = crate::proc::clean_sql(&v);
    let s = s.trim().trim_end_matches(';').trim().to_string();
    if s.is_empty() {
        return (String::new(), dynamic);
    }
    // { call pkg.proc(?) } · { ? = call f(?) }
    let mut s = s;
    if s.starts_with('{') {
        s = s.trim_start_matches('{').trim_end_matches('}').trim().to_string();
        if let Some(r) = s.strip_prefix("?") {
            s = r.trim_start().trim_start_matches('=').trim().to_string();
        }
        if s.len() >= 4 && s[..4].eq_ignore_ascii_case("call") {
            s = s[4..].trim().to_string();
        }
        return (format!("{};", crate::proc::hostvars(&s)), dynamic);
    }
    let first = s.split(|c: char| c.is_whitespace() || c == '(').find(|w| !w.is_empty()).unwrap_or("").to_uppercase();
    let s = crate::proc::hostvars(&s);
    let body = match first.as_str() {
        "SELECT" | "WITH" => format!("FOR r_q IN ({s}) LOOP NULL; END LOOP;"),
        "INSERT" | "UPDATE" | "DELETE" | "MERGE" => format!("{s};"),
        "BEGIN" | "DECLARE" => format!("{s};"),
        "CALL" => format!("{};", s[4..].trim()),
        "TRUNCATE" | "DROP" | "ALTER" | "CREATE" | "LOCK" => format!("EXECUTE IMMEDIATE '{}';", s.replace('\'', "''")),
        _ => String::new(),
    };
    (body, dynamic)
}

const DIRECTIVES: &[&str] = &["if", "elseif", "else", "end", "foreach", "set", "macro", "parse", "include", "stop", "break", "evaluate", "define"];

/// Velocity 지시문을 걷고 `$변수` 를 바인드로. 분기는 모두 남긴다
fn strip_velocity(s: &str) -> (String, bool) {
    let c: Vec<char> = s.chars().collect();
    let mut o = String::new();
    let mut dynamic = false;
    let mut i = 0;
    let ident = |ch: char| ch.is_alphanumeric() || ch == '_';
    while i < c.len() {
        let ch = c[i];
        let prev_ok = i == 0 || !(ident(c[i - 1]) || c[i - 1] == '$' || c[i - 1] == '#');
        if ch == '\'' {
            // 문자열은 그대로 ('' 는 따옴표 하나)
            o.push(ch);
            i += 1;
            while i < c.len() {
                o.push(c[i]);
                if c[i] == '\'' {
                    if c.get(i + 1) == Some(&'\'') {
                        i += 1;
                        o.push('\'');
                    } else {
                        break;
                    }
                }
                i += 1;
            }
            i += 1;
        } else if ch == '#' && c.get(i + 1) == Some(&'#') {
            // ## 줄 주석
            while i < c.len() && c[i] != '\n' {
                i += 1;
            }
        } else if ch == '#' && c.get(i + 1) == Some(&'*') {
            i += 2;
            while i < c.len() && !(c[i] == '*' && c.get(i + 1) == Some(&'#')) {
                i += 1;
            }
            i += 2;
        } else if ch == '#' && prev_ok && {
            let mut j = i + 1;
            let braced = c.get(j) == Some(&'{');
            if braced {
                j += 1;
            }
            let w: String = c[j..].iter().take_while(|x| x.is_ascii_alphabetic()).collect();
            DIRECTIVES.contains(&w.as_str())
        } {
            // 지시문 이름, 있으면 괄호까지 (괄호는 짝을 맞춰 건너뛴다)
            i += 1;
            if c.get(i) == Some(&'{') {
                while i < c.len() && c[i] != '}' {
                    i += 1;
                }
                i += 1;
            } else {
                while i < c.len() && c[i].is_ascii_alphabetic() {
                    i += 1;
                }
            }
            let mut j = i;
            while j < c.len() && (c[j] == ' ' || c[j] == '\t') {
                j += 1;
            }
            if c.get(j) == Some(&'(') {
                let mut depth = 0;
                while j < c.len() {
                    match c[j] {
                        '(' => depth += 1,
                        ')' => {
                            depth -= 1;
                            if depth == 0 {
                                j += 1;
                                break;
                            }
                        }
                        '\'' | '"' => {
                            let q = c[j];
                            j += 1;
                            while j < c.len() && c[j] != q {
                                j += 1;
                            }
                        }
                        _ => {}
                    }
                    j += 1;
                }
                i = j;
            }
            o.push(' ');
        } else if ch == '$' && prev_ok && c.get(i + 1).is_some_and(|x| ident(*x) || *x == '{' || *x == '!') {
            // $var · ${var.f} · $!var — SQL 조각이 들어가는 자리. 앞 낱말이 FROM·JOIN·INTO·UPDATE 면 테이블 자리라 알 수 없다
            let before = o.trim_end().rsplit(|x: char| !x.is_alphanumeric() && x != '_').next().unwrap_or("").to_uppercase();
            if matches!(before.as_str(), "FROM" | "JOIN" | "INTO" | "UPDATE" | "TABLE") {
                dynamic = true;
            }
            i += 1;
            if c.get(i) == Some(&'!') {
                i += 1;
            }
            let braced = c.get(i) == Some(&'{');
            if braced {
                i += 1;
            }
            let mut name = String::new();
            while i < c.len() && (ident(c[i]) || (c[i] == '.' && c.get(i + 1).is_some_and(|x| ident(*x)))) {
                name.push(if c[i] == '.' { '_' } else { c[i] });
                i += 1;
            }
            if braced {
                while i < c.len() && c[i] != '}' {
                    i += 1;
                }
                i += 1;
            }
            // 테이블 자리는 이름을 빈칸으로 (FROM :x 가 테이블로 읽히지 않게) — 값 자리는 바인드로
            if matches!(before.as_str(), "FROM" | "JOIN" | "INTO" | "UPDATE" | "TABLE") {
                o.push_str(" DUAL_DYNAMIC ");
            } else {
                o.push(':');
                o.push_str(if name.is_empty() { "v" } else { &name });
            }
        } else {
            o.push(ch);
            i += 1;
        }
    }
    (o, dynamic)
}

#[cfg(test)]
mod tests {
    use super::*;

    const XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<queryservice xmlns="http://www.anyframejava.org/schema/query/mapping">
  <queries>
    <!-- 옛 쿼리
    <query id="old"><statement><![CDATA[ SELECT * FROM never_read ]]></statement></query>
    -->
    <query id="findOrderList" isDynamic="true" mappingStyle="lcase">
      <statement>
        <![CDATA[
          SELECT o.id, c.name   -- 고객
            FROM orders o JOIN customers c ON c.id = o.cust_id
           WHERE 1 = 1
          ## 속도 때문에
          #if ($vo.status)
             AND o.status = :vo.status
          #end
          #foreach ($id in $ids) OR o.id = $id #end
        ]]>
      </statement>
      <result class="x.OrderVO"/>
    </query>
    <query id="createHist">
      <statement><![CDATA[
        INSERT INTO order_hist (id, amount, ts)
        SELECT id, amount, SYSDATE FROM orders WHERE status = :status
      ]]></statement>
    </query>
    <query id="closeOrder"><statement><![CDATA[ UPDATE orders SET status = 'DONE' WHERE id = ? ]]></statement></query>
    <query id="removeTmp"><statement>DELETE FROM order_tmp WHERE a &lt; 3</statement></query>
    <query id="callAudit"><statement><![CDATA[ { call audit_pkg.log(?, ?) } ]]></statement></query>
    <query id="byTable"><statement><![CDATA[ SELECT * FROM $tbl WHERE id = :id ]]></statement></query>
  </queries>
</queryservice>
"#;

    #[test]
    fn translates_anyframe_xml() {
        let t = translate(XML, "scott", "XQ_ORDER").expect("쿼리가 있다");
        assert_eq!(t.functions, 6, "{}", t.unit.text);
        let out = &t.unit.text;
        // 줄 번호 보존 — 프로시저는 원본 <query> 가 있는 줄에 있다
        let line_of = |text: &str, pat: &str| text.lines().position(|l| l.contains(pat)).unwrap() + 1;
        assert_eq!(line_of(out, "PROCEDURE BYTABLE"), line_of(XML, "id=\"byTable\""), "{out}");
        assert_eq!(line_of(out, "PROCEDURE FINDORDERLIST"), line_of(XML, "id=\"findOrderList\""));
        assert!(!out.contains("NEVER_READ") && !out.to_lowercase().contains("never_read"), "주석 처리된 쿼리는 읽지 않는다");
        assert!(out.contains("PROCEDURE FINDORDERLIST IS BEGIN"));
        assert!(!out.contains("#if") && !out.contains("#end") && !out.contains("속도"), "{out}");
        assert!(out.contains(":vo_status") && out.contains("OR o.id = :id"), "{out}");
        assert!(out.contains("audit_pkg.log(?, ?);"), "{out}");
        assert!(out.contains("a < 3"));
        assert!(out.contains("EXECUTE IMMEDIATE :dynamic_sql;"), "테이블 자리 $tbl 은 동적: {out}");
    }

    #[test]
    fn facts_from_translated_xml() {
        let t = translate(XML, "SCOTT", "XQ_ORDER").unwrap();
        let (st, toks) = crate::plsql::structure(&t.unit.text);
        assert_eq!(st.name, "CODE_UNIT");
        assert_eq!(st.subprograms.len(), 6, "{:?}", st.subprograms.iter().map(|s| &s.name).collect::<Vec<_>>());
        let locals = crate::facts::local_names(&toks);
        let f = crate::facts::extract(&toks, &locals);
        let tab = |n: &str| f.tables.iter().find(|t| t.name == n).map(|t| t.ops.clone());
        assert_eq!(tab("CUSTOMERS").as_deref(), Some("R"), "{:?}", f.tables);
        assert_eq!(tab("ORDER_HIST").as_deref(), Some("C"));
        assert_eq!(tab("ORDER_TMP").as_deref(), Some("D"));
        assert!(tab("ORDERS").unwrap().contains('R') && tab("ORDERS").unwrap().contains('U'));
        assert!(f.tables.iter().all(|t| t.name != "NEVER_READ"));
        assert!(f.calls.iter().any(|c| c.name == "AUDIT_PKG.LOG"), "{:?}", f.calls);
    }

    #[test]
    fn plain_xml_is_none() {
        assert!(translate("<beans><bean id=\"x\"/></beans>", "X", "XQ_X").is_none());
    }
}
