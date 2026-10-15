//! Autocomplete for the SQL editor: tables, columns (through aliases),
//! schemas, functions and keywords, from the connection's live schema.
//!
//! [`complete`] is plain logic over the text and a [`Catalog`]; [`SqlCompletions`]
//! adapts it to the editor's completion menu.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use anyhow::Result;
use gpui::{App, Task, Window};
use gpui_component::input::{CompletionProvider, Rope, RopeExt as _};
use lsp_types::{
    CompletionContext, CompletionItem, CompletionItemKind, CompletionResponse, CompletionTextEdit,
    Range, TextEdit,
};
use xenon_driver::{Connection, ObjectKind, Schema};

/// What the editor completes against: one connection's schema.
pub struct Catalog {
    pub schemas: Vec<Schema>,
    pub default_schema: Option<String>,
    /// Quotes identifiers that need it, in this connection's dialect.
    pub ident: Box<dyn Fn(&str) -> String>,
}

impl Catalog {
    pub fn new(schemas: Vec<Schema>, conn: Arc<dyn Connection>) -> Self {
        Self {
            schemas,
            default_schema: conn.default_schema(),
            ident: Box::new(move |name| conn.ident(name)),
        }
    }

    fn table(
        &self,
        schema: Option<&str>,
        name: &str,
    ) -> Option<(&Schema, &xenon_driver::SchemaObject)> {
        let schemas = self.schemas.iter().filter(|s| match schema {
            Some(schema) => s.name.eq_ignore_ascii_case(schema),
            None => true,
        });
        // Unqualified names prefer the default schema, as the database would.
        let mut found: Vec<_> = schemas
            .flat_map(|s| s.objects.iter().map(move |o| (s, o)))
            .filter(|(_, o)| o.name.eq_ignore_ascii_case(name))
            .collect();
        found.sort_by_key(|(s, _)| Some(s.name.as_str()) != self.default_schema.as_deref());
        found.into_iter().next()
    }

    fn shows_schemas(&self) -> bool {
        self.schemas.len() > 1
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    Column,
    Table,
    View,
    Schema,
    Function,
    Keyword,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Suggestion {
    pub label: String,
    /// What replaces the word being typed (quoted when the name needs it).
    pub insert: String,
    pub kind: Kind,
    pub detail: Option<String>,
}

/// Suggestions for the word ending at `offset`, and where that word starts.
pub fn complete(sql: &str, offset: usize, catalog: &Catalog) -> (usize, Vec<Suggestion>) {
    let offset = offset.min(sql.len());
    let before = &sql[..offset];
    let start = before
        .char_indices()
        .rev()
        .take_while(|(_, c)| c.is_alphanumeric() || *c == '_' || *c == '$')
        .last()
        .map_or(offset, |(ix, _)| ix);
    let prefix = &sql[start..offset];

    // The statement around the cursor: its tables and their aliases.
    let statement = xenon_driver::sql::statement_at(sql, offset).unwrap_or(0..sql.len());
    let scope = tables_in_scope(&sql[statement.clone()], catalog);

    let mut out: Vec<Suggestion> = Vec::new();
    if let Some(qualifier) = qualifier(&sql[..start]) {
        // `alias.` or `table.`: that table's columns.
        let table = scope
            .iter()
            .find(|t| {
                t.alias
                    .as_deref()
                    .is_some_and(|a| a.eq_ignore_ascii_case(&qualifier))
            })
            .or_else(|| {
                scope
                    .iter()
                    .find(|t| t.name.eq_ignore_ascii_case(&qualifier))
            })
            .map(|t| (t.schema.clone(), t.name.clone()))
            .or_else(|| {
                catalog
                    .table(None, &qualifier)
                    .map(|(s, o)| (Some(s.name.clone()), o.name.clone()))
            });
        if let Some((schema, name)) = table {
            if let Some((_, object)) = catalog.table(schema.as_deref(), &name) {
                for column in &object.columns {
                    out.push(Suggestion {
                        label: column.name.clone(),
                        insert: (catalog.ident)(&column.name),
                        kind: Kind::Column,
                        detail: Some(column.type_name.to_lowercase()),
                    });
                }
            }
        } else if let Some(schema) = catalog
            .schemas
            .iter()
            .find(|s| s.name.eq_ignore_ascii_case(&qualifier))
        {
            // `schema.`: its tables.
            for object in &schema.objects {
                out.push(table_suggestion(object, None, catalog));
            }
        }
        return (start, rank(out, prefix));
    }

    let previous = previous_word(&sql[statement.start.min(start)..start]);
    let wants_table = matches!(
        previous.as_deref(),
        Some("from" | "join" | "update" | "into" | "table" | "truncate" | "describe")
    );
    if wants_table {
        for schema in &catalog.schemas {
            for object in &schema.objects {
                let qualified = Some(schema.name.as_str()) != catalog.default_schema.as_deref()
                    && catalog.shows_schemas();
                out.push(table_suggestion(
                    object,
                    qualified.then_some(schema.name.as_str()),
                    catalog,
                ));
            }
        }
        if catalog.shows_schemas() {
            for schema in &catalog.schemas {
                out.push(Suggestion {
                    label: schema.name.clone(),
                    insert: (catalog.ident)(&schema.name),
                    kind: Kind::Schema,
                    detail: Some("schema".into()),
                });
            }
        }
        return (start, rank(out, prefix));
    }

    // Anywhere else: columns of the tables in this statement, then tables,
    // functions and keywords.
    let mut seen: HashMap<String, usize> = HashMap::new();
    for table in &scope {
        let Some((_, object)) = catalog.table(table.schema.as_deref(), &table.name) else {
            continue;
        };
        for column in &object.columns {
            let owner = table.alias.clone().unwrap_or_else(|| table.name.clone());
            match seen.get(&column.name) {
                // The same column in two tables: say which, and qualify it.
                Some(&ix) => {
                    out[ix].detail = Some(format!(
                        "{} · several tables",
                        column.type_name.to_lowercase()
                    ));
                }
                None => {
                    seen.insert(column.name.clone(), out.len());
                    out.push(Suggestion {
                        label: column.name.clone(),
                        insert: (catalog.ident)(&column.name),
                        kind: Kind::Column,
                        detail: Some(format!("{} · {owner}", column.type_name.to_lowercase())),
                    });
                }
            }
        }
    }
    if !prefix.is_empty() {
        for schema in &catalog.schemas {
            for object in &schema.objects {
                out.push(table_suggestion(object, None, catalog));
            }
        }
    }
    let upper = !prefix.is_empty() && prefix.chars().all(|c| !c.is_lowercase());
    let cased = |word: &str| {
        if upper {
            word.to_uppercase()
        } else {
            word.to_string()
        }
    };
    for function in FUNCTIONS {
        out.push(Suggestion {
            label: cased(function),
            insert: cased(function),
            kind: Kind::Function,
            detail: Some("function".into()),
        });
    }
    for keyword in KEYWORDS {
        out.push(Suggestion {
            label: cased(keyword),
            insert: cased(keyword),
            kind: Kind::Keyword,
            detail: None,
        });
    }
    (start, rank(out, prefix))
}

fn table_suggestion(
    object: &xenon_driver::SchemaObject,
    schema: Option<&str>,
    catalog: &Catalog,
) -> Suggestion {
    let insert = match schema {
        Some(schema) => format!(
            "{}.{}",
            (catalog.ident)(schema),
            (catalog.ident)(&object.name)
        ),
        None => (catalog.ident)(&object.name),
    };
    let kind = match object.kind {
        ObjectKind::Table => Kind::Table,
        ObjectKind::View => Kind::View,
    };
    Suggestion {
        label: match schema {
            Some(schema) => format!("{schema}.{}", object.name),
            None => object.name.clone(),
        },
        insert,
        kind,
        detail: Some(if kind == Kind::View { "view" } else { "table" }.into()),
    }
}

/// Matches for `prefix`: names starting with it first, then names containing
/// it, each in the order they came. An empty prefix keeps everything.
fn rank(items: Vec<Suggestion>, prefix: &str) -> Vec<Suggestion> {
    const LIMIT: usize = 80;
    if prefix.is_empty() {
        return items.into_iter().take(LIMIT).collect();
    }
    let needle = prefix.to_lowercase();
    let (mut starts, mut contains): (Vec<_>, Vec<_>) = (Vec::new(), Vec::new());
    for item in items {
        let label = item.label.to_lowercase();
        // The last part of `schema.table` counts as its start too.
        let tail = label.rsplit('.').next().unwrap_or(&label).to_string();
        if label.starts_with(&needle) || tail.starts_with(&needle) {
            // Don't offer exactly what's already typed.
            if label != needle {
                starts.push(item);
            }
        } else if label.contains(&needle) {
            contains.push(item);
        }
    }
    starts.extend(contains);
    starts.truncate(LIMIT);
    starts
}

/// A table named in the statement, with its alias.
#[derive(Debug, PartialEq)]
struct TableRef {
    schema: Option<String>,
    name: String,
    alias: Option<String>,
}

/// Tables after FROM, JOIN, UPDATE or INTO (and commas in a FROM list), with
/// their aliases, that exist in the catalog.
fn tables_in_scope(statement: &str, catalog: &Catalog) -> Vec<TableRef> {
    let words = words(statement);
    let mut out = Vec::new();
    let mut ix = 0;
    while ix < words.len() {
        let introduces = matches!(
            words[ix].to_lowercase().as_str(),
            "from" | "join" | "update" | "into"
        );
        let mut next = ix + 1;
        if introduces {
            // A FROM list: `from a x, b y`.
            while let Some((schema, name, used)) = table_name(&words[next..]) {
                next += used;
                let mut alias = None;
                if let Some(word) = words.get(next) {
                    let lower = word.to_lowercase();
                    if lower == "as" {
                        alias = words.get(next + 1).map(|w| unquote(w));
                        next += 2;
                    } else if is_identifier(word) && !STOP.contains(&lower.as_str()) {
                        alias = Some(unquote(word));
                        next += 1;
                    }
                }
                if catalog.table(schema.as_deref(), &name).is_some() {
                    out.push(TableRef {
                        schema,
                        name,
                        alias,
                    });
                }
                if words.get(next).map(String::as_str) == Some(",") {
                    next += 1;
                } else {
                    break;
                }
            }
        }
        ix = next;
    }
    out
}

/// `schema.table` or `table` at the start of `words`, and how many words it used.
fn table_name(words: &[String]) -> Option<(Option<String>, String, usize)> {
    let first = words.first().filter(|w| is_identifier(w))?;
    if words.get(1).map(String::as_str) == Some(".")
        && let Some(second) = words.get(2).filter(|w| is_identifier(w))
    {
        return Some((Some(unquote(first)), unquote(second), 3));
    }
    Some((None, unquote(first), 1))
}

/// Words that end a table reference instead of being its alias.
const STOP: &[&str] = &[
    "where",
    "join",
    "left",
    "right",
    "inner",
    "outer",
    "full",
    "cross",
    "natural",
    "on",
    "using",
    "group",
    "order",
    "limit",
    "offset",
    "having",
    "union",
    "set",
    "values",
    "select",
    "returning",
    "window",
    "for",
    "lateral",
    "default",
];

/// Identifiers, quoted identifiers, and single punctuation characters;
/// strings and comments are skipped.
fn words(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
        } else if c == '-' && chars.get(i + 1) == Some(&'-') {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
        } else if c == '\'' {
            i += 1;
            while i < chars.len() && chars[i] != '\'' {
                i += 1;
            }
            i += 1;
        } else if matches!(c, '"' | '`' | '[') {
            let close = if c == '[' { ']' } else { c };
            let start = i;
            i += 1;
            while i < chars.len() && chars[i] != close {
                i += 1;
            }
            i += 1;
            out.push(chars[start..i.min(chars.len())].iter().collect());
        } else if c.is_alphanumeric() || c == '_' || c == '$' {
            let start = i;
            while i < chars.len()
                && (chars[i].is_alphanumeric() || chars[i] == '_' || chars[i] == '$')
            {
                i += 1;
            }
            out.push(chars[start..i].iter().collect());
        } else {
            out.push(c.to_string());
            i += 1;
        }
    }
    out
}

fn is_identifier(word: &str) -> bool {
    word.starts_with(['"', '`', '['])
        || word
            .chars()
            .next()
            .is_some_and(|c| c.is_alphabetic() || c == '_')
}

fn unquote(word: &str) -> String {
    word.trim_matches(['"', '`', '[', ']']).to_string()
}

/// The identifier before a `.` that ends `text`, for `alias.col` / `schema.table`.
fn qualifier(text: &str) -> Option<String> {
    let text = text.strip_suffix('.')?;
    let word = words(text).pop()?;
    is_identifier(&word).then(|| unquote(&word))
}

/// The last keyword-ish word before the cursor, lowercased, skipping commas and
/// identifiers in a list so `from a, b|` still counts as after FROM.
fn previous_word(text: &str) -> Option<String> {
    let words = words(text);
    let last = words.last()?.to_lowercase();
    if last != "," {
        return Some(last);
    }
    // `from a x, |`: whatever introduced the list decides.
    let introducer = words
        .iter()
        .rev()
        .skip(1)
        .map(|w| w.to_lowercase())
        .find(|w| {
            matches!(w.as_str(), "from" | "join" | "update" | "into")
                || (KEYWORDS.contains(&w.as_str()) && w != "as")
        });
    Some(introducer.unwrap_or_else(|| ",".into()))
}

const KEYWORDS: &[&str] = &[
    "select",
    "from",
    "where",
    "and",
    "or",
    "not",
    "in",
    "is",
    "null",
    "like",
    "ilike",
    "between",
    "exists",
    "join",
    "left join",
    "right join",
    "inner join",
    "full join",
    "cross join",
    "on",
    "using",
    "group by",
    "order by",
    "having",
    "limit",
    "offset",
    "union",
    "union all",
    "distinct",
    "as",
    "case",
    "when",
    "then",
    "else",
    "end",
    "insert into",
    "values",
    "update",
    "set",
    "delete from",
    "returning",
    "create table",
    "create view",
    "create index",
    "drop table",
    "alter table",
    "add column",
    "primary key",
    "foreign key",
    "references",
    "default",
    "with",
    "asc",
    "desc",
    "nulls first",
    "nulls last",
    "true",
    "false",
    "explain",
    "begin",
    "commit",
    "rollback",
    "truncate",
];

const FUNCTIONS: &[&str] = &[
    "count",
    "sum",
    "avg",
    "min",
    "max",
    "coalesce",
    "nullif",
    "now",
    "current_date",
    "current_timestamp",
    "lower",
    "upper",
    "length",
    "substring",
    "trim",
    "cast",
    "round",
    "abs",
    "extract",
    "concat",
    "row_number",
    "rank",
    "dense_rank",
    "lag",
    "lead",
    "over",
    "partition by",
    "date_trunc",
    "string_agg",
    "array_agg",
    "json_agg",
    "group_concat",
    "ifnull",
    "isnull",
];

/// The editor side: turns [`complete`]'s suggestions into the completion menu's items.
pub struct SqlCompletions {
    /// The tab's connection's schema; replaced as it changes.
    pub catalog: Rc<RefCell<Option<Rc<Catalog>>>>,
}

impl CompletionProvider for SqlCompletions {
    fn completions(
        &self,
        text: &Rope,
        offset: usize,
        _trigger: CompletionContext,
        _: &mut Window,
        _: &mut App,
    ) -> Task<Result<CompletionResponse>> {
        let Some(catalog) = self.catalog.borrow().clone() else {
            return Task::ready(Ok(CompletionResponse::Array(Vec::new())));
        };
        let sql = text.to_string();
        let (start, suggestions) = complete(&sql, offset, &catalog);
        let range = Range::new(
            text.offset_to_position(start),
            text.offset_to_position(offset),
        );
        let items = suggestions
            .into_iter()
            .enumerate()
            .map(|(ix, suggestion)| CompletionItem {
                kind: Some(match suggestion.kind {
                    Kind::Column => CompletionItemKind::FIELD,
                    Kind::Table => CompletionItemKind::CLASS,
                    Kind::View => CompletionItemKind::INTERFACE,
                    Kind::Schema => CompletionItemKind::MODULE,
                    Kind::Function => CompletionItemKind::FUNCTION,
                    Kind::Keyword => CompletionItemKind::KEYWORD,
                }),
                detail: suggestion.detail,
                // Keep this order: it's already ranked.
                sort_text: Some(format!("{ix:04}")),
                filter_text: Some(suggestion.label.clone()),
                text_edit: Some(CompletionTextEdit::Edit(TextEdit::new(
                    range,
                    suggestion.insert,
                ))),
                label: suggestion.label,
                ..Default::default()
            })
            .collect();
        Task::ready(Ok(CompletionResponse::Array(items)))
    }

    fn is_completion_trigger(&self, _offset: usize, new_text: &str, _: &mut App) -> bool {
        new_text
            .chars()
            .last()
            .is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '.')
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use xenon_driver::{ObjectColumn, SchemaObject};

    fn column(name: &str, type_name: &str) -> ObjectColumn {
        ObjectColumn {
            name: name.into(),
            type_name: type_name.into(),
            primary_key: false,
            not_null: false,
        }
    }

    fn catalog() -> Catalog {
        let table = |name: &str, columns: Vec<ObjectColumn>| SchemaObject {
            name: name.into(),
            kind: ObjectKind::Table,
            columns,
        };
        Catalog {
            schemas: vec![
                Schema {
                    name: "public".into(),
                    objects: vec![
                        table(
                            "customers",
                            vec![
                                column("id", "int"),
                                column("email", "text"),
                                column("name", "text"),
                            ],
                        ),
                        table(
                            "orders",
                            vec![
                                column("id", "int"),
                                column("customer_id", "int"),
                                column("total", "numeric"),
                                column("status", "text"),
                            ],
                        ),
                        table("Order Items", vec![column("qty", "int")]),
                    ],
                },
                Schema {
                    name: "analytics".into(),
                    objects: vec![SchemaObject {
                        name: "daily_revenue".into(),
                        kind: ObjectKind::View,
                        columns: vec![column("day", "date"), column("revenue", "numeric")],
                    }],
                },
            ],
            default_schema: Some("public".into()),
            ident: Box::new(|name| {
                if xenon_driver::sql::is_plain_ident(name) {
                    name.to_string()
                } else {
                    format!("\"{name}\"")
                }
            }),
        }
    }

    /// Completions at the `|` in `sql`: (replace-from, labels).
    fn at(sql: &str) -> (usize, Vec<String>) {
        let offset = sql.find('|').unwrap();
        let sql = sql.replace('|', "");
        let (start, items) = complete(&sql, offset, &catalog());
        (start, items.into_iter().map(|s| s.label).collect())
    }

    fn inserts(sql: &str) -> Vec<String> {
        let offset = sql.find('|').unwrap();
        complete(&sql.replace('|', ""), offset, &catalog())
            .1
            .into_iter()
            .map(|s| s.insert)
            .collect()
    }

    #[test]
    fn tables_after_from_and_join() {
        let (start, labels) = at("select * from ord|");
        assert_eq!(start, 14);
        assert_eq!(labels[0], "orders");
        assert!(
            at("select * from orders o join cu|")
                .1
                .starts_with(&["customers".to_string()])
        );
        // Other schemas' tables come qualified; names that need quotes get them.
        assert!(
            at("select * from dai|")
                .1
                .contains(&"analytics.daily_revenue".to_string())
        );
        assert!(inserts("select * from orde|").contains(&"\"Order Items\"".to_string()));
        // A FROM list.
        assert_eq!(at("select * from orders, cust|").1[0], "customers");
    }

    #[test]
    fn columns_through_aliases() {
        let (_, labels) = at("select o.| from orders o");
        assert_eq!(labels, ["id", "customer_id", "total", "status"]);
        let (_, labels) =
            at("select c.em| from orders o join customers as c on c.id = o.customer_id");
        assert_eq!(labels, ["email"]);
        // A table name works as its own qualifier.
        assert_eq!(at("select orders.tot| from orders").1, ["total"]);
        // A schema qualifier lists its tables.
        assert_eq!(at("select * from analytics.|").1, ["daily_revenue"]);
    }

    #[test]
    fn columns_from_the_statement_come_first() {
        let (_, labels) = at("select st| from orders");
        assert_eq!(labels[0], "status");
        let (_, labels) = at("select * from orders where cust|");
        assert_eq!(labels[0], "customer_id");
        // Only the current statement's tables count.
        let (_, labels) = at("select email from customers; select em| from orders");
        assert!(!labels.contains(&"email".to_string()), "{labels:?}");
    }

    #[test]
    fn keywords_follow_the_typed_case() {
        assert!(at("sel|").1.contains(&"select".to_string()));
        assert!(at("SEL|").1.contains(&"SELECT".to_string()));
        assert!(
            at("select * from orders gro|")
                .1
                .contains(&"group by".to_string())
        );
    }

    #[test]
    fn ignores_strings_and_comments() {
        let refs = tables_in_scope(
            "select 'from customers' from orders -- join customers\n",
            &catalog(),
        );
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].name, "orders");
    }
}
