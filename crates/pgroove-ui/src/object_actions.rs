//! Renaming and dropping schemas and the objects in them, from the tree's context menus. Both
//! are offered only in editable mode. Each asks first: a rename takes the new name in a
//! dialog, a drop shows the exact statement it will run. The statements come from
//! `pgroove_core::ddl`; they run on the database's connection, the server's answer (including
//! a refusal because something depends on the object) is shown as it is, and on success the
//! tree is reloaded and the tabs that showed the object are closed.

use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::*;
use gpui_kit::*;
use pgroove_core::catalog::ObjectKind;
use pgroove_core::ddl::{self, Target};

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

/// The message shown when a change went through.
pub fn done_message(target: &Target, new: Option<&str>) -> String {
    match new {
        Some(new) => format!("Renamed {} to {}", target.describe(), new),
        None => format!("Dropped {}", target.describe()),
    }
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
                                ws.run_ddl(sid, target, Some(new), window, cx)
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
        let Some(statement) = ddl::drop_sql(&target) else {
            return;
        };
        let this = cx.entity();
        let title = format!("Drop {}", target.describe());
        window.open_dialog(cx, move |dialog, _, cx| {
            let (this, target) = (this.clone(), target.clone());
            let theme = cx.theme().clone();
            dialog
                .title(title.clone())
                .w(px(520.))
                .child(
                    v_flex()
                        .gap_2()
                        .child(div().text_sm().child(
                            "This runs the statement below and can't be undone. Objects that \
                             depend on it are not dropped: if there are any, the server \
                             refuses and says which.",
                        ))
                        .child(
                            div()
                                .p_2()
                                .rounded(px(4.))
                                .bg(theme.muted)
                                .font_family(theme.mono_font_family.clone())
                                .text_sm()
                                .child(statement.clone()),
                        ),
                )
                .actions("Drop", true, move |_, window, cx| {
                    let target = target.clone();
                    this.update(cx, |ws, cx| ws.run_ddl(sid, target, None, window, cx));
                    true
                })
        });
    }

    /// Runs the rename (`new` is the name) or the drop (`None`), then reports and reloads.
    fn run_ddl(
        &mut self,
        sid: usize,
        target: Target,
        new: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(client) = self.client_of(sid) else {
            return;
        };
        let sql = match &new {
            Some(name) => ddl::rename_sql(&target, name),
            None => ddl::drop_sql(&target),
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
                    window.push_notification(done_message(&target, new.as_deref()), cx);
                    this.close_tabs_of(sid, &target, cx);
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
    use super::{Subject, affects_table, done_message, plain_target, routine_target};
    use pgroove_core::catalog::ObjectKind;
    use pgroove_core::ddl::{self, Target};

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
    fn done_messages_name_the_object() {
        let t = Target::Object {
            schema: "s".into(),
            name: "t".into(),
            kind: ObjectKind::Table,
        };
        assert_eq!(done_message(&t, None), "Dropped table \"s\".\"t\"");
        assert_eq!(
            done_message(&t, Some("u")),
            "Renamed table \"s\".\"t\" to u"
        );
    }
}
