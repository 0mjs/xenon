//! Splitting a SQL script into statements, for "run statement under cursor".
//!
//! This is a lexer, not a parser: it only needs to find the semicolons that end
//! statements. It skips semicolons inside quotes ('…', "…", `…`), comments
//! (`--`, nested `/* */`) and Postgres dollar-quoted bodies (`$$…$$`, `$fn$…$fn$`).

use std::ops::Range;

/// How to read quotes and comments. Standard SQL (and Postgres, SQL Server,
/// SQLite, DuckDB) or MySQL's: `#` comments, backslash escapes in strings,
/// `--` only as a comment when a space follows, and `/*! … */` run as code.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Lexing {
    Standard,
    Mysql,
}

/// Byte ranges of each statement in `sql`, trimmed of surrounding whitespace and
/// excluding the terminating `;`. Statements containing only comments or
/// whitespace are dropped.
pub fn split_statements(sql: &str) -> Vec<Range<usize>> {
    split_as(sql, Lexing::Standard)
}

fn split_as(sql: &str, lexing: Lexing) -> Vec<Range<usize>> {
    segments(sql, lexing)
        .into_iter()
        .filter_map(|segment| trim(sql, segment))
        .filter(|range| has_code(&sql[range.clone()], lexing))
        .collect()
}

/// Where a comment starting at `i` ends, or `None` when there's no comment
/// there (as `lexing` reads it).
fn comment_end(bytes: &[u8], i: usize, lexing: Lexing) -> Option<usize> {
    let line_end = |from: usize| {
        bytes[from..]
            .iter()
            .position(|&b| b == b'\n')
            .map_or(bytes.len(), |p| from + p + 1)
    };
    match (bytes[i], bytes.get(i + 1), lexing) {
        // MySQL needs whitespace after `--`; `1--1` is arithmetic there.
        (b'-', Some(b'-'), Lexing::Standard) => Some(line_end(i)),
        (b'-', Some(b'-'), Lexing::Mysql)
            if bytes.get(i + 2).is_none_or(|b| b.is_ascii_whitespace()) =>
        {
            Some(line_end(i))
        }
        (b'#', _, Lexing::Mysql) => Some(line_end(i)),
        // MySQL runs what's inside `/*! … */`: it's code, not a comment.
        (b'/', Some(b'*'), Lexing::Mysql)
            if bytes.get(i + 2) == Some(&b'!') || bytes[i..].starts_with(b"/*M!") =>
        {
            None
        }
        (b'/', Some(b'*'), _) => Some(skip_block_comment(bytes, i)),
        _ => None,
    }
}

/// Step past the opening of a MySQL executable comment (`/*!` and its version
/// number) at `i`, so what follows is read as code.
fn executable_comment_start(bytes: &[u8], i: usize, lexing: Lexing) -> Option<usize> {
    // MariaDB also runs `/*M! … */`.
    let opening = if bytes[i..].starts_with(b"/*!") {
        3
    } else if bytes[i..].starts_with(b"/*M!") {
        4
    } else {
        return None;
    };
    if lexing != Lexing::Mysql {
        return None;
    }
    let mut j = i + opening;
    while j < bytes.len() && bytes[j].is_ascii_digit() {
        j += 1;
    }
    Some(j)
}

/// Whether a quote at `i` takes backslash escapes, as `lexing` reads it.
fn backslash_escapes(sql: &str, i: usize, lexing: Lexing) -> bool {
    let quote = sql.as_bytes()[i];
    match lexing {
        Lexing::Mysql => quote == b'\'' || quote == b'"',
        Lexing::Standard => quote == b'\'' && is_escape_string(sql, i),
    }
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
fn segments(sql: &str, lexing: Lexing) -> Vec<Range<usize>> {
    let bytes = sql.as_bytes();
    let mut segments = Vec::new();
    let mut start = 0;
    let mut i = 0;

    while i < bytes.len() {
        if let Some(code) = executable_comment_start(bytes, i, lexing) {
            i = code;
            continue;
        }
        if let Some(end) = comment_end(bytes, i, lexing) {
            i = end;
            continue;
        }
        match bytes[i] {
            b'\'' | b'"' | b'`' => i = skip_quoted(bytes, i, backslash_escapes(sql, i, lexing)),
            b'$' if lexing == Lexing::Standard => i = skip_dollar_quoted(sql, i),
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
fn has_code(text: &str, lexing: Lexing) -> bool {
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if executable_comment_start(bytes, i, lexing).is_some() {
            return true;
        }
        if let Some(end) = comment_end(bytes, i, lexing) {
            i = end;
            continue;
        }
        if bytes[i].is_ascii_whitespace() {
            i += 1;
            continue;
        }
        return true;
    }
    false
}

/// How much a script could change, worst statement first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Risk {
    /// Inserts, updates and deletes with a `where`, `create`, grants…
    Write,
    /// `update` or `delete` with no `where`: every row.
    EveryRow,
    /// `drop`, `truncate`, `alter`: removes or reshapes data.
    Destructive,
}

/// The riskiest statement in `sql`, with a short description such as
/// `"DELETE on orders with no WHERE"`. `None` for read-only scripts.
///
/// Deny by default: only statements known to read (`select`, `show`,
/// `explain`, transaction control…) pass, and only when no data-changing verb
/// appears in them. The script is read both as standard SQL and as MySQL reads
/// it (where `#`, backslashes and `/*! … */` mean something else), and the
/// worse reading counts, so quoting tricks can't hide a write.
pub fn assess(sql: &str) -> Option<(Risk, String)> {
    [Lexing::Standard, Lexing::Mysql]
        .into_iter()
        .flat_map(|lexing| {
            split_as(sql, lexing)
                .into_iter()
                .filter_map(move |range| assess_statement(&sql[range], lexing))
        })
        .max_by_key(|(risk, _)| *risk)
}

/// Statements that only read, or only manage the session or transaction.
const READS: &[&str] = &[
    "abort",
    "begin",
    "close",
    "commit",
    "deallocate",
    "declare",
    "desc",
    "describe",
    "discard",
    "end",
    "explain",
    "fetch",
    "listen",
    "pragma",
    "print",
    "release",
    "reset",
    "rollback",
    "savepoint",
    "select",
    "set",
    "show",
    "start",
    "table",
    "unlisten",
    "use",
    "values",
    "waitfor",
    "with",
];

/// Verbs that change data or schema, wherever they appear in a statement, and
/// functions that change something even when called from a `select`.
const WRITE_VERBS: &[&str] = &[
    "insert",
    "update",
    "delete",
    "merge",
    "upsert",
    "replace",
    "drop",
    "truncate",
    "alter",
    "create",
    "rename",
    "grant",
    "revoke",
    "exec",
    "execute",
    "call",
    "copy",
    // Postgres: sessions, settings, sequences, large objects, the server itself.
    "pg_terminate_backend",
    "pg_cancel_backend",
    "pg_reload_conf",
    "pg_rotate_logfile",
    "pg_switch_wal",
    "pg_create_restore_point",
    "pg_promote",
    "set_config",
    "setval",
    "nextval",
    "dblink_exec",
    "lo_import",
    "lo_export",
    "lo_unlink",
    "pg_file_write",
    // SQL Server: the shell.
    "xp_cmdshell",
];

fn assess_statement(statement: &str, lexing: Lexing) -> Option<(Risk, String)> {
    let words = words_outside_literals(statement, lexing);
    let first = words.first()?.as_str();
    let has = |word: &str| words.iter().any(|w| w == word);
    // A write verb somewhere in the statement; `for update` / `for no key update`
    // and `on duplicate key update` lock or belong to an insert, not a write.
    let hidden = || {
        words.iter().enumerate().find_map(|(ix, word)| {
            let lock =
                word == "update" && ix > 0 && matches!(words[ix - 1].as_str(), "for" | "key");
            (WRITE_VERBS.contains(&word.as_str()) && !lock).then_some(word.as_str())
        })
    };
    let verb = if READS.contains(&first) {
        match first {
            // Plain `explain` only plans; `explain analyze` runs the statement.
            "explain" if !(has("analyze") || has("analyse")) => return None,
            "select" if has("into") => return Some((Risk::Write, "SELECT INTO".into())),
            // Changing who you are or the server's settings is a write.
            "set"
                if [
                    "role",
                    "authorization",
                    "global",
                    "persist",
                    "persist_only",
                    "password",
                ]
                .iter()
                .any(|w| has(w)) =>
            {
                return Some((Risk::Write, "SET".into()));
            }
            // `pragma name = value` writes; `pragma name` / `pragma name(arg)` reads.
            "pragma" if statement.contains('=') => return Some((Risk::Write, "PRAGMA".into())),
            _ => hidden()?,
        }
    } else {
        // An unknown statement is a write; one hiding a worse verb (a SQL Server
        // batch without semicolons, `if … drop table`) is as bad as that verb.
        match hidden() {
            Some(worse @ ("drop" | "truncate" | "alter" | "delete" | "update")) => worse,
            _ => first,
        }
    };
    let target = |after: &str| {
        let ix = words.iter().position(|w| w == after)?;
        words.get(ix + 1).cloned()
    };
    let summary = match (verb, words.get(1)) {
        ("drop" | "truncate" | "alter" | "create", Some(object)) if verb == first => {
            format!("{} {}", verb.to_uppercase(), object.to_uppercase())
        }
        _ => verb.to_uppercase(),
    };
    match verb {
        "drop" | "truncate" | "alter" => Some((Risk::Destructive, summary)),
        "delete" | "update" if !has("where") => {
            let table = if verb == "delete" {
                target("from")
            } else {
                words
                    .iter()
                    .position(|w| w == "update")
                    .and_then(|ix| words.get(ix + 1).cloned())
            };
            let on = table.map(|t| format!(" on {t}")).unwrap_or_default();
            Some((
                Risk::EveryRow,
                format!("{}{on} with no WHERE", verb.to_uppercase()),
            ))
        }
        // Anything else that isn't a known read: a write until shown otherwise.
        _ => Some((Risk::Write, summary)),
    }
}

/// True when `sql` changes anything, as far as [`assess`] can tell.
pub fn writes(sql: &str) -> bool {
    assess(sql).is_some()
}

/// True when `sql` manages a transaction itself (`begin`, `commit`,
/// `savepoint`…), so it shouldn't be wrapped in another.
pub fn controls_transaction(sql: &str) -> bool {
    statements_both_ways(sql).any(|words| match words.first().map(String::as_str) {
        Some(
            "begin" | "start" | "commit" | "rollback" | "end" | "savepoint" | "release" | "abort"
            | "prepare",
        ) => true,
        Some("set") => words.get(1).is_some_and(|w| w == "transaction"),
        _ => false,
    })
}

/// Whether running `sql` leaves a transaction open (`Some(true)` after
/// `begin`, `Some(false)` after `commit` / `rollback`), by its last such
/// statement; `None` when it neither begins nor ends one. For engines that can't
/// say themselves.
pub fn transaction_effect(sql: &str) -> Option<bool> {
    split_statements(sql).into_iter().rev().find_map(|range| {
        let words = words_outside_literals(&sql[range], Lexing::Standard);
        let second = words.get(1).map(String::as_str);
        match words.first().map(String::as_str)? {
            // Not T-SQL's `begin … end` block, which is no transaction.
            "begin"
                if matches!(
                    second,
                    None | Some(
                        "tran"
                            | "transaction"
                            | "work"
                            | "read"
                            | "isolation"
                            | "deferred"
                            | "immediate"
                            | "exclusive"
                    )
                ) =>
            {
                Some(true)
            }
            "start" if second == Some("transaction") => Some(true),
            "commit" | "rollback" | "abort" => Some(false),
            // Postgres's `end` commits; T-SQL's ends a block.
            "end" if matches!(second, None | Some("transaction" | "work")) => Some(false),
            _ => None,
        }
    })
}

/// True when `sql` tries to turn a read-only session back into a read-write
/// one (`set transaction read write`, `begin read write`,
/// `set default_transaction_read_only = off`…).
pub fn lifts_read_only(sql: &str) -> bool {
    // Named in a string too: Postgres's `set_config('default_transaction_read_only', 'off', …)`.
    let lower = sql.to_ascii_lowercase();
    if ["transaction_read_only", "tx_read_only"]
        .iter()
        .any(|name| lower.contains(name) && !lower.contains(&format!("show {name}")))
        && (lower.contains("set") || lower.contains("reset"))
    {
        return true;
    }
    statements_both_ways(sql).any(|words| {
        words
            .windows(2)
            .any(|pair| pair[0] == "read" && pair[1] == "write")
            // Also as `@@session.transaction_read_only`.
            || words.iter().any(|word| {
                word.rsplit('.').next().is_some_and(|name| {
                    matches!(
                        name,
                        "transaction_read_only" | "default_transaction_read_only" | "tx_read_only"
                    )
                })
            })
    })
}

/// Each statement's words, read as standard SQL and again as MySQL reads it.
fn statements_both_ways(sql: &str) -> impl Iterator<Item = Vec<String>> + '_ {
    [Lexing::Standard, Lexing::Mysql]
        .into_iter()
        .flat_map(move |lexing| {
            split_as(sql, lexing)
                .into_iter()
                .map(move |range| words_outside_literals(&sql[range], lexing))
        })
}

/// Lowercased words of a statement, skipping comments, string literals and
/// quoted identifiers (so `where` inside a string doesn't count).
fn words_outside_literals(statement: &str, lexing: Lexing) -> Vec<String> {
    let bytes = statement.as_bytes();
    let mut words = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if let Some(code) = executable_comment_start(bytes, i, lexing) {
            i = code;
            continue;
        }
        if let Some(end) = comment_end(bytes, i, lexing) {
            i = end;
            continue;
        }
        match bytes[i] {
            b'\'' => i = skip_quoted(bytes, i, backslash_escapes(statement, i, lexing)),
            b'"' | b'`' => {
                // Keep a quoted identifier as one word, without quotes.
                let end = skip_quoted(bytes, i, backslash_escapes(statement, i, lexing));
                words.push(statement[i + 1..end.saturating_sub(1).max(i + 1)].to_lowercase());
                i = end;
            }
            b'$' if lexing == Lexing::Standard => i = skip_dollar_quoted(statement, i),
            b if b.is_ascii_alphanumeric() || b == b'_' || b >= 0x80 => {
                let start = i;
                while i < bytes.len()
                    && (bytes[i].is_ascii_alphanumeric()
                        || bytes[i] == b'_'
                        || bytes[i] == b'.'
                        || bytes[i] >= 0x80)
                {
                    i += 1;
                }
                words.push(statement[start..i].to_lowercase());
            }
            _ => i += 1,
        }
    }
    words
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SortDirection {
    Ascending,
    Descending,
}

impl SortDirection {
    /// Header clicks cycle ascending → descending → unsorted.
    pub fn cycle(current: Option<SortDirection>) -> Option<SortDirection> {
        match current {
            None => Some(SortDirection::Ascending),
            Some(SortDirection::Ascending) => Some(SortDirection::Descending),
            Some(SortDirection::Descending) => None,
        }
    }
}

/// Words that can't be a bare identifier in at least one engine Xenon supports.
/// Sorted, for binary search.
const RESERVED: &[&str] = &[
    "all",
    "analyse",
    "analyze",
    "and",
    "any",
    "array",
    "as",
    "asc",
    "asymmetric",
    "authorization",
    "between",
    "binary",
    "both",
    "by",
    "call",
    "case",
    "cast",
    "change",
    "check",
    "collate",
    "column",
    "columns",
    "constraint",
    "create",
    "cross",
    "current_date",
    "current_role",
    "current_time",
    "current_timestamp",
    "current_user",
    "database",
    "databases",
    "default",
    "deferrable",
    "delete",
    "desc",
    "describe",
    "distinct",
    "div",
    "do",
    "drop",
    "else",
    "end",
    "except",
    "exists",
    "explain",
    "false",
    "fetch",
    "for",
    "foreign",
    "freeze",
    "from",
    "full",
    "grant",
    "group",
    "having",
    "ilike",
    "in",
    "index",
    "initially",
    "inner",
    "insert",
    "intersect",
    "interval",
    "into",
    "is",
    "isnull",
    "join",
    "key",
    "keys",
    "kill",
    "lateral",
    "leading",
    "left",
    "like",
    "limit",
    "localtime",
    "localtimestamp",
    "lock",
    "match",
    "mod",
    "natural",
    "not",
    "notnull",
    "null",
    "offset",
    "on",
    "only",
    "or",
    "order",
    "outer",
    "over",
    "overlaps",
    "partition",
    "placing",
    "primary",
    "range",
    "read",
    "references",
    "regexp",
    "rename",
    "replace",
    "returning",
    "right",
    "rlike",
    "row",
    "rows",
    "schema",
    "select",
    "session_user",
    "set",
    "show",
    "similar",
    "some",
    "symmetric",
    "table",
    "tablesample",
    "then",
    "to",
    "trailing",
    "trigger",
    "true",
    "union",
    "unique",
    "update",
    "usage",
    "user",
    "using",
    "values",
    "variadic",
    "verbose",
    "when",
    "where",
    "window",
    "with",
    "write",
    "xor",
];

/// True when `ident` can be written without quotes and still mean itself in
/// every engine: lowercase letters, digits and `_`, not starting with a digit,
/// and not a reserved word.
pub fn is_plain_ident(ident: &str) -> bool {
    let mut chars = ident.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c == '_')
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        && RESERVED.binary_search(&ident).is_err()
}

/// The query behind "browse a table": `select *` with an optional raw `where`
/// condition and sort column, capped at `limit` rows. `schema` is left out when
/// it's `None` (the connection's default).
pub fn browse_table(
    quote: impl Fn(&str) -> String,
    schema: Option<&str>,
    table: &str,
    filter: &str,
    order: Option<(&str, SortDirection)>,
    limit: usize,
    top: bool,
) -> String {
    let name = match schema {
        Some(schema) => format!("{}.{}", quote(schema), quote(table)),
        None => quote(table),
    };
    let mut sql = if top {
        format!("select top ({limit}) * from {name}")
    } else {
        format!("select * from {name}")
    };
    let filter = filter.trim().trim_end_matches(';').trim();
    if !filter.is_empty() {
        sql.push_str(&format!("\nwhere {filter}"));
    }
    if let Some((column, direction)) = order {
        let direction = match direction {
            SortDirection::Ascending => "asc",
            SortDirection::Descending => "desc",
        };
        sql.push_str(&format!("\norder by {} {direction}", quote(column)));
    }
    if top {
        sql.push(';');
    } else {
        sql.push_str(&format!("\nlimit {limit};"));
    }
    sql
}

fn same_line(sql: &str, a: usize, b: usize) -> bool {
    let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
    !sql[lo..hi].contains('\n')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mysql_quoting_tricks_cant_hide_a_write() {
        for sql in [
            // MySQL runs the inside of `/*! … */`.
            "/*!50000 DROP TABLE orders */",
            "select 1 /*! ; delete from orders */",
            // MySQL's backslash escape keeps the string open in standard SQL's reading,
            // and closes it in MySQL's: the drop is code there.
            r"select '\''; drop table orders; -- '",
            r#"select "\""; drop table orders; -- ""#,
            // `--1` isn't a comment to MySQL.
            "select 1--1; drop table orders",
        ] {
            assert!(
                assess(sql).is_some_and(|(risk, _)| risk >= Risk::EveryRow),
                "{sql}: {:?}",
                assess(sql)
            );
        }
    }

    #[test]
    fn sql_server_batches_without_semicolons_count_every_statement() {
        for (sql, risk) in [
            (
                "select * from orders\ndelete from orders where id = 1",
                Risk::Write,
            ),
            (
                "declare @x int = 1\ndelete from orders where id = @x",
                Risk::Write,
            ),
            (
                "if exists (select 1 from orders) drop table orders",
                Risk::Destructive,
            ),
            (
                "set nocount on\nupdate orders set status = 'x'",
                Risk::EveryRow,
            ),
            ("/*M!100100 drop table orders */", Risk::Destructive),
        ] {
            assert_eq!(assess(sql).map(|r| r.0), Some(risk), "{sql}");
        }
    }

    #[test]
    fn anything_not_known_to_read_counts_as_a_write() {
        for sql in [
            "lock table orders in exclusive mode",
            "attach database 'other.db' as other",
            "load data infile '/tmp/x' into table orders",
            "vacuum",
            "analyze orders",
            "install plugin x soname 'x.so'",
            "kill 42",
            "set role admin",
            "set global max_connections = 1",
            "pragma foreign_keys = off",
            "begin insert into orders values (1) end",
            "select pg_terminate_backend(42)",
            "select nextval('orders_id_seq')",
            "notify channel",
        ] {
            assert_eq!(assess(sql).map(|r| r.0), Some(Risk::Write), "{sql}");
        }
        for sql in [
            "select * from orders for update",
            "select * from orders for no key update",
            "set search_path = shop",
            "begin",
            "use shop",
            "pragma foreign_keys",
            "pragma table_info(orders)",
            "declare @n int",
            "explain delete from orders",
            "values (1), (2)",
        ] {
            assert_eq!(assess(sql), None, "{sql}");
        }
    }

    #[test]
    fn procedures_blocks_and_select_into_are_writes() {
        for sql in [
            "exec dbo.purge_old",
            "EXECUTE sp_rename 'a', 'b'",
            "call refresh_totals()",
            "do $$ begin delete from t; end $$",
            "select * into backup_orders from orders",
        ] {
            assert!(writes(sql), "{sql}");
        }
        for sql in [
            "select * from orders",
            "select 'into' as word",
            "explain select 1",
        ] {
            assert!(!writes(sql), "{sql}");
        }
    }

    #[test]
    fn spots_scripts_that_manage_their_own_transaction() {
        assert!(controls_transaction("begin; update t set a = 1; commit"));
        assert!(controls_transaction("update t set a = 1;\nSAVEPOINT s1"));
        assert!(controls_transaction(
            "BEGIN TRAN; delete from t where id = 1"
        ));
        assert!(!controls_transaction(
            "update t set a = 1; delete from u where id = 2"
        ));
        assert!(!controls_transaction("select 'begin'; select 1"));
    }

    #[test]
    fn transaction_effect_follows_the_last_begin_or_commit() {
        assert_eq!(transaction_effect("begin; update t set a = 1"), Some(true));
        assert_eq!(
            transaction_effect("start transaction read only"),
            Some(true)
        );
        assert_eq!(
            transaction_effect("BEGIN TRAN; delete from t where id = 1"),
            Some(true)
        );
        assert_eq!(
            transaction_effect("begin; update t set a = 1; commit"),
            Some(false)
        );
        assert_eq!(transaction_effect("rollback"), Some(false));
        assert_eq!(transaction_effect("select 1"), None);
        // A T-SQL block isn't a transaction.
        assert_eq!(
            transaction_effect("begin insert into t values (1) end"),
            None
        );
    }

    #[test]
    fn spots_attempts_to_lift_read_only() {
        for sql in [
            "set transaction read write",
            "BEGIN READ WRITE",
            "start transaction read write",
            "set session characteristics as transaction read write",
            "set default_transaction_read_only = off",
            "select 1; SET SESSION transaction_read_only = 0",
            "set @@session.transaction_read_only = 0",
            "select set_config('default_transaction_read_only', 'off', false)",
            "set transaction read /* sneaky */ write",
        ] {
            assert!(lifts_read_only(sql), "{sql}");
        }
        for sql in [
            "select 'read write'",
            "select * from read_write_log",
            "begin",
        ] {
            assert!(!lifts_read_only(sql), "{sql}");
        }
    }

    #[test]
    fn reserved_words_are_sorted() {
        assert!(RESERVED.windows(2).all(|w| w[0] < w[1]));
    }

    #[test]
    fn plain_identifiers() {
        for plain in ["orders", "order_items", "_tmp", "v2"] {
            assert!(is_plain_ident(plain), "{plain}");
        }
        for quoted in [
            "Orders",
            "order",
            "user",
            "two words",
            "2fa",
            "naïve",
            "",
            "a-b",
        ] {
            assert!(!is_plain_ident(quoted), "{quoted}");
        }
    }

    #[test]
    fn sql_server_browses_with_top() {
        assert_eq!(
            browse_table(
                |i: &str| format!("[{i}]"),
                None,
                "orders",
                "id > 5",
                None,
                1000,
                true
            ),
            "select top (1000) * from [orders]\nwhere id > 5;"
        );
    }

    #[test]
    fn browse_queries_read_like_hand_written_sql() {
        let quote = |i: &str| {
            if is_plain_ident(i) {
                i.to_string()
            } else {
                format!("\"{i}\"")
            }
        };
        assert_eq!(
            browse_table(quote, None, "orders", "", None, 1000, false),
            "select * from orders\nlimit 1000;"
        );
        assert_eq!(
            browse_table(
                quote,
                Some("Analytics"),
                "order",
                "total > 5",
                Some(("created_at", SortDirection::Descending)),
                50,
                false
            ),
            "select * from \"Analytics\".\"order\"\nwhere total > 5\norder by created_at desc\nlimit 50;"
        );
    }

    fn texts(sql: &str) -> Vec<&str> {
        split_statements(sql).into_iter().map(|r| &sql[r]).collect()
    }

    #[test]
    fn splits_on_top_level_semicolons() {
        assert_eq!(
            texts("select 1; select 2;\nselect 3"),
            ["select 1", "select 2", "select 3"]
        );
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
        assert_eq!(
            texts(r"select E'a\';b'; select 2"),
            [r"select E'a\';b'", "select 2"]
        );
        // In a standard string a backslash is literal.
        assert_eq!(
            texts(r"select 'C:\'; select 2"),
            [r"select 'C:\'", "select 2"]
        );
        assert_eq!(
            texts(r"select type'x\'; select 2"),
            [r"select type'x\'", "select 2"]
        );
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
    fn browse_table_builds_filter_and_order() {
        let quote = |s: &str| format!("\"{s}\"");
        assert_eq!(
            browse_table(quote, Some("public"), "orders", "", None, 1000, false),
            "select * from \"public\".\"orders\"\nlimit 1000;"
        );
        assert_eq!(
            browse_table(
                quote,
                Some("public"),
                "orders",
                "  status = 'paid';  ",
                Some(("total", SortDirection::Descending)),
                50,
                false
            ),
            "select * from \"public\".\"orders\"\nwhere status = 'paid'\norder by \"total\" desc\nlimit 50;"
        );
    }

    #[test]
    fn assess_reads_as_safe() {
        for sql in [
            "select * from orders where note = 'drop table x; delete from y'",
            "with t as (select 1) select * from t",
            "explain analyze select 1",
            "-- delete from orders\nselect 1",
            "show server_version; pragma table_info(t)",
            "",
        ] {
            assert_eq!(assess(sql), None, "{sql}");
        }
    }

    #[test]
    fn assess_flags_writes_and_destruction() {
        assert_eq!(
            assess("delete from orders"),
            Some((Risk::EveryRow, "DELETE on orders with no WHERE".into()))
        );
        assert_eq!(
            assess("UPDATE \"Orders\" SET status = 'x'"),
            Some((Risk::EveryRow, "UPDATE on orders with no WHERE".into()))
        );
        assert_eq!(
            assess("update orders set a = 1 where id = 3").map(|r| r.0),
            Some(Risk::Write)
        );
        assert_eq!(
            assess("insert into t values (1)").map(|r| r.0),
            Some(Risk::Write)
        );
        assert_eq!(
            assess("select 1; drop table public.orders; insert into t values (1)"),
            Some((Risk::Destructive, "DROP TABLE".into()))
        );
        assert_eq!(
            assess("truncate orders").map(|r| r.0),
            Some(Risk::Destructive)
        );
        assert_eq!(
            assess("with gone as (delete from t where x) select * from gone").map(|r| r.0),
            Some(Risk::Write)
        );
        // `where` inside a string doesn't make a delete safe.
        assert_eq!(
            assess("delete from t -- where id = 1").map(|r| r.0),
            Some(Risk::EveryRow)
        );
        assert_eq!(assess("explain select 1"), None);
        assert_eq!(assess("explain delete from t where id = 1"), None);
        assert_eq!(
            assess("explain analyze delete from t").map(|r| r.0),
            Some(Risk::EveryRow)
        );
    }

    #[test]
    fn sort_cycles() {
        use SortDirection::*;
        assert_eq!(SortDirection::cycle(None), Some(Ascending));
        assert_eq!(SortDirection::cycle(Some(Ascending)), Some(Descending));
        assert_eq!(SortDirection::cycle(Some(Descending)), None);
    }

    #[test]
    fn multibyte_text_keeps_valid_ranges() {
        let sql = "select 'héllo; wörld'; select '日本'";
        assert_eq!(texts(sql), ["select 'héllo; wörld'", "select '日本'"]);
    }
}
