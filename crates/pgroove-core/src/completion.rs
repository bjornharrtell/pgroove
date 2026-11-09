//! Schema-aware SQL completion.

use anyhow::Result;
use tokio_postgres::Client;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableEntry {
    pub schema: String,
    pub name: String,
    pub columns: Vec<String>,
}

/// Tables and columns of the connected database.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Index {
    pub tables: Vec<TableEntry>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Keyword,
    Schema,
    Table,
    Column,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suggestion {
    pub label: String,
    pub kind: Kind,
    pub detail: Option<String>,
}

/// What to insert: replace the last `prefix_len` bytes before the cursor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Completion {
    pub prefix_len: usize,
    pub items: Vec<Suggestion>,
}

const KEYWORDS: &[&str] = &[
    "select",
    "from",
    "where",
    "group by",
    "order by",
    "having",
    "limit",
    "offset",
    "join",
    "left join",
    "right join",
    "inner join",
    "full join",
    "cross join",
    "on",
    "using",
    "as",
    "and",
    "or",
    "not",
    "in",
    "is null",
    "is not null",
    "like",
    "ilike",
    "between",
    "exists",
    "case",
    "when",
    "then",
    "else",
    "end",
    "distinct",
    "union",
    "union all",
    "intersect",
    "except",
    "with",
    "insert into",
    "values",
    "update",
    "set",
    "delete from",
    "returning",
    "create table",
    "alter table",
    "drop table",
    "create index",
    "explain",
    "analyze",
    "count",
    "sum",
    "avg",
    "min",
    "max",
    "coalesce",
    "asc",
    "desc",
    "true",
    "false",
    "null",
];

/// Words after which a relation name is expected.
const RELATION_CONTEXT: &[&str] = &["from", "join", "update", "into", "table", "truncate"];

pub async fn load(client: &Client) -> Result<Index> {
    let rows = client
        .query(
            "select n.nspname::text, c.relname::text, a.attname::text \
             from pg_class c \
             join pg_namespace n on n.oid = c.relnamespace \
             join pg_attribute a on a.attrelid = c.oid and a.attnum > 0 and not a.attisdropped \
             where c.relkind in ('r','p','v','m','f') \
               and n.nspname !~ '^pg_' and n.nspname <> 'information_schema' \
             order by 1, 2, a.attnum",
            &[],
        )
        .await?;
    let mut tables: Vec<TableEntry> = Vec::new();
    for r in rows {
        let (schema, name, col): (String, String, String) = (r.get(0), r.get(1), r.get(2));
        match tables.last_mut() {
            Some(t) if t.schema == schema && t.name == name => t.columns.push(col),
            _ => tables.push(TableEntry {
                schema,
                name,
                columns: vec![col],
            }),
        }
    }
    Ok(Index { tables })
}

fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// The statement containing `offset` (split on `;`).
pub fn current_statement(text: &str, offset: usize) -> &str {
    let offset = offset.min(text.len());
    let start = text[..offset].rfind(';').map_or(0, |i| i + 1);
    let end = text[offset..].find(';').map_or(text.len(), |i| offset + i);
    &text[start..end]
}

/// Words of the statement, lowercased; punctuation `.` and `,` are separate tokens.
fn tokens(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for c in s.chars() {
        if is_ident(c) || c == '"' {
            cur.push(c);
        } else {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur).replace('"', "").to_lowercase());
            }
            if c == '.' || c == ',' || c == '(' || c == ')' {
                out.push(c.to_string());
            }
        }
    }
    if !cur.is_empty() {
        out.push(cur.replace('"', "").to_lowercase());
    }
    out
}

/// Relations referenced in the statement as `(alias_or_name, table)`.
fn referenced(index: &Index, stmt: &str) -> Vec<(String, usize)> {
    let toks = tokens(stmt);
    let mut found = Vec::new();
    let mut i = 0;
    while i < toks.len() {
        if RELATION_CONTEXT.contains(&toks[i].as_str()) {
            let mut j = i + 1;
            while j < toks.len() {
                let (schema, name, next) = if toks.get(j + 1).map(String::as_str) == Some(".") {
                    (Some(toks[j].as_str()), toks.get(j + 2), j + 3)
                } else {
                    (None, toks.get(j), j + 1)
                };
                let Some(name) = name else { break };
                let table = index
                    .tables
                    .iter()
                    .position(|t| t.name == *name && schema.is_none_or(|s| t.schema == s));
                let Some(table) = table else { break };
                let mut alias = name.clone();
                let mut after = next;
                if toks.get(after).map(String::as_str) == Some("as") {
                    after += 1;
                }
                if let Some(a) = toks.get(after)
                    && a.chars().all(is_ident)
                    && !is_reserved(a)
                {
                    alias = a.clone();
                    after += 1;
                }
                found.push((alias, table));
                if toks.get(after).map(String::as_str) == Some(",") {
                    j = after + 1;
                } else {
                    break;
                }
            }
            i = j.max(i + 1);
        } else {
            i += 1;
        }
    }
    found
}

fn is_reserved(w: &str) -> bool {
    matches!(
        w,
        "where"
            | "join"
            | "left"
            | "right"
            | "inner"
            | "full"
            | "cross"
            | "on"
            | "using"
            | "group"
            | "order"
            | "limit"
            | "offset"
            | "having"
            | "union"
            | "set"
            | "select"
            | "returning"
            | "values"
            | "and"
            | "or"
    )
}

fn matches_prefix(label: &str, prefix: &str) -> bool {
    label.to_lowercase().starts_with(&prefix.to_lowercase())
}

/// Suggestions for the cursor at the end of `before` within `stmt` (the whole statement).
pub fn suggest(index: &Index, before: &str, stmt: &str) -> Completion {
    let prefix: String = before
        .chars()
        .rev()
        .take_while(|c| is_ident(*c))
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    let head = &before[..before.len() - prefix.len()];
    let mut items = Vec::new();
    let push = |items: &mut Vec<Suggestion>, label: &str, kind: Kind, detail: Option<String>| {
        if matches_prefix(label, &prefix) && !items.iter().any(|s: &Suggestion| s.label == label) {
            items.push(Suggestion {
                label: label.to_string(),
                kind,
                detail,
            });
        }
    };

    if let Some(stripped) = head.strip_suffix('.') {
        let stripped = stripped.trim_end_matches('"');
        let qualifier: String = stripped
            .chars()
            .rev()
            .take_while(|c| is_ident(*c))
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<String>()
            .to_lowercase();
        for t in index.tables.iter().filter(|t| t.schema == qualifier) {
            push(&mut items, &t.name, Kind::Table, Some(t.schema.clone()));
        }
        let mut tables: Vec<usize> = referenced(index, stmt)
            .into_iter()
            .filter(|(alias, _)| *alias == qualifier)
            .map(|(_, t)| t)
            .collect();
        if tables.is_empty() {
            tables = index
                .tables
                .iter()
                .enumerate()
                .filter(|(_, t)| t.name == qualifier)
                .map(|(i, _)| i)
                .collect();
        }
        for t in tables {
            let t = &index.tables[t];
            for c in &t.columns {
                push(&mut items, c, Kind::Column, Some(t.name.clone()));
            }
        }
        return Completion {
            prefix_len: prefix.len(),
            items,
        };
    }

    let last = tokens(head).pop().unwrap_or_default();
    if RELATION_CONTEXT.contains(&last.as_str()) {
        let mut schemas: Vec<&str> = index.tables.iter().map(|t| t.schema.as_str()).collect();
        schemas.dedup();
        for s in schemas {
            push(&mut items, s, Kind::Schema, None);
        }
        for t in &index.tables {
            push(&mut items, &t.name, Kind::Table, Some(t.schema.clone()));
        }
    } else {
        for (_, t) in referenced(index, stmt) {
            let t = &index.tables[t];
            for c in &t.columns {
                push(&mut items, c, Kind::Column, Some(t.name.clone()));
            }
        }
        for k in KEYWORDS {
            push(&mut items, k, Kind::Keyword, None);
        }
        for t in &index.tables {
            push(&mut items, &t.name, Kind::Table, Some(t.schema.clone()));
        }
    }
    items.truncate(60);
    Completion {
        prefix_len: prefix.len(),
        items,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn idx() -> Index {
        let t = |s: &str, n: &str, c: &[&str]| TableEntry {
            schema: s.into(),
            name: n.into(),
            columns: c.iter().map(|c| c.to_string()).collect(),
        };
        Index {
            tables: vec![
                t("audit", "log", &["id", "at"]),
                t("public", "customers", &["id", "name"]),
                t("public", "orders", &["id", "customer_id", "total"]),
            ],
        }
    }

    fn labels(c: &Completion) -> Vec<&str> {
        c.items.iter().map(|s| s.label.as_str()).collect()
    }

    fn at(sql: &str) -> Completion {
        suggest(&idx(), sql, sql)
    }

    #[test]
    fn statement_extraction() {
        assert_eq!(current_statement("a; b c; d", 5), " b c");
        assert_eq!(current_statement("a; b c; d", 0), "a");
        assert_eq!(current_statement("a; b c; d", 99), " d");
        assert_eq!(current_statement("x", 1), "x");
    }

    #[test]
    fn relations_after_from() {
        let c = at("select * from cu");
        assert_eq!(c.prefix_len, 2);
        assert_eq!(labels(&c), ["customers"]);
        let c = at("select * from ");
        assert!(labels(&c).contains(&"orders") && labels(&c).contains(&"audit"));
        assert!(!labels(&c).contains(&"select"));
        let c = at("update o");
        assert_eq!(labels(&c), ["orders"]);
    }

    #[test]
    fn schema_qualified_tables() {
        let c = at("select * from public.");
        assert_eq!(labels(&c), ["customers", "orders"]);
        let c = at("select * from audit.l");
        assert_eq!(labels(&c), ["log"]);
    }

    #[test]
    fn alias_columns() {
        let sql = "select o. from orders o join customers c on c.id = o.customer_id";
        let before = "select o.";
        let c = suggest(&idx(), before, sql);
        assert_eq!(labels(&c), ["id", "customer_id", "total"]);
        let c = suggest(
            &idx(),
            "select c.n",
            "select c.n from orders o, customers as c",
        );
        assert_eq!(labels(&c), ["name"]);
        // Table name directly, without alias.
        let c = suggest(&idx(), "select customers.", "select customers.");
        assert_eq!(labels(&c), ["id", "name"]);
        // Unknown qualifier yields nothing.
        assert!(suggest(&idx(), "select z.", "select z.").items.is_empty());
    }

    #[test]
    fn columns_keywords_and_tables_elsewhere() {
        let sql = "select to from orders";
        let c = suggest(&idx(), "select to", sql);
        assert_eq!(labels(&c), ["total"]);
        let c = suggest(&idx(), "select ", "select  from orders");
        assert!(labels(&c).starts_with(&["id", "customer_id", "total"]));
        assert!(labels(&c).contains(&"select"));
        let c = at("sel");
        assert!(labels(&c).contains(&"select"));
        assert_eq!(c.items[0].kind, Kind::Keyword);
        assert!(at("zzz").items.is_empty());
    }

    #[test]
    fn schema_suggestions_and_quoting() {
        let c = at("select * from pu");
        assert_eq!(labels(&c), ["public"]);
        assert_eq!(c.items[0].kind, Kind::Schema);
        let c = suggest(&idx(), "select \"o\".", "select \"o\". from orders \"o\"");
        assert_eq!(labels(&c), ["id", "customer_id", "total"]);
    }

    #[tokio::test]
    async fn load_live() {
        let Some((client, _)) = crate::connect::test_connection().await else {
            return;
        };
        client
            .batch_execute(
                "drop schema if exists pgroove_c cascade; create schema pgroove_c; \
                 create table pgroove_c.a (x int, y text); create view pgroove_c.v as select 1 as z",
            )
            .await
            .unwrap();
        let i = load(&client).await.unwrap();
        let a = i
            .tables
            .iter()
            .find(|t| t.name == "a" && t.schema == "pgroove_c")
            .unwrap();
        assert_eq!(a.columns, ["x", "y"]);
        assert!(i.tables.iter().any(|t| t.name == "v"));
        assert!(i.tables.iter().all(|t| t.schema != "information_schema"));
        client
            .batch_execute("drop schema pgroove_c cascade")
            .await
            .unwrap();
    }
}
