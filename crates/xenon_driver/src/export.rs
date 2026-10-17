//! Turning result cells into text: for the clipboard, or a whole result for a file.

use std::io::{self, Write};

use crate::{Column, Value};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    /// Tab-separated, the format spreadsheets paste natively. A single cell is
    /// copied as its plain value.
    Tsv,
    Csv,
    /// An array of objects keyed by column name.
    Json,
    /// A GitHub-flavoured Markdown table.
    Markdown,
    /// `insert` statements, see [`Sql`].
    Sql,
}

impl Format {
    pub fn extension(self) -> &'static str {
        match self {
            Format::Tsv => "tsv",
            Format::Csv => "csv",
            Format::Json => "json",
            Format::Markdown => "md",
            Format::Sql => "sql",
        }
    }
}

/// How to spell a table, its columns and its values as SQL, for [`Format::Sql`].
pub struct Sql<'a> {
    pub table: String,
    pub ident: &'a dyn Fn(&str) -> String,
    pub literal: &'a dyn Fn(&Value) -> String,
}

/// Rows per `insert` statement: few enough for every engine (SQL Server allows 1,000).
const INSERT_BATCH: usize = 100;

/// Render `rows` (each limited to `columns`) in `format`. Headers are included
/// for CSV and multi-cell TSV.
pub fn render(format: Format, columns: &[&Column], rows: &[Vec<&Value>]) -> String {
    render_sql(format, columns, rows, None)
}

/// [`render`], with what [`Format::Sql`] needs to spell statements.
pub fn render_sql(
    format: Format,
    columns: &[&Column],
    rows: &[Vec<&Value>],
    sql: Option<&Sql>,
) -> String {
    // One cell is copied exactly as it is, formula-like or not: it's pasted
    // into editors far more than into spreadsheets.
    if format == Format::Tsv && columns.len() == 1 && rows.len() == 1 {
        return plain(rows[0][0]);
    }
    let mut out = Vec::new();
    write(
        format,
        columns,
        rows.iter().map(|row| row.iter().copied()),
        sql,
        &mut out,
    )
    .expect("writing to memory");
    String::from_utf8(out).expect("rendered text is UTF-8")
}

/// Write every row in `format` to `out`. [`Format::Sql`] needs `sql`; without it
/// the statements name the table `results`.
pub fn write<'v, R, C>(
    format: Format,
    columns: &[&Column],
    rows: R,
    sql: Option<&Sql>,
    out: &mut dyn Write,
) -> io::Result<()>
where
    R: IntoIterator<Item = C>,
    C: IntoIterator<Item = &'v Value>,
{
    match format {
        Format::Tsv => delimited(columns, rows, '\t', out),
        Format::Csv => delimited(columns, rows, ',', out),
        Format::Json => json(columns, rows, out),
        Format::Markdown => markdown(columns, rows, out),
        Format::Sql => match sql {
            Some(sql) => inserts(columns, rows, sql, out),
            None => {
                let fallback = Sql {
                    table: "results".into(),
                    ident: &|name| {
                        if crate::sql::is_plain_ident(name) {
                            name.to_string()
                        } else {
                            format!("\"{}\"", name.replace('"', "\"\""))
                        }
                    },
                    literal: &ansi_literal,
                };
                inserts(columns, rows, &fallback, out)
            }
        },
    }
}

/// A standard SQL literal, for when no connection says how its engine spells one.
fn ansi_literal(value: &Value) -> String {
    match value {
        Value::Null => "null".into(),
        Value::Integer(v) => v.to_string(),
        Value::Real(v) if v.is_finite() => v.to_string(),
        Value::Decimal(v) if is_json_number(v) => v.clone(),
        Value::Real(_) => format!("'{value}'", value = value.display()),
        Value::Decimal(v) | Value::Text(v) => format!("'{}'", v.replace('\'', "''")),
        Value::Blob(bytes) => format!("X'{}'", &hex(bytes)[2..]),
    }
}

fn plain(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::Blob(bytes) => hex(bytes),
        other => other.display().into_owned(),
    }
}

fn delimited<'v, R, C>(
    columns: &[&Column],
    rows: R,
    separator: char,
    out: &mut dyn Write,
) -> io::Result<()>
where
    R: IntoIterator<Item = C>,
    C: IntoIterator<Item = &'v Value>,
{
    let mut sep = [0; 4];
    let sep = separator.encode_utf8(&mut sep).as_bytes();
    let mut line = |cells: &mut dyn Iterator<Item = String>| -> io::Result<()> {
        for (i, cell) in cells.enumerate() {
            if i > 0 {
                out.write_all(sep)?;
            }
            out.write_all(quote(&cell, separator).as_bytes())?;
        }
        out.write_all(b"\n")
    };
    line(&mut columns.iter().map(|c| defused(c.name.clone())))?;
    for row in rows {
        line(&mut row.into_iter().map(|value| match value {
            // Numbers stay as they are (`-5`); text that a spreadsheet would
            // run as a formula is set apart.
            Value::Text(_) => defused(plain(value)),
            other => plain(other),
        }))?;
    }
    Ok(())
}

/// Text starting with `=`, `+`, `-`, `@`, a tab or a carriage return is run
/// as a formula by spreadsheets opening a CSV or TSV (`=HYPERLINK(…)`,
/// `@SUM(…)`): a leading `'` keeps it text, as the spreadsheet's own escape.
/// A `+` or `-` followed only by digits and punctuation (`+44 20 7946 0958`,
/// `-`) can call nothing, so it's left as it is.
fn defused(cell: String) -> String {
    let harmless = cell.starts_with(['+', '-'])
        && cell
            .chars()
            .all(|c| c.is_ascii_digit() || " .,()+-/".contains(c));
    if cell.starts_with(['=', '+', '-', '@', '\t', '\r']) && !harmless {
        format!("'{cell}")
    } else {
        cell
    }
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

fn json<'v, R, C>(columns: &[&Column], rows: R, out: &mut dyn Write) -> io::Result<()>
where
    R: IntoIterator<Item = C>,
    C: IntoIterator<Item = &'v Value>,
{
    // Two columns of the same name (a join's two `id`s) would be one key twice,
    // which most readers take as the last only: the later ones are `id_2`, `id_3`.
    let mut keys: Vec<String> = Vec::with_capacity(columns.len());
    for column in columns {
        let mut key = column.name.clone();
        let mut n = 1;
        while keys.contains(&key) {
            n += 1;
            key = format!("{}_{n}", column.name);
        }
        keys.push(key);
    }
    out.write_all(b"[")?;
    let mut any = false;
    for row in rows {
        out.write_all(if any { b",\n  {" } else { b"\n  {" })?;
        any = true;
        for (j, (key, value)) in keys.iter().zip(row).enumerate() {
            if j > 0 {
                out.write_all(b", ")?;
            }
            out.write_all(json_string(key).as_bytes())?;
            out.write_all(b": ")?;
            out.write_all(json_value(value).as_bytes())?;
        }
        out.write_all(b"}")?;
    }
    out.write_all(b"\n]")
}

fn markdown<'v, R, C>(columns: &[&Column], rows: R, out: &mut dyn Write) -> io::Result<()>
where
    R: IntoIterator<Item = C>,
    C: IntoIterator<Item = &'v Value>,
{
    // Pipes would end the cell and line breaks the row; a backslash before a
    // pipe would undo its escape, and `<`, `>`, `&` would be read as HTML.
    let cell = |text: &str| {
        text.replace('\\', "\\\\")
            .replace('|', "\\|")
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace("\r\n", "<br>")
            .replace('\n', "<br>")
    };
    let names: Vec<String> = columns.iter().map(|c| cell(&c.name)).collect();
    writeln!(out, "| {} |", names.join(" | "))?;
    writeln!(out, "|{}", " --- |".repeat(columns.len()))?;
    for row in rows {
        let cells: Vec<String> = row
            .into_iter()
            .map(|value| match value {
                Value::Null => "NULL".into(),
                other => cell(&plain(other)),
            })
            .collect();
        writeln!(out, "| {} |", cells.join(" | "))?;
    }
    Ok(())
}

fn inserts<'v, R, C>(columns: &[&Column], rows: R, sql: &Sql, out: &mut dyn Write) -> io::Result<()>
where
    R: IntoIterator<Item = C>,
    C: IntoIterator<Item = &'v Value>,
{
    let names: Vec<String> = columns.iter().map(|c| (sql.ident)(&c.name)).collect();
    let head = format!("insert into {} ({}) values", sql.table, names.join(", "));
    let mut in_batch = 0;
    for row in rows {
        out.write_all(if in_batch == 0 { head.as_bytes() } else { b"," })?;
        let values: Vec<String> = row.into_iter().map(|v| (sql.literal)(v)).collect();
        write!(out, "\n  ({})", values.join(", "))?;
        in_batch += 1;
        if in_batch == INSERT_BATCH {
            out.write_all(b";\n")?;
            in_batch = 0;
        }
    }
    if in_batch > 0 {
        out.write_all(b";\n")?;
    }
    Ok(())
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
    fn json_keys_and_markdown_cells_are_unambiguous() {
        let id = Column {
            name: "id".into(),
            type_name: None,
        };
        let (one, two) = (Value::Integer(1), Value::Integer(2));
        let json = render(Format::Json, &[&id, &id], &[vec![&one, &two]]);
        assert!(json.contains("\"id\": 1, \"id_2\": 2"), "{json}");
        let tricky = Value::Text("a\\|b <i>&".into());
        let md = render(Format::Markdown, &[&id], &[vec![&tricky]]);
        assert!(md.contains("a\\\\\\|b &lt;i&gt;&amp;"), "{md}");
    }

    #[test]
    fn csv_and_tsv_dont_pass_formulas_through() {
        let c = Column {
            name: "note".into(),
            type_name: None,
        };
        let n = Column {
            name: "n".into(),
            type_name: None,
        };
        let formula = Value::Text("=HYPERLINK(\"http://x\")".into());
        let at = Value::Text("@SUM(A1)".into());
        let minus = Value::Integer(-5);
        let rows = vec![vec![&formula, &minus], vec![&at, &minus]];
        let csv = render(Format::Csv, &[&c, &n], &rows);
        assert!(csv.contains("\"'=HYPERLINK(\"\"http://x\"\")\""), "{csv}");
        assert!(csv.contains("'@SUM(A1),-5"), "{csv}");
        let tsv = render(Format::Tsv, &[&c, &n], &rows);
        assert!(tsv.contains("'@SUM(A1)\t-5"), "{tsv}");
    }

    #[test]
    fn signs_before_plain_numbers_arent_formulas() {
        let c = col("phone");
        for (text, out) in [
            ("+44 20 7946 0958", "+44 20 7946 0958"),
            ("-", "-"),
            ("-12.5", "-12.5"),
            ("+1 (555) 010-0199", "+1 (555) 010-0199"),
            ("-cmd|' /C calc'!A0", "'-cmd|' /C calc'!A0"),
            ("+A1", "'+A1"),
        ] {
            let value = Value::Text(text.into());
            let csv = render(Format::Csv, &[&c], &[vec![&value]]);
            assert_eq!(csv.lines().nth(1), Some(out), "{text}");
        }
    }

    #[test]
    fn tsv_and_csv_include_headers_and_quote() {
        let (a, b) = (col("id"), col("note"));
        let rows = vec![
            vec![Value::Integer(1), Value::Text("plain".into())],
            vec![
                Value::Integer(2),
                Value::Text("has \"quotes\", commas\nand lines".into()),
            ],
            vec![Value::Integer(3), Value::Null],
        ];
        let rows = refs(&rows);
        assert_eq!(
            render(Format::Csv, &[&a, &b], &rows),
            "id,note\n1,plain\n2,\"has \"\"quotes\"\", commas\nand lines\"\n3,\n"
        );
        assert_eq!(
            render(Format::Tsv, &[&a, &b], &rows[..1]),
            "id\tnote\n1\tplain\n"
        );
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
            vec![
                Value::Integer(2),
                Value::Null,
                Value::Real(f64::NAN),
                Value::Real(2.5),
            ],
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
    fn markdown_escapes_pipes_and_breaks() {
        let (a, b) = (col("id"), col("note"));
        let rows = vec![
            vec![Value::Integer(1), Value::Text("a|b\nc".into())],
            vec![Value::Integer(2), Value::Null],
        ];
        assert_eq!(
            render(Format::Markdown, &[&a, &b], &refs(&rows)),
            "| id | note |\n| --- | --- |\n| 1 | a\\|b<br>c |\n| 2 | NULL |\n"
        );
    }

    #[test]
    fn sql_batches_inserts() {
        let (a, b) = (col("id"), col("name"));
        let rows: Vec<Vec<Value>> = (0..101)
            .map(|i| vec![Value::Integer(i), Value::Text(format!("it's {i}"))])
            .collect();
        let text = render(Format::Sql, &[&a, &b], &refs(&rows));
        assert!(
            text.starts_with("insert into results (id, name) values\n  (0, 'it''s 0'),\n  (1, ")
        );
        assert_eq!(text.matches("insert into").count(), 2);
        assert!(text.ends_with("values\n  (100, 'it''s 100');\n"));
    }

    #[test]
    fn blobs_are_hex() {
        let c = col("data");
        let v = Value::Blob(vec![0x01, 0xab]);
        assert_eq!(render(Format::Tsv, &[&c], &[vec![&v]]), "\\x01ab");
        assert_eq!(
            render(Format::Json, &[&c], &[vec![&v]]),
            "[\n  {\"data\": \"\\\\x01ab\"}\n]"
        );
    }
}
