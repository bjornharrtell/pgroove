//! SQL statements generated from a table's structure, for the object tree's
//! context menu. Pure: the caller loads the structure, this renders the text.

use crate::data::{TableRef, quote_ident};
use crate::structure::{ConstraintInfo, Structure};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Statement {
    Select,
    Insert,
    InsertOnConflict,
    Update,
    UpdateFrom,
    Delete,
    DeleteUsing,
    Merge,
    Ddl,
}

impl Statement {
    pub const ALL: [Statement; 9] = [
        Statement::Select,
        Statement::Insert,
        Statement::InsertOnConflict,
        Statement::Update,
        Statement::UpdateFrom,
        Statement::Delete,
        Statement::DeleteUsing,
        Statement::Merge,
        Statement::Ddl,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Statement::Select => "SELECT",
            Statement::Insert => "INSERT",
            Statement::InsertOnConflict => "INSERT … ON CONFLICT",
            Statement::Update => "UPDATE",
            Statement::UpdateFrom => "UPDATE … FROM",
            Statement::Delete => "DELETE",
            Statement::DeleteUsing => "DELETE … USING",
            Statement::Merge => "MERGE",
            Statement::Ddl => "DDL (CREATE)",
        }
    }
}

/// `merge` arrived in PostgreSQL 15.
pub fn supports_merge(server_version: &str) -> bool {
    let major: String = server_version
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    major.parse::<u32>().is_ok_and(|major| major >= 15)
}

/// A named placeholder for a column, such as `:order_id`. Placeholders cannot be
/// quoted, so anything outside `[a-z0-9_]` becomes an underscore.
pub fn placeholder(column: &str) -> String {
    let mut out = String::from(":");
    out.extend(column.chars().map(|c| {
        if c.is_ascii_alphanumeric() {
            c.to_ascii_lowercase()
        } else {
            '_'
        }
    }));
    out
}

/// Renders `statement` for `table` as SQL that a user edits before running.
/// `None` when the statement needs something the table lacks: a key for the
/// conflict target or the row filters, or columns left to set.
pub fn generate(table: &TableRef, st: &Structure, statement: Statement) -> Option<String> {
    match statement {
        Statement::Select => Some(select(table, st)),
        Statement::Insert => Some(insert(table, st)),
        Statement::InsertOnConflict => insert_on_conflict(table, st),
        Statement::Update => update(table, st),
        Statement::UpdateFrom => update_from(table, st),
        Statement::Delete => delete(table, st),
        Statement::DeleteUsing => delete_using(table, st),
        Statement::Merge => merge(table, st),
        Statement::Ddl => Some(crate::structure::ddl(table, st)),
    }
}

/// Columns a key constraint covers: the primary key, else the first unique one.
fn key_columns(st: &Structure) -> Vec<String> {
    let find = |kind: char| {
        st.constraints
            .iter()
            .find(|c| c.kind == kind && !c.columns.is_empty())
            .map(|c| c.columns.clone())
    };
    find('p').or_else(|| find('u')).unwrap_or_default()
}

fn primary_columns(st: &Structure) -> Vec<String> {
    st.constraints
        .iter()
        .find(|c: &&ConstraintInfo| c.kind == 'p' && !c.columns.is_empty())
        .map(|c| c.columns.clone())
        .unwrap_or_default()
}

/// Columns a value can be supplied for: identity and generated columns cannot.
fn value_columns(st: &Structure) -> Vec<&str> {
    st.columns
        .iter()
        .filter(|c| c.identity.is_none() && !c.generated)
        .map(|c| c.name.as_str())
        .collect()
}

/// The value columns without the key, which an update leaves alone.
fn updated_columns<'a>(st: &'a Structure, key: &[String]) -> Vec<&'a str> {
    value_columns(st)
        .into_iter()
        .filter(|c| !key.iter().any(|k| k == c))
        .collect()
}

fn quoted(columns: &[&str]) -> String {
    columns
        .iter()
        .map(|c| quote_ident(c))
        .collect::<Vec<_>>()
        .join(", ")
}

/// `a = :a, b = :b`
fn assignments(columns: &[&str], prefix: &str) -> String {
    columns
        .iter()
        .map(|c| format!("{prefix}{} = {}", quote_ident(c), placeholder(c)))
        .collect::<Vec<_>>()
        .join(", ")
}

/// `<prefix>a = :a` lines joined with `and`, for use after `where`.
fn key_filter(key: &[String], prefix: &str) -> String {
    key.iter()
        .map(|c| format!("{prefix}{} = {}", quote_ident(c), placeholder(c)))
        .collect::<Vec<_>>()
        .join("\n  and ")
}

/// `<left>.a = <right>.a` lines joined with `and`, for a join condition.
fn join_on(key: &[String], left: &str, right: &str) -> String {
    key.iter()
        .map(|c| format!("{left}.{} = {right}.{}", quote_ident(c), quote_ident(c)))
        .collect::<Vec<_>>()
        .join("\n  and ")
}

/// `a = <prefix>.a, b = <prefix>.b`: each assignment copies from a joined relation.
fn copied_assignments(columns: &[&str], prefix: &str) -> String {
    columns
        .iter()
        .map(|c| {
            let c = quote_ident(c);
            format!("{c} = {prefix}.{c}")
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn select(table: &TableRef, st: &Structure) -> String {
    let columns: Vec<&str> = st.columns.iter().map(|c| c.name.as_str()).collect();
    let list = if columns.is_empty() {
        "*".to_string()
    } else {
        quoted(&columns)
    };
    format!("select {list}\nfrom {};\n", table.qualified())
}

fn insert(table: &TableRef, st: &Structure) -> String {
    let columns = value_columns(st);
    if columns.is_empty() {
        return format!("insert into {} default values;\n", table.qualified());
    }
    let values = columns
        .iter()
        .map(|c| placeholder(c))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "insert into {} ({})\nvalues ({});\n",
        table.qualified(),
        quoted(&columns),
        values
    )
}

fn insert_on_conflict(table: &TableRef, st: &Structure) -> Option<String> {
    let key = key_columns(st);
    if key.is_empty() {
        return None;
    }
    let target = key
        .iter()
        .map(|c| quote_ident(c))
        .collect::<Vec<_>>()
        .join(", ");
    let conflict = match updated_columns(st, &key) {
        columns if columns.is_empty() => "do nothing".to_string(),
        columns => {
            let set = columns
                .iter()
                .map(|c| format!("{} = excluded.{}", quote_ident(c), quote_ident(c)))
                .collect::<Vec<_>>()
                .join(", ");
            format!("do update\nset {set}")
        }
    };
    let insert = insert(table, st);
    let insert = insert.trim_end().trim_end_matches(';');
    Some(format!("{insert}\non conflict ({target}) {conflict};\n"))
}

fn update(table: &TableRef, st: &Structure) -> Option<String> {
    let key = primary_columns(st);
    let set = updated_columns(st, &key);
    if key.is_empty() || set.is_empty() {
        return None;
    }
    Some(format!(
        "update {}\nset {}\nwhere {};\n",
        table.qualified(),
        assignments(&set, ""),
        key_filter(&key, "")
    ))
}

fn update_from(table: &TableRef, st: &Structure) -> Option<String> {
    let key = primary_columns(st);
    let set = updated_columns(st, &key);
    if key.is_empty() || set.is_empty() {
        return None;
    }
    let qualified = table.qualified();
    Some(format!(
        "-- replace `source` with the relation to update from\n\
         update {qualified} as target\n\
         set {set}\n\
         from {qualified} as source\n\
         where {on}\n  and {filter};\n",
        set = copied_assignments(&set, "source"),
        on = join_on(&key, "target", "source"),
        filter = key_filter(&key, "target.")
    ))
}

fn delete(table: &TableRef, st: &Structure) -> Option<String> {
    let key = primary_columns(st);
    if key.is_empty() {
        return None;
    }
    Some(format!(
        "delete from {}\nwhere {};\n",
        table.qualified(),
        key_filter(&key, "")
    ))
}

fn delete_using(table: &TableRef, st: &Structure) -> Option<String> {
    let key = primary_columns(st);
    if key.is_empty() {
        return None;
    }
    let qualified = table.qualified();
    Some(format!(
        "-- replace `source` with the relation to delete using\n\
         delete from {qualified} as target\n\
         using {qualified} as source\n\
         where {on}\n  and {filter};\n",
        on = join_on(&key, "target", "source"),
        filter = key_filter(&key, "target.")
    ))
}

fn merge(table: &TableRef, st: &Structure) -> Option<String> {
    let key = primary_columns(st);
    if key.is_empty() {
        return None;
    }
    let qualified = table.qualified();
    let mut arms: Vec<String> = Vec::new();
    let set = updated_columns(st, &key);
    if !set.is_empty() {
        arms.push(format!(
            "when matched then update\n  set {}",
            assignments(&set, "")
        ));
    }
    let columns = value_columns(st);
    if !columns.is_empty() {
        let values = columns
            .iter()
            .map(|c| placeholder(c))
            .collect::<Vec<_>>()
            .join(", ");
        arms.push(format!(
            "when not matched then insert ({})\n  values ({values})",
            quoted(&columns)
        ));
    }
    if arms.is_empty() {
        return None;
    }
    Some(format!(
        "merge into {qualified} as target\nusing {qualified} as source\non {on}\n{};\n",
        arms.join("\n"),
        on = join_on(&key, "target", "source")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::ObjectKind;
    use crate::structure::{ColumnInfo, Stats};

    fn column(name: &str) -> ColumnInfo {
        ColumnInfo {
            name: name.into(),
            data_type: "text".into(),
            not_null: false,
            default: None,
            identity: None,
            generated: false,
            comment: None,
        }
    }

    fn key(kind: char, columns: &[&str]) -> ConstraintInfo {
        ConstraintInfo {
            name: format!("{kind}_constraint"),
            kind,
            definition: String::new(),
            references: None,
            columns: columns.iter().map(|c| c.to_string()).collect(),
        }
    }

    /// `public.orders (id text, name text, total text)`, with a key when asked.
    fn orders(with_key: bool) -> Structure {
        let mut st = Structure {
            kind: ObjectKind::Table,
            columns: vec![column("id"), column("name"), column("total")],
            constraints: vec![],
            indexes: vec![],
            triggers: vec![],
            stats: Stats::default(),
            view_definition: None,
        };
        if with_key {
            st.constraints.push(key('p', &["id"]));
        }
        st
    }

    fn table() -> TableRef {
        TableRef::new("public", "orders")
    }

    #[test]
    fn select_lists_the_columns() {
        assert_eq!(
            generate(&table(), &orders(true), Statement::Select).unwrap(),
            "select \"id\", \"name\", \"total\"\nfrom \"public\".\"orders\";\n"
        );
    }

    #[test]
    fn insert_names_a_placeholder_per_column() {
        assert_eq!(
            generate(&table(), &orders(true), Statement::Insert).unwrap(),
            "insert into \"public\".\"orders\" (\"id\", \"name\", \"total\")\nvalues (:id, :name, :total);\n"
        );
    }

    #[test]
    fn insert_skips_identity_and_generated_columns() {
        let mut st = orders(true);
        st.columns[0].identity = Some('a');
        st.columns[2].generated = true;
        assert_eq!(
            generate(&table(), &st, Statement::Insert).unwrap(),
            "insert into \"public\".\"orders\" (\"name\")\nvalues (:name);\n"
        );
        // Every column is supplied by the server: the row still gets inserted.
        let mut st = orders(true);
        st.columns[0].identity = Some('a');
        st.columns[1].generated = true;
        st.columns[2].generated = true;
        assert_eq!(
            generate(&table(), &st, Statement::Insert).unwrap(),
            "insert into \"public\".\"orders\" default values;\n"
        );
    }

    #[test]
    fn insert_on_conflict_updates_from_excluded() {
        assert_eq!(
            generate(&table(), &orders(true), Statement::InsertOnConflict).unwrap(),
            "insert into \"public\".\"orders\" (\"id\", \"name\", \"total\")\nvalues (:id, :name, :total)\n\
             on conflict (\"id\") do update\nset \"name\" = excluded.\"name\", \"total\" = excluded.\"total\";\n"
        );
        // Nothing outside the key: there is nothing to update.
        let mut st = orders(true);
        st.columns = vec![column("id")];
        assert_eq!(
            generate(&table(), &st, Statement::InsertOnConflict).unwrap(),
            "insert into \"public\".\"orders\" (\"id\")\nvalues (:id)\non conflict (\"id\") do nothing;\n"
        );
    }

    #[test]
    fn insert_on_conflict_falls_back_to_a_unique_constraint() {
        let mut st = orders(false);
        st.constraints.push(key('u', &["name"]));
        assert!(
            generate(&table(), &st, Statement::InsertOnConflict)
                .unwrap()
                .contains("on conflict (\"name\") do update")
        );
    }

    #[test]
    fn update_sets_the_rest_and_filters_on_the_key() {
        assert_eq!(
            generate(&table(), &orders(true), Statement::Update).unwrap(),
            "update \"public\".\"orders\"\nset \"name\" = :name, \"total\" = :total\nwhere \"id\" = :id;\n"
        );
        // Composite keys filter on every part.
        let mut st = orders(false);
        st.constraints.push(key('p', &["id", "name"]));
        assert_eq!(
            generate(&table(), &st, Statement::Update).unwrap(),
            "update \"public\".\"orders\"\nset \"total\" = :total\nwhere \"id\" = :id\n  and \"name\" = :name;\n"
        );
    }

    #[test]
    fn update_from_joins_the_table_to_itself() {
        assert_eq!(
            generate(&table(), &orders(true), Statement::UpdateFrom).unwrap(),
            "-- replace `source` with the relation to update from\n\
             update \"public\".\"orders\" as target\n\
             set \"name\" = source.\"name\", \"total\" = source.\"total\"\n\
             from \"public\".\"orders\" as source\n\
             where target.\"id\" = source.\"id\"\n  and target.\"id\" = :id;\n"
        );
    }

    #[test]
    fn delete_filters_on_the_key() {
        assert_eq!(
            generate(&table(), &orders(true), Statement::Delete).unwrap(),
            "delete from \"public\".\"orders\"\nwhere \"id\" = :id;\n"
        );
    }

    #[test]
    fn delete_using_joins_the_table_to_itself() {
        assert_eq!(
            generate(&table(), &orders(true), Statement::DeleteUsing).unwrap(),
            "-- replace `source` with the relation to delete using\n\
             delete from \"public\".\"orders\" as target\n\
             using \"public\".\"orders\" as source\n\
             where target.\"id\" = source.\"id\"\n  and target.\"id\" = :id;\n"
        );
    }

    #[test]
    fn merge_covers_matched_and_missing_rows() {
        assert_eq!(
            generate(&table(), &orders(true), Statement::Merge).unwrap(),
            "merge into \"public\".\"orders\" as target\nusing \"public\".\"orders\" as source\n\
             on target.\"id\" = source.\"id\"\n\
             when matched then update\n  set \"name\" = :name, \"total\" = :total\n\
             when not matched then insert (\"id\", \"name\", \"total\")\n  values (:id, :name, :total);\n"
        );
    }

    #[test]
    fn ddl_comes_from_the_structure() {
        let sql = generate(&table(), &orders(true), Statement::Ddl).unwrap();
        assert!(
            sql.starts_with("create table \"public\".\"orders\" (\n"),
            "{sql}"
        );
        assert!(sql.ends_with(");\n"), "{sql}");
    }

    #[test]
    fn keyed_statements_need_a_primary_key() {
        let st = orders(false);
        for statement in [
            Statement::Update,
            Statement::UpdateFrom,
            Statement::Delete,
            Statement::DeleteUsing,
            Statement::Merge,
            Statement::InsertOnConflict,
        ] {
            assert_eq!(generate(&table(), &st, statement), None, "{statement:?}");
        }
        // What needs no key still generates.
        for statement in [Statement::Select, Statement::Insert, Statement::Ddl] {
            assert!(
                generate(&table(), &st, statement).is_some(),
                "{statement:?}"
            );
        }
    }

    #[test]
    fn a_key_only_table_has_nothing_to_update() {
        let mut st = orders(true);
        st.columns = vec![column("id")];
        assert_eq!(generate(&table(), &st, Statement::Update), None);
        assert_eq!(generate(&table(), &st, Statement::UpdateFrom), None);
        assert!(generate(&table(), &st, Statement::Delete).is_some());
        assert!(generate(&table(), &st, Statement::Merge).is_some());
    }

    #[test]
    fn identifiers_are_quoted_but_placeholders_are_not() {
        let mut st = orders(true);
        st.columns = vec![column("select"), column("Total Amount")];
        st.constraints = vec![key('p', &["select"])];
        assert_eq!(
            generate(&table(), &st, Statement::Select).unwrap(),
            "select \"select\", \"Total Amount\"\nfrom \"public\".\"orders\";\n"
        );
        assert_eq!(
            generate(&table(), &st, Statement::Update).unwrap(),
            "update \"public\".\"orders\"\nset \"Total Amount\" = :total_amount\n\
             where \"select\" = :select;\n"
        );
    }

    #[test]
    fn labels_are_listed_once() {
        assert_eq!(Statement::ALL.len(), 9);
        let mut labels: Vec<&str> = Statement::ALL.iter().map(|s| s.label()).collect();
        assert!(labels.iter().all(|l| !l.is_empty()));
        labels.sort();
        labels.dedup();
        assert_eq!(labels.len(), 9);
    }

    #[test]
    fn merge_needs_postgres_fifteen() {
        assert!(supports_merge("15"));
        assert!(supports_merge("15.3 (Debian 15.3-1)"));
        assert!(supports_merge("18.6 (Ubuntu 18.6-0ubuntu0.26.04.1)"));
        assert!(!supports_merge("14.11"));
        assert!(!supports_merge("9.6.24"));
        assert!(!supports_merge(""), "an unknown version cannot merge");
        assert!(!supports_merge("devel"), "nor can a non-numeric one");
    }
}
