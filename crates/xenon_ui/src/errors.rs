//! Pointing at what a failed query got wrong: the spot in the editor (from the
//! position the server gave, or the unknown name it named), and, for a
//! misspelt table or column, the name it most likely meant.

use std::ops::Range;

use xenon_driver::Schema;

/// A table or column the server said doesn't exist.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Unknown {
    /// As written, without any `schema.` or `alias.` before it.
    pub name: String,
    pub table: bool,
}

/// The unknown name in an error message, as each engine words it.
pub(crate) fn unknown_name(message: &str) -> Option<Unknown> {
    // (what comes before the name, what comes after, whether it's a table)
    const PATTERNS: &[(&str, &str, bool)] = &[
        // Postgres
        ("column \"", "\" does not exist", false),
        ("relation \"", "\" does not exist", true),
        ("column ", " does not exist", false),
        // MySQL / MariaDB
        ("Unknown column '", "' in", false),
        ("Table '", "' doesn't exist", true),
        // SQLite
        ("no such column: ", "", false),
        ("no such table: ", "", true),
        // SQL Server
        ("Invalid column name '", "'", false),
        ("Invalid object name '", "'", true),
        // DuckDB
        ("Referenced column \"", "\" not found", false),
        ("Table with name ", " does not exist", true),
    ];
    for (before, after, table) in PATTERNS {
        let Some(start) = message.find(before).map(|i| i + before.len()) else {
            continue;
        };
        let rest = &message[start..];
        let end = if after.is_empty() {
            rest.find(|c: char| c.is_whitespace() || c == ',' || c == ')')
                .unwrap_or(rest.len())
        } else {
            match rest.find(after) {
                Some(end) => end,
                None => continue,
            }
        };
        let written = rest[..end].trim_matches(|c| c == '"' || c == '`' || c == '[' || c == ']');
        // `o.emial`, `main.orderz`, `shop.orderz`: the last part is the name.
        let name = written.rsplit('.').next().unwrap_or(written).trim();
        if !name.is_empty() && !name.contains(char::is_whitespace) {
            return Some(Unknown {
                name: name.to_string(),
                table: *table,
            });
        }
    }
    None
}

/// The known table or column closest to `unknown`, if one is close enough to
/// be a typo and no other is as close.
pub(crate) fn did_you_mean(unknown: &Unknown, schemas: &[Schema]) -> Option<String> {
    let names: Vec<&str> = schemas
        .iter()
        .flat_map(|s| s.objects.iter())
        .flat_map(|o| {
            let columns = o.columns.iter().map(|c| c.name.as_str());
            if unknown.table {
                Box::new(std::iter::once(o.name.as_str())) as Box<dyn Iterator<Item = &str>>
            } else {
                Box::new(columns)
            }
        })
        .collect();
    closest(&unknown.name, names)
}

fn closest<'a>(name: &str, candidates: impl IntoIterator<Item = &'a str>) -> Option<String> {
    let lower = name.to_lowercase();
    let allowed = match lower.chars().count() {
        0..=4 => 1,
        5..=8 => 2,
        _ => 3,
    };
    let mut best: Option<(usize, &str)> = None;
    let mut tied = false;
    let mut seen = std::collections::HashSet::new();
    for candidate in candidates {
        let folded = candidate.to_lowercase();
        if !seen.insert(folded.clone()) {
            continue;
        }
        // It exists as written (in another table, say): nothing to suggest.
        if folded == lower {
            return None;
        }
        let distance = edit_distance(&lower, &folded);
        if distance > allowed {
            continue;
        }
        match best {
            Some((d, _)) if distance > d => {}
            Some((d, _)) if distance == d => tied = true,
            _ => {
                best = Some((distance, candidate));
                tied = false;
            }
        }
    }
    best.filter(|_| !tied).map(|(_, name)| name.to_string())
}

/// Edits (insert, delete, substitute, swap two neighbours) to turn `a` into `b`.
fn edit_distance(a: &str, b: &str) -> usize {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let mut rows = vec![vec![0usize; b.len() + 1]; a.len() + 1];
    for (i, row) in rows.iter_mut().enumerate() {
        row[0] = i;
    }
    for j in 0..=b.len() {
        rows[0][j] = j;
    }
    for i in 1..=a.len() {
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            let mut d = (rows[i - 1][j] + 1)
                .min(rows[i][j - 1] + 1)
                .min(rows[i - 1][j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                d = d.min(rows[i - 2][j - 2] + 1);
            }
            rows[i][j] = d;
        }
    }
    rows[a.len()][b.len()]
}

/// Where in the editor's `text` the error is: the word at the server's
/// `position` (characters from 1, counted from `ran.start`), or else the one
/// place in `ran` the unknown name appears as a word.
pub(crate) fn locate(
    text: &str,
    ran: Range<usize>,
    position: Option<usize>,
    unknown: Option<&str>,
) -> Option<Range<usize>> {
    let ran_text = text.get(ran.clone())?;
    let word_end = |start: usize| {
        start
            + text[start..]
                .find(|c: char| !(c.is_alphanumeric() || c == '_'))
                .unwrap_or(text.len() - start)
    };
    if let Some(position) = position {
        let (offset, _) = ran_text.char_indices().nth(position.saturating_sub(1))?;
        let start = ran.start + offset;
        return Some(start..word_end(start).max(start));
    }
    let name = unknown?;
    let is_word = |b: Option<char>| b.is_some_and(|c| c.is_alphanumeric() || c == '_');
    let lower = ran_text.to_lowercase();
    let needle = name.to_lowercase();
    // Only when lowercasing keeps every offset: otherwise don't guess.
    if lower.len() != ran_text.len() {
        return None;
    }
    let mut found = lower.match_indices(&needle).filter(|(i, _)| {
        !is_word(ran_text[..*i].chars().next_back())
            && !is_word(ran_text[i + needle.len()..].chars().next())
    });
    let (first, _) = found.next()?;
    // More than once: which one is unclear.
    if found.next().is_some() {
        return None;
    }
    let start = ran.start + first;
    Some(start..start + needle.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use xenon_driver::{ObjectColumn, ObjectKind, SchemaObject};

    fn unknown(message: &str) -> Option<(String, bool)> {
        unknown_name(message).map(|u| (u.name, u.table))
    }

    #[test]
    fn every_engine_names_what_it_couldnt_find() {
        let column = |name: &str| Some((name.to_string(), false));
        let table = |name: &str| Some((name.to_string(), true));
        assert_eq!(unknown("column \"emial\" does not exist"), column("emial"));
        assert_eq!(unknown("column o.emial does not exist"), column("emial"));
        assert_eq!(
            unknown("relation \"orderz\" does not exist"),
            table("orderz")
        );
        assert_eq!(
            unknown("Unknown column 'o.emial' in 'field list'"),
            column("emial")
        );
        assert_eq!(
            unknown("Table 'shop.orderz' doesn't exist"),
            table("orderz")
        );
        assert_eq!(unknown("no such column: emial"), column("emial"));
        assert_eq!(unknown("no such table: main.orderz"), table("orderz"));
        assert_eq!(unknown("Invalid column name 'emial'."), column("emial"));
        assert_eq!(
            unknown("Invalid object name 'dbo.orderz'."),
            table("orderz")
        );
        assert_eq!(
            unknown("Binder Error: Referenced column \"emial\" not found in FROM clause!"),
            column("emial")
        );
        assert_eq!(
            unknown("Catalog Error: Table with name orderz does not exist!"),
            table("orderz")
        );
        assert_eq!(unknown("syntax error at or near \"selec\""), None);
    }

    #[test]
    fn suggestions_are_close_and_unambiguous() {
        let schemas = vec![Schema {
            name: "public".into(),
            objects: vec![
                SchemaObject {
                    name: "orders".into(),
                    kind: ObjectKind::Table,
                    columns: ["id", "email", "total", "status", "created_at"]
                        .into_iter()
                        .map(|name| ObjectColumn {
                            name: name.into(),
                            type_name: "text".into(),
                            primary_key: false,
                            not_null: false,
                        })
                        .collect(),
                },
                SchemaObject {
                    name: "customers".into(),
                    kind: ObjectKind::Table,
                    columns: Vec::new(),
                },
            ],
        }];
        let column = |name: &str| Unknown {
            name: name.into(),
            table: false,
        };
        let table = |name: &str| Unknown {
            name: name.into(),
            table: true,
        };
        assert_eq!(
            did_you_mean(&column("emial"), &schemas).as_deref(),
            Some("email")
        );
        assert_eq!(
            did_you_mean(&column("Totl"), &schemas).as_deref(),
            Some("total")
        );
        assert_eq!(
            did_you_mean(&column("createdat"), &schemas).as_deref(),
            Some("created_at")
        );
        assert_eq!(
            did_you_mean(&table("orderz"), &schemas).as_deref(),
            Some("orders")
        );
        assert_eq!(
            did_you_mean(&table("custmers"), &schemas).as_deref(),
            Some("customers")
        );
        // Nothing close: no guess.
        assert_eq!(did_you_mean(&column("revenue"), &schemas), None);
        // A column name isn't offered for a table.
        assert_eq!(did_you_mean(&table("emial"), &schemas), None);
    }

    #[test]
    fn errors_are_located_by_position_or_by_name() {
        let text = "-- first\nselect emial, total from orders;";
        let ran = 9..text.len();
        // Postgres: the 8th character of what ran.
        let at = locate(text, ran.clone(), Some(8), None).unwrap();
        assert_eq!(&text[at], "emial");
        // No position: the one place the name appears.
        let at = locate(text, ran.clone(), None, Some("emial")).unwrap();
        assert_eq!(&text[at], "emial");
        // Not inside a longer word, and not when it appears twice.
        assert_eq!(locate("select totals", 0..13, None, Some("total")), None);
        assert_eq!(locate("select a, a", 0..11, None, Some("a")), None);
    }
}
