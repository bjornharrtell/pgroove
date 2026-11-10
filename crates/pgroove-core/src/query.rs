//! Query execution via the simple query protocol (multi-statement, text values).

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use tokio_postgres::{Client, NoTls, SimpleQueryMessage, error::DbError};

use crate::sqlsplit;

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
    /// Shared handles: a view can keep one allocation and hold a cheap reference to it.
    pub sets: Vec<Arc<ResultSet>>,
    pub elapsed: Duration,
    /// The row limit that applied to this run, if it was a limited run.
    pub limit: Option<usize>,
}

impl QueryOutput {
    /// Index of the last statement that returned columns.
    fn last_rowset_ix(&self) -> Option<usize> {
        self.sets.iter().rposition(|s| !s.columns.is_empty())
    }

    /// The last statement that returned columns.
    pub fn last_rowset(&self) -> Option<&ResultSet> {
        self.sets.get(self.last_rowset_ix()?).map(|s| &**s)
    }

    /// The same set as a shared handle, for a view that keeps it.
    pub fn last_rowset_shared(&self) -> Option<Arc<ResultSet>> {
        self.sets.get(self.last_rowset_ix()?).cloned()
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
                let cut = match (s.truncated, self.limit) {
                    (true, Some(l)) => {
                        format!(" (limit of {l} reached: more rows exist, use Export for all)")
                    }
                    (true, None) => format!(" (showing first {})", s.rows.len()),
                    _ => String::new(),
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

/// Wraps produced sets once, so they can be handed out without copying.
fn shared(sets: Vec<ResultSet>) -> Vec<Arc<ResultSet>> {
    sets.into_iter().map(Arc::new).collect()
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

/// Runs one or more statements. A single query (`select`, `with`, `values`, `table`) is fetched
/// with a `LIMIT` so the server stops after `max_rows + 1` rows; `truncated` means more exist.
/// Anything else runs as written.
pub async fn run(client: &Client, sql: &str, max_rows: usize) -> Result<QueryOutput> {
    let start = Instant::now();
    let statements = sqlsplit::split_statements(sql);
    let sets = match statements.as_slice() {
        [one] if sqlsplit::is_select_like(&one.text) => {
            let char_start = sql[..one.start].chars().count();
            run_one(client, &one.text, max_rows, char_start).await?
        }
        _ => {
            let messages = client
                .simple_query(sql)
                .await
                .map_err(|e| anyhow::Error::new(QueryError::from_driver(&e)))?;
            collect(messages, max_rows)
        }
    };
    Ok(QueryOutput {
        sets: shared(sets),
        elapsed: start.elapsed(),
        limit: None,
    })
}

/// Characters of the wrapper that precede the user's statement.
const WRAP_PREFIX: &str = "select * from (";

/// Wraps a query, asking for one row past `limit` so an extra row reveals more.
fn wrapped(text: &str, limit: usize) -> String {
    format!("{WRAP_PREFIX}{text}\n) as _pgroove_q limit {}", limit + 1)
}

fn shift_position(e: QueryError, remove: usize, add: usize) -> QueryError {
    QueryError {
        position: e.position.map(|p| p.saturating_sub(remove).max(1) + add),
        ..e
    }
}

/// Wrapping fails for a few statement shapes (data-modifying CTEs, `select into`); those
/// run as written instead.
fn wrapping_not_possible(e: &tokio_postgres::Error) -> bool {
    e.as_db_error()
        .is_some_and(|d| matches!(d.code().code(), "0A000" | "42601"))
}

/// Trims result sets collected with `limit + 1` rows to `limit`, flagging the ones that had more.
fn apply_limit(mut sets: Vec<ResultSet>, limit: usize) -> Vec<ResultSet> {
    for set in &mut sets {
        if set.rows.len() > limit {
            set.rows.truncate(limit);
            set.truncated = true;
            set.count = limit as u64;
        }
    }
    sets
}

async fn run_one(
    client: &Client,
    text: &str,
    limit: usize,
    char_start: usize,
) -> Result<Vec<ResultSet>> {
    if sqlsplit::is_select_like(text) {
        let capped = wrapped(text, limit);
        match client.simple_query(&capped).await {
            Ok(msgs) => return Ok(apply_limit(collect(msgs, limit + 1), limit)),
            Err(e) if wrapping_not_possible(&e) => {}
            Err(e) => {
                let q = QueryError::from_driver(&e);
                let remove = WRAP_PREFIX.chars().count();
                return Err(anyhow::Error::new(shift_position(q, remove, char_start)));
            }
        }
    }
    let msgs = client.simple_query(text).await.map_err(|e| {
        anyhow::Error::new(shift_position(QueryError::from_driver(&e), 0, char_start))
    })?;
    Ok(apply_limit(collect(msgs, limit + 1), limit))
}

/// Runs a script statement by statement. Queries (`select`, `with`, `values`, `table`) return
/// at most `limit` rows, fetched with a `LIMIT` so the server stops early; `truncated` on a
/// result set means more rows exist.
pub async fn run_limited(client: &Client, sql: &str, limit: usize) -> Result<QueryOutput> {
    let limit = limit.clamp(1, crate::settings::MAX_ROW_LIMIT as usize);
    let statements = sqlsplit::split_statements(sql);
    if statements.is_empty() {
        anyhow::bail!("There is no SQL to run (only comments or empty statements).");
    }
    let start = Instant::now();
    let mut sets = Vec::new();
    for st in &statements {
        let char_start = sql[..st.start].chars().count();
        sets.extend(run_one(client, &st.text, limit, char_start).await?);
    }
    Ok(QueryOutput {
        sets: shared(sets),
        elapsed: start.elapsed(),
        limit: Some(limit),
    })
}

/// The one query an export can stream: a single `select`, `with` or `values` statement.
pub fn export_statement(sql: &str) -> Result<String> {
    let statements = sqlsplit::split_statements(sql);
    match statements.as_slice() {
        [] => anyhow::bail!("There is no SQL to export."),
        [one] if sqlsplit::is_select_like(&one.text) => Ok(one.text.clone()),
        [_] => anyhow::bail!("Export works on a SELECT, WITH or VALUES query."),
        _ => anyhow::bail!("Export works on one query. Select the statement to export."),
    }
}

/// Streams a query's full result to a CSV file (with a header row) without holding it in
/// memory. Returns the bytes written. The file is replaced only when the export succeeds.
pub async fn export_csv(client: &Client, statement: &str, dest: &std::path::Path) -> Result<u64> {
    use futures_util::StreamExt as _;
    use tokio::io::AsyncWriteExt as _;
    let mut part = dest.as_os_str().to_owned();
    part.push(".part");
    let part = std::path::PathBuf::from(part);
    let result: Result<u64> = async {
        let copy = format!("copy (\n{statement}\n) to stdout with (format csv, header true)");
        let stream = client
            .copy_out(copy.as_str())
            .await
            .map_err(|e| anyhow::Error::new(QueryError::from_driver(&e)))?;
        futures_util::pin_mut!(stream);
        let mut file = tokio::fs::File::create(&part).await?;
        let mut bytes = 0u64;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| anyhow::Error::new(QueryError::from_driver(&e)))?;
            file.write_all(&chunk).await?;
            bytes += chunk.len() as u64;
        }
        file.flush().await?;
        Ok(bytes)
    }
    .await;
    match result {
        Ok(bytes) => {
            tokio::fs::rename(&part, dest).await?;
            Ok(bytes)
        }
        Err(e) => {
            let _ = tokio::fs::remove_file(&part).await;
            Err(e)
        }
    }
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
            sets: shared(sets),
            elapsed: Duration::from_millis(7),
            limit: None,
        }
    }

    #[test]
    fn result_sets_are_shared() {
        let o = out(vec![ResultSet {
            columns: vec!["a".into()],
            rows: vec![vec![Some("1".into())]],
            count: 1,
            truncated: false,
        }]);
        let set = o.last_rowset().unwrap();
        assert!(
            std::ptr::eq(set, o.sets[0].as_ref()),
            "the rowset is read through the shared handle"
        );
        assert_eq!(set.rows[0][0].as_deref(), Some("1"));
        // Holding the set elsewhere keeps the same allocation.
        let held = o.sets[0].clone();
        assert!(Arc::ptr_eq(&held, &o.sets[0]));
        assert_eq!(held.rows[0][0].as_deref(), Some("1"));
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

    #[test]
    fn wrapping_fetches_one_row_past_the_limit() {
        assert_eq!(
            wrapped("select 1", 9),
            "select * from (select 1\n) as _pgroove_q limit 10"
        );
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

    /// Needs `PGROOVE_TEST_PG` (see `connect` tests).
    #[tokio::test]
    async fn run_caps_live() {
        let Some((client, _)) = crate::connect::test_connection().await else {
            return;
        };
        // Far more rows than the cap: the server stops early, so only the cap arrives.
        let o = run(
            &client,
            "select g from generate_series(1, 100000) g order by g",
            5,
        )
        .await
        .unwrap();
        let set = &o.sets[0];
        assert_eq!(o.limit, None);
        assert_eq!((set.rows.len(), set.count, set.truncated), (5, 5, true));
        assert_eq!(set.rows[4][0].as_deref(), Some("5"));
        // A query with its own limit is nested, not broken: below the cap it is not cut.
        let o = run(
            &client,
            "select g from generate_series(1, 100) g limit 4",
            10,
        )
        .await
        .unwrap();
        assert_eq!(
            (o.sets[0].rows.len(), o.sets[0].count, o.sets[0].truncated),
            (4, 4, false)
        );
        // Above the cap it is cut like any other query.
        let o = run(
            &client,
            "select g from generate_series(1, 100) g limit 50",
            10,
        )
        .await
        .unwrap();
        assert_eq!(
            (o.sets[0].rows.len(), o.sets[0].count, o.sets[0].truncated),
            (10, 10, true)
        );
        // Shapes the wrapper can't hold still run: a data-modifying CTE and `select into`.
        client
            .batch_execute(
                "drop table if exists pg_temp.pgroove_run_d; \
                 create temp table pgroove_run_d as select g as x from generate_series(1, 10) g",
            )
            .await
            .unwrap();
        let o = run(
            &client,
            "with d as (delete from pg_temp.pgroove_run_d returning x) select * from d",
            3,
        )
        .await
        .unwrap();
        assert_eq!(
            (o.sets[0].rows.len(), o.sets[0].count, o.sets[0].truncated),
            (3, 3, true)
        );
        let left: i64 = client
            .query_one("select count(*) from pg_temp.pgroove_run_d", &[])
            .await
            .unwrap()
            .get(0);
        assert_eq!(left, 0, "the fallback ran the delete");
        let o = run(
            &client,
            "select g as x into temp table pgroove_run_into from generate_series(1, 3) g",
            2,
        )
        .await
        .unwrap();
        assert_eq!(o.sets.len(), 1);
        assert!(o.sets[0].columns.is_empty());
        let into: i64 = client
            .query_one("select count(*) from pgroove_run_into", &[])
            .await
            .unwrap()
            .get(0);
        assert_eq!(into, 3, "the fallback ran select into");
        // Non-select statements run as written: the server count is kept and rows are cut
        // client-side, as before.
        let o = run(
            &client,
            "insert into pg_temp.pgroove_run_d select g from generate_series(1, 50) g returning x",
            3,
        )
        .await
        .unwrap();
        assert_eq!(
            (o.sets[0].rows.len(), o.sets[0].count, o.sets[0].truncated),
            (3, 50, true)
        );
        // Wrapped errors keep their position relative to the user's text.
        let sql = "  /* lead */ select * from nope_table";
        let err = run(&client, sql, 3).await.unwrap_err();
        let qe = err.downcast_ref::<QueryError>().unwrap();
        assert_eq!(
            qe.span_in(sql),
            Some(sql.find("nope_table").unwrap()..sql.len())
        );
    }

    #[test]
    fn export_needs_exactly_one_query() {
        assert_eq!(export_statement(" select 1; ").unwrap(), "select 1");
        assert_eq!(
            export_statement("with x as (select 1) select * from x")
                .unwrap()
                .len(),
            36
        );
        for bad in ["", "-- nothing", "delete from t", "select 1; select 2"] {
            assert!(export_statement(bad).is_err(), "{bad:?}");
        }
        assert_eq!(
            shift_position(
                QueryError {
                    message: "m".into(),
                    position: Some(20)
                },
                15,
                4
            )
            .position,
            Some(9)
        );
        assert_eq!(
            shift_position(
                QueryError {
                    message: "m".into(),
                    position: None
                },
                15,
                4
            )
            .position,
            None
        );
    }

    #[test]
    fn limit_messages_say_so() {
        let o = QueryOutput {
            sets: shared(vec![ResultSet {
                columns: vec!["a".into()],
                rows: vec![vec![None]; 2],
                count: 2,
                truncated: true,
            }]),
            elapsed: Duration::from_millis(1),
            limit: Some(2),
        };
        assert_eq!(
            o.messages()[0],
            "Statement 1: 2 rows returned (limit of 2 reached: more rows exist, use Export for all)"
        );
    }

    #[tokio::test]
    async fn limited_runs_live() {
        let Some((client, _)) = crate::connect::test_connection().await else {
            return;
        };
        // More rows than the limit: exactly `limit` come back, flagged as cut.
        let o = run_limited(
            &client,
            "select g from generate_series(1, 1000) g order by g",
            10,
        )
        .await
        .unwrap();
        let set = &o.sets[0];
        assert_eq!(
            (set.rows.len(), set.truncated, o.limit),
            (10, true, Some(10))
        );
        assert_eq!(set.rows[9][0].as_deref(), Some("10"));
        // Fewer rows than the limit: all rows, not cut.
        let o = run_limited(&client, "select g from generate_series(1, 5) g;", 10)
            .await
            .unwrap();
        assert_eq!((o.sets[0].rows.len(), o.sets[0].truncated), (5, false));
        // Exactly the limit is not "cut".
        let o = run_limited(&client, "values (1),(2),(3)", 3).await.unwrap();
        assert_eq!((o.sets[0].rows.len(), o.sets[0].truncated), (3, false));
        // Scripts run statement by statement; non-queries run as written.
        let o = run_limited(
            &client,
            "create temp table lim_t (x int); insert into lim_t select generate_series(1, 50); select * from lim_t -- trailing comment",
            20,
        )
        .await
        .unwrap();
        assert_eq!(o.sets.len(), 3);
        assert_eq!(o.sets[1].count, 50, "the insert reports its row count");
        assert_eq!((o.sets[2].rows.len(), o.sets[2].truncated), (20, true));
        // Statements the wrapper can't hold still run: a data-modifying CTE and a plain DML.
        let o = run_limited(
            &client,
            "with d as (delete from lim_t returning x) select * from d",
            5,
        )
        .await
        .unwrap();
        assert_eq!((o.sets[0].rows.len(), o.sets[0].truncated), (5, true));
        // Errors keep their position relative to the user's text, in any statement.
        let sql = "select 1;\nselect * from nope_table";
        let err = run_limited(&client, sql, 10).await.unwrap_err();
        let qe = err.downcast_ref::<QueryError>().unwrap();
        assert_eq!(
            qe.span_in(sql),
            Some(sql.find("nope_table").unwrap()..sql.len())
        );
        assert!(run_limited(&client, "-- only a comment", 10).await.is_err());
        // A limit above the maximum is clamped, not rejected.
        assert!(
            run_limited(&client, "select 1", 1_000_000)
                .await
                .unwrap()
                .limit
                == Some(10_000)
        );
    }

    #[tokio::test]
    async fn exports_all_rows_to_csv() {
        let Some((client, _)) = crate::connect::test_connection().await else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("out.csv");
        let stmt = export_statement(
            "select g as n, 'row, ' || g as note from generate_series(1, 25000) g",
        )
        .unwrap();
        let bytes = export_csv(&client, &stmt, &dest).await.unwrap();
        let text = std::fs::read_to_string(&dest).unwrap();
        assert_eq!(bytes as usize, text.len());
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(
            lines.len(),
            25_001,
            "header plus every row, far above the run limit"
        );
        assert_eq!(lines[0], "n,note");
        assert_eq!(lines[1], "1,\"row, 1\"");
        assert!(!dir.path().join("out.csv.part").exists());
        // A failing query leaves nothing behind and keeps an existing file.
        let err = export_csv(&client, "select * from nope_table", &dest)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("nope_table"), "{err}");
        assert!(dest.exists() && !dir.path().join("out.csv.part").exists());
    }
}
