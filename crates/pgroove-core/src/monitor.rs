//! Server monitoring queries and session control.

use std::sync::Arc;

use anyhow::{Result, bail};
use tokio_postgres::Client;

use crate::query::{self, ResultSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum View {
    Sessions,
    Blocking,
    Statements,
    Databases,
    Tables,
    Replication,
    Settings,
}

impl View {
    pub const ALL: [View; 7] = [
        View::Sessions,
        View::Blocking,
        View::Statements,
        View::Databases,
        View::Tables,
        View::Replication,
        View::Settings,
    ];

    pub fn label(self) -> &'static str {
        match self {
            View::Sessions => "Sessions",
            View::Blocking => "Blocking",
            View::Statements => "Statements",
            View::Databases => "Databases",
            View::Tables => "Tables",
            View::Replication => "Replication",
            View::Settings => "Settings",
        }
    }

    /// Column holding the backend pid that cancel and terminate act on.
    pub fn pid_column(self) -> Option<&'static str> {
        match self {
            View::Sessions => Some("pid"),
            View::Blocking => Some("blocking_pid"),
            _ => None,
        }
    }

    pub fn sql(self) -> &'static str {
        match self {
            View::Sessions => {
                "select pid, usename::text as \"user\", datname::text as database, \
                 application_name as app, client_addr::text as client, state, \
                 concat_ws(':', wait_event_type, wait_event) as wait, \
                 date_trunc('second', now() - xact_start)::text as xact_age, \
                 date_trunc('second', now() - query_start)::text as query_age, \
                 query \
                 from pg_stat_activity \
                 where backend_type = 'client backend' and pid <> pg_backend_pid() \
                 order by query_start nulls last"
            }
            View::Blocking => {
                "select b.pid as blocked_pid, b.usename::text as blocked_user, \
                 date_trunc('second', now() - b.query_start)::text as waiting, \
                 b.query as blocked_query, k.pid as blocking_pid, \
                 k.usename::text as blocking_user, k.state as blocking_state, \
                 k.query as blocking_query \
                 from pg_stat_activity b \
                 cross join lateral unnest(pg_blocking_pids(b.pid)) as bp(pid) \
                 join pg_stat_activity k on k.pid = bp.pid \
                 order by b.query_start"
            }
            View::Statements => {
                "select query, calls, round(total_exec_time::numeric, 1) as total_ms, \
                 round(mean_exec_time::numeric, 2) as mean_ms, rows \
                 from pg_stat_statements order by total_exec_time desc limit 50"
            }
            View::Databases => {
                "select datname::text as database, \
                 pg_size_pretty(pg_database_size(datname)) as size, \
                 numbackends as connections, xact_commit as commits, xact_rollback as rollbacks, \
                 blks_hit as cache_hits, blks_read as disk_reads \
                 from pg_stat_database where datname is not null \
                 order by pg_database_size(datname) desc"
            }
            View::Tables => {
                "select n.nspname::text as schema, c.relname::text as name, \
                 c.relkind::text as kind, \
                 pg_size_pretty(pg_total_relation_size(c.oid)) as total, \
                 pg_size_pretty(pg_relation_size(c.oid)) as table_size, \
                 pg_size_pretty(pg_indexes_size(c.oid)) as index_size, \
                 c.reltuples::bigint as est_rows \
                 from pg_class c join pg_namespace n on n.oid = c.relnamespace \
                 where c.relkind in ('r','m','p') \
                 and n.nspname !~ '^pg_' and n.nspname <> 'information_schema' \
                 order by pg_total_relation_size(c.oid) desc limit 50"
            }
            View::Replication => {
                "select pid, usename::text as \"user\", application_name as app, \
                 client_addr::text as client, state, sync_state, \
                 pg_wal_lsn_diff(sent_lsn, replay_lsn)::bigint as replay_lag_bytes, \
                 replay_lag::text as replay_lag \
                 from pg_stat_replication order by application_name"
            }
            View::Settings => {
                "select name, setting, unit, category, short_desc as description \
                 from pg_settings order by category, name"
            }
        }
    }
}

/// Fetches a monitoring view as a result set, as a handle the view can share with the grid.
pub async fn fetch(client: &Client, view: View) -> Result<Arc<ResultSet>> {
    if view == View::Statements {
        let installed = client
            .query_opt(
                "select 1 from pg_extension where extname = 'pg_stat_statements'",
                &[],
            )
            .await?
            .is_some();
        if !installed {
            bail!(
                "pg_stat_statements is not installed in this database. \
                 Run `create extension pg_stat_statements;` (it also needs \
                 shared_preload_libraries)."
            );
        }
    }
    let out = query::run(client, view.sql(), query::MAX_ROWS).await?;
    Ok(out.sets.into_iter().next().unwrap_or_default())
}

/// Asks a backend to cancel its current query. Returns whether the signal was sent.
pub async fn cancel_backend(client: &Client, pid: i32) -> Result<bool> {
    Ok(client
        .query_one("select pg_cancel_backend($1)", &[&pid])
        .await?
        .get(0))
}

/// Terminates a backend connection. Returns whether the signal was sent.
pub async fn terminate_backend(client: &Client, pid: i32) -> Result<bool> {
    Ok(client
        .query_one("select pg_terminate_backend($1)", &[&pid])
        .await?
        .get(0))
}

/// Finds the pid in a row of a view's result, using the view's pid column.
pub fn row_pid(view: View, set: &ResultSet, row: usize) -> Option<i32> {
    let col = view.pid_column()?;
    let ix = set.columns.iter().position(|c| c == col)?;
    set.rows.get(row)?.get(ix)?.as_deref()?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_and_pid_columns() {
        assert_eq!(View::ALL.len(), 7);
        for v in View::ALL {
            assert!(!v.label().is_empty());
            assert!(v.sql().starts_with("select"));
        }
        assert_eq!(View::Sessions.pid_column(), Some("pid"));
        assert_eq!(View::Blocking.pid_column(), Some("blocking_pid"));
        assert_eq!(View::Settings.pid_column(), None);
    }

    #[test]
    fn row_pid_lookup() {
        let set = ResultSet {
            columns: vec!["x".into(), "pid".into()],
            rows: vec![
                vec![None, Some("42".into())],
                vec![None, Some("nope".into())],
                vec![None, None],
            ],
            count: 3,
            truncated: false,
            types: vec![],
        };
        assert_eq!(row_pid(View::Sessions, &set, 0), Some(42));
        assert_eq!(row_pid(View::Sessions, &set, 1), None);
        assert_eq!(row_pid(View::Sessions, &set, 2), None);
        assert_eq!(row_pid(View::Sessions, &set, 9), None);
        assert_eq!(row_pid(View::Settings, &set, 0), None);
        assert_eq!(row_pid(View::Blocking, &set, 0), None);
    }

    #[tokio::test]
    async fn views_live() {
        let Some((client, _)) = crate::connect::test_connection().await else {
            return;
        };
        for v in View::ALL {
            match fetch(&client, v).await {
                Ok(set) => assert!(!set.columns.is_empty(), "{v:?} has columns"),
                Err(e) => assert!(
                    v == View::Statements && e.to_string().contains("pg_stat_statements"),
                    "{v:?}: {e}"
                ),
            }
        }
        let sessions = fetch(&client, View::Sessions).await.unwrap();
        assert!(sessions.columns.contains(&"pid".to_string()));
    }

    #[tokio::test]
    async fn cancel_and_terminate_live() {
        let Some((client, _)) = crate::connect::test_connection().await else {
            return;
        };
        let (victim, _) = crate::connect::test_connection().await.unwrap();
        let pid: i32 = victim
            .query_one("select pg_backend_pid()", &[])
            .await
            .unwrap()
            .get(0);
        let sleeper = tokio::spawn(async move { victim.simple_query("select pg_sleep(30)").await });
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        let sessions = fetch(&client, View::Sessions).await.unwrap();
        let row = sessions
            .rows
            .iter()
            .position(|r| r[0].as_deref() == Some(&pid.to_string()))
            .expect("sleeping session is listed");
        assert_eq!(row_pid(View::Sessions, &sessions, row), Some(pid));
        assert!(cancel_backend(&client, pid).await.unwrap());
        let res = sleeper.await.unwrap();
        assert!(res.is_err(), "cancelled query errors");

        let (victim2, _) = crate::connect::test_connection().await.unwrap();
        let pid2: i32 = victim2
            .query_one("select pg_backend_pid()", &[])
            .await
            .unwrap()
            .get(0);
        assert!(terminate_backend(&client, pid2).await.unwrap());
        assert!(victim2.simple_query("select 1").await.is_err());
        assert!(!cancel_backend(&client, 0x7fff_fff0).await.unwrap());
    }
}
