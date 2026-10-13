//! Turning a block of result cells into text for the clipboard.

use crate::{Column, Value};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    /// Tab-separated, the format spreadsheets paste natively. A single cell is
    /// copied as its plain value.
    Tsv,
    Csv,
    /// An array of objects keyed by column name.
    Json,
}

/// Render `rows` (each limited to `columns`) in `format`. Headers are included
/// for CSV and multi-cell TSV.
pub fn render(format: Format, columns: &[&Column], rows: &[Vec<&Value>]) -> String {
    match format {
        Format::Tsv if columns.len() == 1 && rows.len() == 1 => plain(rows[0][0]),
        Format::Tsv => delimited(columns, rows, '\t'),
        Format::Csv => delimited(columns, rows, ','),
        Format::Json => json(columns, rows),
    }
}

fn plain(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::Blob(bytes) => hex(bytes),
        other => other.display().into_owned(),
    }
}

fn delimited(columns: &[&Column], rows: &[Vec<&Value>], separator: char) -> String {
    let mut out = String::new();
    let mut line = |cells: &mut dyn Iterator<Item = String>| {
        let cells: Vec<String> = cells.map(|cell| quote(&cell, separator)).collect();
        out.push_str(&cells.join(&separator.to_string()));
        out.push('\n');
    };
    line(&mut columns.iter().map(|c| c.name.clone()));
    for row in rows {
        line(&mut row.iter().map(|value| plain(value)));
    }
    out
}

/// RFC 4180 quoting: wrap in quotes when the cell holds the separator, a quote
/// or a line break, doubling any quotes inside.
fn quote(cell: &str, separator: char) -> String {
    if cell.contains([separator, '"', '\n', '\r']) {
        format!("\"{}\"", cell.replace('"', "\"\""))
    } else {
        cell.to_string()
    }
}

fn json(columns: &[&Column], rows: &[Vec<&Value>]) -> String {
    let mut out = String::from("[\n");
    for (i, row) in rows.iter().enumerate() {
        out.push_str("  {");
        for (j, (column, value)) in columns.iter().zip(row).enumerate() {
            if j > 0 {
                out.push_str(", ");
            }
            out.push_str(&json_string(&column.name));
            out.push_str(": ");
            out.push_str(&json_value(value));
        }
        out.push('}');
        if i + 1 < rows.len() {
            out.push(',');
        }
        out.push('\n');
    }
    out.push(']');
    out
}

fn json_value(value: &Value) -> String {
    match value {
        Value::Null => "null".into(),
        Value::Integer(v) => v.to_string(),
        Value::Real(v) if v.is_finite() => v.to_string(),
        Value::Real(v) => json_string(&v.to_string()),
        // Exact numerics stay numbers when they look like JSON numbers (not `$1.00`, `NaN`).
        Value::Decimal(v) if is_json_number(v) => v.clone(),
        Value::Decimal(v) | Value::Text(v) => json_string(v),
        Value::Blob(bytes) => json_string(&hex(bytes)),
    }
}

fn is_json_number(text: &str) -> bool {
    let digits = text.strip_prefix('-').unwrap_or(text);
    let mut parts = digits.splitn(2, '.');
    let whole = parts.next().unwrap_or("");
    let fraction = parts.next();
    !whole.is_empty()
        && whole.bytes().all(|b| b.is_ascii_digit())
        && (whole == "0" || !whole.starts_with('0'))
        && fraction.is_none_or(|f| !f.is_empty() && f.bytes().all(|b| b.is_ascii_digit()))
}

fn json_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(2 + bytes.len() * 2);
    out.push_str("\\x");
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn col(name: &str) -> Column {
        Column {
            name: name.into(),
            type_name: None,
        }
    }

    #[test]
    fn single_cell_tsv_is_the_plain_value() {
        let c = col("email");
        let v = Value::Text("a@x.io".into());
        assert_eq!(render(Format::Tsv, &[&c], &[vec![&v]]), "a@x.io");
        assert_eq!(render(Format::Tsv, &[&c], &[vec![&Value::Null]]), "");
    }

    fn refs(rows: &[Vec<Value>]) -> Vec<Vec<&Value>> {
        rows.iter().map(|row| row.iter().collect()).collect()
    }

    #[test]
    fn tsv_and_csv_include_headers_and_quote() {
        let (a, b) = (col("id"), col("note"));
        let rows = vec![
            vec![Value::Integer(1), Value::Text("plain".into())],
            vec![Value::Integer(2), Value::Text("has \"quotes\", commas\nand lines".into())],
            vec![Value::Integer(3), Value::Null],
        ];
        let rows = refs(&rows);
        assert_eq!(
            render(Format::Csv, &[&a, &b], &rows),
            "id,note\n1,plain\n2,\"has \"\"quotes\"\", commas\nand lines\"\n3,\n"
        );
        assert_eq!(render(Format::Tsv, &[&a, &b], &rows[..1]), "id\tnote\n1\tplain\n");
    }

    #[test]
    fn json_keeps_types() {
        let (a, b, c, d) = (col("id"), col("total"), col("name"), col("price"));
        let rows = vec![
            vec![
                Value::Integer(1),
                Value::Decimal("9876543210.99".into()),
                Value::Text("Line \"one\"\n".into()),
                Value::Decimal("$1.00".into()),
            ],
            vec![Value::Integer(2), Value::Null, Value::Real(f64::NAN), Value::Real(2.5)],
        ];
        assert_eq!(
            render(Format::Json, &[&a, &b, &c, &d], &refs(&rows)),
            "[\n  {\"id\": 1, \"total\": 9876543210.99, \"name\": \"Line \\\"one\\\"\\n\", \"price\": \"$1.00\"},\n  \
             {\"id\": 2, \"total\": null, \"name\": \"NaN\", \"price\": 2.5}\n]"
        );
    }

    #[test]
    fn json_number_detection() {
        for yes in ["0", "12", "-3.50", "0.1"] {
            assert!(is_json_number(yes), "{yes}");
        }
        for no in ["", "-", "01", "1.", ".5", "1e5", "$1", "NaN", "1,000"] {
            assert!(!is_json_number(no), "{no}");
        }
    }

    #[test]
    fn blobs_are_hex() {
        let c = col("data");
        let v = Value::Blob(vec![0x01, 0xab]);
        assert_eq!(render(Format::Tsv, &[&c], &[vec![&v]]), "\\x01ab");
        assert_eq!(render(Format::Json, &[&c], &[vec![&v]]), "[\n  {\"data\": \"\\\\x01ab\"}\n]");
    }
}
