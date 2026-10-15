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

/// Why `text` isn't valid JSON, if it isn't.
pub(crate) fn json_error(text: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(text)
        .err()
        .map(|err| format!("Not valid JSON: {err}."))
}

/// JSON laid out for editing: indented, one key per line.
pub(crate) fn pretty(text: &str) -> String {
    serde_json::from_str::<serde_json::Value>(text)
        .ok()
        .and_then(|value| serde_json::to_string_pretty(&value).ok())
        .unwrap_or_else(|| text.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

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
