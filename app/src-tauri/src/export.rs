//! 결과 그리드 → 엑셀(.xlsx).
//!
//! 셀은 화면과 같은 문자열로 온다. 엑셀에서 합계·필터가 되도록 숫자와 날짜는 형식을 살려 쓴다.
//! 단, 15자리를 넘는 숫자(주민번호·계좌번호·긴 ID)는 엑셀 숫자로 바꾸면 끝자리가 0 이 되므로 글자로 둔다.

use rust_xlsxwriter::{Color, ExcelDateTime, Format, FormatBorder, Workbook, Worksheet};
use serde::Deserialize;

use crate::state::ErrView;

type R<T> = Result<T, ErrView>;

/// 엑셀 한 시트의 최대 행 (머리글 한 줄을 뺀다)
pub const MAX_ROWS: usize = 1_048_575;
/// 엑셀 셀 하나의 최대 글자 수
const MAX_CELL_CHARS: usize = 32_767;
/// 열 너비 상한 (글자 수)
const MAX_COL_WIDTH: f64 = 60.0;

#[derive(Deserialize)]
pub struct ExportColumn {
    name: String,
    type_name: String,
}

#[derive(Deserialize)]
pub struct ExportArgs {
    path: String,
    columns: Vec<ExportColumn>,
    rows: Vec<Vec<Option<String>>>,
    /// 결과를 만든 SQL — 있으면 두 번째 시트에 남긴다
    #[serde(default)]
    sql: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Kind {
    Number,
    Date,
    Text,
}

fn kind_of(type_name: &str) -> Kind {
    let t = type_name.trim().to_ascii_uppercase();
    if t.starts_with("NUMBER") || t.starts_with("FLOAT") || t.starts_with("BINARY_") || t.starts_with("INTEGER") {
        Kind::Number
    } else if t == "DATE" || (t.starts_with("TIMESTAMP") && !t.contains("ZONE")) {
        Kind::Date
    } else {
        Kind::Text
    }
}

/// 엑셀 숫자로 바꿔도 값이 그대로인지 (유효 숫자 15자리 이하)
fn as_number(s: &str) -> Option<f64> {
    let t = s.trim();
    let digits = t.trim_start_matches(['-', '+']).chars().filter(|c| c.is_ascii_digit()).collect::<String>();
    let significant = digits.trim_start_matches('0').len();
    if significant > 15 || t.contains(['e', 'E']) {
        return None;
    }
    t.parse::<f64>().ok().filter(|v| v.is_finite())
}

/// "YYYY-MM-DD HH:MM:SS[.ffffff]" (오라클 DATE/TIMESTAMP 를 문자열로 읽은 모양)
fn as_datetime(s: &str) -> Option<ExcelDateTime> {
    let t = s.trim();
    let (date, time) = t.split_once(' ').unwrap_or((t, "00:00:00"));
    let mut d = date.split('-');
    let (y, m, dd): (u16, u8, u8) = (d.next()?.parse().ok()?, d.next()?.parse().ok()?, d.next()?.parse().ok()?);
    let (hms, frac) = time.split_once('.').unwrap_or((time, "0"));
    let mut tt = hms.split(':');
    let (hh, mi, ss): (u16, u8, f64) = (tt.next()?.parse().ok()?, tt.next()?.parse().ok()?, tt.next()?.parse().ok()?);
    let frac: f64 = format!("0.{frac}").parse().unwrap_or(0.0);
    // 엑셀은 1900-01-01 이전 날짜를 날짜로 못 쓴다
    if y < 1900 {
        return None;
    }
    ExcelDateTime::from_ymd(y, m, dd).ok()?.and_hms(hh, mi, ss + frac).ok()
}

fn cut(s: &str) -> &str {
    match s.char_indices().nth(MAX_CELL_CHARS) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

fn xerr(e: rust_xlsxwriter::XlsxError) -> ErrView {
    ErrView::msg("io", format!("엑셀 파일을 만들 수 없습니다: {e}"))
}

fn write_sheet(ws: &mut Worksheet, a: &ExportArgs) -> R<()> {
    let head = Format::new()
        .set_bold()
        .set_background_color(Color::RGB(0xE8EEF7))
        .set_border_bottom(FormatBorder::Thin);
    let date_fmt = Format::new().set_num_format("yyyy-mm-dd hh:mm:ss");
    let kinds: Vec<Kind> = a.columns.iter().map(|c| kind_of(&c.type_name)).collect();
    let mut widths: Vec<f64> = a.columns.iter().map(|c| c.name.chars().count() as f64 + 2.0).collect();

    for (c, col) in a.columns.iter().enumerate() {
        ws.write_string_with_format(0, c as u16, &col.name, &head).map_err(xerr)?;
    }
    for (r, row) in a.rows.iter().enumerate() {
        let xr = (r + 1) as u32;
        for (c, cell) in row.iter().enumerate().take(a.columns.len()) {
            let Some(v) = cell else { continue };
            let xc = c as u16;
            let written_as_text = match kinds[c] {
                Kind::Number => match as_number(v) {
                    Some(n) => {
                        ws.write_number(xr, xc, n).map_err(xerr)?;
                        false
                    }
                    None => true,
                },
                Kind::Date => match as_datetime(v) {
                    Some(dt) => {
                        ws.write_datetime_with_format(xr, xc, &dt, &date_fmt).map_err(xerr)?;
                        widths[c] = widths[c].max(20.0);
                        false
                    }
                    None => true,
                },
                Kind::Text => true,
            };
            if written_as_text {
                ws.write_string(xr, xc, cut(v)).map_err(xerr)?;
            }
            // 한글은 영문의 두 배 폭
            let w: f64 = v.chars().take(80).map(|ch| if ch.is_ascii() { 1.0 } else { 2.0 }).sum();
            widths[c] = widths[c].max(w + 2.0);
        }
    }
    for (c, w) in widths.iter().enumerate() {
        ws.set_column_width(c as u16, w.min(MAX_COL_WIDTH)).map_err(xerr)?;
    }
    ws.set_freeze_panes(1, 0).map_err(xerr)?;
    if !a.columns.is_empty() {
        ws.autofilter(0, 0, a.rows.len() as u32, (a.columns.len() - 1) as u16).map_err(xerr)?;
    }
    Ok(())
}

pub fn write_xlsx(a: &ExportArgs) -> R<()> {
    if a.columns.is_empty() {
        return Err(ErrView::msg("invalid", "내보낼 열이 없습니다"));
    }
    if a.rows.len() > MAX_ROWS {
        return Err(ErrView::msg(
            "invalid",
            format!("엑셀 한 시트는 {MAX_ROWS}행까지입니다 ({}행). 조건을 좁혀 주세요.", a.rows.len()),
        ));
    }
    if a.columns.len() > 16_384 {
        return Err(ErrView::msg("invalid", "엑셀 한 시트는 16,384열까지입니다"));
    }
    let mut wb = Workbook::new();
    let ws = wb.add_worksheet();
    ws.set_name("결과").map_err(xerr)?;
    write_sheet(ws, a)?;
    if let Some(sql) = a.sql.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        let ws2 = wb.add_worksheet();
        ws2.set_name("SQL").map_err(xerr)?;
        ws2.set_column_width(0, 120).map_err(xerr)?;
        let wrap = Format::new().set_text_wrap().set_font_name("Consolas");
        ws2.write_string_with_format(0, 0, cut(sql), &wrap).map_err(xerr)?;
    }
    // 임시 파일에 쓰고 바꿔치기 — 엑셀에서 열려 있으면 바꿔치기에서 실패한다
    let tmp = format!("{}.sqlstudio-tmp", a.path);
    wb.save(&tmp).map_err(xerr)?;
    if let Err(e) = std::fs::rename(&tmp, &a.path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(ErrView::msg(
            "io",
            format!("{}: {e}\n엑셀에서 같은 파일을 열어 두었다면 닫고 다시 시도하세요.", a.path),
        ));
    }
    Ok(())
}

/// 대용량은 수십 MB 를 만든다 — 화면을 막지 않게 블로킹 스레드에서
#[tauri::command]
pub async fn export_xlsx(args: ExportArgs) -> R<usize> {
    let n = args.rows.len();
    tokio::task::spawn_blocking(move || write_xlsx(&args))
        .await
        .map_err(|e| ErrView::msg("io", format!("엑셀 내보내기가 중단되었습니다: {e}")))??;
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds() {
        assert_eq!(kind_of("NUMBER(10,2)"), Kind::Number);
        assert_eq!(kind_of("BINARY_DOUBLE"), Kind::Number);
        assert_eq!(kind_of("DATE"), Kind::Date);
        assert_eq!(kind_of("TIMESTAMP(6)"), Kind::Date);
        assert_eq!(kind_of("TIMESTAMP(6) WITH TIME ZONE"), Kind::Text);
        assert_eq!(kind_of("VARCHAR2(20)"), Kind::Text);
    }

    #[test]
    fn numbers_keep_precision() {
        assert_eq!(as_number("1.5"), Some(1.5));
        assert_eq!(as_number("-0.00012"), Some(-0.00012));
        assert_eq!(as_number("123456789012345"), Some(123456789012345.0));
        // 16자리 이상은 글자로 (끝자리가 0 이 되지 않게)
        assert_eq!(as_number("1234567890123456"), None);
        assert_eq!(as_number("abc"), None);
    }

    #[test]
    fn datetimes() {
        assert!(as_datetime("2026-10-06 02:47:01").is_some());
        assert!(as_datetime("2026-10-06 02:47:01.123456").is_some());
        assert!(as_datetime("2026-10-06").is_some());
        assert!(as_datetime("1899-12-31 00:00:00").is_none());
        assert!(as_datetime("06-OCT-26").is_none());
    }

    #[test]
    fn writes_a_valid_workbook() {
        let dir = std::env::temp_dir().join(format!("sqls-xlsx-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.xlsx");
        let a = ExportArgs {
            path: path.display().to_string(),
            columns: vec![
                ExportColumn { name: "N".into(), type_name: "NUMBER".into() },
                ExportColumn { name: "이름".into(), type_name: "VARCHAR2(20)".into() },
                ExportColumn { name: "D".into(), type_name: "DATE".into() },
            ],
            rows: vec![
                vec![Some("1".into()), Some("한글".into()), Some("2026-10-06 02:47:01".into())],
                vec![Some("12345678901234567890".into()), None, None],
            ],
            sql: Some("select * from t".into()),
        };
        write_xlsx(&a).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(&bytes[..2], b"PK"); // zip
        assert!(!std::path::Path::new(&format!("{}.sqlstudio-tmp", a.path)).exists());
        // 눈으로 확인할 때: SQLS_KEEP_XLSX=경로 cargo test -p sqlstudio --lib export
        if let Ok(keep) = std::env::var("SQLS_KEEP_XLSX") {
            std::fs::copy(&path, keep).unwrap();
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rejects_too_many_rows_and_no_columns() {
        let a = ExportArgs { path: "x.xlsx".into(), columns: vec![], rows: vec![], sql: None };
        assert!(write_xlsx(&a).is_err());
    }
}
