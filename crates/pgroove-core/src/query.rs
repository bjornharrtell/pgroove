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
                    cur.rows.push(
                        (0..row.len())
                            .map(|i| row.get(i).map(str::to_string))
                            .collect(),
                    );
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
        .map_err(|e| anyhow::Error::msg(describe_error(&e)))?;
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
        let err = run(&client, "select * from nope", 3).await.unwrap_err();
        assert!(err.to_string().contains("nope"), "{err}");
        client.simple_query("select 1").await.unwrap();
        cancel(&client).await.unwrap();
    }
}
