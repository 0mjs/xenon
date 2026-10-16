//! Editor commands: Format SQL (⌥⇧F) and Toggle Comment (⌘/). Each works on
//! the selection, or for formatting the whole editor, and is one undo step.

use std::ops::Range;

use gpui::{Context, Window};
use sqlformat::{FormatOptions, Indent, QueryParams};
use xenon_driver::sql::same_code;

use crate::widgets::Engine;
use crate::workspace::Workspace;
use crate::{FormatSql, ToggleComment};

/// `sql` laid out one clause per line, or `None` when the formatter would
/// change more than whitespace (it doesn't know every dialect: Postgres `$$`
/// bodies, MySQL `#` comments), so nothing is changed.
pub(crate) fn format(sql: &str, mysql: bool) -> Option<String> {
    let options = FormatOptions {
        indent: Indent::Spaces(2),
        // Keep keywords as written.
        uppercase: None,
        lines_between_queries: 2,
        ignore_case_convert: None,
    };
    let mut formatted = sqlformat::format(sql, &QueryParams::None, &options);
    // Keep the whitespace the text ended with (a final newline, say).
    let tail = &sql[sql.trim_end().len()..];
    formatted.truncate(formatted.trim_end().len());
    formatted.push_str(tail);
    same_code(sql, &formatted, mysql).then_some(formatted)
}

/// Comment out the lines `selection` touches with `-- `, or uncomment them if
/// they all are. Returns the lines' range and their new text. A line that
/// starts inside a string or a block comment is left as it is: a `-- ` there
/// would change the text, not comment anything out. MySQL only reads `--` as a
/// comment when a space follows it.
pub(crate) fn toggle_comment(
    text: &str,
    selection: Range<usize>,
    mysql: bool,
) -> (Range<usize>, String) {
    let start = text[..selection.start].rfind('\n').map_or(0, |i| i + 1);
    // A selection ending at the start of a line doesn't take that line.
    let mut end = selection.end;
    if selection.end > selection.start && text[..end].ends_with('\n') {
        end -= 1;
    }
    let end = text[end..].find('\n').map_or(text.len(), |i| end + i);
    let lines: Vec<&str> = text[start..end].split('\n').collect();
    // Where each line starts, and whether that's in code.
    let mut at = start;
    let in_code: Vec<bool> = lines
        .iter()
        .map(|line| {
            let here = xenon_driver::sql::in_code(text, at, mysql);
            at += line.len() + 1;
            here
        })
        .collect();
    let is_comment = |l: &str| {
        let l = l.trim_start();
        if mysql {
            l.starts_with("-- ") || l == "--" || l.starts_with("--\t")
        } else {
            l.starts_with("--")
        }
    };
    let code = |(ix, line): &(usize, &&str)| in_code[*ix] && !line.trim().is_empty();
    let commented = lines
        .iter()
        .enumerate()
        .filter(code)
        .all(|(_, l)| is_comment(l));
    let new: Vec<String> = if commented {
        lines
            .iter()
            .enumerate()
            .map(|(ix, line)| {
                if !in_code[ix] {
                    return line.to_string();
                }
                let indent = line.len() - line.trim_start().len();
                let rest = &line[indent..];
                let rest = rest
                    .strip_prefix("-- ")
                    .or_else(|| rest.strip_prefix("--"))
                    .unwrap_or(rest);
                format!("{}{rest}", &line[..indent])
            })
            .collect()
    } else {
        // At the shallowest indent, so the block stays lined up.
        let indent = lines
            .iter()
            .enumerate()
            .filter(code)
            .map(|(_, l)| l.len() - l.trim_start().len())
            .min()
            .unwrap_or(0);
        lines
            .iter()
            .enumerate()
            .map(|(ix, line)| {
                if line.trim().is_empty() || !in_code[ix] {
                    line.to_string()
                } else {
                    format!("{}-- {}", &line[..indent], &line[indent..])
                }
            })
            .collect()
    };
    (start..end, new.join("\n"))
}

impl Workspace {
    pub(crate) fn format_sql(
        &mut self,
        _: &FormatSql,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mysql = self
            .active_session()
            .is_some_and(|s| matches!(s.engine, Engine::MySql | Engine::MariaDb));
        let editor = self.active_tab().editor.clone();
        let (text, selection) = {
            let editor = editor.read(cx);
            (editor.value().to_string(), editor.selected_range())
        };
        let range = if selection.is_empty() || text.get(selection.clone()).is_none() {
            0..text.len()
        } else {
            selection
        };
        let original = &text[range.clone()];
        if original.trim().is_empty() {
            return;
        }
        let Some(formatted) = format(original, mysql) else {
            self.flash(
                "Couldn't format this without changing more than its layout, so it's as it was.",
                true,
                cx,
            );
            return;
        };
        if formatted == original {
            return;
        }
        editor.update(cx, |editor, cx| {
            editor.set_selected_range(range, cx);
            editor.replace(formatted, window, cx);
        });
    }

    pub(crate) fn toggle_comment(
        &mut self,
        _: &ToggleComment,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let editor = self.active_tab().editor.clone();
        let (text, selection) = {
            let editor = editor.read(cx);
            (editor.value().to_string(), editor.selected_range())
        };
        if text.get(selection.clone()).is_none() {
            return;
        }
        let mysql = self
            .active_session()
            .is_some_and(|s| matches!(s.engine, Engine::MySql | Engine::MariaDb));
        let (lines, new) = toggle_comment(&text, selection, mysql);
        let start = lines.start;
        editor.update(cx, |editor, cx| {
            editor.set_selected_range(lines, cx);
            let len = new.len();
            editor.replace(new, window, cx);
            // Keep the lines selected, so pressing it again undoes it.
            editor.set_selected_range(start..start + len, cx);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toggle_comment_leaves_strings_alone() {
        // The second line is inside a string: a `-- ` there would change it.
        let text = "select 'a\nb' as x,\n  1";
        let (_, new) = toggle_comment(text, 0..text.len(), false);
        assert_eq!(new, "-- select 'a\nb' as x,\n--   1");
        // MySQL: `--1` isn't a comment, so the line counts as code.
        let (_, new) = toggle_comment("--1", 0..3, true);
        assert_eq!(new, "-- --1");
    }

    #[test]
    fn formatting_lays_out_and_keeps_everything_else() {
        let sql = "select a, b from orders o join customers c on c.id = o.customer_id where o.total > 100 and c.plan = 'team  plan' order by a;\n";
        let formatted = format(sql, false).expect("formats");
        assert!(formatted.contains("\nfrom\n"), "{formatted}");
        assert!(formatted.contains("'team  plan'"));
        assert!(formatted.ends_with(";\n"));
        assert!(same_code(sql, &formatted, false));
    }

    #[test]
    fn formatting_never_changes_what_runs() {
        // Whatever the formatter makes of these, Xenon either keeps it exactly
        // equivalent or leaves the SQL alone.
        for (sql, mysql) in [
            (
                "create function f() returns int as $$ select  1 $$ language sql",
                false,
            ),
            ("select a -- the a column\nfrom t", false),
            ("select a # the a column\nfrom t", true),
            ("select x::int, data->>'k', a <= b from t", false),
            ("select `odd  name` from t where s = 'it''s'", true),
            ("select \"Two  Words\" from t", false),
        ] {
            if let Some(formatted) = format(sql, mysql) {
                assert!(same_code(sql, &formatted, mysql), "{sql}\n=>\n{formatted}");
            }
        }
    }

    #[test]
    fn comments_toggle_on_whole_lines() {
        let text = "select a,\n  b\nfrom t;";
        // Cursor on the second line: just that line.
        let (range, new) = toggle_comment(text, 12..12, false);
        assert_eq!(&text[range.clone()], "  b");
        assert_eq!(new, "  -- b");
        // Several lines: lined up at the shallowest indent, and back again.
        let (range, new) = toggle_comment(text, 0..text.len(), false);
        assert_eq!(new, "-- select a,\n--   b\n-- from t;");
        let mut commented = text.to_string();
        commented.replace_range(range, &new);
        let (_, back) = toggle_comment(&commented, 0..commented.len(), false);
        assert_eq!(back, text);
        // A selection ending at the start of a line leaves that line be.
        let (range, _) = toggle_comment(text, 0..10, false);
        assert_eq!(&text[range], "select a,");
    }
}
