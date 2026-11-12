//! Paged table data access for the data viewer.

use std::sync::Arc;

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

    /// The name as written in SQL a person reads: quotes only where needed.
    pub fn pretty(&self) -> String {
        format!(
            "{}.{}",
            pretty_ident(&self.schema),
            pretty_ident(&self.name)
        )
    }
}

/// Quotes an identifier, doubling embedded quotes.
pub fn quote_ident(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

/// Quotes an identifier only when it has to be, for generated SQL that a person
/// reads and edits. Execution paths keep using [`quote_ident`].
pub fn pretty_ident(s: &str) -> String {
    if crate::ident::needs_quotes(s) {
        quote_ident(s)
    } else {
        s.to_string()
    }
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
    /// Also fetch each row's physical location (`ctid`) and version (`xmin`), for tables
    /// without a primary key. They come back in `Page::row_ids`, not in the data.
    pub row_ids: bool,
}

/// Names of the hidden columns used to carry row ids.
const CTID_COLUMN: &str = "__pgroove_ctid";
const XMIN_COLUMN: &str = "__pgroove_xmin";

impl PageRequest {
    pub fn page(page: u64, size: u64) -> Self {
        Self {
            sort: None,
            filter: None,
            limit: size,
            offset: page * size,
            row_ids: false,
        }
    }
}

/// `select *` with the optional filter and sort, without paging.
fn base_select(
    table: &TableRef,
    sort: Option<&Sort>,
    filter: Option<&str>,
    row_ids: bool,
) -> String {
    let extra = if row_ids {
        format!(", ctid::text as \"{CTID_COLUMN}\", xmin::text as \"{XMIN_COLUMN}\"")
    } else {
        String::new()
    };
    let mut sql = format!("select *{extra} from {}", table.qualified());
    if let Some(f) = filter.map(str::trim).filter(|f| !f.is_empty()) {
        sql.push_str(&format!(" where {f}"));
    }
    if let Some(s) = sort {
        sql.push_str(&format!(
            " order by {} {}",
            quote_ident(&s.column),
            if s.descending { "desc" } else { "asc" }
        ));
    }
    sql
}

pub fn build_select(table: &TableRef, req: &PageRequest) -> String {
    format!(
        "{} limit {} offset {}",
        base_select(table, req.sort.as_ref(), req.filter.as_deref(), req.row_ids),
        req.limit,
        req.offset
    )
}

/// The query for exporting a whole table: the viewer's filter and sort, but no paging.
pub fn build_export_select(table: &TableRef, sort: Option<&Sort>, filter: Option<&str>) -> String {
    base_select(table, sort, filter, false)
}

/// Asks the server to cancel what this connection is running (for example an export).
pub async fn cancel_running(client: &Client) -> Result<()> {
    query::cancel(client).await
}

/// A fetched page; `has_more` is true when another page exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page {
    /// A shared handle, so the grid can show it without a copy.
    pub data: Arc<ResultSet>,
    pub has_more: bool,
    /// `(ctid, xmin)` per row, when requested with `PageRequest::row_ids`.
    pub row_ids: Vec<(String, String)>,
}

/// Fetches one page, requesting one extra row to detect a following page.
pub async fn fetch_page(client: &Client, table: &TableRef, req: &PageRequest) -> Result<Page> {
    let probe = PageRequest {
        limit: req.limit + 1,
        ..req.clone()
    };
    let out = query::run(client, &build_select(table, &probe), probe.limit as usize).await?;
    // Freshly produced and unshared, so this takes the allocation instead of copying it.
    let mut data = out
        .sets
        .into_iter()
        .next()
        .and_then(Arc::into_inner)
        .unwrap_or_default();
    let has_more = data.rows.len() as u64 > req.limit;
    data.rows.truncate(req.limit as usize);
    data.truncated = false;
    let mut row_ids = Vec::new();
    if req.row_ids {
        // The last two columns carry the row ids; take them out of the data.
        let keep = data.columns.len().saturating_sub(2);
        data.columns.truncate(keep);
        data.types.truncate(keep);
        for row in &mut data.rows {
            let xmin = row.pop().flatten().unwrap_or_default();
            let ctid = row.pop().flatten().unwrap_or_default();
            row_ids.push((ctid, xmin));
        }
    }
    data.count = data.rows.len() as u64;
    Ok(Page {
        data: Arc::new(data),
        has_more,
        row_ids,
    })
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
    fn pretty_quotes_only_when_needed() {
        assert_eq!(pretty_ident("orders"), "orders");
        assert_eq!(pretty_ident("Orders"), "\"Orders\"");
        assert_eq!(pretty_ident("user"), "\"user\"");
        assert_eq!(pretty_ident("a\"b"), "\"a\"\"b\"");
        assert_eq!(TableRef::new("public", "orders").pretty(), "public.orders");
        assert_eq!(TableRef::new("public", "T x").pretty(), "public.\"T x\"");
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
        let sort = Sort {
            column: "id".into(),
            descending: false,
        };
        assert_eq!(
            build_export_select(&t, Some(&sort), Some(" total > 5 ")),
            "select * from \"public\".\"orders\" where total > 5 order by \"id\" asc"
        );
        assert_eq!(
            build_export_select(&t, None, None),
            "select * from \"public\".\"orders\""
        );
    }

    #[test]
    fn select_with_row_ids() {
        let t = TableRef::new("public", "t");
        let mut r = PageRequest::page(0, 10);
        r.row_ids = true;
        assert_eq!(
            build_select(&t, &r),
            "select *, ctid::text as \"__pgroove_ctid\", xmin::text as \"__pgroove_xmin\" from \"public\".\"t\" limit 10 offset 0"
        );
        assert!(!build_export_select(&t, None, None).contains("ctid"));
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

    /// Sorting a table with huge values must not hold the full values in memory.
    #[tokio::test]
    async fn wide_rows_stay_bounded_when_sorted() {
        let Some((client, _)) = crate::connect::test_connection().await else {
            return;
        };
        client
            .batch_execute(
                "drop table if exists pg_temp.pgroove_w; \
                 create temp table pgroove_w as \
                   select g as id, repeat(md5(g::text), 20000) as big from generate_series(1, 60) g",
            )
            .await
            .unwrap();
        let t = TableRef::new("pg_temp", "pgroove_w");
        let mut req = PageRequest::page(0, 50);
        req.sort = Some(Sort {
            column: "big".into(),
            descending: true,
        });
        let page = fetch_page(&client, &t, &req).await.unwrap();
        assert_eq!(page.data.rows.len(), 50);
        assert!(page.has_more);
        let stored: usize = page
            .data
            .rows
            .iter()
            .flatten()
            .flatten()
            .map(String::len)
            .sum();
        // Each `big` value is 640 000 bytes on the server; only a bounded prefix is kept.
        assert!(
            stored < 50 * (query::MAX_CELL_BYTES + 128),
            "stored {stored} bytes"
        );
        assert!(
            page.data
                .rows
                .iter()
                .all(|r| query::is_clipped(r[1].as_deref().unwrap()))
        );
    }

    /// A whole-table export honours the viewer's filter and sort and is not limited to a page.
    #[tokio::test]
    async fn export_whole_table_live() {
        let Some((client, _)) = crate::connect::test_connection().await else {
            return;
        };
        client
            .batch_execute(
                "drop table if exists pg_temp.pgroove_e; \
                 create temp table pgroove_e as select g as id from generate_series(1, 3000) g",
            )
            .await
            .unwrap();
        let t = TableRef::new("pg_temp", "pgroove_e");
        let sort = Sort {
            column: "id".into(),
            descending: true,
        };
        let sql = build_export_select(&t, Some(&sort), Some("id > 1000"));
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("t.csv");
        query::export_csv(&client, &sql, &dest).await.unwrap();
        let text = std::fs::read_to_string(&dest).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(
            lines.len(),
            1 + 2000,
            "header plus every filtered row, not one page"
        );
        assert_eq!((lines[0], lines[1], lines[2000]), ("id", "3000", "1001"));
    }
}
