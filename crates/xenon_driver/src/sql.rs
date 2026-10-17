//! Splitting a SQL script into statements, for "run statement under cursor".
//!
//! This is a lexer, not a parser: it only needs to find the semicolons that end
//! statements. It skips semicolons inside quotes ('…', "…", `…`), comments
//! (`--`, nested `/* */`) and Postgres dollar-quoted bodies (`$$…$$`, `$fn$…$fn$`).

use std::ops::Range;

/// How to read quotes and comments. Engines differ, and a safety check has to
/// read a script the way the server will: where a comment ends decides what's
/// code. See [`READINGS`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Lexing {
    /// MySQL's rules: `#` comments, `--` a comment only when whitespace
    /// follows, and `/*! … */` run as code.
    mysql: bool,
    /// A lone `\r` ends a `--` comment too (Postgres, DuckDB).
    cr_ends_line: bool,
    /// `/* /* */ */` nests (Postgres, DuckDB, SQL Server; not MySQL or SQLite).
    nested_comments: bool,
    /// Where a backslash escapes the next character inside `'…'`.
    backslash: Backslash,
    /// `"…"` is a string taking backslash escapes like `'…'` (MySQL without
    /// `ANSI_QUOTES`); otherwise it quotes a name.
    double_quote_string: bool,
    /// `[…]` quotes a name (SQL Server, SQLite).
    brackets: bool,
    /// `$tag$ … $tag$` bodies (Postgres, DuckDB).
    dollar_quotes: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Backslash {
    Never,
    /// Only in Postgres `E'…'` strings.
    EscapeStrings,
    /// In every string (MySQL by default; Postgres with
    /// `standard_conforming_strings` off).
    Always,
}

/// Standard SQL as Xenon splits and formats it.
const STANDARD: Lexing = Lexing {
    mysql: false,
    cr_ends_line: false,
    nested_comments: true,
    backslash: Backslash::EscapeStrings,
    double_quote_string: false,
    brackets: false,
    dollar_quotes: true,
};

/// MySQL and MariaDB with their default `sql_mode`.
const MYSQL: Lexing = Lexing {
    mysql: true,
    cr_ends_line: false,
    nested_comments: false,
    backslash: Backslash::Always,
    double_quote_string: true,
    brackets: false,
    dollar_quotes: false,
};

const POSTGRES: Lexing = Lexing {
    cr_ends_line: true,
    ..STANDARD
};

const SQL_SERVER: Lexing = Lexing {
    cr_ends_line: true,
    backslash: Backslash::Never,
    brackets: true,
    dollar_quotes: false,
    ..STANDARD
};

const SQLITE: Lexing = Lexing {
    nested_comments: false,
    backslash: Backslash::Never,
    brackets: true,
    dollar_quotes: false,
    ..STANDARD
};

/// Every way an engine Xenon connects to might read a script: Postgres and
/// DuckDB (also with `standard_conforming_strings` off), SQL Server (whether or
/// not a lone `\r` ends a line), SQLite, and MySQL with and without
/// `NO_BACKSLASH_ESCAPES` and `ANSI_QUOTES`. The safety checks read a script
/// every one of these ways and take the worst, so a write can't hide in a
/// comment or string that only Xenon thinks is one.
const READINGS: [Lexing; 10] = [
    STANDARD,
    POSTGRES,
    Lexing {
        backslash: Backslash::Always,
        ..POSTGRES
    },
    SQL_SERVER,
    Lexing {
        cr_ends_line: false,
        ..SQL_SERVER
    },
    SQLITE,
    MYSQL,
    Lexing {
        backslash: Backslash::Never,
        ..MYSQL
    },
    Lexing {
        double_quote_string: false,
        ..MYSQL
    },
    Lexing {
        backslash: Backslash::Never,
        double_quote_string: false,
        ..MYSQL
    },
];

/// Byte ranges of each statement in `sql`, trimmed of surrounding whitespace and
/// excluding the terminating `;`. Statements containing only comments or
/// whitespace are dropped.
pub fn split_statements(sql: &str) -> Vec<Range<usize>> {
    split_as(sql, STANDARD)
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
            .position(|&b| b == b'\n' || (lexing.cr_ends_line && b == b'\r'))
            .map_or(bytes.len(), |p| from + p + 1)
    };
    match (bytes[i], bytes.get(i + 1), lexing.mysql) {
        // MySQL needs whitespace after `--`; `1--1` is arithmetic there.
        (b'-', Some(b'-'), false) => Some(line_end(i)),
        (b'-', Some(b'-'), true) if bytes.get(i + 2).is_none_or(|b| b.is_ascii_whitespace()) => {
            Some(line_end(i))
        }
        (b'#', _, true) => Some(line_end(i)),
        // MySQL runs what's inside `/*! … */`: it's code, not a comment.
        (b'/', Some(b'*'), true)
            if bytes.get(i + 2) == Some(&b'!') || bytes[i..].starts_with(b"/*M!") =>
        {
            None
        }
        (b'/', Some(b'*'), _) => Some(skip_block_comment(bytes, i, lexing.nested_comments)),
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
    if !lexing.mysql {
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
    let strings = match lexing.backslash {
        Backslash::Never => false,
        Backslash::EscapeStrings => quote == b'\'' && is_escape_string(sql, i),
        Backslash::Always => true,
    };
    strings && (quote == b'\'' || (quote == b'"' && lexing.double_quote_string))
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
            b'[' if lexing.brackets => i = skip_bracketed(bytes, i),
            b'$' if lexing.dollar_quotes => i = skip_dollar_quoted(sql, i),
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

/// Skip a `[bracketed name]` starting at `i`; `]]` inside is an escaped `]`.
fn skip_bracketed(bytes: &[u8], i: usize) -> usize {
    let mut j = i + 1;
    while j < bytes.len() {
        if bytes[j] == b']' {
            if bytes.get(j + 1) == Some(&b']') {
                j += 2;
                continue;
            }
            return j + 1;
        }
        j += 1;
    }
    bytes.len()
}

/// Skip a `/* … */` comment starting at `i`: to the first `*/`, or the matching
/// one when comments nest.
fn skip_block_comment(bytes: &[u8], i: usize, nested: bool) -> usize {
    if !nested {
        return bytes[i + 2..]
            .windows(2)
            .position(|w| w == b"*/")
            .map_or(bytes.len(), |p| i + 2 + p + 2);
    }
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
/// appears in them. The script is read every way an engine might read it (see
/// [`READINGS`]: where comments end, whether they nest, backslashes, `[names]`,
/// MySQL's `#` and `/*! … */`), and the worst reading counts, so quoting tricks
/// can't hide a write.
pub fn assess(sql: &str) -> Option<(Risk, String)> {
    READINGS
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
    "pg_file_unlink",
    "pg_file_rename",
    "dblink",
    "lo_create",
    "lo_creat",
    "lo_put",
    "lo_from_bytea",
    "lo_truncate",
    "lo_truncate64",
    "lowrite",
    "pg_notify",
    "pg_logical_emit_message",
    "pg_create_logical_replication_slot",
    "pg_create_physical_replication_slot",
    "pg_drop_replication_slot",
    "pg_replication_origin_create",
    "pg_replication_origin_drop",
    "pg_stat_reset",
    "pg_stat_reset_shared",
    "pg_stat_reset_single_table_counters",
    "pg_stat_reset_single_function_counters",
    "pg_stat_reset_slru",
    "pg_stat_reset_replication_slot",
    "pg_stat_reset_subscription_stats",
    "pg_stat_reset_backend_stats",
    "pg_stat_statements_reset",
    // Index maintenance: changes the index on disk.
    "gin_clean_pending_list",
    "brin_summarize_new_values",
    "brin_summarize_range",
    "brin_desummarize_range",
    "pg_backup_start",
    "pg_backup_stop",
    "pg_start_backup",
    "pg_stop_backup",
    "pg_wal_replay_pause",
    "pg_wal_replay_resume",
    "pg_import_system_collations",
    // SQL Server: the server, sessions, and other servers' data.
    "kill",
    "shutdown",
    "dbcc",
    "reconfigure",
    "deny",
    "openquery",
    "openrowset",
    "opendatasource",
];

/// Statements of two words that change something, found anywhere in a
/// statement (a SQL Server batch needs no semicolons): `backup database`,
/// `waitfor (receive …)`. Two words, so a column named `backup` isn't one.
const WRITE_PAIRS: &[(&str, &[&str])] = &[
    (
        "backup",
        &["database", "log", "certificate", "master", "service"],
    ),
    (
        "restore",
        &[
            "database",
            "log",
            "headeronly",
            "filelistonly",
            "verifyonly",
            "labelonly",
            "master",
            "service",
        ],
    ),
    ("disable", &["trigger"]),
    ("enable", &["trigger"]),
    ("waitfor", &["receive", "get"]),
    ("send", &["on"]),
    ("load", &["data", "xml"]),
    ("lock", &["table", "tables"]),
    ("unlock", &["tables"]),
    ("install", &["plugin", "component"]),
    // `next value for a_sequence` takes the next value.
    ("next", &["value"]),
];

/// The write a statement's words hide, if any: a verb from [`WRITE_VERBS`]
/// (also schema-qualified, `pg_catalog.setval`), an `xp_` procedure, or a
/// pair from [`WRITE_PAIRS`]. `for update` / `for no key update` and
/// `on duplicate key update` lock or belong to an insert, not a write.
fn hidden_write(words: &[String]) -> Option<&str> {
    words.iter().enumerate().find_map(|(ix, word)| {
        let name = word.rsplit('.').next().unwrap_or(word);
        let lock = name == "update" && ix > 0 && matches!(words[ix - 1].as_str(), "for" | "key");
        if (WRITE_VERBS.contains(&name) || name.starts_with("xp_")) && !lock {
            return Some(name);
        }
        let next = words.get(ix + 1)?;
        WRITE_PAIRS
            .iter()
            .any(|(first, seconds)| name == *first && seconds.contains(&next.as_str()))
            .then_some(name)
    })
}

/// `set` targets that change the server for everyone, or make Xenon or DuckDB
/// write files or fetch code: writes, even on a read-only connection.
const WRITING_SETTINGS: &[&str] = &[
    "global",
    "persist",
    "persist_only",
    "role",
    "authorization",
    "password",
    // DuckDB: profiles, logs and extensions.
    "enable_profiling",
    "enable_profile",
    "profiling_output",
    "profile_output",
    "profiling_mode",
    "log_query_path",
    "enable_http_logging",
    "http_logging_output",
    "enable_external_access",
    "autoinstall_known_extensions",
    "autoload_known_extensions",
    "allow_unsigned_extensions",
    "allow_community_extensions",
    "custom_extension_repository",
    "autoinstall_extension_repository",
    "extension_directory",
    "allowed_directories",
    "allowed_paths",
    "secret_directory",
    "temp_directory",
    "home_directory",
];

/// `pragma name(arg)` forms that only read; any other pragma with an argument
/// sets something (`pragma user_version(7)`).
const READING_PRAGMAS: &[&str] = &[
    "table_info",
    "table_xinfo",
    "table_list",
    "index_info",
    "index_xinfo",
    "index_list",
    "foreign_key_list",
    "foreign_key_check",
    "integrity_check",
    "quick_check",
    "storage_info",
    "show",
    "metadata_info",
];

fn assess_statement(statement: &str, lexing: Lexing) -> Option<(Risk, String)> {
    let words = words_outside_literals(statement, lexing);
    let first = words.first()?.as_str();
    let has = |word: &str| words.iter().any(|w| w == word);
    let second = words.get(1).map(String::as_str);
    let hidden = || hidden_write(&words);
    let verb = if READS.contains(&first) {
        match first {
            // Plain `explain` only plans; `explain analyze` runs the statement.
            "explain" if !(has("analyze") || has("analyse")) => return None,
            // `select … into` makes a table (or a file, on MySQL), after a CTE
            // or under `explain analyze` too.
            "select" | "with" | "explain" if has("into") && has("select") => {
                return Some((Risk::Write, "SELECT INTO".into()));
            }
            // MySQL's `reset master`, `reset persist`…; Postgres's `reset name` reads.
            "reset"
                if matches!(
                    second,
                    Some("master" | "replica" | "slave" | "persist" | "binary" | "query")
                ) =>
            {
                return Some((Risk::Write, "RESET".into()));
            }
            // Only `start transaction` starts nothing else (`start replica`).
            "start" if second != Some("transaction") => {
                return Some((Risk::Write, "START".into()));
            }
            // Changing who you are or the server's settings is a write, also as
            // `@@global.name`.
            "set"
                if words.iter().any(|word| {
                    let scope = word.split('.').next().unwrap_or(word);
                    let name = word.rsplit('.').next().unwrap_or(word);
                    WRITING_SETTINGS.contains(&scope) || WRITING_SETTINGS.contains(&name)
                }) =>
            {
                return Some((Risk::Write, "SET".into()));
            }
            // `pragma name = value` and `pragma name(value)` set something
            // (but `pragma table_info(t)` reads), and some pragmas act.
            "pragma"
                if statement.contains('=')
                    || (statement.contains('(')
                        && !second.is_some_and(|name| READING_PRAGMAS.contains(&name)))
                    || second.is_some_and(|name| {
                        ["enable_", "disable_", "force_", "create_", "drop_"]
                            .iter()
                            .any(|prefix| name.starts_with(prefix))
                            || matches!(name, "optimize" | "wal_checkpoint" | "incremental_vacuum")
                    }) =>
            {
                return Some((Risk::Write, "PRAGMA".into()));
            }
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

/// True when any engine might read `sql` as more than one statement (see
/// [`READINGS`]). What decides whether a script must run as one transaction:
/// Xenon's own split can see one statement where the server sees two.
pub fn several_statements(sql: &str) -> bool {
    READINGS
        .into_iter()
        .any(|lexing| split_as(sql, lexing).len() > 1)
}

/// True when `sql` changes anything, as far as [`assess`] can tell.
pub fn writes(sql: &str) -> bool {
    assess(sql).is_some()
}

/// True when `sql` manages a transaction itself (`begin`, `commit`,
/// `savepoint`…), so it shouldn't be wrapped in another.
pub fn controls_transaction(sql: &str) -> bool {
    statements_every_way(sql).any(|words| match words.first().map(String::as_str) {
        Some(
            "begin" | "start" | "commit" | "rollback" | "end" | "savepoint" | "release" | "abort"
            | "prepare",
        ) => true,
        Some("set") => {
            words.get(1).is_some_and(|w| w == "transaction")
                // `set autocommit = 0` leaves every statement in a transaction.
                || words.iter().any(|w| w.rsplit('.').next() == Some("autocommit"))
        }
        _ => false,
    })
}

/// True when `sql` manages a transaction in a way that takes it out of
/// Xenon's read-only wrapping on production: it starts one, or runs other
/// statements alongside `begin`/`commit`. Ending one on its own (`commit`,
/// `rollback`) doesn't count.
pub fn opens_unguarded_transaction(sql: &str) -> bool {
    if !controls_transaction(sql) {
        return false;
    }
    let ends_only = statements_every_way(sql).all(|words| {
        matches!(
            words.first().map(String::as_str),
            Some("commit" | "rollback" | "abort" | "end" | "release")
        )
    });
    !ends_only
}

/// Whether running `sql` leaves a transaction open (`Some(true)` after
/// `begin`, `Some(false)` after `commit` / `rollback`), by its last such
/// statement; `None` when it neither begins nor ends one. For engines that can't
/// say themselves.
pub fn transaction_effect(sql: &str) -> Option<bool> {
    split_statements(sql).into_iter().rev().find_map(|range| {
        let words = words_outside_literals(&sql[range], STANDARD);
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

/// Statements MySQL and MariaDB end a transaction before (and commit after):
/// schema changes, grants, `lock tables`… An "all or nothing" script holding
/// one can't be undone as a whole there.
fn mysql_commits_implicitly(words: &[String]) -> bool {
    let second = words.get(1).map(String::as_str);
    match words.first().map(String::as_str) {
        Some(
            "create" | "alter" | "drop" | "truncate" | "rename" | "grant" | "revoke" | "install"
            | "uninstall" | "flush" | "analyze" | "optimize" | "repair" | "cache" | "load",
        ) => !matches!(
            (words.first().map(String::as_str), second),
            (Some("create" | "drop"), Some("temporary"))
        ),
        Some("lock" | "unlock") => matches!(second, Some("table" | "tables" | "instance")),
        Some("set") => second == Some("password"),
        _ => false,
    }
}

/// True when MySQL would commit part of `sql` implicitly (see
/// [`mysql_commits_implicitly`]).
pub fn mysql_has_implicit_commit(sql: &str) -> bool {
    split_as(sql, MYSQL)
        .into_iter()
        .any(|range| mysql_commits_implicitly(&words_outside_literals(&sql[range], MYSQL)))
}

/// MySQL's transaction state after running `sql`, statement by statement,
/// from `(open, autocommit_off)` before it: `begin` / `start transaction` /
/// `xa start` open one, `commit` / `rollback` end it, schema changes commit
/// it, and with `autocommit = 0` any other statement opens one. MySQL can't be
/// asked (MariaDB can), so its driver follows what ran.
pub fn mysql_transaction_state(sql: &str, open: bool, autocommit_off: bool) -> (bool, bool) {
    let (open, autocommit_off, _) = mysql_transaction_walk(sql, open, autocommit_off);
    (open, autocommit_off)
}

/// After `sql` failed part-way: whether a transaction may be open, which it is
/// if one was open at any point (the statements after the failure, a closing
/// `commit` included, didn't run).
pub fn mysql_transaction_open_after_failure(sql: &str, open: bool, autocommit_off: bool) -> bool {
    mysql_transaction_walk(sql, open, autocommit_off).2
}

/// [`mysql_transaction_state`], and whether a transaction was open at any point.
fn mysql_transaction_walk(
    sql: &str,
    mut open: bool,
    mut autocommit_off: bool,
) -> (bool, bool, bool) {
    let mut ever = open;
    for range in split_as(sql, MYSQL) {
        let words = words_outside_literals(&sql[range], MYSQL);
        let second = words.get(1).map(String::as_str);
        match words.first().map(String::as_str) {
            Some("begin") if !matches!(second, Some("not")) => open = true,
            Some("start") if second == Some("transaction") => open = true,
            Some("xa") if matches!(second, Some("start" | "begin")) => open = true,
            Some("xa") if matches!(second, Some("commit" | "rollback")) => open = false,
            // `commit and chain` starts the next one at once.
            Some("commit" | "rollback") => {
                let chain = words.windows(2).any(|w| w[0] == "and" && w[1] == "chain")
                    && !words.windows(2).any(|w| w[0] == "no" && w[1] == "chain");
                open = chain;
            }
            Some("set")
                if words
                    .iter()
                    .any(|w| w.rsplit('.').next() == Some("autocommit")) =>
            {
                let value = words
                    .iter()
                    .skip_while(|w| w.rsplit('.').next() != Some("autocommit"))
                    .nth(1)
                    .map(String::as_str);
                match value {
                    Some("0" | "off" | "false") => autocommit_off = true,
                    // Turning it back on commits.
                    Some(_) => {
                        autocommit_off = false;
                        open = false;
                    }
                    None => {}
                }
            }
            _ if mysql_commits_implicitly(&words) => open = false,
            Some(_) if autocommit_off => open = true,
            _ => {}
        }
        ever |= open;
    }
    (open, autocommit_off, ever)
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
    statements_every_way(sql).any(|words| {
        // Reading a setting (`show default_transaction_read_only`) changes nothing.
        if words.first().is_some_and(|first| first == "show") {
            return false;
        }
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

/// Each statement's words, read every way in [`READINGS`].
fn statements_every_way(sql: &str) -> impl Iterator<Item = Vec<String>> + '_ {
    READINGS.into_iter().flat_map(move |lexing| {
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
            b'[' if lexing.brackets => {
                let end = skip_bracketed(bytes, i);
                words.push(statement[i + 1..end.saturating_sub(1).max(i + 1)].to_lowercase());
                i = end;
            }
            b'$' if lexing.dollar_quotes => i = skip_dollar_quoted(statement, i),
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
    "accessible",
    "all",
    "analyse",
    "analyze",
    "and",
    "any",
    "array",
    "as",
    "asc",
    "asensitive",
    "asymmetric",
    "authorization",
    "backup",
    "before",
    "between",
    "binary",
    "both",
    "browse",
    "bulk",
    "by",
    "call",
    "cascade",
    "case",
    "cast",
    "change",
    "check",
    "checkpoint",
    "clustered",
    "coalesce",
    "collate",
    "column",
    "columns",
    "commit",
    "condition",
    "constraint",
    "contains",
    "containstable",
    "continue",
    "convert",
    "create",
    "cross",
    "cume_dist",
    "current_catalog",
    "current_date",
    "current_role",
    "current_schema",
    "current_time",
    "current_timestamp",
    "current_user",
    "cursor",
    "database",
    "databases",
    "day",
    "dbcc",
    "dec",
    "declare",
    "default",
    "deferrable",
    "delayed",
    "delete",
    "dense_rank",
    "deny",
    "desc",
    "describe",
    "deterministic",
    "distinct",
    "distinctrow",
    "div",
    "do",
    "drop",
    "dual",
    "dump",
    "each",
    "else",
    "elseif",
    "empty",
    "enclosed",
    "end",
    "escaped",
    "except",
    "exists",
    "exit",
    "explain",
    "false",
    "fetch",
    "file",
    "first_value",
    "float4",
    "float8",
    "for",
    "force",
    "foreign",
    "freetext",
    "freetexttable",
    "freeze",
    "from",
    "full",
    "fulltext",
    "function",
    "generated",
    "get",
    "grant",
    "group",
    "groups",
    "having",
    "high_priority",
    "holdlock",
    "hour",
    "identity",
    "identitycol",
    "if",
    "ignore",
    "ilike",
    "in",
    "index",
    "infile",
    "initially",
    "inner",
    "inout",
    "insensitive",
    "insert",
    "int1",
    "int2",
    "int3",
    "int4",
    "int8",
    "integer",
    "intersect",
    "interval",
    "into",
    "is",
    "isnull",
    "iterate",
    "join",
    "json_table",
    "key",
    "key_block",
    "keys",
    "kill",
    "lag",
    "last_value",
    "lateral",
    "lead",
    "leading",
    "leave",
    "left",
    "like",
    "limit",
    "linear",
    "lineno",
    "lines",
    "load",
    "localtime",
    "localtimestamp",
    "lock",
    "long",
    "longblob",
    "longtext",
    "loop",
    "low_priority",
    "master_bind",
    "match",
    "maxvalue",
    "mediumblob",
    "mediumint",
    "mediumtext",
    "merge",
    "middleint",
    "minute",
    "mod",
    "modifies",
    "month",
    "national",
    "natural",
    "no_write_to_binlog",
    "nocheck",
    "nonclustered",
    "not",
    "notnull",
    "nth_value",
    "ntile",
    "null",
    "nullif",
    "offset",
    "offsets",
    "on",
    "only",
    "opendatasource",
    "openquery",
    "openrowset",
    "optimize",
    "option",
    "optionally",
    "options",
    "or",
    "order",
    "out",
    "outer",
    "outfile",
    "over",
    "overlaps",
    "partition",
    "percent",
    "percent_rank",
    "pivot",
    "placing",
    "plan",
    "primary",
    "proc",
    "procedure",
    "purge",
    "range",
    "rank",
    "read",
    "read_write",
    "reads",
    "reconfigure",
    "recursive",
    "references",
    "regexp",
    "release",
    "rename",
    "repeat",
    "replace",
    "require",
    "resignal",
    "restore",
    "restrict",
    "return",
    "returning",
    "revert",
    "revoke",
    "right",
    "rlike",
    "rollback",
    "row",
    "row_number",
    "rowcount",
    "rowguidcol",
    "rows",
    "save",
    "savepoint",
    "schema",
    "second",
    "select",
    "session_user",
    "set",
    "setuser",
    "show",
    "shutdown",
    "signal",
    "similar",
    "some",
    "spatial",
    "specific",
    "sql",
    "sqlexception",
    "sqlstate",
    "sqlwarning",
    "ssl",
    "starting",
    "statistics",
    "stored",
    "straight_join",
    "symmetric",
    "system_user",
    "table",
    "tables",
    "tablesample",
    "terminated",
    "textsize",
    "then",
    "tinyblob",
    "tinyint",
    "tinytext",
    "to",
    "top",
    "trailing",
    "tran",
    "transaction",
    "trigger",
    "true",
    "try_convert",
    "tsequal",
    "undo",
    "union",
    "unique",
    "unlock",
    "unpivot",
    "unsigned",
    "update",
    "usage",
    "use",
    "user",
    "using",
    "utc_date",
    "utc_time",
    "utc_timestamp",
    "values",
    "varbinary",
    "varcharacter",
    "variadic",
    "varying",
    "verbose",
    "view",
    "virtual",
    "when",
    "where",
    "while",
    "window",
    "with",
    "write",
    "xor",
    "year",
    "year_month",
    "zerofill",
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
/// condition and sort column, capped at `limit` rows. `name` is the table's
/// name as SQL, already quoted (see [`crate::qualified_name`]).
pub fn browse_table(
    quote: impl Fn(&str) -> String,
    name: &str,
    filter: &str,
    order: Option<(&str, SortDirection)>,
    limit: usize,
    top: bool,
) -> String {
    browse_page(quote, name, filter, order, limit, 0, top)
}

/// [`browse_table`] from row `offset` on: `limit … offset …`, or on SQL Server
/// (`top`) `offset … rows fetch next … rows only`, which needs an `order by`.
#[allow(clippy::too_many_arguments)]
pub fn browse_page(
    quote: impl Fn(&str) -> String,
    name: &str,
    filter: &str,
    order: Option<(&str, SortDirection)>,
    limit: usize,
    offset: usize,
    top: bool,
) -> String {
    let fetch = top && offset > 0;
    let mut sql = if top && !fetch {
        format!("select top ({limit}) * from {name}")
    } else {
        format!("select * from {name}")
    };
    let filter = filter.trim().trim_end_matches(';').trim();
    if !filter.is_empty() {
        sql.push_str(&format!("\nwhere {filter}"));
    }
    let direction = |d: SortDirection| match d {
        SortDirection::Ascending => "asc",
        SortDirection::Descending => "desc",
    };
    match order {
        Some((column, d)) => {
            sql.push_str(&format!("\norder by {} {}", quote(column), direction(d)));
        }
        // SQL Server pages only an ordered query: the query's own order.
        None if fetch => sql.push_str("\norder by (select null)"),
        None => {}
    }
    if fetch {
        sql.push_str(&format!(
            "\noffset {offset} rows fetch next {limit} rows only;"
        ));
    } else if top {
        sql.push(';');
    } else if offset > 0 {
        sql.push_str(&format!("\nlimit {limit} offset {offset};"));
    } else {
        sql.push_str(&format!("\nlimit {limit};"));
    }
    sql
}

fn same_line(sql: &str, a: usize, b: usize) -> bool {
    let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
    !sql[lo..hi].contains('\n')
}

/// The placeholders an engine takes besides `:name`, which every engine gets.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ParamStyle {
    /// `$1`, `$2`… (Postgres, DuckDB).
    pub dollar: bool,
    /// `?` (MySQL, SQLite, DuckDB; on Postgres it's a JSON operator).
    pub question: bool,
    /// Read quotes and comments as MySQL does.
    pub mysql: bool,
    /// `[names]` are quoted names (SQLite, SQL Server): `[a:b]` has no `:b`.
    pub brackets: bool,
}

/// A placeholder in a query, filled in before it runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Param {
    pub range: Range<usize>,
    /// What it's asked for and remembered by: `:status`, `$1`, or `?1` for
    /// the first `?`. The same `:name` twice is one parameter.
    pub key: String,
}

/// The placeholders in `sql`, in order, skipping strings, quoted names,
/// comments, dollar-quoted bodies, `::casts`, `:=` and slices like `a[1:n]`.
pub fn parameters(sql: &str, style: ParamStyle) -> Vec<Param> {
    let lexing = if style.mysql {
        MYSQL
    } else {
        Lexing {
            brackets: style.brackets,
            ..STANDARD
        }
    };
    let bytes = sql.as_bytes();
    let mut params = Vec::new();
    let mut questions = 0;
    let mut i = 0;
    let ident = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    while i < bytes.len() {
        if let Some(code) = executable_comment_start(bytes, i, lexing) {
            i = code;
            continue;
        }
        if let Some(end) = comment_end(bytes, i, lexing) {
            i = end;
            continue;
        }
        let after_word = i > 0 && (ident(bytes[i - 1]) || bytes[i - 1] >= 0x80);
        match bytes[i] {
            b'\'' | b'"' | b'`' => i = skip_quoted(bytes, i, backslash_escapes(sql, i, lexing)),
            b'[' if lexing.brackets => i = skip_bracketed(bytes, i),
            b'$' if style.dollar
                && !after_word
                && bytes.get(i + 1).is_some_and(u8::is_ascii_digit) =>
            {
                let end = (i + 1..bytes.len())
                    .find(|&j| !bytes[j].is_ascii_digit())
                    .unwrap_or(bytes.len());
                params.push(Param {
                    range: i..end,
                    key: sql[i..end].to_string(),
                });
                i = end;
            }
            b'$' if lexing.dollar_quotes => i = skip_dollar_quoted(sql, i),
            // `::cast` and `:=` aren't placeholders.
            b':' if bytes.get(i + 1) == Some(&b':') => i += 2,
            b':' if !after_word
                && bytes
                    .get(i + 1)
                    .is_some_and(|b| b.is_ascii_alphabetic() || *b == b'_') =>
            {
                let end = (i + 1..bytes.len())
                    .find(|&j| !ident(bytes[j]))
                    .unwrap_or(bytes.len());
                params.push(Param {
                    range: i..end,
                    key: sql[i..end].to_string(),
                });
                i = end;
            }
            b'?' if style.question => {
                questions += 1;
                params.push(Param {
                    range: i..i + 1,
                    key: format!("?{questions}"),
                });
                i += 1;
            }
            _ => i += 1,
        }
    }
    params
}

/// `sql` with each placeholder replaced by the SQL literal `value` gives for its key.
///
/// A literal that would run into an operator next to it is set apart by a
/// space: `0 -:n` with `n = -1` becomes `0 - -1`, not `0 --1`, which would
/// comment out the rest of the line.
pub fn bind(sql: &str, params: &[Param], value: impl Fn(&str) -> String) -> String {
    let mut out = sql.to_string();
    let operator =
        |c: Option<char>| c.is_some_and(|c| c.is_ascii() && OPERATOR.contains(&(c as u8)));
    for param in params.iter().rev() {
        let mut literal = value(&param.key);
        if operator(out[..param.range.start].chars().next_back())
            && operator(literal.chars().next())
        {
            literal.insert(0, ' ');
        }
        if operator(out[param.range.end..].chars().next()) && operator(literal.chars().next_back())
        {
            literal.push(' ');
        }
        out.replace_range(param.range.clone(), &literal);
    }
    out
}

/// Whether byte `offset` of `sql` is in code, not inside a string, a quoted
/// name, a comment or a dollar-quoted body (as standard SQL reads it, or MySQL).
pub fn in_code(sql: &str, offset: usize, mysql: bool) -> bool {
    let lexing = if mysql { MYSQL } else { STANDARD };
    let bytes = sql.as_bytes();
    let mut i = 0;
    while i < bytes.len() && i < offset {
        let end = if let Some(code) = executable_comment_start(bytes, i, lexing) {
            i = code;
            continue;
        } else if let Some(end) = comment_end(bytes, i, lexing) {
            end
        } else {
            match bytes[i] {
                b'\'' | b'"' | b'`' => skip_quoted(bytes, i, backslash_escapes(sql, i, lexing)),
                b'$' if lexing.dollar_quotes => skip_dollar_quoted(sql, i),
                _ => {
                    i += 1;
                    continue;
                }
            }
        };
        // A line comment ends just after its newline: the next line is code.
        if offset < end && !(bytes[end - 1] == b'\n' && offset == end) {
            return false;
        }
        i = end.max(i + 1);
    }
    true
}

/// Whether `a` and `b` are the same SQL, differing only in whitespace between
/// tokens: strings, quoted names, dollar-quoted bodies and comments must match
/// exactly. A formatter's output is used only when this holds, so it can't, say,
/// join a line onto a `-- comment` and comment out the code after it.
pub fn same_code(a: &str, b: &str, mysql: bool) -> bool {
    let lexing = if mysql { MYSQL } else { STANDARD };
    tokens(a, lexing) == tokens(b, lexing)
}

const OPERATOR: &[u8] = b"<>=!~+-*/%|&^@#:?";

/// `sql` as a sequence of tokens with whitespace between them dropped.
fn tokens(sql: &str, lexing: Lexing) -> Vec<&str> {
    let bytes = sql.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if let Some(code) = executable_comment_start(bytes, i, lexing) {
            out.push(&sql[i..code]);
            i = code;
            continue;
        }
        if let Some(end) = comment_end(bytes, i, lexing) {
            out.push(sql[i..end].trim_end());
            i = end;
            continue;
        }
        let end = match bytes[i] {
            b'\'' | b'"' | b'`' => skip_quoted(bytes, i, backslash_escapes(sql, i, lexing)),
            b'$' if lexing.dollar_quotes => skip_dollar_quoted(sql, i),
            b if b.is_ascii_whitespace() => {
                i += 1;
                continue;
            }
            // A word or number, as one token.
            b if b.is_ascii_alphanumeric() || b == b'_' || b >= 0x80 => (i..bytes.len())
                .find(|&j| {
                    !(bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_' || bytes[j] >= 0x80)
                })
                .unwrap_or(bytes.len()),
            // An operator (`<=`, `::`, `->>`), as one token: `< =` isn't `<=`.
            b if OPERATOR.contains(&b) => (i..bytes.len())
                .find(|&j| !OPERATOR.contains(&bytes[j]) || comment_end(bytes, j, lexing).is_some())
                .unwrap_or(bytes.len()),
            // Brackets, commas, dots, semicolons: one each.
            _ => i + 1,
        };
        // A string right after its prefix (`E'…'`, `N'…'`, `X'41'`, `U&'…'`,
        // `_utf8mb4'…'`) is one token with it: spaced apart, it means something
        // else.
        let mut start = i;
        if matches!(bytes[i], b'\'' | b'"') {
            let offset = |token: &str| token.as_ptr() as usize - sql.as_ptr() as usize;
            let word = |t: &str| t.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_');
            while let Some(last) = out.last().copied() {
                let joined = offset(last) + last.len() == start;
                let before = out.len().checked_sub(2).map(|ix| out[ix]);
                let prefix = word(last)
                    || (last == "&"
                        && before.is_some_and(|u| {
                            u.eq_ignore_ascii_case("u") && offset(u) + u.len() == offset(last)
                        }));
                if !(joined && prefix) {
                    break;
                }
                start = offset(last);
                out.pop();
            }
        }
        out.push(&sql[start..end]);
        i = end;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn later_pages_skip_the_earlier_rows() {
        let quote = |i: &str| i.to_string();
        assert_eq!(
            browse_page(quote, "orders", "", None, 100, 200, false),
            "select * from orders\nlimit 100 offset 200;"
        );
        assert_eq!(
            browse_page(quote, "orders", "id > 5", None, 100, 200, true),
            "select * from orders\nwhere id > 5\norder by (select null)\noffset 200 rows fetch next 100 rows only;"
        );
        assert_eq!(
            browse_page(
                quote,
                "orders",
                "",
                Some(("total", SortDirection::Ascending)),
                100,
                100,
                true
            ),
            "select * from orders\norder by total asc\noffset 100 rows fetch next 100 rows only;"
        );
        // The first page on SQL Server keeps `top`.
        assert_eq!(
            browse_page(quote, "orders", "", None, 100, 0, true),
            "select top (100) * from orders;"
        );
    }

    #[test]
    fn a_string_prefix_stays_with_its_string() {
        assert!(!same_code(
            "select U&'\\0041'",
            "select U & '\\0041'",
            false
        ));
        assert!(!same_code("select X'41'", "select X '41'", true));
        assert!(!same_code("select E'a'", "select E 'a'", false));
        // Ordinary layout still counts as the same.
        assert!(same_code(
            "select a='x' from t",
            "select a = 'x'\nfrom t",
            false
        ));
    }

    #[test]
    fn same_code_ignores_layout_but_nothing_else() {
        assert!(same_code(
            "select a,b from t",
            "select\n  a,\n  b\nfrom\n  t",
            false
        ));
        // String contents, quoted names and comments must be untouched.
        assert!(!same_code("select 'a  b'", "select 'a b'", false));
        assert!(!same_code(
            "select \"two  words\"",
            "select \"two words\"",
            false
        ));
        assert!(!same_code("select $$ a  b $$", "select $$ a b $$", false));
        // Joining a line onto a comment would comment out the code after it.
        assert!(!same_code(
            "select a -- note\nfrom t",
            "select a -- note from t",
            false
        ));
        assert!(!same_code(
            "select a # note\nfrom t",
            "select a # note from t",
            true
        ));
        assert!(same_code(
            "select a -- note\nfrom t",
            "select\n  a -- note\nfrom\n  t",
            false
        ));
        // Splitting or joining tokens changes the code.
        assert!(!same_code("select a < = b", "select a <= b", false));
        assert!(!same_code("select ab", "select a b", false));
    }

    #[test]
    fn parameters_are_found_outside_strings_comments_and_casts() {
        let keys = |sql: &str, style: ParamStyle| {
            parameters(sql, style)
                .into_iter()
                .map(|p| p.key)
                .collect::<Vec<_>>()
        };
        let pg = ParamStyle {
            dollar: true,
            ..Default::default()
        };
        let mysql = ParamStyle {
            question: true,
            mysql: true,
            ..Default::default()
        };
        assert_eq!(
            keys("select * from t where a = :status and b = :status", pg),
            [":status", ":status"]
        );
        assert_eq!(keys("select $1, $2 from t", pg), ["$1", "$2"]);
        // Casts, assignments, slices, strings, quoted names and comments.
        assert_eq!(
            keys(
                "select x::int, a[1:n], ':no', \":no\", $$ :no $$ -- :no\n /* :no */ from t where y = :yes",
                pg
            ),
            [":yes"]
        );
        // `?` is a JSON operator on Postgres, a placeholder elsewhere.
        assert!(keys("select data ? 'k' from t", pg).is_empty());
        assert_eq!(
            keys("select ? , ? from t where c = :c", mysql),
            ["?1", "?2", ":c"]
        );
        assert!(keys("set @a := 1; select '\\' ? '", mysql).is_empty());
        assert!(keys("select 'it''s :not' from t", ParamStyle::default()).is_empty());
    }

    #[test]
    fn binding_replaces_each_placeholder() {
        let sql = "select * from t where a = :a and b = $1 and c = :a";
        let style = ParamStyle {
            dollar: true,
            ..Default::default()
        };
        let params = parameters(sql, style);
        let bound = bind(sql, &params, |key| match key {
            ":a" => "'x''y'".into(),
            "$1" => "42".into(),
            other => panic!("{other}"),
        });
        assert_eq!(
            bound,
            "select * from t where a = 'x''y' and b = 42 and c = 'x''y'"
        );
    }

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
    fn comments_and_quotes_read_as_each_engine_does_cant_hide_a_write() {
        for sql in [
            // Postgres and DuckDB end a `--` comment at a lone carriage return.
            "select 1 -- x\rdelete from orders",
            "select 1 -- x\r; delete from orders",
            // SQLite and MySQL don't nest comments: the delete is code to them.
            "select 1; /* a /* */ delete from orders; /* */",
            // A quote inside a `[name]` (SQL Server, SQLite) isn't a quote.
            "select [it's] from t; delete from orders; select 'x'",
            "select [a]]--] from t; delete from orders",
            // MySQL with NO_BACKSLASH_ESCAPES: the backslash doesn't escape.
            r"select 'a\'; delete from orders; -- '",
            // MySQL with ANSI_QUOTES: `"…"` is a name, with no backslash escapes.
            r#"select "a\"; delete from orders; -- ""#,
            // Postgres with standard_conforming_strings off: backslashes escape.
            r"select 'a\'; delete from orders; -- '",
        ] {
            assert!(
                assess(sql).is_some_and(|(risk, _)| risk >= Risk::EveryRow),
                "{sql:?}: {:?}",
                assess(sql)
            );
            assert!(writes(sql), "{sql:?}");
        }
        // Still reads.
        for sql in [
            "select 1 -- a comment\r\n",
            "select [order], \"group\" from [my table] /* c /* d */ */",
            "select 'it''s' from t where a = $1",
        ] {
            assert_eq!(assess(sql), None, "{sql:?}");
        }
    }

    #[test]
    fn statements_that_change_the_server_or_files_are_writes() {
        for sql in [
            // MySQL: binary logs, persisted settings, replication.
            "reset master",
            "reset persist",
            "start replica",
            "set @@global.max_connections = 10",
            "set @@persist.max_connections = 10",
            // SQL Server batches without semicolons.
            "select 1\nkill 55",
            "select 1 shutdown",
            "select 1\nbackup database shop to disk = 'x'",
            "select 1\nrestore database shop from disk = 'x'",
            "select 1\ndbcc checkdb",
            "select 1\ndeny select on t to bob",
            "select 1\ndisable trigger t on orders",
            "waitfor (receive top (1) * from q)",
            "select next value for order_ids",
            "select * from openquery(remote, 'delete from t')",
            "begin select 1 kill 55 end",
            "exec xp_dirtree 'c:\\'",
            // `into` after a CTE, or under explain analyze.
            "with c as (select 1 as n) select n into made from c",
            "explain analyze select 1 into made",
            "select 1 into outfile '/tmp/x'",
            // Functions, schema-qualified or not on the list before.
            "select pg_catalog.pg_terminate_backend(42)",
            "select pg_catalog.setval('s', 1)",
            "select dblink('dbname=x', 'delete from t')",
            "select lo_from_bytea(0, 'x')",
            "select pg_notify('c', 'x')",
            "select gin_clean_pending_list('idx'::regclass)",
            "select brin_summarize_new_values('idx')",
            "select pg_stat_statements_reset()",
            // SQLite pragmas that set or act; DuckDB settings that write files
            // or fetch extensions.
            "pragma user_version(7)",
            "pragma optimize",
            "pragma enable_profiling",
            "set enable_profiling = 'json'",
            "set profiling_output = '/tmp/p.json'",
            "set autoinstall_known_extensions = true",
            "lock tables orders write",
            "load data infile 'x' into table t",
        ] {
            assert!(writes(sql), "{sql:?} read as a read");
        }
        for sql in [
            "reset all",
            "reset search_path",
            "start transaction",
            "start transaction read only",
            "set search_path = app",
            "set @@session.sql_mode = ''",
            "select backup, restore, lock from settings",
            "pragma table_info(orders)",
            "pragma table_info('orders')",
            "pragma user_version",
            "select next_value from t",
            "explain select 1",
        ] {
            assert!(!writes(sql), "{sql:?} read as a write");
        }
    }

    #[test]
    fn transactions_that_escape_the_read_only_wrap() {
        for sql in [
            "begin",
            "start transaction",
            "begin; select 1; commit",
            "select 1; commit",
        ] {
            assert!(opens_unguarded_transaction(sql), "{sql}");
        }
        for sql in ["commit", "rollback", "end", "select 1", "commit; rollback"] {
            assert!(!opens_unguarded_transaction(sql), "{sql}");
        }
    }

    #[test]
    fn bracketed_names_hold_no_placeholders() {
        let style = ParamStyle {
            question: true,
            brackets: true,
            ..Default::default()
        };
        let found = parameters("select [a:b], [c?] from t where x = :x", style);
        assert_eq!(
            found.iter().map(|p| p.key.as_str()).collect::<Vec<_>>(),
            [":x"]
        );
    }

    #[test]
    fn a_bound_value_never_joins_an_operator_into_a_comment() {
        let sql = "delete from accounts where balance > 0 -:n and id = :id";
        let bound = bind(sql, &parameters(sql, ParamStyle::default()), |key| {
            if key == ":n" { "-1" } else { "7" }.to_string()
        });
        assert_eq!(
            bound,
            "delete from accounts where balance > 0 - -1 and id = 7"
        );
        assert!(assess(&bound).is_some_and(|(risk, _)| risk == Risk::Write));
        let sql = "select 1 /:n";
        assert_eq!(
            bind(sql, &parameters(sql, ParamStyle::default()), |_| "*2"
                .into()),
            "select 1 / *2"
        );
    }

    #[test]
    fn mysql_transactions_are_followed_statement_by_statement() {
        let state = mysql_transaction_state;
        assert_eq!(
            state("begin; update t set a = 1", false, false),
            (true, false)
        );
        assert_eq!(
            state("begin; create table x (a int)", false, false),
            (false, false)
        );
        assert_eq!(state("commit", true, false), (false, false));
        // With autocommit off, any statement opens one; turning it on commits.
        assert_eq!(state("set autocommit = 0", false, false), (false, true));
        assert_eq!(state("insert into t values (1)", false, true), (true, true));
        assert_eq!(
            state("set @@session.autocommit = 1", true, true),
            (false, false)
        );
        assert_eq!(state("select 1", false, false), (false, false));
        assert_eq!(state("xa start 'x'", false, false), (true, false));
        assert_eq!(state("commit and chain", true, false), (true, false));
        assert_eq!(state("commit and no chain", true, false), (false, false));
        // Failed part-way, a transaction opened on the way is still open.
        assert!(mysql_transaction_open_after_failure(
            "start transaction; insert into t values (1); insert into nope values (2); commit",
            false,
            false
        ));
        assert!(!mysql_transaction_open_after_failure(
            "select 1; select nope",
            false,
            false
        ));
        assert!(mysql_has_implicit_commit(
            "insert into t values (1); alter table t add b int"
        ));
        assert!(!mysql_has_implicit_commit(
            "insert into t values (1); create temporary table x (a int)"
        ));
    }

    #[test]
    fn reading_the_read_only_setting_doesnt_lift_it() {
        assert!(!lifts_read_only("show default_transaction_read_only"));
        assert!(!lifts_read_only("show transaction_read_only"));
        assert!(lifts_read_only("set default_transaction_read_only = off"));
        assert!(lifts_read_only("set @@session.transaction_read_only = 0"));
    }

    #[test]
    fn several_statements_counts_every_engines_reading() {
        assert!(!several_statements("select 1"));
        assert!(!several_statements("select 1; -- done"));
        assert!(several_statements("select 1; select 2"));
        // One statement to Xenon's own split, two to DuckDB or SQLite.
        assert!(several_statements(
            "insert into t values (1) -- c\r; insert into u values (2)"
        ));
        assert!(several_statements(
            "insert into t values (1) /* a /* */; insert into u values (2); /* */"
        ));
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
                "[orders]",
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
            browse_table(quote, "orders", "", None, 1000, false),
            "select * from orders\nlimit 1000;"
        );
        assert_eq!(
            browse_table(
                quote,
                "\"Analytics\".\"order\"",
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
            browse_table(quote, "\"public\".\"orders\"", "", None, 1000, false),
            "select * from \"public\".\"orders\"\nlimit 1000;"
        );
        assert_eq!(
            browse_table(
                quote,
                "\"public\".\"orders\"",
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
