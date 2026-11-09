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
}

impl ObjectKind {
    /// Maps a `pg_class.relkind` character.
    pub fn from_relkind(relkind: &str) -> Option<Self> {
        Some(match relkind {
            "r" => Self::Table,
            "p" => Self::PartitionedTable,
            "f" => Self::ForeignTable,
            "v" => Self::View,
            "m" => Self::MaterializedView,
            "S" => Self::Sequence,
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
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaObject {
    pub name: String,
    pub kind: ObjectKind,
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

/// Relations in a schema (partitions excluded), sorted by name.
pub async fn objects(client: &Client, schema: &str) -> Result<Vec<SchemaObject>> {
    let rows = client
        .query(
            "select c.relname::text, c.relkind::text from pg_class c \
             join pg_namespace n on n.oid = c.relnamespace \
             where n.nspname = $1 and c.relkind in ('r','p','f','v','m','S') \
             and not c.relispartition order by 1",
            &[&schema],
        )
        .await?;
    Ok(rows
        .iter()
        .filter_map(|r| {
            let kind = ObjectKind::from_relkind(r.get::<_, &str>(1))?;
            Some(SchemaObject {
                name: r.get(0),
                kind,
            })
        })
        .collect())
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
        ] {
            assert_eq!(ObjectKind::from_relkind(c), Some(k));
            assert!(!k.label().is_empty());
        }
        assert_eq!(ObjectKind::from_relkind("i"), None);
    }
}
