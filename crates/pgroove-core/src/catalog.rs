//! Catalog queries for the schema browser.

use anyhow::Result;
use tokio_postgres::Client;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectKind {
    Table,
    PartitionedTable,
    ForeignTable,
    View,
    MaterializedView,
    Sequence,
    Function,
    Procedure,
    Aggregate,
    EnumType,
    CompositeType,
    DomainType,
}

impl ObjectKind {
    /// Maps a `pg_class.relkind` character, plus the extra codes `objects` uses for
    /// functions (`F`), procedures (`P`), aggregates (`A`) and types (`E`, `C`, `D`).
    pub fn from_relkind(relkind: &str) -> Option<Self> {
        Some(match relkind {
            "r" => Self::Table,
            "p" => Self::PartitionedTable,
            "f" => Self::ForeignTable,
            "v" => Self::View,
            "m" => Self::MaterializedView,
            "S" => Self::Sequence,
            "F" => Self::Function,
            "P" => Self::Procedure,
            "A" => Self::Aggregate,
            "E" => Self::EnumType,
            "C" => Self::CompositeType,
            "D" => Self::DomainType,
            _ => return None,
        })
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Table => "table",
            Self::PartitionedTable => "partitioned table",
            Self::ForeignTable => "foreign table",
            Self::View => "view",
            Self::MaterializedView => "materialized view",
            Self::Sequence => "sequence",
            Self::Function => "function",
            Self::Procedure => "procedure",
            Self::Aggregate => "aggregate",
            Self::EnumType => "enum type",
            Self::CompositeType => "composite type",
            Self::DomainType => "domain",
        }
    }

    /// Whether the object holds rows that can be browsed.
    pub fn is_relation(self) -> bool {
        matches!(
            self,
            Self::Table
                | Self::PartitionedTable
                | Self::ForeignTable
                | Self::View
                | Self::MaterializedView
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaObject {
    pub name: String,
    pub kind: ObjectKind,
}

/// Databases that accept connections (templates excluded), sorted by name.
pub async fn databases(client: &Client) -> Result<Vec<String>> {
    let rows = client
        .query(
            "select datname::text from pg_database \
             where datallowconn and not datistemplate order by 1",
            &[],
        )
        .await?;
    Ok(rows.iter().map(|r| r.get(0)).collect())
}

/// User-visible schemas (system schemas excluded), sorted by name.
pub async fn schemas(client: &Client) -> Result<Vec<String>> {
    let rows = client
        .query(
            "select nspname::text from pg_namespace \
             where nspname !~ '^pg_' and nspname <> 'information_schema' \
             order by 1",
            &[],
        )
        .await?;
    Ok(rows.iter().map(|r| r.get(0)).collect())
}

/// SQL listing objects as `(schema, name, kind)`; `schema_cond` restricts the schemas
/// and may use `n.nspname` and the parameter `$1`.
fn objects_sql(schema_cond: &str) -> String {
    format!(
        "select nsp, name, kind from ( \
           select n.nspname::text as nsp, c.relname::text as name, c.relkind::text as kind, 0 as grp \
           from pg_class c join pg_namespace n on n.oid = c.relnamespace \
           where {schema_cond} and c.relkind in ('r','p','f','v','m','S') \
             and not c.relispartition \
           union all \
           select n.nspname::text, t.typname::text, \
                  case t.typtype when 'e' then 'E' when 'd' then 'D' else 'C' end, 1 \
           from pg_type t join pg_namespace n on n.oid = t.typnamespace \
           where {schema_cond} \
             and (t.typtype in ('e','d') or (t.typtype = 'c' and exists \
                  (select 1 from pg_class c where c.oid = t.typrelid and c.relkind = 'c'))) \
           union all \
           select n.nspname::text, \
                  p.proname::text || '(' || pg_get_function_identity_arguments(p.oid) || ')', \
                  case p.prokind when 'p' then 'P' when 'a' then 'A' else 'F' end, 2 \
           from pg_proc p join pg_namespace n on n.oid = p.pronamespace \
           where {schema_cond} and p.prokind in ('f','p','a') \
             and not exists (select 1 from pg_depend d where d.objid = p.oid and d.deptype = 'e') \
         ) o order by nsp, grp, name"
    )
}

fn to_objects(rows: &[tokio_postgres::Row]) -> Vec<(String, SchemaObject)> {
    rows.iter()
        .filter_map(|r| {
            let kind = ObjectKind::from_relkind(r.get::<_, &str>(2))?;
            Some((
                r.get::<_, String>(0),
                SchemaObject {
                    name: r.get(1),
                    kind,
                },
            ))
        })
        .collect()
}

/// Objects in a schema: relations (partitions excluded), then types, then
/// functions, each group sorted by name.
pub async fn objects(client: &Client, schema: &str) -> Result<Vec<SchemaObject>> {
    let rows = client
        .query(&objects_sql("n.nspname = $1"), &[&schema])
        .await?;
    Ok(to_objects(&rows).into_iter().map(|(_, o)| o).collect())
}

/// Every object of every user schema as `(schema, object)`, for searching.
pub async fn all_objects(client: &Client) -> Result<Vec<(String, SchemaObject)>> {
    let rows = client
        .query(
            &objects_sql("n.nspname !~ '^pg_' and n.nspname <> 'information_schema'"),
            &[],
        )
        .await?;
    Ok(to_objects(&rows))
}

/// Installed extensions as `(name, version)`.
pub async fn extensions(client: &Client) -> Result<Vec<(String, String)>> {
    let rows = client
        .query(
            "select extname::text, extversion from pg_extension order by 1",
            &[],
        )
        .await?;
    Ok(rows.iter().map(|r| (r.get(0), r.get(1))).collect())
}

/// Source text for a function, procedure or enum type; `name` is the entry
/// returned by `objects` (functions include their argument list).
pub async fn definition(
    client: &Client,
    schema: &str,
    name: &str,
    kind: ObjectKind,
) -> Result<String> {
    match kind {
        ObjectKind::Function | ObjectKind::Procedure => {
            let row = client
                .query_opt(
                    "select pg_get_functiondef(p.oid) from pg_proc p \
                     join pg_namespace n on n.oid = p.pronamespace \
                     where n.nspname = $1 \
                       and p.proname || '(' || pg_get_function_identity_arguments(p.oid) || ')' = $2",
                    &[&schema, &name],
                )
                .await?
                .ok_or_else(|| anyhow::anyhow!("{name} no longer exists"))?;
            Ok(format!("{};\n", row.get::<_, String>(0).trim_end()))
        }
        ObjectKind::EnumType => {
            let rows = client
                .query(
                    "select e.enumlabel::text from pg_enum e \
                     join pg_type t on t.oid = e.enumtypid \
                     join pg_namespace n on n.oid = t.typnamespace \
                     where n.nspname = $1 and t.typname = $2 order by e.enumsortorder",
                    &[&schema, &name],
                )
                .await?;
            if rows.is_empty() {
                anyhow::bail!("{name} no longer exists");
            }
            let labels: Vec<String> = rows
                .iter()
                .map(|r| format!("'{}'", r.get::<_, String>(0).replace('\'', "''")))
                .collect();
            Ok(format!(
                "create type {}.{} as enum ({});\n",
                crate::data::quote_ident(schema),
                crate::data::quote_ident(name),
                labels.join(", ")
            ))
        }
        _ => anyhow::bail!("No definition is available for a {}", kind.label()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relkind_mapping() {
        for (c, k) in [
            ("r", ObjectKind::Table),
            ("p", ObjectKind::PartitionedTable),
            ("f", ObjectKind::ForeignTable),
            ("v", ObjectKind::View),
            ("m", ObjectKind::MaterializedView),
            ("S", ObjectKind::Sequence),
            ("F", ObjectKind::Function),
            ("P", ObjectKind::Procedure),
            ("A", ObjectKind::Aggregate),
            ("E", ObjectKind::EnumType),
            ("C", ObjectKind::CompositeType),
            ("D", ObjectKind::DomainType),
        ] {
            assert_eq!(ObjectKind::from_relkind(c), Some(k));
            assert!(!k.label().is_empty());
        }
        assert_eq!(ObjectKind::from_relkind("i"), None);
        assert!(ObjectKind::View.is_relation());
        assert!(!ObjectKind::Sequence.is_relation());
        assert!(!ObjectKind::Function.is_relation());
    }

    #[tokio::test]
    async fn schema_objects_live() {
        let Some((client, _)) = crate::connect::test_connection().await else {
            return;
        };
        client
            .batch_execute(
                "drop schema if exists pgroove_k cascade; create schema pgroove_k; \
                 create table pgroove_k.t (id int); \
                 create type pgroove_k.mood as enum ('sad', 'it''s ok'); \
                 create type pgroove_k.pair as (a int, b int); \
                 create domain pgroove_k.posint as int check (value > 0); \
                 create function pgroove_k.add(a int, b int) returns int \
                   language sql as 'select a + b'; \
                 create procedure pgroove_k.noop() language sql as ''; \
                 create aggregate pgroove_k.mysum(int) (sfunc = int4pl, stype = int, initcond = '0')",
            )
            .await
            .unwrap();
        let objs = objects(&client, "pgroove_k").await.unwrap();
        let got: Vec<_> = objs.iter().map(|o| (o.name.as_str(), o.kind)).collect();
        assert_eq!(
            got,
            [
                ("t", ObjectKind::Table),
                ("mood", ObjectKind::EnumType),
                ("pair", ObjectKind::CompositeType),
                ("posint", ObjectKind::DomainType),
                ("add(a integer, b integer)", ObjectKind::Function),
                ("mysum(integer)", ObjectKind::Aggregate),
                ("noop()", ObjectKind::Procedure),
            ]
        );
        let f = definition(
            &client,
            "pgroove_k",
            "add(a integer, b integer)",
            ObjectKind::Function,
        )
        .await
        .unwrap();
        assert!(f.contains("CREATE OR REPLACE FUNCTION pgroove_k.add") && f.ends_with(";\n"));
        let e = definition(&client, "pgroove_k", "mood", ObjectKind::EnumType)
            .await
            .unwrap();
        assert_eq!(
            e,
            "create type \"pgroove_k\".\"mood\" as enum ('sad', 'it''s ok');\n"
        );
        assert!(
            definition(&client, "pgroove_k", "nope()", ObjectKind::Function)
                .await
                .is_err()
        );
        assert!(
            definition(&client, "pgroove_k", "nope", ObjectKind::EnumType)
                .await
                .is_err()
        );
        assert!(
            definition(&client, "pgroove_k", "t", ObjectKind::Table)
                .await
                .is_err()
        );
        assert!(extensions(&client).await.is_ok());
        let all = all_objects(&client).await.unwrap();
        assert!(
            all.iter().any(|(s, o)| s == "pgroove_k"
                && o.name == "mood"
                && o.kind == ObjectKind::EnumType)
        );
        assert!(
            all.iter()
                .all(|(s, _)| s != "information_schema" && !s.starts_with("pg_"))
        );
        let dbs = databases(&client).await.unwrap();
        assert!(dbs.contains(&"postgres".to_string()));
        assert!(!dbs.iter().any(|d| d.starts_with("template")));
        client
            .batch_execute("drop schema pgroove_k cascade")
            .await
            .unwrap();
    }
}
