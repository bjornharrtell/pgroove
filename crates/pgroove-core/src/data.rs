//! Paged table data access for the data viewer.

use anyhow::Result;
use tokio_postgres::Client;

use crate::query::{self, ResultSet};

/// A schema-qualified relation.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TableRef {
    pub schema: String,
    pub name: String,
}

impl TableRef {
    pub fn new(schema: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            schema: schema.into(),
            name: name.into(),
        }
    }

    pub fn qualified(&self) -> String {
        format!("{}.{}", quote_ident(&self.schema), quote_ident(&self.name))
    }
}

/// Quotes an identifier, doubling embedded quotes.
pub fn quote_ident(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sort {
    pub column: String,
    pub descending: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageRequest {
    pub sort: Option<Sort>,
    /// Raw SQL boolean expression inserted after `where`.
    pub filter: Option<String>,
    pub limit: u64,
    pub offset: u64,
}

impl PageRequest {
    pub fn page(page: u64, size: u64) -> Self {
        Self {
            sort: None,
            filter: None,
            limit: size,
            offset: page * size,
        }
    }
}

pub fn build_select(table: &TableRef, req: &PageRequest) -> String {
    let mut sql = format!("select * from {}", table.qualified());
    if let Some(f) = req
        .filter
        .as_deref()
        .map(str::trim)
        .filter(|f| !f.is_empty())
    {
        sql.push_str(&format!(" where {f}"));
    }
    if let Some(s) = &req.sort {
        sql.push_str(&format!(
            " order by {} {}",
            quote_ident(&s.column),
            if s.descending { "desc" } else { "asc" }
        ));
    }
    sql.push_str(&format!(" limit {} offset {}", req.limit, req.offset));
    sql
}

/// A fetched page; `has_more` is true when another page exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page {
    pub data: ResultSet,
    pub has_more: bool,
}

/// Fetches one page, requesting one extra row to detect a following page.
pub async fn fetch_page(client: &Client, table: &TableRef, req: &PageRequest) -> Result<Page> {
    let probe = PageRequest {
        limit: req.limit + 1,
        ..req.clone()
    };
    let out = query::run(client, &build_select(table, &probe), probe.limit as usize).await?;
    let mut data = out.sets.into_iter().next().unwrap_or_default();
    let has_more = data.rows.len() as u64 > req.limit;
    data.rows.truncate(req.limit as usize);
    data.truncated = false;
    data.count = data.rows.len() as u64;
    Ok(Page { data, has_more })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoting() {
        assert_eq!(quote_ident("a\"b"), "\"a\"\"b\"");
        assert_eq!(
            TableRef::new("public", "T x").qualified(),
            "\"public\".\"T x\""
        );
    }

    #[test]
    fn select_building() {
        let t = TableRef::new("public", "orders");
        let mut r = PageRequest::page(2, 100);
        assert_eq!(
            build_select(&t, &r),
            "select * from \"public\".\"orders\" limit 100 offset 200"
        );
        r.filter = Some(" total > 5 ".into());
        r.sort = Some(Sort {
            column: "id".into(),
            descending: true,
        });
        assert_eq!(
            build_select(&t, &r),
            "select * from \"public\".\"orders\" where total > 5 order by \"id\" desc limit 100 offset 200"
        );
        r.filter = Some("  ".into());
        assert!(!build_select(&t, &r).contains("where"));
    }

    #[tokio::test]
    async fn fetch_live() {
        let Some((client, _)) = crate::connect::test_connection().await else {
            return;
        };
        client
            .batch_execute(
                "drop table if exists pg_temp.pgroove_d; \
                 create temp table pgroove_d as select g as id from generate_series(1,5) g",
            )
            .await
            .unwrap();
        let t = TableRef::new("pg_temp", "pgroove_d");
        let mut r = PageRequest::page(0, 3);
        r.sort = Some(Sort {
            column: "id".into(),
            descending: true,
        });
        let p = fetch_page(&client, &t, &r).await.unwrap();
        assert_eq!(p.data.columns, ["id"]);
        assert_eq!(p.data.rows.len(), 3);
        assert_eq!(p.data.rows[0][0].as_deref(), Some("5"));
        assert!(p.has_more);
        let p2 = fetch_page(&client, &t, &PageRequest::page(1, 3))
            .await
            .unwrap();
        assert_eq!(p2.data.rows.len(), 2);
        assert!(!p2.has_more);
        r.filter = Some("id > 100".into());
        let p3 = fetch_page(&client, &t, &r).await.unwrap();
        assert!(p3.data.rows.is_empty());
        assert_eq!(p3.data.columns, ["id"]);
        assert!(
            fetch_page(
                &client,
                &t,
                &PageRequest {
                    filter: Some("bogus(".into()),
                    ..r
                }
            )
            .await
            .is_err()
        );
    }
}
