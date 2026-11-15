//! Renaming and dropping schemas and the objects in them: the statements, the check of a new
//! name, and the lookup of a routine's exact signature. The statements are built here, from
//! names the catalog gave, and every identifier is quoted, so a name with capitals, spaces or
//! quotes is handled as it is.

use anyhow::Result;
use tokio_postgres::Client;

use crate::catalog::ObjectKind;
use crate::data::quote_ident;

/// The longest identifier PostgreSQL keeps (`NAMEDATALEN` is 64, with room for the end of the
/// string); longer ones are cut silently, so they are refused here.
pub const MAX_NAME_BYTES: usize = 63;

/// What a statement acts on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// A schema.
    Schema(String),
    /// A table, view, sequence or type: found by schema and name alone.
    Object {
        schema: String,
        name: String,
        kind: ObjectKind,
    },
    /// A function, procedure or aggregate: also needs its identity arguments, since several
    /// can share a name.
    Routine {
        schema: String,
        name: String,
        /// As `pg_get_function_identity_arguments` writes them; empty for none.
        args: String,
        kind: ObjectKind,
    },
}

/// A routine as the catalog names it: `name` is the bare name, `args` its identity arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Routine {
    pub name: String,
    pub args: String,
}

impl Target {
    /// What to call it in a dialog: `table "public"."orders"`.
    pub fn describe(&self) -> String {
        match self {
            Target::Schema(name) => format!("schema {}", quote_ident(name)),
            Target::Object { schema, name, kind } => format!(
                "{} {}.{}",
                kind.label(),
                quote_ident(schema),
                quote_ident(name)
            ),
            Target::Routine {
                schema,
                name,
                args,
                kind,
            } => format!(
                "{} {}.{}({})",
                kind.label(),
                quote_ident(schema),
                quote_ident(name),
                args
            ),
        }
    }

    /// The object's current name, without schema or arguments.
    pub fn name(&self) -> &str {
        match self {
            Target::Schema(name) | Target::Object { name, .. } | Target::Routine { name, .. } => {
                name
            }
        }
    }

    /// The keyword the statements use for the kind of object.
    fn keyword(&self) -> &'static str {
        match self {
            Target::Schema(_) => "schema",
            Target::Object { kind, .. } | Target::Routine { kind, .. } => keyword(*kind),
        }
    }

    /// The object as the statements name it: qualified, quoted, with arguments for a routine.
    /// `None` for a routine kind given without arguments, or a plain object given a routine's
    /// kind, which no statement could name.
    fn reference(&self) -> Option<String> {
        Some(match self {
            Target::Schema(name) => quote_ident(name),
            Target::Object { schema, name, kind } => {
                if is_routine(*kind) {
                    return None;
                }
                format!("{}.{}", quote_ident(schema), quote_ident(name))
            }
            Target::Routine {
                schema,
                name,
                args,
                kind,
            } => {
                if !is_routine(*kind) {
                    return None;
                }
                // An aggregate over no arguments is written `(*)`.
                let args = if *kind == ObjectKind::Aggregate && args.trim().is_empty() {
                    "*"
                } else {
                    args
                };
                format!("{}.{}({})", quote_ident(schema), quote_ident(name), args)
            }
        })
    }
}

fn is_routine(kind: ObjectKind) -> bool {
    matches!(
        kind,
        ObjectKind::Function | ObjectKind::Procedure | ObjectKind::Aggregate
    )
}

/// The statement keyword for a kind of object.
fn keyword(kind: ObjectKind) -> &'static str {
    match kind {
        // A partitioned table is dropped and renamed as a table.
        ObjectKind::Table | ObjectKind::PartitionedTable => "table",
        ObjectKind::ForeignTable => "foreign table",
        ObjectKind::View => "view",
        ObjectKind::MaterializedView => "materialized view",
        ObjectKind::Sequence => "sequence",
        ObjectKind::Function => "function",
        ObjectKind::Procedure => "procedure",
        ObjectKind::Aggregate => "aggregate",
        // Enum and composite types are both `type`; a domain has its own statements.
        ObjectKind::EnumType | ObjectKind::CompositeType => "type",
        ObjectKind::DomainType => "domain",
    }
}

/// `drop … <object>`. Never `cascade`: if something depends on the object the server refuses
/// and says what, which is the answer a person needs before deciding to go further.
pub fn drop_sql(target: &Target) -> Option<String> {
    Some(format!("drop {} {}", target.keyword(), target.reference()?))
}

/// `alter … <object> rename to <new>`; `new` is quoted here.
pub fn rename_sql(target: &Target, new: &str) -> Option<String> {
    Some(format!(
        "alter {} {} rename to {}",
        target.keyword(),
        target.reference()?,
        quote_ident(new)
    ))
}

/// Checks a name typed for a rename and returns it as it will be used (surrounding blanks
/// removed). `old` is the current name, which is not a change.
pub fn validate_name(new: &str, old: &str) -> Result<String, String> {
    let name = new.trim();
    if name.is_empty() {
        return Err("Enter a name.".into());
    }
    if name.contains('\0') {
        return Err("A name cannot contain a NUL character.".into());
    }
    if name.len() > MAX_NAME_BYTES {
        return Err(format!(
            "A name is at most {MAX_NAME_BYTES} bytes; this one is {}.",
            name.len()
        ));
    }
    if name == old {
        return Err("That is already the name.".into());
    }
    Ok(name.to_string())
}

/// Looks up the routine the tree lists as `entry` (`add(a integer, b integer)`), for the
/// statements that need its bare name and identity arguments.
pub async fn routine(client: &Client, schema: &str, entry: &str) -> Result<Routine> {
    let row = client
        .query_opt(
            "select p.proname::text, pg_get_function_identity_arguments(p.oid) from pg_proc p \
             join pg_namespace n on n.oid = p.pronamespace \
             where n.nspname = $1 \
               and p.proname || '(' || pg_get_function_identity_arguments(p.oid) || ')' = $2",
            &[&schema, &entry],
        )
        .await?
        .ok_or_else(|| anyhow::anyhow!("{entry} no longer exists"))?;
    Ok(Routine {
        name: row.get(0),
        args: row.get(1),
    })
}

/// Upkeep of a table that is not a change of its name or existence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Maintenance {
    /// Refreshes the planner's statistics.
    Analyze,
    /// Reclaims dead rows (plain `vacuum`, not `full`: that rewrites the table under an
    /// exclusive lock).
    Vacuum,
    /// Empties the table.
    Truncate,
}

impl Maintenance {
    pub const ALL: [Maintenance; 3] = [Self::Analyze, Self::Vacuum, Self::Truncate];

    pub fn label(self) -> &'static str {
        match self {
            Self::Analyze => "Analyze",
            Self::Vacuum => "Vacuum",
            Self::Truncate => "Truncate",
        }
    }

    /// Whether the server accepts the operation on this kind of object: analyze on anything
    /// with rows (a foreign table too), vacuum on local tables and materialized views, truncate
    /// on tables only. Views, sequences, types and routines have nothing to maintain.
    pub fn applies_to(self, kind: ObjectKind) -> bool {
        use ObjectKind::*;
        match self {
            Self::Analyze => matches!(
                kind,
                Table | PartitionedTable | MaterializedView | ForeignTable
            ),
            Self::Vacuum => matches!(kind, Table | PartitionedTable | MaterializedView),
            Self::Truncate => matches!(kind, Table | PartitionedTable),
        }
    }
}

/// The statement for `op` on `target`; `None` when the target is not an object the operation
/// applies to. Truncate does not restart identities and does not cascade: a table that a
/// foreign key points at makes the server refuse, and its message says so.
pub fn maintenance_sql(op: Maintenance, target: &Target) -> Option<String> {
    let Target::Object { schema, name, kind } = target else {
        return None;
    };
    if !op.applies_to(*kind) {
        return None;
    }
    let table = format!("{}.{}", quote_ident(schema), quote_ident(name));
    Some(match op {
        Maintenance::Analyze => format!("analyze {table}"),
        Maintenance::Vacuum => format!("vacuum {table}"),
        Maintenance::Truncate => format!("truncate table {table}"),
    })
}

/// Runs one statement built by this module.
pub async fn run(client: &Client, sql: &str) -> Result<()> {
    client.batch_execute(sql).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn object(kind: ObjectKind) -> Target {
        Target::Object {
            schema: "s".into(),
            name: "t".into(),
            kind,
        }
    }

    fn routine_target(kind: ObjectKind, args: &str) -> Target {
        Target::Routine {
            schema: "s".into(),
            name: "f".into(),
            args: args.into(),
            kind,
        }
    }

    #[test]
    fn drops_each_kind_with_its_keyword() {
        let sql = |k| drop_sql(&object(k)).unwrap();
        assert_eq!(sql(ObjectKind::Table), r#"drop table "s"."t""#);
        assert_eq!(sql(ObjectKind::PartitionedTable), r#"drop table "s"."t""#);
        assert_eq!(
            sql(ObjectKind::ForeignTable),
            r#"drop foreign table "s"."t""#
        );
        assert_eq!(sql(ObjectKind::View), r#"drop view "s"."t""#);
        assert_eq!(
            sql(ObjectKind::MaterializedView),
            r#"drop materialized view "s"."t""#
        );
        assert_eq!(sql(ObjectKind::Sequence), r#"drop sequence "s"."t""#);
        assert_eq!(sql(ObjectKind::EnumType), r#"drop type "s"."t""#);
        assert_eq!(sql(ObjectKind::CompositeType), r#"drop type "s"."t""#);
        assert_eq!(sql(ObjectKind::DomainType), r#"drop domain "s"."t""#);
        assert_eq!(
            drop_sql(&Target::Schema("s".into())).unwrap(),
            r#"drop schema "s""#
        );
    }

    #[test]
    fn renames_each_kind_with_its_keyword() {
        let sql = |k| rename_sql(&object(k), "n").unwrap();
        assert_eq!(
            sql(ObjectKind::Table),
            r#"alter table "s"."t" rename to "n""#
        );
        assert_eq!(
            sql(ObjectKind::ForeignTable),
            r#"alter foreign table "s"."t" rename to "n""#
        );
        assert_eq!(sql(ObjectKind::View), r#"alter view "s"."t" rename to "n""#);
        assert_eq!(
            sql(ObjectKind::MaterializedView),
            r#"alter materialized view "s"."t" rename to "n""#
        );
        assert_eq!(
            sql(ObjectKind::Sequence),
            r#"alter sequence "s"."t" rename to "n""#
        );
        assert_eq!(
            sql(ObjectKind::EnumType),
            r#"alter type "s"."t" rename to "n""#
        );
        assert_eq!(
            sql(ObjectKind::DomainType),
            r#"alter domain "s"."t" rename to "n""#
        );
        assert_eq!(
            rename_sql(&Target::Schema("s".into()), "n").unwrap(),
            r#"alter schema "s" rename to "n""#
        );
    }

    #[test]
    fn routines_carry_their_arguments() {
        let f = routine_target(ObjectKind::Function, "a integer, b integer");
        assert_eq!(
            drop_sql(&f).unwrap(),
            r#"drop function "s"."f"(a integer, b integer)"#
        );
        assert_eq!(
            rename_sql(&f, "g").unwrap(),
            r#"alter function "s"."f"(a integer, b integer) rename to "g""#
        );
        assert_eq!(
            drop_sql(&routine_target(ObjectKind::Procedure, "")).unwrap(),
            r#"drop procedure "s"."f"()"#
        );
        assert_eq!(
            drop_sql(&routine_target(ObjectKind::Aggregate, "integer")).unwrap(),
            r#"drop aggregate "s"."f"(integer)"#
        );
        // An aggregate over no arguments is `(*)`; a function over none stays `()`.
        assert_eq!(
            drop_sql(&routine_target(ObjectKind::Aggregate, "")).unwrap(),
            r#"drop aggregate "s"."f"(*)"#
        );
        assert_eq!(
            drop_sql(&routine_target(ObjectKind::Function, "")).unwrap(),
            r#"drop function "s"."f"()"#
        );
    }

    #[test]
    fn a_target_that_cannot_be_named_gives_no_statement() {
        assert_eq!(drop_sql(&object(ObjectKind::Function)), None);
        assert_eq!(rename_sql(&object(ObjectKind::Aggregate), "n"), None);
        assert_eq!(drop_sql(&routine_target(ObjectKind::Table, "")), None);
    }

    #[test]
    fn odd_names_are_quoted() {
        let t = Target::Object {
            schema: "My Schema".into(),
            name: "a\"b".into(),
            kind: ObjectKind::Table,
        };
        assert_eq!(drop_sql(&t).unwrap(), r#"drop table "My Schema"."a""b""#);
        assert_eq!(
            rename_sql(&t, "x\"; drop table y; --").unwrap(),
            r#"alter table "My Schema"."a""b" rename to "x""; drop table y; --""#
        );
    }

    #[test]
    fn targets_describe_themselves() {
        assert_eq!(Target::Schema("s".into()).describe(), r#"schema "s""#);
        assert_eq!(object(ObjectKind::View).describe(), r#"view "s"."t""#);
        assert_eq!(
            routine_target(ObjectKind::Function, "a integer").describe(),
            r#"function "s"."f"(a integer)"#
        );
        assert_eq!(Target::Schema("s".into()).name(), "s");
        assert_eq!(object(ObjectKind::View).name(), "t");
        assert_eq!(routine_target(ObjectKind::Function, "").name(), "f");
    }

    #[test]
    fn new_names_are_checked() {
        assert_eq!(validate_name("  fresh ", "old").as_deref(), Ok("fresh"));
        assert_eq!(validate_name("", "old"), Err("Enter a name.".into()));
        assert_eq!(validate_name("   ", "old"), Err("Enter a name.".into()));
        assert_eq!(
            validate_name("old", "old"),
            Err("That is already the name.".into())
        );
        assert_eq!(
            validate_name(" old ", "old"),
            Err("That is already the name.".into())
        );
        assert!(validate_name("a\0b", "old").unwrap_err().contains("NUL"));
        let max = "a".repeat(MAX_NAME_BYTES);
        assert!(validate_name(&max, "old").is_ok());
        let long = "a".repeat(MAX_NAME_BYTES + 1);
        assert!(validate_name(&long, "old").unwrap_err().contains("64"));
        // The limit is in bytes, not characters.
        let wide = "é".repeat(MAX_NAME_BYTES / 2 + 1);
        assert!(validate_name(&wide, "old").is_err());
        // Capitals and spaces are legal names, quoted when used.
        assert_eq!(validate_name("My Table", "old").as_deref(), Ok("My Table"));
    }

    #[test]
    fn maintenance_applies_only_where_the_server_accepts_it() {
        use ObjectKind::*;
        let applies = |op: Maintenance| -> Vec<ObjectKind> {
            [
                Table,
                PartitionedTable,
                ForeignTable,
                View,
                MaterializedView,
                Sequence,
                Function,
                Procedure,
                Aggregate,
                EnumType,
                CompositeType,
                DomainType,
            ]
            .into_iter()
            .filter(|k| op.applies_to(*k))
            .collect()
        };
        assert_eq!(
            applies(Maintenance::Analyze),
            [Table, PartitionedTable, ForeignTable, MaterializedView]
        );
        assert_eq!(
            applies(Maintenance::Vacuum),
            [Table, PartitionedTable, MaterializedView]
        );
        assert_eq!(applies(Maintenance::Truncate), [Table, PartitionedTable]);
        assert_eq!(
            Maintenance::ALL.map(Maintenance::label),
            ["Analyze", "Vacuum", "Truncate"]
        );
    }

    #[test]
    fn maintenance_statements() {
        let sql = |op, k| maintenance_sql(op, &object(k));
        assert_eq!(
            sql(Maintenance::Analyze, ObjectKind::Table).as_deref(),
            Some(r#"analyze "s"."t""#)
        );
        assert_eq!(
            sql(Maintenance::Vacuum, ObjectKind::MaterializedView).as_deref(),
            Some(r#"vacuum "s"."t""#)
        );
        assert_eq!(
            sql(Maintenance::Truncate, ObjectKind::PartitionedTable).as_deref(),
            Some(r#"truncate table "s"."t""#)
        );
        // Refused kinds and non-object targets give nothing.
        assert_eq!(sql(Maintenance::Truncate, ObjectKind::View), None);
        assert_eq!(sql(Maintenance::Vacuum, ObjectKind::ForeignTable), None);
        assert_eq!(sql(Maintenance::Analyze, ObjectKind::Sequence), None);
        assert_eq!(
            maintenance_sql(Maintenance::Analyze, &Target::Schema("s".into())),
            None
        );
        assert_eq!(
            maintenance_sql(
                Maintenance::Vacuum,
                &routine_target(ObjectKind::Function, "")
            ),
            None
        );
        // Names are quoted like everywhere else.
        let odd = Target::Object {
            schema: "A b".into(),
            name: "c\"d".into(),
            kind: ObjectKind::Table,
        };
        assert_eq!(
            maintenance_sql(Maintenance::Truncate, &odd).as_deref(),
            Some(r#"truncate table "A b"."c""d""#)
        );
    }

    #[tokio::test]
    async fn analyzes_vacuums_and_truncates_live() {
        let Some((client, _)) = crate::connect::test_connection().await else {
            return;
        };
        client
            .batch_execute(
                "drop schema if exists pgroove_mt cascade; create schema pgroove_mt; \
                 create table pgroove_mt.parent (id int primary key); \
                 create table pgroove_mt.child (id int references pgroove_mt.parent(id)); \
                 create table pgroove_mt.t as select generate_series(1, 1000) as id; \
                 create materialized view pgroove_mt.m as select id from pgroove_mt.t; \
                 insert into pgroove_mt.parent values (1); insert into pgroove_mt.child values (1)",
            )
            .await
            .unwrap();
        let target = |name: &str, kind| Target::Object {
            schema: "pgroove_mt".into(),
            name: name.into(),
            kind,
        };
        let count = |name: &'static str| {
            let client = &client;
            async move {
                client
                    .query_one(&format!("select count(*) from pgroove_mt.{name}"), &[])
                    .await
                    .unwrap()
                    .get::<_, i64>(0)
            }
        };
        // Analyze fills in the statistics; vacuum runs outside a transaction block, as it must.
        let t = target("t", ObjectKind::Table);
        run(&client, &maintenance_sql(Maintenance::Analyze, &t).unwrap())
            .await
            .unwrap();
        let tuples: f32 = client
            .query_one(
                "select reltuples from pg_class where oid = 'pgroove_mt.t'::regclass",
                &[],
            )
            .await
            .unwrap()
            .get(0);
        assert_eq!(tuples as i64, 1000, "analyze should have counted the rows");
        run(&client, &maintenance_sql(Maintenance::Vacuum, &t).unwrap())
            .await
            .unwrap();
        let m = target("m", ObjectKind::MaterializedView);
        run(&client, &maintenance_sql(Maintenance::Analyze, &m).unwrap())
            .await
            .unwrap();
        run(&client, &maintenance_sql(Maintenance::Vacuum, &m).unwrap())
            .await
            .unwrap();
        assert_eq!(count("t").await, 1000, "neither touches the rows");
        // Truncate empties the table.
        run(
            &client,
            &maintenance_sql(Maintenance::Truncate, &t).unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(count("t").await, 0);
        // A table a foreign key points at is refused, and nothing is lost.
        let parent = target("parent", ObjectKind::Table);
        let err = run(
            &client,
            &maintenance_sql(Maintenance::Truncate, &parent).unwrap(),
        )
        .await
        .unwrap_err();
        assert!(format!("{err:#}").contains("foreign key"), "{err:#}");
        assert_eq!(count("parent").await, 1);
        client
            .batch_execute("drop schema pgroove_mt cascade")
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn renames_and_drops_every_kind_live() {
        let Some((client, _)) = crate::connect::test_connection().await else {
            return;
        };
        client
            .batch_execute(
                "drop schema if exists pgroove_ddl cascade; drop schema if exists pgroove_ddl2 cascade; \
                 create schema pgroove_ddl; \
                 create table pgroove_ddl.t (id int); \
                 create table pgroove_ddl.p (id int) partition by range (id); \
                 create view pgroove_ddl.v as select id from pgroove_ddl.t; \
                 create materialized view pgroove_ddl.m as select id from pgroove_ddl.t; \
                 create sequence pgroove_ddl.s; \
                 create type pgroove_ddl.mood as enum ('a'); \
                 create type pgroove_ddl.pair as (a int, b int); \
                 create domain pgroove_ddl.posint as int; \
                 create function pgroove_ddl.add(a int, b int) returns int language sql as 'select a + b'; \
                 create function pgroove_ddl.add(a text) returns text language sql as 'select a'; \
                 create procedure pgroove_ddl.noop() language sql as ''; \
                 create aggregate pgroove_ddl.mysum(int) (sfunc = int4pl, stype = int, initcond = '0'); \
                 create aggregate pgroove_ddl.cnt(*) (sfunc = int8inc, stype = int8, initcond = '0')",
            )
            .await
            .unwrap();
        let names = |objs: Vec<crate::catalog::SchemaObject>| -> Vec<String> {
            objs.into_iter().map(|o| o.name).collect()
        };

        // Rename every plain kind, then check the catalog shows the new names.
        for (name, kind) in [
            ("t", ObjectKind::Table),
            ("p", ObjectKind::PartitionedTable),
            ("v", ObjectKind::View),
            ("m", ObjectKind::MaterializedView),
            ("s", ObjectKind::Sequence),
            ("mood", ObjectKind::EnumType),
            ("pair", ObjectKind::CompositeType),
            ("posint", ObjectKind::DomainType),
        ] {
            let target = Target::Object {
                schema: "pgroove_ddl".into(),
                name: name.into(),
                kind,
            };
            let sql = rename_sql(&target, &format!("{name} 2")).unwrap();
            run(&client, &sql)
                .await
                .unwrap_or_else(|e| panic!("{sql}: {e:#}"));
        }
        let listed = names(
            crate::catalog::objects(&client, "pgroove_ddl")
                .await
                .unwrap(),
        );
        for old in ["t", "p", "v", "m", "s", "mood", "pair", "posint"] {
            assert!(
                !listed.contains(&old.to_string()),
                "{old} should be renamed"
            );
            assert!(
                listed.contains(&format!("{old} 2")),
                "{old} 2 missing in {listed:?}"
            );
        }

        // Routines: two functions share a name; the lookup picks the one the tree listed.
        let r = routine(&client, "pgroove_ddl", "add(a integer, b integer)")
            .await
            .unwrap();
        assert_eq!(
            r,
            Routine {
                name: "add".into(),
                args: "a integer, b integer".into()
            }
        );
        assert!(routine(&client, "pgroove_ddl", "add(zzz)").await.is_err());
        let f = Target::Routine {
            schema: "pgroove_ddl".into(),
            name: r.name,
            args: r.args,
            kind: ObjectKind::Function,
        };
        run(&client, &rename_sql(&f, "plus").unwrap())
            .await
            .unwrap();
        let listed = names(
            crate::catalog::objects(&client, "pgroove_ddl")
                .await
                .unwrap(),
        );
        assert!(
            listed.contains(&"plus(a integer, b integer)".to_string()),
            "{listed:?}"
        );
        assert!(
            listed.contains(&"add(a text)".to_string()),
            "the other add is untouched"
        );

        // The aggregate over no arguments needs `(*)`.
        let cnt = routine(&client, "pgroove_ddl", "cnt(*)").await;
        let cnt = match cnt {
            Ok(r) => r,
            Err(_) => routine(&client, "pgroove_ddl", "cnt()").await.unwrap(),
        };
        let agg = Target::Routine {
            schema: "pgroove_ddl".into(),
            name: cnt.name,
            args: cnt.args,
            kind: ObjectKind::Aggregate,
        };
        run(&client, &drop_sql(&agg).unwrap()).await.unwrap();
        let mysum = routine(&client, "pgroove_ddl", "mysum(integer)")
            .await
            .unwrap();
        let agg = Target::Routine {
            schema: "pgroove_ddl".into(),
            name: mysum.name,
            args: mysum.args,
            kind: ObjectKind::Aggregate,
        };
        run(&client, &rename_sql(&agg, "total").unwrap())
            .await
            .unwrap();
        run(&client, &drop_sql(&agg_renamed()).unwrap())
            .await
            .unwrap();
        let noop = routine(&client, "pgroove_ddl", "noop()").await.unwrap();
        let proc_ = Target::Routine {
            schema: "pgroove_ddl".into(),
            name: noop.name,
            args: noop.args,
            kind: ObjectKind::Procedure,
        };
        run(&client, &drop_sql(&proc_).unwrap()).await.unwrap();

        // A drop with a dependent object is refused, and says why (no cascade).
        let dep = Target::Object {
            schema: "pgroove_ddl".into(),
            name: "t 2".into(),
            kind: ObjectKind::Table,
        };
        let err = run(&client, &drop_sql(&dep).unwrap()).await.unwrap_err();
        assert!(format!("{err:#}").contains("depend"), "{err:#}");
        // Drop the dependents, then the table, then the rest, then the schema.
        for (name, kind) in [
            ("v 2", ObjectKind::View),
            ("m 2", ObjectKind::MaterializedView),
            ("t 2", ObjectKind::Table),
            ("p 2", ObjectKind::PartitionedTable),
            ("s 2", ObjectKind::Sequence),
            ("mood 2", ObjectKind::EnumType),
            ("pair 2", ObjectKind::CompositeType),
            ("posint 2", ObjectKind::DomainType),
        ] {
            let t = Target::Object {
                schema: "pgroove_ddl".into(),
                name: name.into(),
                kind,
            };
            let sql = drop_sql(&t).unwrap();
            run(&client, &sql)
                .await
                .unwrap_or_else(|e| panic!("{sql}: {e:#}"));
        }
        // A schema that still holds objects is refused; once empty it renames and drops.
        let schema = Target::Schema("pgroove_ddl".into());
        let err = run(&client, &drop_sql(&schema).unwrap()).await.unwrap_err();
        assert!(format!("{err:#}").contains("depend"), "{err:#}");
        client
            .batch_execute(
                "drop function pgroove_ddl.plus(int, int); drop function pgroove_ddl.add(text)",
            )
            .await
            .unwrap();
        run(&client, &rename_sql(&schema, "pgroove_ddl2").unwrap())
            .await
            .unwrap();
        let renamed = Target::Schema("pgroove_ddl2".into());
        run(&client, &drop_sql(&renamed).unwrap()).await.unwrap();
        let left = crate::catalog::schemas(&client).await.unwrap();
        assert!(
            !left.iter().any(|s| s.starts_with("pgroove_ddl")),
            "{left:?}"
        );
    }

    fn agg_renamed() -> Target {
        Target::Routine {
            schema: "pgroove_ddl".into(),
            name: "total".into(),
            args: "integer".into(),
            kind: ObjectKind::Aggregate,
        }
    }
}
