//! Staged row edits: pure change tracking, SQL generation and transactional apply.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Result, bail};
use tokio_postgres::{Client, SimpleQueryMessage};

use crate::data::{TableRef, quote_ident};

pub type Cell = Option<String>;

/// Quotes a text value as an untyped SQL literal (the server infers the type).
pub fn quote_literal(v: &str) -> String {
    format!("'{}'", v.replace('\'', "''"))
}

fn lit(v: &Cell) -> String {
    v.as_deref().map_or_else(|| "NULL".into(), quote_literal)
}

/// One generated statement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Statement {
    pub sql: String,
    /// Updates and deletes must affect exactly one row.
    pub expect_one: bool,
}

/// How the rows of a table are identified when they are updated or deleted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowIdentity {
    /// By primary key columns.
    PrimaryKey(Vec<String>),
    /// By physical location and version (`ctid` and `xmin`): for ordinary tables without a
    /// primary key. A row that changed since it was loaded no longer matches and is refused.
    Ctid,
    /// Not editable (views, foreign tables, ...).
    None,
}

/// How a session addresses a loaded row.
#[derive(Debug, Clone)]
enum Key {
    Columns(Vec<usize>),
    /// `(ctid, xmin)` per loaded row.
    RowIds(Vec<(String, String)>),
}

/// Pending edits against a loaded page of a table that has a primary key or row ids.
///
/// Inserted rows use `None` for "use the column default".
#[derive(Debug, Clone)]
pub struct EditSession {
    columns: Vec<String>,
    key: Key,
    original: Vec<Vec<Cell>>,
    edits: BTreeMap<(usize, usize), Cell>,
    deleted: BTreeSet<usize>,
    inserts: Vec<Vec<Cell>>,
}

impl EditSession {
    /// Fails when a key column is missing from the loaded columns.
    pub fn new(columns: Vec<String>, pk_names: &[String], rows: Vec<Vec<Cell>>) -> Result<Self> {
        let pk = pk_names
            .iter()
            .map(|n| {
                columns
                    .iter()
                    .position(|c| c == n)
                    .ok_or_else(|| anyhow::anyhow!("key column {n} is not in the result"))
            })
            .collect::<Result<Vec<_>>>()?;
        if pk.is_empty() {
            bail!("table has no primary key");
        }
        Ok(Self {
            columns,
            key: Key::Columns(pk),
            original: rows,
            edits: BTreeMap::new(),
            deleted: BTreeSet::new(),
            inserts: Vec::new(),
        })
    }

    /// A session for a table without a primary key, addressing rows by `(ctid, xmin)`.
    /// Fails when the ids don't match the rows or aren't what the server produces.
    pub fn with_row_ids(
        columns: Vec<String>,
        rows: Vec<Vec<Cell>>,
        ids: Vec<(String, String)>,
    ) -> Result<Self> {
        if ids.len() != rows.len() {
            bail!("row ids do not match the loaded rows");
        }
        let valid_ctid = |c: &str| {
            c.starts_with('(')
                && c.ends_with(')')
                && c[1..c.len() - 1].split(',').count() == 2
                && c[1..c.len() - 1]
                    .split(',')
                    .all(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        };
        let valid_xmin = |x: &str| !x.is_empty() && x.bytes().all(|b| b.is_ascii_digit());
        if let Some((c, x)) = ids.iter().find(|(c, x)| !valid_ctid(c) || !valid_xmin(x)) {
            bail!("unexpected row id ({c}, {x})");
        }
        Ok(Self {
            columns,
            key: Key::RowIds(ids),
            original: rows,
            edits: BTreeMap::new(),
            deleted: BTreeSet::new(),
            inserts: Vec::new(),
        })
    }

    pub fn columns(&self) -> &[String] {
        &self.columns
    }

    pub fn row_count(&self) -> usize {
        self.original.len() + self.inserts.len()
    }

    pub fn is_inserted(&self, row: usize) -> bool {
        row >= self.original.len()
    }

    pub fn is_deleted(&self, row: usize) -> bool {
        self.deleted.contains(&row)
    }

    pub fn is_edited(&self, row: usize, col: usize) -> bool {
        self.edits.contains_key(&(row, col))
    }

    /// The value to display, including pending edits.
    pub fn value(&self, row: usize, col: usize) -> Option<&str> {
        if let Some(i) = row.checked_sub(self.original.len()) {
            return self.inserts.get(i)?.get(col)?.as_deref();
        }
        match self.edits.get(&(row, col)) {
            Some(v) => v.as_deref(),
            None => self.original.get(row)?.get(col)?.as_deref(),
        }
    }

    pub fn set_value(&mut self, row: usize, col: usize, value: Cell) {
        if col >= self.columns.len() {
            return;
        }
        if let Some(i) = row.checked_sub(self.original.len()) {
            if let Some(cell) = self.inserts.get_mut(i).and_then(|r| r.get_mut(col)) {
                *cell = value;
            }
            return;
        }
        let Some(orig) = self.original.get(row).and_then(|r| r.get(col)) else {
            return;
        };
        if *orig == value {
            self.edits.remove(&(row, col));
        } else {
            self.edits.insert((row, col), value);
        }
    }

    /// Marks an existing row for deletion, or unmarks it; inserted rows are removed.
    pub fn toggle_delete(&mut self, row: usize) {
        if let Some(i) = row.checked_sub(self.original.len()) {
            if i < self.inserts.len() {
                self.inserts.remove(i);
            }
        } else if row < self.original.len() && !self.deleted.remove(&row) {
            self.deleted.insert(row);
        }
    }

    pub fn add_insert(&mut self, mut values: Vec<Cell>) {
        values.resize(self.columns.len(), None);
        self.inserts.push(values);
    }

    pub fn discard(&mut self) {
        self.edits.clear();
        self.deleted.clear();
        self.inserts.clear();
    }

    /// Number of statements that would run.
    pub fn pending_count(&self) -> usize {
        let edited_rows: BTreeSet<_> = self
            .edits
            .keys()
            .map(|(r, _)| *r)
            .filter(|r| !self.deleted.contains(r))
            .collect();
        edited_rows.len() + self.deleted.len() + self.inserts.len()
    }

    pub fn is_dirty(&self) -> bool {
        self.pending_count() > 0
    }

    fn key_clause(&self, row: usize) -> String {
        match &self.key {
            Key::Columns(pk) => pk
                .iter()
                .map(|&c| {
                    format!(
                        "{} = {}",
                        quote_ident(&self.columns[c]),
                        lit(&self.original[row][c])
                    )
                })
                .collect::<Vec<_>>()
                .join(" and "),
            Key::RowIds(ids) => {
                let (ctid, xmin) = &ids[row];
                format!("ctid = '{ctid}'::tid and xmin::text = '{xmin}'")
            }
        }
    }

    pub fn statements(&self, table: &TableRef) -> Vec<Statement> {
        let t = table.qualified();
        let mut out = Vec::new();
        let edited_rows: BTreeSet<_> = self.edits.keys().map(|(r, _)| *r).collect();
        for row in edited_rows
            .into_iter()
            .filter(|r| !self.deleted.contains(r))
        {
            let sets = self
                .edits
                .range((row, 0)..=(row, usize::MAX))
                .map(|((_, c), v)| format!("{} = {}", quote_ident(&self.columns[*c]), lit(v)))
                .collect::<Vec<_>>()
                .join(", ");
            out.push(Statement {
                sql: format!("update {t} set {sets} where {}", self.key_clause(row)),
                expect_one: true,
            });
        }
        for &row in &self.deleted {
            out.push(Statement {
                sql: format!("delete from {t} where {}", self.key_clause(row)),
                expect_one: true,
            });
        }
        for values in &self.inserts {
            let given: Vec<_> = self
                .columns
                .iter()
                .zip(values)
                .filter_map(|(c, v)| v.as_ref().map(|v| (quote_ident(c), quote_literal(v))))
                .collect();
            let sql = if given.is_empty() {
                format!("insert into {t} default values")
            } else {
                format!(
                    "insert into {t} ({}) values ({})",
                    given
                        .iter()
                        .map(|g| g.0.as_str())
                        .collect::<Vec<_>>()
                        .join(", "),
                    given
                        .iter()
                        .map(|g| g.1.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            };
            out.push(Statement {
                sql,
                expect_one: false,
            });
        }
        out
    }
}

/// How to identify the rows of a table: by primary key, by `ctid` and `xmin` for ordinary
/// tables without one, or not at all for relations that can't be edited.
pub async fn row_identity(client: &Client, table: &TableRef) -> Result<RowIdentity> {
    let kind = client
        .query_opt(
            "select c.relkind::text from pg_class c \
             join pg_namespace n on n.oid = c.relnamespace \
             where n.nspname = $1 and c.relname = $2",
            &[&table.schema, &table.name],
        )
        .await?
        .map(|r| r.get::<_, String>(0));
    if !matches!(kind.as_deref(), Some("r" | "p")) {
        return Ok(RowIdentity::None);
    }
    let pk = primary_key(client, table).await?;
    Ok(if pk.is_empty() {
        RowIdentity::Ctid
    } else {
        RowIdentity::PrimaryKey(pk)
    })
}

/// Primary key columns in key order; empty when the table has none.
pub async fn primary_key(client: &Client, table: &TableRef) -> Result<Vec<String>> {
    let rows = client
        .query(
            "select a.attname::text from pg_index i \
             join pg_class c on c.oid = i.indrelid \
             join pg_namespace n on n.oid = c.relnamespace \
             join pg_attribute a on a.attrelid = i.indrelid and a.attnum = any(i.indkey) \
             where i.indisprimary and n.nspname = $1 and c.relname = $2 \
             order by array_position(i.indkey::int2[], a.attnum)",
            &[&table.schema, &table.name],
        )
        .await?;
    Ok(rows.iter().map(|r| r.get(0)).collect())
}

/// Turns the server-side default read-only transaction mode on or off.
pub async fn set_read_only(client: &Client, read_only: bool) -> Result<()> {
    let v = if read_only { "on" } else { "off" };
    client
        .batch_execute(&format!("set default_transaction_read_only = {v}"))
        .await?;
    Ok(())
}

/// Runs the statements in one transaction; rolls back on any error or when an
/// update or delete does not affect exactly one row.
pub async fn apply(client: &Client, statements: &[Statement]) -> Result<()> {
    client.batch_execute("begin").await?;
    let result = run_all(client, statements).await;
    match result {
        Ok(()) => {
            client.batch_execute("commit").await?;
            Ok(())
        }
        Err(e) => {
            let _ = client.batch_execute("rollback").await;
            Err(e)
        }
    }
}

async fn run_all(client: &Client, statements: &[Statement]) -> Result<()> {
    for (i, st) in statements.iter().enumerate() {
        let msgs = client.simple_query(&st.sql).await.map_err(|e| {
            anyhow::anyhow!("statement {}: {}", i + 1, crate::query::describe_error(&e))
        })?;
        let affected = msgs
            .iter()
            .find_map(|m| match m {
                SimpleQueryMessage::CommandComplete(n) => Some(*n),
                _ => None,
            })
            .unwrap_or(0);
        if st.expect_one && affected != 1 {
            bail!(
                "statement {} affected {affected} rows instead of 1; the row may have changed. Reload and try again",
                i + 1
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &str) -> Cell {
        Some(v.to_string())
    }

    fn session() -> EditSession {
        EditSession::new(
            vec!["id".into(), "name".into(), "note".into()],
            &["id".to_string()],
            vec![
                vec![s("1"), s("a"), None],
                vec![s("2"), s("b"), s("x")],
                vec![s("3"), s("c"), s("y")],
            ],
        )
        .unwrap()
    }

    fn t() -> TableRef {
        TableRef::new("public", "t")
    }

    #[test]
    fn literals() {
        assert_eq!(quote_literal("it's"), "'it''s'");
        assert_eq!(lit(&None), "NULL");
        assert_eq!(lit(&s("a\\b")), "'a\\b'");
    }

    #[test]
    fn construction_errors() {
        assert!(EditSession::new(vec!["a".into()], &["b".to_string()], vec![]).is_err());
        assert!(EditSession::new(vec!["a".into()], &[], vec![]).is_err());
    }

    #[test]
    fn edit_tracking_and_revert() {
        let mut e = session();
        assert!(!e.is_dirty());
        e.set_value(0, 1, s("z"));
        assert!(e.is_edited(0, 1));
        assert_eq!(e.value(0, 1), Some("z"));
        e.set_value(0, 1, s("a"));
        assert!(
            !e.is_edited(0, 1),
            "setting the original value clears the edit"
        );
        e.set_value(0, 2, s("n"));
        e.set_value(99, 0, s("n"));
        e.set_value(0, 99, s("n"));
        assert_eq!(e.pending_count(), 1);
        e.discard();
        assert!(!e.is_dirty());
        assert_eq!(e.columns().len(), 3);
    }

    #[test]
    fn statements_for_all_kinds() {
        let mut e = session();
        e.set_value(0, 1, s("it's"));
        e.set_value(0, 2, s("n"));
        e.set_value(1, 2, None);
        e.toggle_delete(2);
        e.set_value(2, 1, s("ignored"));
        e.add_insert(vec![s("9"), s("new")]);
        e.add_insert(vec![]);
        assert_eq!(e.row_count(), 5);
        assert_eq!(e.pending_count(), 5);
        let st = e.statements(&t());
        let sql: Vec<_> = st.iter().map(|s| s.sql.as_str()).collect();
        assert_eq!(
            sql,
            [
                "update \"public\".\"t\" set \"name\" = 'it''s', \"note\" = 'n' where \"id\" = '1'",
                "update \"public\".\"t\" set \"note\" = NULL where \"id\" = '2'",
                "delete from \"public\".\"t\" where \"id\" = '3'",
                "insert into \"public\".\"t\" (\"id\", \"name\") values ('9', 'new')",
                "insert into \"public\".\"t\" default values",
            ]
        );
        assert!(st[..3].iter().all(|s| s.expect_one));
        assert!(st[3..].iter().all(|s| !s.expect_one));
    }

    #[test]
    fn delete_toggle_and_insert_edit() {
        let mut e = session();
        e.toggle_delete(1);
        assert!(e.is_deleted(1));
        e.toggle_delete(1);
        assert!(!e.is_deleted(1));
        e.add_insert(vec![s("9")]);
        assert!(e.is_inserted(3) && !e.is_inserted(2));
        e.set_value(3, 1, s("v"));
        assert_eq!(e.value(3, 1), Some("v"));
        assert_eq!(e.value(3, 2), None);
        assert_eq!(e.value(7, 0), None);
        e.toggle_delete(3);
        assert_eq!(e.row_count(), 3);
        e.toggle_delete(50);
    }

    #[test]
    fn composite_key_clause() {
        let e = EditSession::new(
            vec!["a".into(), "b".into(), "v".into()],
            &["a".to_string(), "b".to_string()],
            vec![vec![s("1"), s("2"), s("x")]],
        )
        .unwrap();
        let mut e = e;
        e.set_value(0, 2, s("y"));
        assert!(
            e.statements(&t())[0]
                .sql
                .ends_with("where \"a\" = '1' and \"b\" = '2'")
        );
    }

    #[test]
    fn row_id_sessions() {
        let cols = vec!["a".to_string(), "b".to_string()];
        let rows = vec![vec![s("1"), s("x")], vec![s("2"), s("y")]];
        let ids = vec![
            ("(0,1)".to_string(), "700".to_string()),
            ("(0,2)".to_string(), "701".to_string()),
        ];
        let mut e = EditSession::with_row_ids(cols.clone(), rows.clone(), ids.clone()).unwrap();
        e.set_value(1, 1, s("z"));
        e.toggle_delete(0);
        let sql: Vec<_> = e.statements(&t()).into_iter().map(|s| s.sql).collect();
        assert_eq!(
            sql,
            [
                "update \"public\".\"t\" set \"b\" = 'z' where ctid = '(0,2)'::tid and xmin::text = '701'",
                "delete from \"public\".\"t\" where ctid = '(0,1)'::tid and xmin::text = '700'",
            ]
        );
        // Mismatched or suspicious ids are rejected before any SQL is built.
        assert!(EditSession::with_row_ids(cols.clone(), rows.clone(), ids[..1].to_vec()).is_err());
        for bad in [
            ("(0,1)'; drop table x; --", "1"),
            ("(0,1)", "1 or 1=1"),
            ("0,1", "1"),
            ("(a,b)", "1"),
            ("()", "1"),
            ("(0,1)", ""),
        ] {
            let r = EditSession::with_row_ids(
                cols.clone(),
                vec![rows[0].clone()],
                vec![(bad.0.to_string(), bad.1.to_string())],
            );
            assert!(r.is_err(), "{bad:?}");
        }
    }

    #[tokio::test]
    async fn tables_without_primary_key_are_editable_live() {
        use crate::data::{PageRequest, fetch_page};
        let Some((client, _)) = crate::connect::test_connection().await else {
            return;
        };
        client
            .batch_execute(
                "drop schema if exists pgroove_n cascade; create schema pgroove_n; \
                 create table pgroove_n.nopk (a int, b text); \
                 insert into pgroove_n.nopk values (1,'one'),(2,'two'),(2,'two'); \
                 create table pgroove_n.haspk (id int primary key); \
                 create view pgroove_n.v as select 1 as x",
            )
            .await
            .unwrap();
        let ident = |n: &str| {
            let table = TableRef::new("pgroove_n", n);
            let client = &client;
            async move { row_identity(client, &table).await.unwrap() }
        };
        assert_eq!(ident("nopk").await, RowIdentity::Ctid);
        assert_eq!(
            ident("haspk").await,
            RowIdentity::PrimaryKey(vec!["id".into()])
        );
        assert_eq!(ident("v").await, RowIdentity::None);
        assert_eq!(ident("missing").await, RowIdentity::None);

        let table = TableRef::new("pgroove_n", "nopk");
        let mut req = PageRequest::page(0, 10);
        req.row_ids = true;
        let page = fetch_page(&client, &table, &req).await.unwrap();
        assert_eq!(
            page.data.columns,
            ["a", "b"],
            "row ids are not part of the data"
        );
        assert_eq!((page.data.rows.len(), page.row_ids.len()), (3, 3));
        // Delete one of two identical rows and edit the first: only those rows change.
        let mut e = EditSession::with_row_ids(
            page.data.columns.clone(),
            page.data.rows.clone(),
            page.row_ids.clone(),
        )
        .unwrap();
        let dup = (0..3)
            .filter(|&i| page.data.rows[i][0].as_deref() == Some("2"))
            .collect::<Vec<_>>();
        e.toggle_delete(dup[0]);
        let one = (0..3)
            .find(|&i| page.data.rows[i][0].as_deref() == Some("1"))
            .unwrap();
        e.set_value(one, 1, s("uno"));
        apply(&client, &e.statements(&table)).await.unwrap();
        let rows = client
            .query("select a, b from pgroove_n.nopk order by a, b", &[])
            .await
            .unwrap();
        let got: Vec<(i32, String)> = rows.iter().map(|r| (r.get(0), r.get(1))).collect();
        assert_eq!(got, [(1, "uno".to_string()), (2, "two".to_string())]);
        // A row that changed after loading is refused instead of matched by position.
        let mut stale = EditSession::with_row_ids(
            page.data.columns.clone(),
            page.data.rows.clone(),
            page.row_ids.clone(),
        )
        .unwrap();
        stale.set_value(one, 1, s("again"));
        let err = apply(&client, &stale.statements(&table)).await.unwrap_err();
        assert!(err.to_string().contains("affected 0 rows"), "{err}");
        client
            .batch_execute("drop schema pgroove_n cascade")
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn apply_live() {
        let Some((client, _)) = crate::connect::test_connection().await else {
            return;
        };
        client
            .batch_execute(
                "drop schema if exists pgroove_e cascade; create schema pgroove_e; \
                 create table pgroove_e.t (id int primary key, name text, \
                    k1 int, k2 int); \
                 insert into pgroove_e.t values (1,'a',1,1),(2,'b',1,2)",
            )
            .await
            .unwrap();
        let table = TableRef::new("pgroove_e", "t");
        assert_eq!(primary_key(&client, &table).await.unwrap(), ["id"]);
        let mut e = EditSession::new(
            vec!["id".into(), "name".into(), "k1".into(), "k2".into()],
            &["id".to_string()],
            vec![
                vec![s("1"), s("a"), s("1"), s("1")],
                vec![s("2"), s("b"), s("1"), s("2")],
            ],
        )
        .unwrap();
        e.set_value(0, 1, s("it's"));
        e.toggle_delete(1);
        e.add_insert(vec![s("3"), s("c")]);
        apply(&client, &e.statements(&table)).await.unwrap();
        let rows = client
            .query("select id, name from pgroove_e.t order by id", &[])
            .await
            .unwrap();
        let got: Vec<(i32, String)> = rows.iter().map(|r| (r.get(0), r.get(1))).collect();
        assert_eq!(got, [(1, "it's".to_string()), (3, "c".to_string())]);

        // A stale row rolls everything back.
        let mut stale = EditSession::new(
            vec!["id".into(), "name".into()],
            &["id".to_string()],
            vec![vec![s("1"), s("it's")], vec![s("2"), s("gone")]],
        )
        .unwrap();
        stale.set_value(0, 1, s("changed"));
        stale.set_value(1, 1, s("changed"));
        let err = apply(&client, &stale.statements(&table)).await.unwrap_err();
        assert!(err.to_string().contains("affected 0 rows"), "{err}");
        let n: String = client
            .query_one("select name from pgroove_e.t where id = 1", &[])
            .await
            .unwrap()
            .get(0);
        assert_eq!(n, "it's");

        // SQL errors roll back too.
        let bad = [Statement {
            sql: "insert into pgroove_e.t (id) values (1)".into(),
            expect_one: false,
        }];
        assert!(apply(&client, &bad).await.is_err());

        // Read-only mode blocks writes.
        set_read_only(&client, true).await.unwrap();
        let blocked = [Statement {
            sql: "delete from pgroove_e.t where id = 1".into(),
            expect_one: true,
        }];
        assert!(apply(&client, &blocked).await.is_err());
        set_read_only(&client, false).await.unwrap();
        client
            .batch_execute("drop schema pgroove_e cascade")
            .await
            .unwrap();
    }
}
