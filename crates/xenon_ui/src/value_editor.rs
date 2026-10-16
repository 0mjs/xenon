//! Which editor a cell gets: a line, a text area, a JSON editor, a date with
//! "now", a choice of an enum's values, or true / false.

use xenon_driver::Value;

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum ValueKind {
    /// A one-line field.
    Line,
    /// Long or multi-line text: a text area.
    Text,
    /// JSON: a text area with highlighting, checked before staging.
    Json,
    /// A date, time or timestamp, with how it's written.
    Date(DateKind),
    /// One of an enum's values.
    Choice(Vec<String>),
    /// true / false; `numeric` when stored as 1 / 0 (MySQL `tinyint(1)`, SQL Server `bit`).
    Bool { numeric: bool },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DateKind {
    Date,
    Time,
    Timestamp,
}

impl DateKind {
    /// How values are written, shown as a hint.
    pub(crate) fn hint(self) -> &'static str {
        match self {
            DateKind::Date => "YYYY-MM-DD",
            DateKind::Time => "HH:MM:SS",
            DateKind::Timestamp => "YYYY-MM-DD HH:MM:SS",
        }
    }

    /// The current local date and/or time, written as the database expects.
    pub(crate) fn now(self) -> String {
        let now = chrono::Local::now();
        let format = match self {
            DateKind::Date => "%Y-%m-%d",
            DateKind::Time => "%H:%M:%S",
            DateKind::Timestamp => "%Y-%m-%d %H:%M:%S",
        };
        now.format(format).to_string()
    }
}

/// Values longer than this get a text area even in a plain text column.
const LONG_TEXT: usize = 100;

/// The editor for a column of `type_name` (as the engine names it), whose enum
/// `options` are known, currently holding `value`.
pub(crate) fn kind_of(
    type_name: Option<&str>,
    options: &[String],
    value: Option<&Value>,
) -> ValueKind {
    if !options.is_empty() {
        return ValueKind::Choice(options.to_vec());
    }
    let kind = type_name.unwrap_or("").trim().to_ascii_lowercase();
    let base = kind.split(['(', ' ']).next().unwrap_or("");
    if kind.contains("json") {
        return ValueKind::Json;
    }
    match (base, kind.as_str()) {
        ("bool" | "boolean", _) => {
            return ValueKind::Bool {
                numeric: matches!(value, Some(Value::Integer(_))),
            };
        }
        ("bit", _) | (_, "tinyint(1)") => return ValueKind::Bool { numeric: true },
        _ => {}
    }
    if base.starts_with("timestamp") || base.starts_with("datetime") || base == "smalldatetime" {
        return ValueKind::Date(DateKind::Timestamp);
    }
    if base == "date" {
        return ValueKind::Date(DateKind::Date);
    }
    if base.starts_with("time") {
        return ValueKind::Date(DateKind::Time);
    }
    match value {
        Some(Value::Text(text)) if text.contains('\n') || text.chars().count() > LONG_TEXT => {
            ValueKind::Text
        }
        _ => ValueKind::Line,
    }
}

/// Whether a column's type (as the engine names it) holds numbers: matched
/// as a whole name, so `interval`, `point` or `zip_code` aren't.
pub(crate) fn is_numeric_type(type_name: &str) -> bool {
    let lower = type_name.to_ascii_lowercase();
    let base = lower
        .split('(')
        .next()
        .unwrap_or("")
        .trim()
        .trim_end_matches(" unsigned")
        .trim();
    matches!(
        base,
        "int"
            | "integer"
            | "int1"
            | "int2"
            | "int4"
            | "int8"
            | "tinyint"
            | "smallint"
            | "mediumint"
            | "bigint"
            | "hugeint"
            | "utinyint"
            | "usmallint"
            | "uinteger"
            | "ubigint"
            | "uhugeint"
            | "serial"
            | "smallserial"
            | "bigserial"
            | "numeric"
            | "decimal"
            | "number"
            | "dec"
            | "float"
            | "float4"
            | "float8"
            | "double"
            | "double precision"
            | "real"
            | "money"
            | "smallmoney"
    )
}

/// Whether `text` is a plain number to write as one: an optional sign, digits
/// with an optional decimal point, and an optional exponent. Not `inf` or
/// `nan` (they'd be written as bare words), not `1_000` or `0x10`.
pub(crate) fn is_plain_number(text: &str) -> bool {
    let bytes = text.as_bytes();
    let mut i = usize::from(matches!(bytes.first(), Some(b'+' | b'-')));
    let digits = |i: &mut usize| {
        let start = *i;
        while bytes.get(*i).is_some_and(u8::is_ascii_digit) {
            *i += 1;
        }
        *i - start
    };
    let whole = digits(&mut i);
    let fraction = if bytes.get(i) == Some(&b'.') {
        i += 1;
        digits(&mut i)
    } else {
        0
    };
    if whole + fraction == 0 {
        return false;
    }
    if matches!(bytes.get(i), Some(b'e' | b'E')) {
        i += 1;
        i += usize::from(matches!(bytes.get(i), Some(b'+' | b'-')));
        if digits(&mut i) == 0 {
            return false;
        }
    }
    i == bytes.len()
}

/// Why `text` isn't valid JSON, if it isn't.
pub(crate) fn json_error(text: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(text)
        .err()
        .map(|err| format!("Not valid JSON: {err}."))
}

/// JSON laid out for editing: indented, one key per line, and otherwise exactly
/// as stored (numbers too big for a float, `1.10`, duplicate keys), so saving
/// an edit to one field changes nothing else.
pub(crate) fn pretty(text: &str) -> String {
    if json_error(text).is_some() {
        return text.to_string();
    }
    crate::views::pretty(text).trim_end().to_string()
}

/// Whether `a` and `b` are the same JSON, written the same way apart from the
/// spacing between tokens (laid out for editing, say).
pub(crate) fn same_json(a: &str, b: &str) -> bool {
    json_error(a).is_none() && json_error(b).is_none() && pretty(a) == pretty(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_are_numbers_only_when_plainly_so() {
        for n in ["1", "-2", "+3.5", "0.25", ".5", "1e10", "6.02E-23", "02139"] {
            assert!(is_plain_number(n), "{n}");
        }
        for n in [
            "inf", "nan", "infinity", "1_000", "0x10", "1.2.3", "", "-", "1e", "e5", " 1",
        ] {
            assert!(!is_plain_number(n), "{n}");
        }
        for t in [
            "integer",
            "INT(11)",
            "bigint unsigned",
            "numeric(12,2)",
            "double precision",
            "int4",
        ] {
            assert!(is_numeric_type(t), "{t}");
        }
        for t in [
            "interval",
            "point",
            "zip_point",
            "text",
            "varchar(20)",
            "timestamp",
        ] {
            assert!(!is_numeric_type(t), "{t}");
        }
    }

    #[test]
    fn json_is_edited_exactly_as_stored() {
        let stored = r#"{"big": 123456789012345678901234567890, "price": 1.10, "a": 1, "a": 2}"#;
        let shown = pretty(stored);
        for kept in [
            "123456789012345678901234567890",
            "1.10",
            "\"a\": 1",
            "\"a\": 2",
        ] {
            assert!(shown.contains(kept), "{kept} in {shown}");
        }
        assert!(same_json(stored, &shown));
        // A changed big number is a change, not "the same value".
        let edited = shown.replace(
            "123456789012345678901234567890",
            "123456789012345678901234567891",
        );
        assert!(!same_json(stored, &edited));
    }

    #[test]
    fn editors_by_type() {
        let text = |t: &str| Some(Value::Text(t.into()));
        assert_eq!(kind_of(Some("jsonb"), &[], None), ValueKind::Json);
        assert_eq!(kind_of(Some("JSON"), &[], None), ValueKind::Json);
        assert_eq!(
            kind_of(Some("order_status"), &["paid".into(), "new".into()], None),
            ValueKind::Choice(vec!["paid".into(), "new".into()])
        );
        assert_eq!(
            kind_of(Some("boolean"), &[], text("true").as_ref()),
            ValueKind::Bool { numeric: false }
        );
        assert_eq!(
            kind_of(Some("tinyint(1)"), &[], None),
            ValueKind::Bool { numeric: true }
        );
        assert_eq!(
            kind_of(Some("bit"), &[], None),
            ValueKind::Bool { numeric: true }
        );
        assert_eq!(
            kind_of(Some("timestamp with time zone"), &[], None),
            ValueKind::Date(DateKind::Timestamp)
        );
        assert_eq!(
            kind_of(Some("datetime2(7)"), &[], None),
            ValueKind::Date(DateKind::Timestamp)
        );
        assert_eq!(
            kind_of(Some("date"), &[], None),
            ValueKind::Date(DateKind::Date)
        );
        assert_eq!(
            kind_of(Some("time without time zone"), &[], None),
            ValueKind::Date(DateKind::Time)
        );
        assert_eq!(
            kind_of(Some("text"), &[], text("short").as_ref()),
            ValueKind::Line
        );
        assert_eq!(
            kind_of(Some("text"), &[], text("two\nlines").as_ref()),
            ValueKind::Text
        );
        assert_eq!(
            kind_of(Some("varchar"), &[], text(&"x".repeat(200)).as_ref()),
            ValueKind::Text
        );
        assert_eq!(kind_of(Some("integer"), &[], None), ValueKind::Line);
    }

    #[test]
    fn json_is_checked_and_laid_out() {
        assert!(json_error(r#"{"a": [1, 2]}"#).is_none());
        assert!(
            json_error(r#"{"a": }"#)
                .unwrap()
                .starts_with("Not valid JSON")
        );
        assert_eq!(pretty(r#"{"a":1}"#), "{\n  \"a\": 1\n}");
        assert_eq!(pretty("not json"), "not json");
    }

    #[test]
    fn now_has_the_hinted_shape() {
        assert_eq!(DateKind::Date.now().len(), DateKind::Date.hint().len());
        assert_eq!(DateKind::Time.now().len(), DateKind::Time.hint().len());
        assert_eq!(
            DateKind::Timestamp.now().len(),
            DateKind::Timestamp.hint().len()
        );
    }
}
