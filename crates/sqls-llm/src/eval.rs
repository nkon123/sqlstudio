//! 평가 하네스 — 프롬프트·문법 카드·모델·생각 설정을 바꿀 때 감이 아니라 점수로 비교한다.
//!
//! 사례(질문 + 테이블 요약 + 서버 버전 + 판정 기준)를 모델에 돌려, 첫 답과 고친 뒤의 통과율, 시간, 토큰을 잰다.
//! DB 없이 앱 안의 검사만 쓴다 (버전 문법, 꼭 들어갈·들어가면 안 되는 낱말, 스키마에 없는 컬럼).
//! 실행: `cargo run -p sqls-llm --example eval -- --model qwen3.5:4b --cases evals`

use std::path::Path;
use std::time::Instant;

use serde::Deserialize;

use crate::harness;
use crate::prompts::{self, Context, Task};
use crate::{Client, Message, ProviderConfig};

/// 사례 하나 (evals/*.json — 한 파일에 하나 또는 배열)
#[derive(Debug, Clone, Deserialize)]
pub struct Case {
    pub id: String,
    #[serde(default = "default_task")]
    pub task: Task,
    pub question: String,
    #[serde(default)]
    pub sql: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
    /// "11.2.0.4.0" 같은 서버 버전
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub user: Option<String>,
    /// meta::schema_brief 형식의 테이블 요약 ("TABLE APP.ORDERS  -- 주문\n  ORDER_ID NUMBER PK\n …")
    #[serde(default)]
    pub schema: Vec<String>,
    #[serde(default)]
    pub expect: Expect,
}

fn default_task() -> Task {
    Task::Generate
}

/// 판정 기준
#[derive(Debug, Clone, Deserialize)]
pub struct Expect {
    /// SQL 블록이 있어야 하는지
    #[serde(default = "yes")]
    pub sql: bool,
    /// SQL 에 꼭 들어갈 낱말 (대소문자 무시, 주석·문자열 밖에서). "A|B" 는 둘 중 하나
    #[serde(default)]
    pub must: Vec<String>,
    /// 들어가면 안 되는 낱말·구절 ("FETCH FIRST")
    #[serde(default)]
    pub must_not: Vec<String>,
    /// `별칭.컬럼` 의 컬럼이 스키마에 있는지 본다
    #[serde(default = "yes")]
    pub columns: bool,
}

impl Default for Expect {
    fn default() -> Self {
        Self { sql: true, must: vec![], must_not: vec![], columns: true }
    }
}

fn yes() -> bool {
    true
}

/// 폴더의 *.json 을 모두 읽는다 (파일 이름 순)
pub fn load_cases(dir: &Path) -> Result<Vec<Case>, String> {
    let mut files: Vec<_> = std::fs::read_dir(dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    files.sort();
    let mut out = Vec::new();
    for f in files {
        let t = std::fs::read_to_string(&f).map_err(|e| format!("{}: {e}", f.display()))?;
        let v: serde_json::Value = serde_json::from_str(&t).map_err(|e| format!("{}: {e}", f.display()))?;
        let list = if v.is_array() { v } else { serde_json::Value::Array(vec![v]) };
        out.extend(serde_json::from_value::<Vec<Case>>(list).map_err(|e| format!("{}: {e}", f.display()))?);
    }
    Ok(out)
}

/// 돌릴 설정
#[derive(Debug, Clone)]
pub struct Options {
    /// None 이면 앱과 같게 (로컬 모델의 생성·설명은 끔)
    pub think: Option<String>,
    pub repairs: u32,
    /// 키워드로 고른 문법 카드를 넣을지
    pub guides: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self { think: None, repairs: 2, guides: true }
    }
}

#[derive(Debug, Clone)]
pub struct CaseResult {
    pub id: String,
    pub pass_first: bool,
    pub pass_final: bool,
    pub repairs: u32,
    pub secs: f32,
    pub out_tokens: u64,
    /// 마지막 답에 남은 문제 (통과면 빈칸)
    pub problems: Vec<String>,
    /// 첫 답의 문제
    pub first_problems: Vec<String>,
    pub sql: Option<String>,
}

/// 스키마 요약에서 테이블·컬럼 이름
fn schema_names(schema: &[String]) -> (Vec<String>, Vec<String>) {
    let (mut tables, mut cols) = (Vec::new(), Vec::new());
    for line in schema.iter().flat_map(|s| s.lines()) {
        let code = line.split("  --").next().unwrap_or("");
        if code.trim().is_empty() {
            continue;
        }
        if !line.starts_with(' ') {
            // "TABLE APP.ORDERS" · "MATERIALIZED VIEW APP.MV"
            if let Some(q) = code.split_whitespace().last() {
                tables.extend(q.split('.').map(|x| x.to_uppercase()));
            }
        } else if let Some(w) = code.split_whitespace().next() {
            if !matches!(w, "INDEX" | "UNIQUE" | "…") {
                cols.push(w.to_uppercase());
            }
        }
    }
    (tables, cols)
}

/// 답의 SQL 판정 — 문제 목록 (빈칸이면 통과)
pub fn judge(case: &Case, answer: &str) -> Vec<String> {
    let mut out = Vec::new();
    let Some(sql) = prompts::extract_sql(answer) else {
        if case.expect.sql {
            out.push("SQL 블록이 없다".into());
        }
        return out;
    };
    let major = case.version.as_deref().and_then(prompts::major_version);
    out.extend(harness::lint_version(&sql, major));
    let code = harness::code_only(&sql);
    let words: Vec<&str> = code.split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '$' || c == '#')).filter(|w| !w.is_empty()).collect();
    let has_phrase = |p: &str| {
        let seq: Vec<String> = p.split_whitespace().map(|w| w.to_uppercase()).collect();
        !seq.is_empty() && words.windows(seq.len()).any(|w| w.iter().zip(&seq).all(|(a, b)| a == b))
    };
    for m in &case.expect.must {
        if !m.split('|').any(|alt| has_phrase(alt.trim())) {
            out.push(format!("'{m}' 이 없다"));
        }
    }
    for m in &case.expect.must_not {
        if has_phrase(m) {
            out.push(format!("'{m}' 을 썼다"));
        }
    }
    if case.expect.columns && !case.schema.is_empty() {
        out.extend(unknown_names(&code, &case.schema));
    }
    out
}

/// 스키마에 없는 테이블·컬럼 — `별칭.컬럼` 의 컬럼, `소유자.테이블`·FROM/JOIN 뒤의 테이블을 본다.
/// `AS` 앞뒤 이름(컬럼 별칭·WITH 이름)은 SQL 안에서 만든 이름으로 친다. 인라인 뷰의 범위까지는 보지 않는다 (서버 파싱이 할 일)
fn unknown_names(code: &str, schema: &[String]) -> Vec<String> {
    let (tables, cols) = schema_names(schema);
    // (낱말, 바로 뒤 공백 아닌 글자, 바로 뒤가 `.이름` 인지 — `o.*` 는 아니다)
    let chars: Vec<char> = code.chars().collect();
    let ident = |c: char| c.is_alphanumeric() || c == '_' || c == '$' || c == '#';
    let mut toks: Vec<(String, Option<char>, bool)> = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if ident(chars[i]) {
            let s = i;
            while i < chars.len() && ident(chars[i]) {
                i += 1;
            }
            let next = chars[i..].iter().copied().find(|c| !c.is_whitespace());
            let dotted = chars.get(i) == Some(&'.') && chars.get(i + 1).is_some_and(|c| ident(*c));
            toks.push((chars[s..i].iter().collect(), next, dotted));
        } else {
            i += 1;
        }
    }
    let made: Vec<&str> = toks
        .iter()
        .enumerate()
        .filter(|(_, (w, _, _))| w == "AS")
        .flat_map(|(i, _)| [i.checked_sub(1), Some(i + 1)])
        .flatten()
        .filter_map(|j| toks.get(j).map(|(w, _, _)| w.as_str()))
        .collect();
    let known_table = |w: &str| tables.iter().any(|t| t == w) || made.contains(&w) || w == "DUAL";
    let mut out: Vec<String> = Vec::new();
    let mut push = |m: String| {
        if !out.contains(&m) {
            out.push(m);
        }
    };
    for (i, (w, next, dotted)) in toks.iter().enumerate() {
        let after = toks.get(i + 1);
        if *dotted {
            let Some((right, rnext, _)) = after else { continue };
            if w.chars().all(|c| c.is_ascii_digit()) || *rnext == Some('(') || matches!(right.as_str(), "NEXTVAL" | "CURRVAL") {
                continue;
            }
            if tables.contains(w) {
                // 소유자.테이블 (또는 테이블.컬럼)
                if !(known_table(right) || cols.contains(right)) {
                    push(format!("스키마에 없는 테이블: {w}.{right}"));
                }
            } else if !(cols.contains(right) || made.contains(&right.as_str()) || tables.contains(right)) {
                push(format!("스키마에 없는 컬럼: {w}.{right}"));
            }
        } else if matches!(w.as_str(), "FROM" | "JOIN" | "INTO" | "USING" | "UPDATE") && next.is_some_and(ident) {
            // EXTRACT(YEAR FROM 컬럼), TRIM(… FROM 컬럼) 은 컬럼이다
            let Some((t, tnext, tdotted)) = after else { continue };
            let prev = i.checked_sub(1).and_then(|j| toks.get(j)).map(|(p, _, _)| p.as_str()).unwrap_or("");
            if matches!(prev, "YEAR" | "MONTH" | "DAY" | "HOUR" | "MINUTE" | "SECOND" | "LEADING" | "TRAILING" | "BOTH") {
                continue;
            }
            if *tdotted || *tnext == Some('(') || t == "SET" || known_table(t) || cols.contains(t) {
                continue;
            }
            push(format!("스키마에 없는 테이블: {t}"));
        }
    }
    out
}

/// 사례 하나를 돌린다 — 첫 답을 판정하고, 걸리면 문제와 전체 스키마를 주고 `repairs` 번까지 다시 묻는다
pub async fn run_case(client: &Client, cfg: &ProviderConfig, case: &Case, opts: &Options) -> crate::Result<CaseResult> {
    let mut ctx = Context {
        server_version: case.version.clone(),
        current_user: case.user.clone(),
        schema: case.schema.iter().map(|s| if s.ends_with('\n') { s.clone() } else { format!("{s}\n") }).collect(),
        sql: case.sql.clone(),
        error: case.error.clone(),
        question: Some(case.question.clone()),
        ..Default::default()
    };
    let major = case.version.as_deref().and_then(prompts::major_version);
    if opts.guides {
        let all = crate::guides::load(None);
        let text = [Some(case.question.as_str()), case.sql.as_deref(), case.error.as_deref()].into_iter().flatten().collect::<Vec<_>>().join("\n");
        let picked = crate::guides::pick_with(&all, case.task, &text, major, &[]);
        ctx.guides = crate::guides::render(&picked.iter().map(|(g, _)| *g).collect::<Vec<_>>());
    }
    let mut c = cfg.clone();
    if c.think.is_none() {
        c.think = match &opts.think {
            Some(t) if t != "auto" => Some(t.clone()),
            _ if harness::is_local_model(cfg) && matches!(case.task, Task::Generate | Task::Explain | Task::Chat) => Some("off".into()),
            _ => None,
        };
    }
    if harness::is_local_model(cfg) && c.temperature.is_none() {
        c.temperature = Some(if c.think.as_deref() == Some("off") { 0.2 } else { 0.6 });
    }
    let req = prompts::build(case.task, &ctx, &[]);
    let started = Instant::now();
    let mut resp = client.chat(&c, &req, |_| {}).await?;
    let mut out_tokens = resp.output_tokens.unwrap_or(0);
    let first = judge(case, &resp.text);
    let mut problems = first.clone();
    let mut repairs = 0;
    let mut msgs = req.messages.clone();
    while !problems.is_empty() && repairs < opts.repairs && prompts::extract_sql(&resp.text).is_some() {
        repairs += 1;
        msgs.push(Message::assistant(harness::strip_think(&resp.text)));
        msgs.push(Message::user(prompts::repair_message(&problems.join("\n"), &ctx.schema)));
        let rreq = crate::ChatRequest { system: req.system.clone(), messages: msgs.clone(), json_schema: None };
        resp = client.chat(&c, &rreq, |_| {}).await?;
        out_tokens += resp.output_tokens.unwrap_or(0);
        problems = judge(case, &resp.text);
    }
    Ok(CaseResult {
        id: case.id.clone(),
        pass_first: first.is_empty(),
        pass_final: problems.is_empty(),
        repairs,
        secs: started.elapsed().as_secs_f32(),
        out_tokens,
        problems,
        first_problems: first,
        sql: prompts::extract_sql(&resp.text),
    })
}

/// 결과 표 (마크다운) — 사례별 줄과 합계
pub fn report(results: &[CaseResult]) -> String {
    let mut s = String::from("| 사례 | 첫 답 | 고친 뒤 | 고침 | 초 | 토큰 | 남은 문제 |\n|---|---|---|---|---|---|---|\n");
    let mark = |b: bool| if b { "✓" } else { "✗" };
    for r in results {
        s.push_str(&format!(
            "| {} | {} | {} | {} | {:.1} | {} | {} |\n",
            r.id,
            mark(r.pass_first),
            mark(r.pass_final),
            r.repairs,
            r.secs,
            r.out_tokens,
            r.problems.join("; ").replace('|', "/")
        ));
    }
    let n = results.len().max(1) as f32;
    let first = results.iter().filter(|r| r.pass_first).count();
    let fin = results.iter().filter(|r| r.pass_final).count();
    s.push_str(&format!(
        "\n첫 답 통과 {first}/{} ({:.0}%) · 고친 뒤 통과 {fin}/{} ({:.0}%) · 평균 {:.1}초 · 평균 {:.0} 토큰\n",
        results.len(),
        first as f32 * 100.0 / n,
        results.len(),
        fin as f32 * 100.0 / n,
        results.iter().map(|r| r.secs).sum::<f32>() / n,
        results.iter().map(|r| r.out_tokens as f32).sum::<f32>() / n,
    ));
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn case() -> Case {
        serde_json::from_value(serde_json::json!({
            "id": "top10", "question": "급여 상위 10명", "version": "11.2.0.4.0",
            "schema": ["TABLE APP.EMP  -- 사원\n  EMPNO NUMBER PK\n  ENAME VARCHAR2(10)\n  SAL NUMBER\n  INDEX EMP_SAL(SAL)\n"],
            "expect": { "must": ["ROWNUM|ROW_NUMBER"], "must_not": ["FETCH FIRST"] }
        }))
        .unwrap()
    }

    #[test]
    fn judge_checks() {
        let c = case();
        assert_eq!(c.task, Task::Generate);
        let good = "```sql\nSELECT * FROM (SELECT e.empno, e.ename, e.sal FROM app.emp e ORDER BY e.sal DESC) WHERE ROWNUM <= 10;\n```";
        assert!(judge(&c, good).is_empty(), "{:?}", judge(&c, good));
        let bad = "```sql\nSELECT e.empno, e.salary FROM emp e ORDER BY e.sal DESC FETCH FIRST 10 ROWS ONLY;\n```";
        let p = judge(&c, bad);
        assert!(p.iter().any(|x| x.contains("FETCH FIRST")), "{p:?}");
        assert!(p.iter().any(|x| x.contains("ROWNUM|ROW_NUMBER")), "{p:?}");
        assert!(p.iter().any(|x| x == "스키마에 없는 컬럼: E.SALARY"), "{p:?}");
        // 함수 호출, 숫자, 시퀀스, 문자열 안은 컬럼으로 보지 않는다
        let ok = "```sql\nSELECT DBMS_RANDOM.VALUE(), 1.5, emp_seq.NEXTVAL, 'x.y' FROM dual WHERE ROWNUM = 1;\n```";
        assert!(judge(&c, ok).is_empty(), "{:?}", judge(&c, ok));
        assert_eq!(judge(&c, "모르겠습니다"), vec!["SQL 블록이 없다"]);
        // SQL 안에서 만든 이름(AS 별칭·WITH 이름), MERGE 의 UPDATE SET, EXTRACT(… FROM 컬럼) 은 괜찮다
        let star = "```sql
SELECT o.* FROM app.emp o WHERE ROWNUM = 1;
```";
        assert!(judge(&c, star).is_empty(), "{:?}", judge(&c, star));
        let made = "```sql
WITH top AS (SELECT empno, sal AS pay FROM app.emp) SELECT t.pay, EXTRACT(YEAR FROM SYSDATE) FROM top t WHERE ROWNUM <= 10;
```";
        assert!(judge(&c, made).is_empty(), "{:?}", judge(&c, made));
        let m = "```sql
MERGE INTO emp t USING (SELECT empno AS id, sal AS amt FROM emp) s ON (t.empno = s.id) WHEN MATCHED THEN UPDATE SET t.sal = s.amt;
```";
        let mut c2 = c.clone();
        c2.expect.must.clear();
        assert!(judge(&c2, m).is_empty(), "{:?}", judge(&c2, m));
        // 없는 테이블
        let t = "```sql
SELECT h.use_date FROM app.holiday h JOIN dept d ON 1 = 1 WHERE ROWNUM = 1;
```";
        let p = judge(&c, t);
        assert!(p.contains(&"스키마에 없는 테이블: APP.HOLIDAY".to_string()) && p.contains(&"스키마에 없는 테이블: DEPT".to_string()), "{p:?}");
    }

    #[test]
    fn cases_from_dir() {
        let dir = std::env::temp_dir().join(format!("sqls-eval-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.json"), r#"[{"id":"a1","question":"q"},{"id":"a2","task":"fix","question":"q","sql":"select","error":"ORA-00942"}]"#).unwrap();
        std::fs::write(dir.join("b.json"), r#"{"id":"b1","question":"q","expect":{"sql":false}}"#).unwrap();
        std::fs::write(dir.join("readme.txt"), "x").unwrap();
        let cs = load_cases(&dir).unwrap();
        assert_eq!(cs.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(), vec!["a1", "a2", "b1"]);
        assert_eq!(cs[1].task, Task::Fix);
        assert!(!cs[2].expect.sql && cs[0].expect.columns);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn bundled_cases_parse() {
        // 저장소의 evals/ — 형식이 깨지지 않았는지, 모범 답이 통과하는지 (판정 기준이 지나치게 빡빡하지 않게)
        let cs = load_cases(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../../evals")).unwrap();
        assert!(cs.len() >= 10);
        let top = cs.iter().find(|c| c.id == "top-n-11g").unwrap();
        let a = "```sql
SELECT * FROM (SELECT e.ename, d.dname, e.sal FROM emp e JOIN dept d ON d.deptno = e.deptno ORDER BY e.sal DESC) WHERE ROWNUM <= 10;
```";
        assert!(judge(top, a).is_empty(), "{:?}", judge(top, a));
    }

    #[test]
    fn report_totals() {
        let r = |id: &str, a, b| CaseResult { id: id.into(), pass_first: a, pass_final: b, repairs: 0, secs: 2.0, out_tokens: 100, problems: vec![], first_problems: vec![], sql: None };
        let s = report(&[r("x", true, true), r("y", false, true)]);
        assert!(s.contains("첫 답 통과 1/2 (50%)") && s.contains("고친 뒤 통과 2/2 (100%)"), "{s}");
    }
}
