//! Renaming and dropping schemas and the objects in them, from the tree's context menus. Both
//! are offered only in editable mode. Each asks first: a rename takes the new name in a
//! dialog, a drop shows the exact statement it will run. The statements come from
//! `pgroove_core::ddl`; they run on the database's connection, the server's answer (including
//! a refusal because something depends on the object) is shown as it is, and on success the
//! tree is reloaded and the tabs that showed the object are closed.

use std::cell::Cell;
use std::rc::Rc;

use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::*;
use gpui_kit::*;
use pgroove_core::catalog::ObjectKind;
use pgroove_core::ddl::{self, Maintenance, Target};

use crate::dialogs::DialogActions;
use crate::tabs::TabKind;
use crate::workspace::{Workspace, flatten};

/// What a menu entry was opened on: the tree's own description, before a routine's signature
/// is looked up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Subject {
    Schema(String),
    /// `name` is the tree's entry: for a routine it includes the argument list.
    Object {
        schema: String,
        name: String,
        kind: ObjectKind,
    },
}

impl Subject {
    /// Whether finding the exact target needs the catalog (routines do).
    pub fn is_routine(&self) -> bool {
        matches!(
            self,
            Subject::Object {
                kind: ObjectKind::Function | ObjectKind::Procedure | ObjectKind::Aggregate,
                ..
            }
        )
    }
}

/// The target of a subject that needs no lookup; `None` for a routine.
pub fn plain_target(subject: &Subject) -> Option<Target> {
    match subject {
        Subject::Schema(name) => Some(Target::Schema(name.clone())),
        Subject::Object { .. } if subject.is_routine() => None,
        Subject::Object { schema, name, kind } => Some(Target::Object {
            schema: schema.clone(),
            name: name.clone(),
            kind: *kind,
        }),
    }
}

/// The target of a routine once the catalog gave its bare name and identity arguments.
pub fn routine_target(subject: &Subject, found: ddl::Routine) -> Option<Target> {
    match subject {
        Subject::Object { schema, kind, .. } if subject.is_routine() => Some(Target::Routine {
            schema: schema.clone(),
            name: found.name,
            args: found.args,
            kind: *kind,
        }),
        _ => None,
    }
}

/// Which table tabs show something that a change to `target` removes or renames: the table
/// itself, or every table of a schema. Routines, types and sequences have no tab.
pub fn affects_table(target: &Target, schema: &str, table: &str) -> bool {
    match target {
        Target::Schema(name) => name == schema,
        Target::Object {
            schema: s,
            name,
            kind,
        } => kind.is_relation() && s == schema && name == table,
        Target::Routine { .. } => false,
    }
}

/// The message shown when a maintenance operation went through.
pub fn maintenance_message(op: Maintenance, target: &Target, cascade: bool) -> String {
    let done = match op {
        Maintenance::Analyze => "Analyzed",
        Maintenance::Vacuum => "Vacuumed",
        Maintenance::Truncate => "Truncated",
    };
    let tail = if cascade && op.has_cascade() {
        " and the tables that reference it"
    } else {
        ""
    };
    format!("{done} {}{tail}", target.describe())
}

/// The message shown while a maintenance operation runs (a vacuum can take a while).
pub fn maintenance_started(op: Maintenance, target: &Target) -> String {
    let doing = match op {
        Maintenance::Analyze => "Analyzing",
        Maintenance::Vacuum => "Vacuuming",
        Maintenance::Truncate => "Truncating",
    };
    format!("{doing} {}…", target.describe())
}

/// The message shown when a change went through.
pub fn done_message(target: &Target, new: Option<&str>, cascade: bool) -> String {
    match new {
        Some(new) => format!("Renamed {} to {}", target.describe(), new),
        None if cascade => format!(
            "Dropped {} and everything that depended on it",
            target.describe()
        ),
        None => format!("Dropped {}", target.describe()),
    }
}

/// What the cascade checkbox says, for a drop or for a truncate.
pub fn cascade_label(truncate: bool) -> &'static str {
    if truncate {
        "Cascade: also empty every table that references this one"
    } else {
        "Cascade: also drop everything that depends on it"
    }
}

/// The explanation in the dialog. With cascade off the server refuses when something depends on
/// the object; with it on, the dependents are removed too and nothing is listed beforehand.
pub fn confirm_text(truncate: bool, cascade: bool) -> &'static str {
    match (truncate, cascade) {
        (false, false) => {
            "This runs the statement below and can't be undone. Objects that depend on it are not dropped: if there are any, the server refuses and says which."
        }
        (false, true) => {
            "This runs the statement below and can't be undone. Everything that depends on the object, such as views, foreign keys and functions that use it, is dropped with it, and you are not shown which."
        }
        (true, false) => {
            "This deletes every row of the table and can't be undone. A table that another table's foreign key points at is not truncated: the server refuses and says which."
        }
        (true, true) => {
            "This deletes every row of the table and of every table that references it, and can't be undone. You are not shown which tables that are."
        }
    }
}

/// A checkbox bound to a flag the dialog's buttons read; the dialog is redrawn when it flips.
fn cascade_checkbox(flag: &Rc<Cell<bool>>, truncate: bool) -> Checkbox {
    let shared = flag.clone();
    Checkbox::new("cascade")
        .label(cascade_label(truncate))
        .checked(flag.get())
        .on_click(move |on, window, _| {
            shared.set(*on);
            window.refresh();
        })
}

impl Workspace {
    /// Asks for a new name for the subject, then renames it.
    pub(crate) fn rename_object(
        &mut self,
        sid: usize,
        subject: Subject,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.with_target(sid, subject, window, cx, move |ws, target, window, cx| {
            ws.ask_new_name(sid, target, window, cx)
        });
    }

    /// Shows the statement a drop would run and drops the subject when confirmed.
    pub(crate) fn drop_object(
        &mut self,
        sid: usize,
        subject: Subject,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.with_target(sid, subject, window, cx, move |ws, target, window, cx| {
            ws.confirm_drop(sid, target, window, cx)
        });
    }

    /// Runs `op` on a table. Analyze and vacuum are harmless and run at once; truncate shows its
    /// statement and waits for a confirmation, since it empties the table for good.
    pub(crate) fn maintain_object(
        &mut self,
        sid: usize,
        subject: Subject,
        op: Maintenance,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(target) = plain_target(&subject) else {
            return;
        };
        if ddl::maintenance_sql(op, &target, false).is_none() {
            return;
        }
        if op != Maintenance::Truncate {
            return self.run_maintenance(sid, target, op, false, window, cx);
        }
        let this = cx.entity();
        let title = format!("Truncate {}", target.describe());
        // Off until ticked, every time the dialog opens.
        let cascade = Rc::new(Cell::new(false));
        window.open_dialog(cx, move |dialog, _, cx| {
            let (this, target, cascade) = (this.clone(), target.clone(), cascade.clone());
            let theme = cx.theme().clone();
            let statement = ddl::maintenance_sql(Maintenance::Truncate, &target, cascade.get())
                .unwrap_or_default();
            dialog
                .title(title.clone())
                .w(px(520.))
                .child(
                    v_flex()
                        .gap_2()
                        .child(div().text_sm().child(confirm_text(true, cascade.get())))
                        .child(cascade_checkbox(&cascade, true))
                        .child(
                            div()
                                .p_2()
                                .rounded(px(4.))
                                .bg(theme.muted)
                                .font_family(theme.mono_font_family.clone())
                                .text_sm()
                                .child(statement),
                        ),
                )
                .actions("Truncate", true, move |_, window, cx| {
                    let (target, cascade) = (target.clone(), cascade.get());
                    this.update(cx, |ws, cx| {
                        ws.run_maintenance(sid, target, Maintenance::Truncate, cascade, window, cx)
                    });
                    true
                })
        });
    }

    fn run_maintenance(
        &mut self,
        sid: usize,
        target: Target,
        op: Maintenance,
        cascade: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(client) = self.client_of(sid) else {
            return;
        };
        let Some(sql) = ddl::maintenance_sql(op, &target, cascade) else {
            return;
        };
        window.push_notification(maintenance_started(op, &target), cx);
        let task = self
            .runtime
            .spawn(async move { ddl::run(&client, &sql).await });
        cx.spawn_in(window, async move |this, cx| {
            let result = flatten(task.await);
            this.update_in(cx, |this, window, cx| match result {
                Ok(()) => {
                    window.push_notification(maintenance_message(op, &target, cascade), cx);
                    // Row counts and sizes in the tree, and the rows an open tab shows, changed.
                    // A cascade empties tables we cannot name, so every table tab of the database
                    // reloads then.
                    if cascade && op.has_cascade() {
                        this.reload_all_tabs_of(sid, cx);
                    } else {
                        this.reload_tabs_of(sid, &target, cx);
                    }
                    this.refresh_session(sid, cx);
                }
                Err(e) => window.push_notification(Notification::error(e), cx),
            })
            .ok();
        })
        .detach();
    }

    /// Reloads the table tabs that show `target`, for after an operation that changed its rows.
    fn reload_tabs_of(&mut self, sid: usize, target: &Target, cx: &mut Context<Self>) {
        for t in &self.tabs {
            if let TabKind::Table(v, s) = t
                && *s == sid
            {
                let table = v.read(cx).table().clone();
                if affects_table(target, &table.schema, &table.name) {
                    v.update(cx, |v, cx| v.reload(cx));
                }
            }
        }
    }

    /// Reloads every table tab of the database.
    fn reload_all_tabs_of(&mut self, sid: usize, cx: &mut Context<Self>) {
        for t in &self.tabs {
            if let TabKind::Table(v, s) = t
                && *s == sid
            {
                v.update(cx, |v, cx| v.reload(cx));
            }
        }
    }

    /// Resolves the subject to a target (a routine needs a catalog lookup for its signature)
    /// and hands it on.
    fn with_target(
        &mut self,
        sid: usize,
        subject: Subject,
        window: &mut Window,
        cx: &mut Context<Self>,
        then: impl FnOnce(&mut Workspace, Target, &mut Window, &mut Context<Workspace>) + 'static,
    ) {
        if let Some(target) = plain_target(&subject) {
            return then(self, target, window, cx);
        }
        let Some(client) = self.client_of(sid) else {
            return;
        };
        let Subject::Object { schema, name, .. } = subject.clone() else {
            return;
        };
        let task = self
            .runtime
            .spawn(async move { ddl::routine(&client, &schema, &name).await });
        cx.spawn_in(window, async move |this, cx| {
            let found = flatten(task.await);
            this.update_in(cx, |this, window, cx| {
                match found.map(|f| routine_target(&subject, f)) {
                    Ok(Some(target)) => then(this, target, window, cx),
                    Ok(None) => {}
                    Err(e) => window.push_notification(Notification::error(e), cx),
                }
            })
            .ok();
        })
        .detach();
    }

    fn ask_new_name(
        &mut self,
        sid: usize,
        target: Target,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let this = cx.entity();
        let old = target.name().to_string();
        let input = cx.new(|cx| InputState::new(window, cx).default_value(old.clone()));
        let title = format!("Rename {}", target.describe());
        window.open_dialog(cx, move |dialog, _, _| {
            let (this, input, target, old) =
                (this.clone(), input.clone(), target.clone(), old.clone());
            dialog
                .title(title.clone())
                .w(px(460.))
                .child(
                    v_flex()
                        .gap_2()
                        .child(div().text_sm().child("New name"))
                        .child(Input::new(&input)),
                )
                .actions("Rename", false, move |_, window, cx| {
                    let typed = input.read(cx).value().to_string();
                    match ddl::validate_name(&typed, &old) {
                        Ok(new) => {
                            let target = target.clone();
                            this.update(cx, |ws, cx| {
                                ws.run_ddl(sid, target, Some(new), false, window, cx)
                            });
                            true
                        }
                        Err(message) => {
                            window.push_notification(Notification::error(message), cx);
                            false
                        }
                    }
                })
        });
    }

    fn confirm_drop(
        &mut self,
        sid: usize,
        target: Target,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if ddl::drop_sql(&target, false).is_none() {
            return;
        }
        let this = cx.entity();
        let title = format!("Drop {}", target.describe());
        // Off until ticked, every time the dialog opens.
        let cascade = Rc::new(Cell::new(false));
        window.open_dialog(cx, move |dialog, _, cx| {
            let (this, target, cascade) = (this.clone(), target.clone(), cascade.clone());
            let theme = cx.theme().clone();
            let statement = ddl::drop_sql(&target, cascade.get()).unwrap_or_default();
            dialog
                .title(title.clone())
                .w(px(520.))
                .child(
                    v_flex()
                        .gap_2()
                        .child(div().text_sm().child(confirm_text(false, cascade.get())))
                        .child(cascade_checkbox(&cascade, false))
                        .child(
                            div()
                                .p_2()
                                .rounded(px(4.))
                                .bg(theme.muted)
                                .font_family(theme.mono_font_family.clone())
                                .text_sm()
                                .child(statement),
                        ),
                )
                .actions("Drop", true, move |_, window, cx| {
                    let (target, cascade) = (target.clone(), cascade.get());
                    this.update(cx, |ws, cx| {
                        ws.run_ddl(sid, target, None, cascade, window, cx)
                    });
                    true
                })
        });
    }

    /// Runs the rename (`new` is the name) or the drop (`None`, with `cascade` if ticked), then
    /// reports and reloads.
    fn run_ddl(
        &mut self,
        sid: usize,
        target: Target,
        new: Option<String>,
        cascade: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(client) = self.client_of(sid) else {
            return;
        };
        let sql = match &new {
            Some(name) => ddl::rename_sql(&target, name),
            None => ddl::drop_sql(&target, cascade),
        };
        let Some(sql) = sql else {
            return;
        };
        let task = self
            .runtime
            .spawn(async move { ddl::run(&client, &sql).await });
        cx.spawn_in(window, async move |this, cx| {
            let result = flatten(task.await);
            this.update_in(cx, |this, window, cx| match result {
                Ok(()) => {
                    window.push_notification(done_message(&target, new.as_deref(), cascade), cx);
                    this.close_tabs_of(sid, &target, cx);
                    // A cascade drops objects we cannot name; a table tab of one of them now
                    // reloads into the server's "does not exist" instead of showing stale rows.
                    if cascade && new.is_none() {
                        this.reload_all_tabs_of(sid, cx);
                    }
                    this.refresh_session(sid, cx);
                }
                Err(e) => window.push_notification(Notification::error(e), cx),
            })
            .ok();
        })
        .detach();
    }

    /// Closes the table tabs of the database that showed what `target` removed or renamed, so
    /// none is left pointing at something that is gone. Query tabs keep their text.
    fn close_tabs_of(&mut self, sid: usize, target: &Target, cx: &mut Context<Self>) {
        let keep: Vec<bool> = self
            .tabs
            .iter()
            .map(|t| match t {
                TabKind::Table(v, s) if *s == sid => {
                    let table = v.read(cx).table();
                    !affects_table(target, &table.schema, &table.name)
                }
                _ => true,
            })
            .collect();
        if keep.iter().all(|k| *k) {
            return;
        }
        self.retain_tabs(|i| keep.get(i).copied().unwrap_or(true), cx);
    }
}

#[cfg(test)]
mod tests {
    // Explicit imports: a glob of `super::*` would bring in `gpui_kit::test`, which shadows the
    // built-in `#[test]`.
    use super::{
        Subject, affects_table, cascade_label, confirm_text, done_message, maintenance_message,
        maintenance_started, plain_target, routine_target,
    };
    use pgroove_core::catalog::ObjectKind;
    use pgroove_core::ddl::{self, Maintenance, Target};

    fn object(kind: ObjectKind) -> Subject {
        Subject::Object {
            schema: "s".into(),
            name: "t".into(),
            kind,
        }
    }

    #[test]
    fn plain_subjects_need_no_lookup() {
        assert_eq!(
            plain_target(&Subject::Schema("s".into())),
            Some(Target::Schema("s".into()))
        );
        assert_eq!(
            plain_target(&object(ObjectKind::Table)),
            Some(Target::Object {
                schema: "s".into(),
                name: "t".into(),
                kind: ObjectKind::Table
            })
        );
        for kind in [
            ObjectKind::Function,
            ObjectKind::Procedure,
            ObjectKind::Aggregate,
        ] {
            assert!(object(kind).is_routine());
            assert_eq!(plain_target(&object(kind)), None);
        }
        assert!(!object(ObjectKind::View).is_routine());
        assert!(!Subject::Schema("s".into()).is_routine());
    }

    #[test]
    fn a_routine_target_takes_the_catalog_name_and_arguments() {
        let found = ddl::Routine {
            name: "add".into(),
            args: "a integer".into(),
        };
        let subject = Subject::Object {
            schema: "s".into(),
            name: "add(a integer)".into(),
            kind: ObjectKind::Function,
        };
        assert_eq!(
            routine_target(&subject, found.clone()),
            Some(Target::Routine {
                schema: "s".into(),
                name: "add".into(),
                args: "a integer".into(),
                kind: ObjectKind::Function
            })
        );
        // Anything that is not a routine has nothing to resolve.
        assert_eq!(
            routine_target(&object(ObjectKind::Table), found.clone()),
            None
        );
        assert_eq!(routine_target(&Subject::Schema("s".into()), found), None);
    }

    #[test]
    fn only_the_tabs_of_what_changed_close() {
        let table = Target::Object {
            schema: "s".into(),
            name: "t".into(),
            kind: ObjectKind::Table,
        };
        assert!(affects_table(&table, "s", "t"));
        assert!(!affects_table(&table, "s", "other"));
        assert!(!affects_table(&table, "other", "t"));
        let view = Target::Object {
            schema: "s".into(),
            name: "t".into(),
            kind: ObjectKind::View,
        };
        assert!(
            affects_table(&view, "s", "t"),
            "a view opens as a table tab"
        );
        // A sequence or type of the same name has no tab to close.
        let sequence = Target::Object {
            schema: "s".into(),
            name: "t".into(),
            kind: ObjectKind::Sequence,
        };
        assert!(!affects_table(&sequence, "s", "t"));
        let schema = Target::Schema("s".into());
        assert!(affects_table(&schema, "s", "anything"));
        assert!(!affects_table(&schema, "other", "anything"));
        let routine = Target::Routine {
            schema: "s".into(),
            name: "t".into(),
            args: String::new(),
            kind: ObjectKind::Function,
        };
        assert!(!affects_table(&routine, "s", "t"));
    }

    #[test]
    fn maintenance_messages_say_what_was_done() {
        let t = Target::Object {
            schema: "s".into(),
            name: "t".into(),
            kind: ObjectKind::Table,
        };
        let table = "table \"s\".\"t\"";
        assert_eq!(
            maintenance_message(Maintenance::Analyze, &t, false),
            format!("Analyzed {table}")
        );
        assert_eq!(
            maintenance_message(Maintenance::Vacuum, &t, false),
            format!("Vacuumed {table}")
        );
        assert_eq!(
            maintenance_message(Maintenance::Truncate, &t, false),
            format!("Truncated {table}")
        );
        // Only a truncate has a cascade to report.
        assert_eq!(
            maintenance_message(Maintenance::Truncate, &t, true),
            format!("Truncated {table} and the tables that reference it")
        );
        assert_eq!(
            maintenance_message(Maintenance::Vacuum, &t, true),
            format!("Vacuumed {table}")
        );
        assert_eq!(
            maintenance_started(Maintenance::Analyze, &t),
            format!("Analyzing {table}…")
        );
        assert_eq!(
            maintenance_started(Maintenance::Vacuum, &t),
            format!("Vacuuming {table}…")
        );
        assert_eq!(
            maintenance_started(Maintenance::Truncate, &t),
            format!("Truncating {table}…")
        );
    }

    #[test]
    fn done_messages_name_the_object() {
        let t = Target::Object {
            schema: "s".into(),
            name: "t".into(),
            kind: ObjectKind::Table,
        };
        assert_eq!(done_message(&t, None, false), "Dropped table \"s\".\"t\"");
        assert_eq!(
            done_message(&t, None, true),
            "Dropped table \"s\".\"t\" and everything that depended on it"
        );
        assert_eq!(
            done_message(&t, Some("u"), false),
            "Renamed table \"s\".\"t\" to u"
        );
        // A rename never cascades, whatever the flag.
        assert_eq!(
            done_message(&t, Some("u"), true),
            "Renamed table \"s\".\"t\" to u"
        );
    }

    #[test]
    fn the_dialog_says_what_the_cascade_does() {
        // The text follows the checkbox, for both a drop and a truncate.
        assert!(confirm_text(false, false).contains("the server refuses"));
        assert!(confirm_text(false, true).contains("is dropped with it"));
        assert!(confirm_text(true, false).contains("the server refuses"));
        assert!(confirm_text(true, true).contains("every table that references it"));
        for (truncate, cascade) in [(false, false), (false, true), (true, false), (true, true)] {
            assert!(confirm_text(truncate, cascade).contains("can't be undone"));
        }
        assert!(cascade_label(false).starts_with("Cascade"));
        assert!(cascade_label(true).contains("references"));
        assert_ne!(cascade_label(false), cascade_label(true));
    }
}
