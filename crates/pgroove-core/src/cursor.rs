//! Server-side cursors behind the result grid: a query is opened once and its rows are fetched
//! in batches as the user scrolls, so a result is no longer cut off at a row limit.
//!
//! A cursor lives inside a read-only transaction, so it gets a connection of its own: the
//! session's shared connection stays free for other tabs and is never left inside a
//! transaction. Dropping the cursor closes that connection, which ends the transaction.

use anyhow::Result;
use tokio_postgres::Client;

use crate::query::{self, QueryError, ResultSet};
use crate::{ConnectionProfile, connect::connect, sqlsplit};

const CURSOR: &str = "_pgroove_c";

/// Rows a grid keeps loading before it stops and points to Export. The rows are held in
/// memory as text, so the scroll is bounded rather than endless.
pub const MAX_LOADED_ROWS: usize = 100_000;

/// Cancels the fetch running on a cursor's connection.
pub type CancelToken = tokio_postgres::CancelToken;

pub async fn cancel(token: &CancelToken) -> Result<()> {
    token.cancel_query(tokio_postgres::NoTls).await?;
    Ok(())
}

pub struct RowCursor {
    client: Client,
    columns: Vec<String>,
    types: Vec<String>,
    loaded: usize,
    done: bool,
}

impl RowCursor {
    /// Opens `sql` as a cursor and fetches its first `first` rows.
    ///
    /// `Ok(None)` means this statement can't be read through a cursor (it isn't a plain
    /// query, or the server refuses to declare one for it); the caller runs it the ordinary
    /// way, which also reports any error in the SQL with its position.
    pub async fn open(
        profile: &ConnectionProfile,
        password: Option<&str>,
        sql: &str,
        first: usize,
    ) -> Result<Option<(RowCursor, ResultSet)>> {
        let text = sql.trim().trim_end_matches(';').trim_end();
        if !sqlsplit::is_select_like(text) {
            return Ok(None);
        }
        let (client, _) = connect(profile, password).await?;
        client.batch_execute("begin read only").await?;
        let declare = format!("declare {CURSOR} no scroll cursor for {text}");
        if client.batch_execute(&declare).await.is_err() {
            return Ok(None);
        }
        // Types come from describing the statement; a failure just leaves them out.
        let types = match client.prepare(text).await {
            Ok(stmt) => stmt
                .columns()
                .iter()
                .map(|c| c.type_().name().to_string())
                .collect(),
            Err(_) => Vec::new(),
        };
        let mut cursor = RowCursor {
            client,
            columns: Vec::new(),
            types,
            loaded: 0,
            done: false,
        };
        let set = cursor.fetch(first).await?;
        Ok(Some((cursor, set)))
    }

    /// The next `n` rows (fewer, and `is_done`, at the end of the result).
    pub async fn fetch(&mut self, n: usize) -> Result<ResultSet> {
        let n = n.max(1);
        let messages = self
            .client
            .simple_query(&format!("fetch forward {n} from {CURSOR}"))
            .await
            .map_err(|e| anyhow::Error::new(QueryError::from_driver(&e)))?;
        let mut set = query::collect(messages, n).pop().unwrap_or_default();
        if self.columns.is_empty() {
            self.columns = set.columns.clone();
        }
        set.columns = self.columns.clone();
        if self.types.len() == set.columns.len() {
            set.types = self.types.clone();
        }
        query::decode_geometries(&mut set);
        self.loaded += set.rows.len();
        self.done = set.rows.len() < n;
        set.truncated = !self.done;
        Ok(set)
    }

    /// The whole result has been fetched.
    pub fn is_done(&self) -> bool {
        self.done
    }

    /// Rows fetched so far.
    pub fn loaded(&self) -> usize {
        self.loaded
    }

    /// A handle that cancels the fetch running on the cursor's own connection.
    pub fn cancel_token(&self) -> CancelToken {
        self.client.cancel_token()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn live() -> Option<(ConnectionProfile, Option<String>)> {
        let spec = std::env::var("PGROOVE_TEST_PG").ok()?;
        let f: Vec<&str> = spec.splitn(5, ':').collect();
        let profile = ConnectionProfile {
            name: "live".into(),
            host: f[0].into(),
            port: f[1].parse().ok()?,
            user: f[2].into(),
            database: f[4].into(),
            ssl: crate::SslMode::Prefer,
            ..Default::default()
        };
        Some((profile, (!f[3].is_empty()).then(|| f[3].to_string())))
    }

    #[tokio::test]
    async fn fetches_in_batches_until_done() {
        let Some((profile, pw)) = live() else { return };
        let (mut cur, first) = RowCursor::open(
            &profile,
            pw.as_deref(),
            "select g as n, 'row ' || g as label from generate_series(1, 25) g;",
            10,
        )
        .await
        .unwrap()
        .expect("a select can be a cursor");
        assert_eq!(first.columns, ["n", "label"]);
        assert_eq!(first.types, ["int4", "text"]);
        assert_eq!(first.rows.len(), 10);
        assert!(first.truncated && !cur.is_done());
        let second = cur.fetch(10).await.unwrap();
        assert_eq!(second.rows[0][0].as_deref(), Some("11"));
        let last = cur.fetch(10).await.unwrap();
        assert_eq!((last.rows.len(), last.truncated), (5, false));
        assert!(cur.is_done());
        assert_eq!(cur.loaded(), 25);
        // Nothing is left; further fetches are empty.
        assert!(cur.fetch(10).await.unwrap().rows.is_empty());
    }

    #[tokio::test]
    async fn empty_results_still_have_columns() {
        let Some((profile, pw)) = live() else { return };
        let (cur, set) = RowCursor::open(&profile, pw.as_deref(), "select 1 as a where false", 5)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            (set.columns.clone(), set.rows.len()),
            (vec!["a".to_string()], 0)
        );
        assert!(cur.is_done());
    }

    #[tokio::test]
    async fn statements_that_cannot_be_cursors_fall_back() {
        let Some((profile, pw)) = live() else { return };
        for sql in [
            "create temp table t_nocursor (a int)",
            "select * from no_such_table_here",
            "with x as (insert into pg_temp.nothing select 1 returning 1) select * from x",
        ] {
            let opened = RowCursor::open(&profile, pw.as_deref(), sql, 5)
                .await
                .unwrap();
            assert!(opened.is_none(), "{sql}");
        }
    }

    #[test]
    fn non_queries_are_declined_without_connecting() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let profile = ConnectionProfile {
            host: "127.0.0.1".into(),
            port: 1, // nothing listens: reaching the network would be an error
            ..Default::default()
        };
        let opened = rt.block_on(RowCursor::open(&profile, None, "update t set a = 1", 5));
        assert!(matches!(opened, Ok(None)));
    }
}
