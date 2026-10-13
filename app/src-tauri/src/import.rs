//! 데이터 가져오기 — 엑셀(.xlsx/.xls)·CSV 를 테이블에 넣는다 (Toad 의 Import Table Data).
//!
//! - 읽기: 엑셀은 calamine, CSV 는 csv. CSV 인코딩은 UTF-8(BOM 포함)을 먼저 보고 아니면 CP949. 구분자는 첫 줄로 짐작.
//! - 넣기: INSERT … VALUES (:1, TO_DATE(:2, '형식'), …) 를 array DML 로 묶어 보낸다 (500행씩). 형 변환은 SQL 쪽에서.
//! - 커밋하지 않는다 — 결과를 보고 커밋/롤백한다. 운영 DB 면 서버가 확인을 요구한다. 읽기 전용 프로필은 세션이 막는다.
//! - 오류 행 건너뛰기(batch errors) 또는 첫 오류에서 멈춤. 멈춤 버튼은 스크립트 중지와 같은 신호를 쓴다.

use std::sync::atomic::Ordering;

use serde::{Deserialize, Serialize};
use tauri::ipc::Channel;
use tauri::State;

use crate::state::{AppState, ErrView};

type R<T> = Result<T, ErrView>;

const CHUNK: usize = 500;
const MAX_ERRORS: usize = 200;

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ReadOpts {
    /// 엑셀 시트 (없으면 첫 시트)
    pub sheet: Option<String>,
    /// 첫 줄이 컬럼 이름인지
    #[serde(default = "yes")]
    pub header: bool,
    /// CSV 구분자 (없으면 짐작)
    pub delimiter: Option<String>,
    /// CSV 인코딩 "utf-8" | "cp949" (없으면 짐작)
    pub encoding: Option<String>,
    /// 앞에서 건너뛸 줄 수 (제목 줄 등)
    #[serde(default)]
    pub skip: usize,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Serialize)]
pub struct Table {
    pub sheets: Vec<String>,
    pub sheet: Option<String>,
    pub encoding: Option<String>,
    pub delimiter: Option<String>,
    pub headers: Vec<String>,
    pub rows: Vec<Vec<Option<String>>>,
}

fn is_excel(path: &str) -> bool {
    let p = path.to_lowercase();
    p.ends_with(".xlsx") || p.ends_with(".xlsm") || p.ends_with(".xls") || p.ends_with(".xlsb") || p.ends_with(".ods")
}

/// 파일 전체를 읽는다 (머리글 분리, 빈 줄 제외)
pub fn read_table(path: &str, o: &ReadOpts) -> Result<Table, String> {
    let (sheets, sheet, encoding, delimiter, mut all) = if is_excel(path) {
        use calamine::{open_workbook_auto, Data, Reader};
        let mut wb = open_workbook_auto(path).map_err(|e| format!("엑셀 파일을 열 수 없습니다: {e}"))?;
        let sheets = wb.sheet_names().to_vec();
        let name = o.sheet.clone().filter(|s| sheets.contains(s)).or_else(|| sheets.first().cloned()).ok_or("시트가 없습니다")?;
        let range = wb.worksheet_range(&name).map_err(|e| format!("{name}: {e}"))?;
        let rows: Vec<Vec<Option<String>>> = range
            .rows()
            .map(|r| r.iter().map(|c| match c {
                Data::Empty => None,
                Data::String(s) => Some(s.clone()),
                Data::Int(i) => Some(i.to_string()),
                Data::Float(f) => Some(num_text(*f)),
                Data::Bool(b) => Some(if *b { "1".into() } else { "0".into() }),
                Data::DateTime(d) => {
                    let (y, mo, da, h, mi, s, _) = d.to_ymd_hms_milli();
                    Some(format!("{y:04}-{mo:02}-{da:02} {h:02}:{mi:02}:{s:02}"))
                }
                Data::DateTimeIso(s) | Data::DurationIso(s) => Some(s.replace('T', " ")),
                Data::Error(e) => Some(format!("#{e:?}")),
            }).collect())
            .collect();
        (sheets, Some(name), None, None, rows)
    } else {
        let bytes = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
        let (text, enc) = decode(&bytes, o.encoding.as_deref());
        let delim = o.delimiter.as_deref().and_then(|d| if d == "\\t" { Some('\t') } else { d.chars().next() }).unwrap_or_else(|| guess_delimiter(&text, o.skip));
        let mut rdr = csv::ReaderBuilder::new().has_headers(false).flexible(true).delimiter(delim as u8).from_reader(text.as_bytes());
        let mut rows = Vec::new();
        for rec in rdr.records() {
            let rec = rec.map_err(|e| format!("CSV {}행: {e}", rows.len() + 1))?;
            rows.push(rec.iter().map(|v| if v.is_empty() { None } else { Some(v.to_string()) }).collect());
        }
        (Vec::new(), None, Some(enc.to_string()), Some(if delim == '\t' { "\\t".into() } else { delim.to_string() }), rows)
    };
    // 앞 줄 건너뛰기, 완전히 빈 줄 빼기
    all.drain(..o.skip.min(all.len()));
    all.retain(|r| r.iter().any(|c| c.as_deref().is_some_and(|v| !v.trim().is_empty())));
    let width = all.iter().map(|r| r.len()).max().unwrap_or(0);
    let headers = if o.header && !all.is_empty() {
        let h = all.remove(0);
        (0..width).map(|i| h.get(i).cloned().flatten().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).unwrap_or_else(|| format!("열{}", i + 1))).collect()
    } else {
        (0..width).map(|i| format!("열{}", i + 1)).collect()
    };
    for r in &mut all {
        r.resize(width, None);
    }
    Ok(Table { sheets, sheet, encoding, delimiter, headers, rows: all })
}

/// 엑셀 실수 → 글자 (1.0 → "1", 지수 표기 없이)
fn num_text(f: f64) -> String {
    if f.fract() == 0.0 && f.abs() < 1e15 {
        format!("{}", f as i64)
    } else {
        let s = format!("{f}");
        if s.contains('e') || s.contains('E') { format!("{f:.10}").trim_end_matches('0').trim_end_matches('.').to_string() } else { s }
    }
}

fn decode(bytes: &[u8], want: Option<&str>) -> (String, &'static str) {
    if want == Some("cp949") {
        return (encoding_rs::EUC_KR.decode(bytes).0.into_owned(), "cp949");
    }
    if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return (String::from_utf8_lossy(rest).into_owned(), "utf-8");
    }
    match std::str::from_utf8(bytes) {
        Ok(s) => (s.to_string(), "utf-8"),
        Err(_) if want == Some("utf-8") => (String::from_utf8_lossy(bytes).into_owned(), "utf-8"),
        Err(_) => (encoding_rs::EUC_KR.decode(bytes).0.into_owned(), "cp949"),
    }
}

/// 건너뛸 줄 다음의 몇 줄에서 가장 많이, 고르게 나오는 문자
fn guess_delimiter(text: &str, skip: usize) -> char {
    let lines: Vec<&str> = text.lines().skip(skip).filter(|l| !l.trim().is_empty()).take(5).collect();
    [',', '\t', ';', '|']
        .into_iter()
        .map(|d| (d, lines.iter().map(|l| l.matches(d).count()).min().unwrap_or(0)))
        .filter(|(_, n)| *n > 0)
        .max_by_key(|(_, n)| *n)
        .map(|(d, _)| d)
        .unwrap_or(',')
}

/// 미리 보기: 앞 `limit` 행
#[tauri::command]
pub fn import_read(path: String, opts: ReadOpts, limit: Option<usize>) -> R<serde_json::Value> {
    let mut t = read_table(&path, &opts).map_err(|e| ErrView::msg("invalid", e))?;
    let total = t.rows.len();
    t.rows.truncate(limit.unwrap_or(100));
    Ok(serde_json::json!({ "table": t, "total": total }))
}

/// 대상 컬럼 하나 — 파일의 몇 번째 열을 어떻게 넣을지
#[derive(Debug, Clone, Deserialize)]
pub struct ColMap {
    pub column: String,
    pub src: usize,
    /// text | number | date | timestamp
    #[serde(default)]
    pub kind: String,
    /// date/timestamp 의 형식 (Oracle 형식, 예: YYYY-MM-DD HH24:MI:SS)
    pub format: Option<String>,
}

fn quote_ident(s: &str) -> String {
    format!("\"{}\"", s.replace('"', ""))
}

/// INSERT 문 (바인드는 :1, :2 … 대상 컬럼 순서)
pub fn insert_sql(owner: &str, table: &str, cols: &[ColMap]) -> String {
    let names: Vec<String> = cols.iter().map(|c| quote_ident(&c.column)).collect();
    let vals: Vec<String> = cols
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let b = format!(":{}", i + 1);
            let fmt = c.format.as_deref().unwrap_or("").replace('\'', "''");
            match c.kind.as_str() {
                "date" if !fmt.is_empty() => format!("TO_DATE({b}, '{fmt}')"),
                "timestamp" if !fmt.is_empty() => format!("TO_TIMESTAMP({b}, '{fmt}')"),
                _ => b,
            }
        })
        .collect();
    format!("INSERT INTO {}.{} ({}) VALUES ({})", quote_ident(owner), quote_ident(table), names.join(", "), vals.join(", "))
}

/// 값 손질: 숫자는 천 단위 쉼표·공백·통화 기호를 뺀다
pub fn clean(v: &Option<String>, kind: &str) -> Option<String> {
    let s = v.as_deref()?.trim();
    if s.is_empty() {
        return None;
    }
    Some(match kind {
        "number" => s.chars().filter(|c| !matches!(c, ',' | ' ' | '₩' | '$' | '\u{a0}')).collect(),
        _ => s.to_string(),
    })
}

#[derive(Serialize, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ImportEvent {
    Progress { done: usize, total: usize, inserted: u64, failed: usize },
}

#[derive(Serialize)]
pub struct ImportSummary {
    total: usize,
    inserted: u64,
    failed: usize,
    stopped: bool,
    /// (파일의 행 번호 — 머리글 제외 1부터, 오류)
    errors: Vec<(usize, String)>,
    txn_pending: bool,
    sql: String,
}

#[derive(Deserialize)]
pub struct RunArgs {
    id: u64,
    path: String,
    opts: ReadOpts,
    owner: String,
    table: String,
    columns: Vec<ColMap>,
    /// 오류 행은 건너뛰고 계속 (아니면 첫 오류에서 멈춤)
    #[serde(default)]
    skip_errors: bool,
    #[serde(default)]
    confirmed: bool,
}

#[tauri::command]
pub async fn import_run(st: State<'_, AppState>, args: RunArgs, on_event: Channel<ImportEvent>) -> R<ImportSummary> {
    if args.columns.is_empty() {
        return Err(ErrView::msg("invalid", "넣을 컬럼을 하나 이상 고르세요"));
    }
    let (s, txn, stop) = st.session(args.id)?;
    if !args.confirmed && crate::db::is_production(&st, args.id) {
        return Err(ErrView::msg("confirm_required", format!("운영 DB 입니다 — {}.{} 에 데이터를 넣습니다.", args.owner, args.table)));
    }
    let path = args.path.clone();
    let opts = args.opts.clone();
    let t = tauri::async_runtime::spawn_blocking(move || read_table(&path, &opts))
        .await
        .map_err(|e| ErrView::msg("invalid", e.to_string()))?
        .map_err(|e| ErrView::msg("invalid", e))?;
    let width = t.headers.len();
    if let Some(c) = args.columns.iter().find(|c| c.src >= width) {
        return Err(ErrView::msg("invalid", format!("{} 의 원본 열({}번)이 파일에 없습니다", c.column, c.src + 1)));
    }
    let sql = insert_sql(&args.owner, &args.table, &args.columns);
    stop.store(false, Ordering::Relaxed);
    let total = t.rows.len();
    let (mut inserted, mut errors, mut failed, mut stopped) = (0u64, Vec::new(), 0usize, false);
    for (ci, chunk) in t.rows.chunks(CHUNK).enumerate() {
        if stop.load(Ordering::Relaxed) {
            stopped = true;
            break;
        }
        let rows: Vec<Vec<Option<String>>> = chunk.iter().map(|r| args.columns.iter().map(|c| clean(&r[c.src], &c.kind)).collect()).collect();
        let base = ci * CHUNK;
        match s.batch(&sql, rows, args.skip_errors).await {
            Ok(r) => {
                inserted += r.inserted;
                failed += r.errors.len();
                for (off, msg) in r.errors {
                    if errors.len() < MAX_ERRORS {
                        errors.push((base + off + 1, msg));
                    }
                }
            }
            Err(e) => {
                // 멈춤 모드: 이 묶음은 통째로 실패 (앞 묶음은 들어가 있다 — 롤백하면 된다)
                txn.store(inserted > 0, Ordering::Relaxed);
                let ev: ErrView = e.into();
                errors.push((base + 1, format!("{}행부터의 묶음({}행)에서 실패: {}", base + 1, chunk.len(), ev.message)));
                failed += chunk.len();
                stopped = true;
                break;
            }
        }
        if inserted > 0 {
            txn.store(true, Ordering::Relaxed);
        }
        let _ = on_event.send(ImportEvent::Progress { done: (base + chunk.len()).min(total), total, inserted, failed });
    }
    Ok(ImportSummary { total, inserted, failed, stopped, errors, txn_pending: txn.load(Ordering::Relaxed), sql })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str, bytes: &[u8]) -> String {
        let p = std::env::temp_dir().join(format!("sqls-imp-{}-{name}", std::process::id()));
        std::fs::write(&p, bytes).unwrap();
        p.to_string_lossy().to_string()
    }

    #[test]
    fn csv_utf8_and_cp949_and_delimiters() {
        let p = tmp("a.csv", "\u{feff}ID,이름,금액\n1,홍길동,\"1,234\"\n\n2,,5\n".as_bytes());
        let t = read_table(&p, &ReadOpts { header: true, ..Default::default() }).unwrap();
        assert_eq!(t.headers, vec!["ID", "이름", "금액"]);
        assert_eq!(t.rows, vec![vec![Some("1".into()), Some("홍길동".into()), Some("1,234".into())], vec![Some("2".into()), None, Some("5".into())]]);
        assert_eq!(t.encoding.as_deref(), Some("utf-8"));
        let (cp, _, _) = encoding_rs::EUC_KR.encode("코드\t이름\nA\t가나\n");
        let p = tmp("b.txt", &cp);
        let t = read_table(&p, &ReadOpts { header: true, ..Default::default() }).unwrap();
        assert_eq!(t.encoding.as_deref(), Some("cp949"));
        assert_eq!(t.delimiter.as_deref(), Some("\\t"));
        assert_eq!(t.rows[0][1].as_deref(), Some("가나"));
        // 머리글 없음 + 앞 줄 건너뛰기
        let p = tmp("c.csv", b"title line\nx;y\n1;2\n");
        let t = read_table(&p, &ReadOpts { header: false, skip: 1, ..Default::default() }).unwrap();
        assert_eq!(t.headers, vec!["열1", "열2"]);
        assert_eq!(t.rows.len(), 2);
    }

    #[test]
    fn excel_types() {
        use rust_xlsxwriter::{ExcelDateTime, Format, Workbook};
        let mut wb = Workbook::new();
        let ws = wb.add_worksheet();
        ws.write_string(0, 0, "ID").unwrap();
        ws.write_string(0, 1, "날짜").unwrap();
        ws.write_string(0, 2, "금액").unwrap();
        ws.write_number(1, 0, 12345678901234.0).unwrap();
        let d = ExcelDateTime::parse_from_str("2026-10-12 13:45:00").unwrap();
        ws.write_datetime_with_format(1, 1, &d, &Format::new().set_num_format("yyyy-mm-dd hh:mm:ss")).unwrap();
        ws.write_number(1, 2, 0.1 + 0.2).unwrap();
        let p = std::env::temp_dir().join(format!("sqls-imp-{}-x.xlsx", std::process::id()));
        wb.save(&p).unwrap();
        let t = read_table(&p.to_string_lossy(), &ReadOpts { header: true, ..Default::default() }).unwrap();
        assert_eq!(t.sheets, vec!["Sheet1"]);
        assert_eq!(t.rows[0][0].as_deref(), Some("12345678901234"));
        assert_eq!(t.rows[0][1].as_deref(), Some("2026-10-12 13:45:00"));
        assert_eq!(t.rows[0][2].as_deref(), Some("0.30000000000000004"));
    }

    #[test]
    fn sql_and_cleaning() {
        let cols = vec![
            ColMap { column: "ID".into(), src: 0, kind: "number".into(), format: None },
            ColMap { column: "HIRED".into(), src: 1, kind: "date".into(), format: Some("YYYY-MM-DD".into()) },
            ColMap { column: "NOTE\"x".into(), src: 2, kind: "text".into(), format: Some("x'".into()) },
        ];
        assert_eq!(insert_sql("APP", "EMP", &cols), "INSERT INTO \"APP\".\"EMP\" (\"ID\", \"HIRED\", \"NOTEx\") VALUES (:1, TO_DATE(:2, 'YYYY-MM-DD'), :3)");
        assert_eq!(clean(&Some(" 1,234,567 ".into()), "number").as_deref(), Some("1234567"));
        assert_eq!(clean(&Some("₩ 5,000".into()), "number").as_deref(), Some("5000"));
        assert_eq!(clean(&Some("  ".into()), "text"), None);
        assert_eq!(clean(&Some(" a b ".into()), "text").as_deref(), Some("a b"));
        assert_eq!(num_text(3.0), "3");
        assert_eq!(num_text(1e20), "100000000000000000000");
    }
}
