//! Saving a whole result to a file: the text formats from `xenon_driver::export`, plus Excel.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context as _, Result, bail};
use rust_xlsxwriter::{Format as CellFormat, Workbook};
use xenon_driver::export::{self, Format, Sql};
use xenon_driver::{Connection, ResultSet, Value};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileFormat {
    Text(Format),
    Excel,
}

impl FileFormat {
    pub fn extension(self) -> &'static str {
        match self {
            FileFormat::Text(format) => format.extension(),
            FileFormat::Excel => "xlsx",
        }
    }
}

/// The formats offered by Export, with their menu labels.
pub const FORMATS: [(FileFormat, &str); 6] = [
    (FileFormat::Text(Format::Csv), "CSV"),
    (FileFormat::Excel, "Excel"),
    (FileFormat::Text(Format::Json), "JSON"),
    (FileFormat::Text(Format::Sql), "SQL inserts"),
    (FileFormat::Text(Format::Markdown), "Markdown"),
    (FileFormat::Text(Format::Tsv), "TSV"),
];

/// Rows below the header that fit on one Excel sheet.
const EXCEL_MAX_ROWS: usize = 1_048_575;
/// Characters one Excel cell can hold.
const EXCEL_MAX_TEXT: usize = 32_767;

/// What to export: a result, in the order the grid shows it.
pub struct Job {
    pub data: Arc<ResultSet>,
    pub order: Arc<Vec<usize>>,
    /// The table SQL inserts go into, already quoted, and the connection that
    /// spells its values.
    pub sql: Option<(String, Arc<dyn Connection>)>,
}

/// Write the job to `path`; returns how many rows were written.
pub fn save(job: &Job, format: FileFormat, path: &Path) -> Result<usize> {
    match format {
        FileFormat::Text(format) => save_text(job, format, path)?,
        FileFormat::Excel => save_excel(job, path)?,
    }
    Ok(job.order.len())
}

fn save_text(job: &Job, format: Format, path: &Path) -> Result<()> {
    let file = File::create(path).with_context(|| format!("Couldn't create {}", path.display()))?;
    let mut out = BufWriter::new(file);
    let columns: Vec<_> = job.data.columns.iter().collect();
    let rows = job.order.iter().map(|&ix| job.data.rows[ix].iter());
    let ident = |name: &str| match &job.sql {
        Some((_, conn)) => conn.ident(name),
        None => name.to_string(),
    };
    let literal = |value: &Value| match &job.sql {
        Some((_, conn)) => conn.literal(value),
        None => value.display().into_owned(),
    };
    let sql = job.sql.as_ref().map(|(table, _)| Sql {
        table: table.clone(),
        ident: &ident,
        literal: &literal,
    });
    export::write(format, &columns, rows, sql.as_ref(), &mut out)
        .and_then(|()| out.flush())
        .with_context(|| format!("Couldn't write {}", path.display()))
}

fn save_excel(job: &Job, path: &Path) -> Result<()> {
    let rows = job.order.len();
    if rows > EXCEL_MAX_ROWS {
        bail!(
            "Excel sheets hold up to {EXCEL_MAX_ROWS} rows; this result has {rows}. Export it as CSV instead."
        );
    }
    if job.data.columns.len() > u16::MAX as usize {
        bail!("Too many columns for Excel.");
    }
    let mut workbook = Workbook::new();
    let sheet = workbook.add_worksheet_with_constant_memory();
    let bold = CellFormat::new().set_bold();
    let mut widths: Vec<usize> = job
        .data
        .columns
        .iter()
        .map(|c| c.name.chars().count())
        .collect();
    for (col, column) in job.data.columns.iter().enumerate() {
        sheet.write_string_with_format(0, col as u16, &column.name, &bold)?;
    }
    for (line, &ix) in job.order.iter().enumerate() {
        let row = line as u32 + 1;
        for (col, value) in job.data.rows[ix].iter().enumerate() {
            let col16 = col as u16;
            match cell(value) {
                Cell::Empty => {}
                Cell::Number(n) => {
                    sheet.write_number(row, col16, n)?;
                }
                Cell::Text(text) => {
                    sheet.write_string(row, col16, text.as_str())?;
                }
            }
            // Size columns from the first rows, as the grid does.
            if line < 200 {
                widths[col] = widths[col].max(value.display().chars().count());
            }
        }
    }
    for (col, width) in widths.into_iter().enumerate() {
        sheet.set_column_width(col as u16, (width + 2).clamp(6, 60) as f64)?;
    }
    sheet.set_freeze_panes(1, 0)?;
    if !job.data.columns.is_empty() {
        sheet.autofilter(0, 0, rows as u32, job.data.columns.len() as u16 - 1)?;
    }
    workbook
        .save(path)
        .with_context(|| format!("Couldn't write {}", path.display()))
}

enum Cell {
    Empty,
    Number(f64),
    Text(String),
}

/// A value as Excel should hold it: a number when it survives as one (Excel keeps
/// 15 significant digits), text otherwise.
fn cell(value: &Value) -> Cell {
    match value {
        Value::Null => Cell::Empty,
        Value::Integer(v) if v.unsigned_abs() < 1_000_000_000_000_000 => Cell::Number(*v as f64),
        Value::Integer(v) => Cell::Text(v.to_string()),
        Value::Real(v) if v.is_finite() => Cell::Number(*v),
        Value::Decimal(v) if fits_excel_number(v) => v
            .parse()
            .map_or_else(|_| Cell::Text(v.clone()), Cell::Number),
        Value::Blob(bytes) => Cell::Text(xenon_driver::hex(bytes)),
        other => {
            let text = other.display();
            match text.char_indices().nth(EXCEL_MAX_TEXT) {
                Some((cut, _)) => Cell::Text(text[..cut].to_string()),
                None => Cell::Text(text.into_owned()),
            }
        }
    }
}

/// A plain decimal (`-12.50`) with no more digits than Excel keeps.
fn fits_excel_number(text: &str) -> bool {
    let digits = text.strip_prefix('-').unwrap_or(text);
    let (whole, fraction) = digits.split_once('.').unwrap_or((digits, ""));
    !whole.is_empty()
        && whole.bytes().all(|b| b.is_ascii_digit())
        && fraction.bytes().all(|b| b.is_ascii_digit())
        && (digits.contains('.') == !fraction.is_empty())
        && whole.trim_start_matches('0').len() + fraction.len() <= 15
}

#[cfg(test)]
mod tests {
    use super::*;
    use xenon_driver::Column;

    fn job() -> Job {
        let data = ResultSet {
            columns: vec![
                Column {
                    name: "id".into(),
                    type_name: None,
                },
                Column {
                    name: "total".into(),
                    type_name: None,
                },
            ],
            rows: vec![
                vec![Value::Integer(2), Value::Decimal("10.50".into())],
                vec![Value::Integer(1), Value::Null],
            ],
            truncated: false,
        };
        Job {
            data: Arc::new(data),
            order: Arc::new(vec![1, 0]),
            sql: None,
        }
    }

    #[test]
    fn text_follows_the_grid_order() {
        let dir = std::env::temp_dir().join(format!("xenon-export-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("out.csv");
        assert_eq!(
            save(&job(), FileFormat::Text(Format::Csv), &path).unwrap(),
            2
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "id,total\n1,\n2,10.50\n"
        );
        let path = dir.join("out.xlsx");
        save(&job(), FileFormat::Excel, &path).unwrap();
        // An .xlsx file is a zip archive.
        assert_eq!(&std::fs::read(&path).unwrap()[..2], b"PK");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn excel_numbers_only_when_exact() {
        assert!(matches!(
            cell(&Value::Decimal("123.45".into())),
            Cell::Number(_)
        ));
        assert!(matches!(
            cell(&Value::Decimal("1234567890.1234567".into())),
            Cell::Text(_)
        ));
        assert!(matches!(
            cell(&Value::Decimal("$1.00".into())),
            Cell::Text(_)
        ));
        assert!(matches!(cell(&Value::Integer(i64::MAX)), Cell::Text(_)));
        assert!(matches!(cell(&Value::Integer(42)), Cell::Number(_)));
        assert!(matches!(cell(&Value::Null), Cell::Empty));
    }
}
