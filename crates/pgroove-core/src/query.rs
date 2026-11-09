//! Query execution via the simple query protocol (multi-statement, text values).

use std::time::{Duration, Instant};

use anyhow::Result;
use tokio_postgres::{Client, NoTls, SimpleQueryMessage, error::DbError};

/// Rows kept per result set; the rest are dropped and `truncated` is set.
pub const MAX_ROWS: usize = 10_000;

/// Outcome of one statement.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResultSet {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Option<String>>>,
    /// Rows returned or affected, as reported by the server.
    pub count: u64,
    pub truncated: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct QueryOutput {
    pub sets: Vec<ResultSet>,
    pub elapsed: Duration,
}

impl QueryOutput {
    /// The last statement that returned columns.
    pub fn last_rowset(&self) -> Option<&ResultSet> {
        self.sets.iter().rev().find(|s| !s.columns.is_empty())
    }

    /// One summary line per statement.
    pub fn messages(&self) -> Vec<String> {
        let mut lines: Vec<String> = self
            .sets
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let what = if s.columns.is_empty() {
                    "affected"
                } else {
                    "returned"
                };
                let cut = if s.truncated {
                    format!(" (showing first {})", s.rows.len())
                } else {
                    String::new()
                };
                format!("Statement {}: {} rows {what}{cut}", i + 1, s.count)
            })
            .collect();
        lines.push(format!("Finished in {} ms", self.elapsed.as_millis()));
        lines
    }
}

/// Longest cell value kept, in bytes. Longer values (huge geometries, documents, blobs) are
/// cut and marked, so one wide result can't exhaust memory.
pub const MAX_CELL_BYTES: usize = 64 * 1024;

const CLIP_MARK: &str = "…[truncated";

/// Cuts a value to `MAX_CELL_BYTES` (at a character boundary) and marks it with its full size.
pub fn clip_cell(v: &str) -> String {
    if v.len() <= MAX_CELL_BYTES {
        return v.to_string();
    }
    let mut end = MAX_CELL_BYTES;
    while !v.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{CLIP_MARK}: {} bytes]", &v[..end], v.len())
}

/// Whether a cell value was cut by [`clip_cell`].
pub fn is_clipped(v: &str) -> bool {
    v.rfind(CLIP_MARK)
        .and_then(|i| v[i + CLIP_MARK.len()..].strip_prefix(": "))
        .and_then(|rest| rest.strip_suffix(" bytes]"))
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

/// Folds simple-query messages into result sets.
pub fn collect(messages: Vec<SimpleQueryMessage>, max_rows: usize) -> Vec<ResultSet> {
    let mut sets = Vec::new();
    let mut cur = ResultSet::default();
    for m in messages {
        match m {
            SimpleQueryMessage::RowDescription(cols) => {
                cur.columns = cols.iter().map(|c| c.name().to_string()).collect();
            }
            SimpleQueryMessage::Row(row) => {
                if cur.columns.is_empty() {
                    cur.columns = row.columns().iter().map(|c| c.name().to_string()).collect();
                }
                if cur.rows.len() < max_rows {
                    cur.rows
                        .push((0..row.len()).map(|i| row.get(i).map(clip_cell)).collect());
                } else {
                    cur.truncated = true;
                }
            }
            SimpleQueryMessage::CommandComplete(n) => {
                cur.count = n;
                sets.push(std::mem::take(&mut cur));
            }
            _ => {}
        }
    }
    sets
}

/// A failed statement, keeping the server's error position for the editor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryError {
    pub message: String,
    /// 1-based character position within the executed SQL.
    pub position: Option<usize>,
}

impl std::fmt::Display for QueryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for QueryError {}

impl QueryError {
    pub fn from_driver(e: &tokio_postgres::Error) -> Self {
        let position = e.as_db_error().and_then(|d| match d.position() {
            Some(tokio_postgres::error::ErrorPosition::Original(p)) => Some(*p as usize),
            _ => None,
        });
        Self {
            message: describe_error(e),
            position,
        }
    }

    /// Byte range of the offending token in `sql`, for highlighting.
    pub fn span_in(&self, sql: &str) -> Option<std::ops::Range<usize>> {
        let start = sql.char_indices().nth(self.position?.checked_sub(1)?)?.0;
        let rest = &sql[start..];
        let len = rest
            .char_indices()
            .find(|(i, c)| *i > 0 && !(c.is_alphanumeric() || *c == '_'))
            .map_or(rest.len(), |(i, _)| i)
            .max(rest.chars().next().map_or(0, char::len_utf8));
        Some(start..start + len)
    }
}

/// Human-readable error text including server detail, hint and position.
pub fn describe_error(e: &tokio_postgres::Error) -> String {
    match e.as_db_error() {
        Some(db) => describe_db_error(db),
        None => e.to_string(),
    }
}

pub fn describe_db_error(db: &DbError) -> String {
    let mut s = db.message().to_string();
    if let Some(tokio_postgres::error::ErrorPosition::Original(p)) = db.position() {
        s.push_str(&format!(" (at character {p})"));
    }
    if let Some(d) = db.detail() {
        s.push_str(&format!("\nDetail: {d}"));
    }
    if let Some(h) = db.hint() {
        s.push_str(&format!("\nHint: {h}"));
    }
    s
}

/// Runs one or more statements.
pub async fn run(client: &Client, sql: &str, max_rows: usize) -> Result<QueryOutput> {
    let start = Instant::now();
    let messages = client
        .simple_query(sql)
        .await
        .map_err(|e| anyhow::Error::new(QueryError::from_driver(&e)))?;
    Ok(QueryOutput {
        sets: collect(messages, max_rows),
        elapsed: start.elapsed(),
    })
}

/// Asks the server to cancel the running query on this client.
pub async fn cancel(client: &Client) -> Result<()> {
    client.cancel_token().cancel_query(NoTls).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn out(sets: Vec<ResultSet>) -> QueryOutput {
        QueryOutput {
            sets,
            elapsed: Duration::from_millis(7),
        }
    }

    #[test]
    fn last_rowset_and_messages() {
        let o = out(vec![
            ResultSet {
                columns: vec!["a".into()],
                rows: vec![vec![Some("1".into())]],
                count: 1,
                truncated: false,
            },
            ResultSet {
                count: 3,
                ..Default::default()
            },
            ResultSet {
                columns: vec!["b".into()],
                rows: vec![vec![None]; 2],
                count: 5,
                truncated: true,
            },
        ]);
        assert_eq!(o.last_rowset().unwrap().columns, ["b"]);
        assert_eq!(
            o.messages(),
            [
                "Statement 1: 1 rows returned",
                "Statement 2: 3 rows affected",
                "Statement 3: 5 rows returned (showing first 2)",
                "Finished in 7 ms",
            ]
        );
        assert!(out(vec![]).last_rowset().is_none());
    }

    #[test]
    fn error_span() {
        let e = QueryError {
            message: "m".into(),
            position: Some(8),
        };
        assert_eq!(e.span_in("select foo_bar from t"), Some(7..14));
        let at_end = QueryError {
            position: Some(6),
            ..e.clone()
        };
        assert_eq!(at_end.span_in("select"), Some(5..6));
        let multi = QueryError {
            position: Some(3),
            ..e.clone()
        };
        assert_eq!(
            multi.span_in("åäö x"),
            Some(4..6),
            "positions count characters"
        );
        assert_eq!(
            QueryError {
                position: None,
                ..e.clone()
            }
            .span_in("x"),
            None
        );
        assert_eq!(
            QueryError {
                position: Some(99),
                ..e.clone()
            }
            .span_in("x"),
            None
        );
        assert_eq!(
            QueryError {
                position: Some(0),
                ..e.clone()
            }
            .span_in("x"),
            None
        );
        assert_eq!(e.to_string(), "m");
    }

    #[test]
    fn long_cells_are_clipped_and_marked() {
        assert_eq!(clip_cell("short"), "short");
        let exact = "a".repeat(MAX_CELL_BYTES);
        assert_eq!(clip_cell(&exact), exact);
        let long = "b".repeat(MAX_CELL_BYTES + 10);
        let clipped = clip_cell(&long);
        assert!(clipped.starts_with(&"b".repeat(MAX_CELL_BYTES)));
        assert!(clipped.ends_with("…[truncated: 65546 bytes]"));
        assert!(is_clipped(&clipped));
        assert!(!is_clipped(&long));
        assert!(!is_clipped("…[truncated: soon bytes]"));
        assert!(!is_clipped("…[truncated: bytes]"));
        assert!(!is_clipped("plain"));
        // Multi-byte characters are never split.
        let wide = "é".repeat(MAX_CELL_BYTES);
        let clipped = clip_cell(&wide);
        assert!(clipped.len() < wide.len());
        assert!(is_clipped(&clipped));
        assert!(clipped.chars().take_while(|c| *c == 'é').count() * 2 <= MAX_CELL_BYTES);
    }

    #[test]
    fn collect_empty() {
        assert!(collect(vec![], 10).is_empty());
        assert!(collect(vec![SimpleQueryMessage::CommandComplete(2)], 10)[0].count == 2);
    }

    /// Needs `PGROOVE_TEST_PG` (see `connect` tests).
    #[tokio::test]
    async fn run_live() {
        let Some((client, _)) = crate::connect::test_connection().await else {
            return;
        };
        let o = run(
            &client,
            "select 1 as a, null as b; create temp table t (x int); select generate_series(1,5) as n",
            3,
        )
        .await
        .unwrap();
        assert_eq!(o.sets.len(), 3);
        assert_eq!(o.sets[0].rows, [[Some("1".to_string()), None]]);
        let last = o.last_rowset().unwrap();
        assert_eq!((last.rows.len(), last.count, last.truncated), (3, 5, true));
        let big = run(&client, "select repeat('é', 200000) as v", 3)
            .await
            .unwrap();
        let cell = big.sets[0].rows[0][0].as_deref().unwrap();
        assert!(
            is_clipped(cell) && cell.len() < MAX_CELL_BYTES + 64,
            "{}",
            cell.len()
        );
        let err = run(&client, "select * from nope", 3).await.unwrap_err();
        assert!(err.to_string().contains("nope"), "{err}");
        let qe = err.downcast_ref::<QueryError>().expect("structured error");
        assert_eq!(qe.position, Some(15));
        assert_eq!(qe.span_in("select * from nope"), Some(14..18));
        client.simple_query("select 1").await.unwrap();
        cancel(&client).await.unwrap();
    }
}
