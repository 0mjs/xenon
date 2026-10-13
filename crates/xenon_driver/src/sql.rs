//! Splitting a SQL script into statements, for "run statement under cursor".
//!
//! This is a lexer, not a parser: it only needs to find the semicolons that end
//! statements. It skips semicolons inside quotes ('…', "…", `…`), comments
//! (`--`, nested `/* */`) and Postgres dollar-quoted bodies (`$$…$$`, `$fn$…$fn$`).

use std::ops::Range;

/// Byte ranges of each statement in `sql`, trimmed of surrounding whitespace and
/// excluding the terminating `;`. Statements containing only comments or
/// whitespace are dropped.
pub fn split_statements(sql: &str) -> Vec<Range<usize>> {
    segments(sql)
        .into_iter()
        .filter_map(|segment| trim(sql, segment))
        .filter(|range| has_code(&sql[range.clone()]))
        .collect()
}

/// The statement to run for a cursor at byte `offset`: the one the cursor is in,
/// or, when the cursor sits after a statement's `;` with nothing but whitespace
/// or comments following on its way to the next one, the statement before it.
pub fn statement_at(sql: &str, offset: usize) -> Option<Range<usize>> {
    let statements = split_statements(sql);
    // The last statement starting at or before the cursor…
    let before = statements.iter().rposition(|r| r.start <= offset);
    match before {
        Some(ix) => {
            let current = &statements[ix];
            if offset <= current.end + 1 {
                return Some(current.clone());
            }
            // …unless the cursor is already on the next statement's first line.
            match statements.get(ix + 1) {
                Some(next) if same_line(sql, offset, next.start) => Some(next.clone()),
                _ => Some(current.clone()),
            }
        }
        None => statements.first().cloned(),
    }
}

/// Raw segments between top-level semicolons (the `;` itself excluded).
fn segments(sql: &str) -> Vec<Range<usize>> {
    let bytes = sql.as_bytes();
    let mut segments = Vec::new();
    let mut start = 0;
    let mut i = 0;

    while i < bytes.len() {
        match bytes[i] {
            b'\'' => i = skip_quoted(bytes, i, is_escape_string(sql, i)),
            b'"' | b'`' => i = skip_quoted(bytes, i, false),
            b'-' if bytes.get(i + 1) == Some(&b'-') => {
                i = bytes[i..]
                    .iter()
                    .position(|&b| b == b'\n')
                    .map_or(bytes.len(), |p| i + p + 1);
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => i = skip_block_comment(bytes, i),
            b'$' => i = skip_dollar_quoted(sql, i),
            b';' => {
                segments.push(start..i);
                i += 1;
                start = i;
            }
            _ => i += 1,
        }
    }
    segments.push(start..bytes.len());
    segments
}

/// Skip a quoted string or identifier starting at `i`. A doubled quote is an
/// escape; so is a backslash when `backslash_escapes` (Postgres `E'…'` strings).
fn skip_quoted(bytes: &[u8], i: usize, backslash_escapes: bool) -> usize {
    let quote = bytes[i];
    let mut j = i + 1;
    while j < bytes.len() {
        if bytes[j] == quote {
            if bytes.get(j + 1) == Some(&quote) {
                j += 2;
                continue;
            }
            return j + 1;
        }
        if backslash_escapes && bytes[j] == b'\\' {
            j += 2;
            continue;
        }
        j += 1;
    }
    bytes.len()
}

fn skip_block_comment(bytes: &[u8], i: usize) -> usize {
    let mut depth = 0;
    let mut j = i;
    while j + 1 < bytes.len() {
        match (bytes[j], bytes[j + 1]) {
            (b'/', b'*') => {
                depth += 1;
                j += 2;
            }
            (b'*', b'/') => {
                depth -= 1;
                j += 2;
                if depth == 0 {
                    return j;
                }
            }
            _ => j += 1,
        }
    }
    bytes.len()
}

/// Skip `$tag$ … $tag$` if one starts at `i`; otherwise step over the `$`
/// (a positional parameter like `$1`, or part of an identifier).
fn skip_dollar_quoted(sql: &str, i: usize) -> usize {
    let rest = &sql[i + 1..];
    let tag_len = rest
        .find(|c: char| !(c.is_alphanumeric() || c == '_'))
        .unwrap_or(rest.len());
    let tag = &rest[..tag_len];
    let opens = rest[tag_len..].starts_with('$')
        && !tag.starts_with(|c: char| c.is_ascii_digit())
        && !preceded_by_identifier(sql, i);
    if !opens {
        return i + 1;
    }
    let delimiter = format!("${tag}$");
    let body = i + delimiter.len();
    sql[body..]
        .find(&delimiter)
        .map_or(sql.len(), |p| body + p + delimiter.len())
}

/// True for the quote of a Postgres escape string: `E'…'` (but not `somE'…'`).
fn is_escape_string(sql: &str, quote: usize) -> bool {
    let before = &sql[..quote];
    before.ends_with(['E', 'e']) && !preceded_by_identifier(before, before.len() - 1)
}

fn preceded_by_identifier(sql: &str, i: usize) -> bool {
    sql[..i]
        .chars()
        .next_back()
        .is_some_and(|c| c.is_alphanumeric() || c == '_')
}

fn trim(sql: &str, range: Range<usize>) -> Option<Range<usize>> {
    let text = &sql[range.clone()];
    let start = range.start + (text.len() - text.trim_start().len());
    let end = range.start + text.trim_end().len();
    (start < end).then_some(start..end)
}

/// True if `text` contains something other than whitespace and comments.
fn has_code(text: &str) -> bool {
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b if b.is_ascii_whitespace() => i += 1,
            b'-' if bytes.get(i + 1) == Some(&b'-') => {
                i = bytes[i..]
                    .iter()
                    .position(|&b| b == b'\n')
                    .map_or(bytes.len(), |p| i + p + 1);
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => i = skip_block_comment(bytes, i),
            _ => return true,
        }
    }
    false
}

fn same_line(sql: &str, a: usize, b: usize) -> bool {
    let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
    !sql[lo..hi].contains('\n')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(sql: &str) -> Vec<&str> {
        split_statements(sql).into_iter().map(|r| &sql[r]).collect()
    }

    #[test]
    fn splits_on_top_level_semicolons() {
        assert_eq!(texts("select 1; select 2;\nselect 3"), ["select 1", "select 2", "select 3"]);
    }

    #[test]
    fn ignores_semicolons_in_strings_identifiers_and_comments() {
        let sql = "select ';', \"a;b\", 'it''s; fine' -- trailing; comment\nfrom t; /* a; /* nested; */ b; */ select 2";
        assert_eq!(
            texts(sql),
            [
                "select ';', \"a;b\", 'it''s; fine' -- trailing; comment\nfrom t",
                "/* a; /* nested; */ b; */ select 2"
            ]
        );
    }

    #[test]
    fn handles_dollar_quoting_and_parameters() {
        let sql = "create function f() returns int as $$ select 1; $$ language sql; \
                   do $body$ begin perform 1; end $body$; select $1, a$b from t";
        assert_eq!(
            texts(sql),
            [
                "create function f() returns int as $$ select 1; $$ language sql",
                "do $body$ begin perform 1; end $body$",
                "select $1, a$b from t"
            ]
        );
    }

    #[test]
    fn backslash_escapes_only_in_escape_strings() {
        assert_eq!(texts(r"select E'a\';b'; select 2"), [r"select E'a\';b'", "select 2"]);
        // In a standard string a backslash is literal.
        assert_eq!(texts(r"select 'C:\'; select 2"), [r"select 'C:\'", "select 2"]);
        assert_eq!(texts(r"select type'x\'; select 2"), [r"select type'x\'", "select 2"]);
    }

    #[test]
    fn drops_comment_only_and_empty_statements() {
        assert_eq!(texts(";; -- nothing\n; /* x */ ; select 1;"), ["select 1"]);
        assert!(texts("   ").is_empty());
    }

    #[test]
    fn unterminated_constructs_run_to_the_end() {
        assert_eq!(texts("select 'oops; select 2"), ["select 'oops; select 2"]);
    }

    #[test]
    fn statement_at_cursor() {
        let sql = "select 1;\nselect 2;\n\nselect 3;";
        let at = |offset| statement_at(sql, offset).map(|r| &sql[r]);
        assert_eq!(at(0), Some("select 1"));
        assert_eq!(at(4), Some("select 1"));
        // Just after the semicolon still means the statement before it.
        assert_eq!(at(9), Some("select 1"));
        assert_eq!(at(10), Some("select 2"));
        // On the blank line between statements: the one above.
        assert_eq!(at(20), Some("select 2"));
        assert_eq!(at(sql.len()), Some("select 3"));
    }

    #[test]
    fn statement_at_prefers_the_statement_on_the_cursor_line() {
        let sql = "select 1;   select 2";
        // Cursor in the gap but on the same line as the next statement.
        assert_eq!(statement_at(sql, 10).map(|r| &sql[r]), Some("select 2"));
    }

    #[test]
    fn statement_at_on_empty_input() {
        assert_eq!(statement_at("", 0), None);
        assert_eq!(statement_at("-- only a comment", 5), None);
    }

    #[test]
    fn multibyte_text_keeps_valid_ranges() {
        let sql = "select 'héllo; wörld'; select '日本'";
        assert_eq!(texts(sql), ["select 'héllo; wörld'", "select '日本'"]);
    }
}
