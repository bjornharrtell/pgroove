use std::rc::Rc;

use gpui_kit::component::notification::Notification;
use gpui_kit::component::*;
use gpui_kit::*;
use pgroove_core::ConnectionProfile;
use pgroove_core::connect::connect;

use crate::connections::ConnectionForm;
use crate::dialogs::DialogActions;
use crate::workspace::{Workspace, flatten};

impl Workspace {
    fn save(
        &mut self,
        original: Option<&str>,
        profile: ConnectionProfile,
        password: &str,
    ) -> anyhow::Result<()> {
        let name = profile.name.clone();
        self.store.upsert(original, profile)?;
        if !password.is_empty() {
            self.secrets.set(&name, password)?;
        }
        if let Some(old) = original.filter(|o| *o != name) {
            if let Some(pw) = self.secrets.get(old)? {
                self.secrets.set(&name, &pw)?;
            }
            self.secrets.delete(old)?;
        }
        Ok(())
    }

    /// Asks before removing a saved connection, since that also drops its stored password.
    pub(crate) fn confirm_delete(
        &mut self,
        name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let this = cx.entity();
        window.open_dialog(cx, move |dialog, _, _| {
            let (this, name) = (this.clone(), name.clone());
            dialog
                .title("Delete connection")
                .w(px(420.))
                .child(div().text_sm().child(format!(
                    "Delete “{name}”? This removes the saved connection and its stored \
                     password, and closes any databases open on it. It can't be undone."
                )))
                .actions("Delete", true, move |_, _, cx| {
                    this.update(cx, |ws, cx| ws.delete(&name, cx));
                    true
                })
        });
    }

    fn delete(&mut self, name: &str, cx: &mut Context<Self>) {
        self.close_profile(name, cx);
        if let Err(e) = self
            .store
            .remove(name)
            .and_then(|_| self.secrets.delete(name))
        {
            log::error!("delete failed: {e:#}");
        }
        cx.notify();
    }

    /// Connects with the form's values and reports the outcome in a notification.
    fn test_connection(
        &mut self,
        values: crate::connections::FormValues,
        original: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Err(msg) = values.profile.validate() {
            window.push_notification(Notification::error(msg), cx);
            return;
        }
        let secrets = self.secrets.clone();
        let handle = window.window_handle();
        window.push_notification("Testing connection…", cx);
        let task = self.runtime.spawn(async move {
            let saved = match (values.password.is_empty(), original) {
                (true, Some(name)) => secrets.get(&name)?,
                _ => None,
            };
            let pw = (!values.password.is_empty())
                .then_some(values.password)
                .or(saved);
            let (client, version) = connect(&values.profile, pw.as_deref()).await?;
            drop(client);
            anyhow::Ok(version)
        });
        cx.spawn(async move |_, cx| {
            let msg = match flatten(task.await) {
                Ok(v) => Notification::success(format!("Connected to PostgreSQL {v}")),
                Err(e) => Notification::error(format!("Couldn't connect: {e}")),
            };
            cx.update(|cx| {
                handle
                    .update(cx, |_, window, cx| window.push_notification(msg, cx))
                    .ok();
            });
        })
        .detach();
    }

    /// Offers to import connections from DBeaver. At startup (`first_time`) it only asks when
    /// something new was found, and never again afterwards; from the palette it always reports.
    pub(crate) fn offer_dbeaver_import(
        &mut self,
        first_time: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let found = pgroove_core::import_dbeaver::detect();
        if self.show_dbeaver_dialog(found, !first_time, window, cx) && first_time {
            self.settings.dbeaver_import_offered = true;
            self.save_settings();
        }
    }

    /// Lets the user pick a DBeaver project (or workspace) folder anywhere, then offers its
    /// connections.
    pub(crate) fn import_dbeaver_from_folder(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: true,
            multiple: false,
            prompt: Some("Choose a DBeaver project folder".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = rx.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            this.update_in(cx, |this, window, cx| {
                let projects = pgroove_core::import_dbeaver::projects_in(&path);
                if projects.is_empty() {
                    window.push_notification(
                        format!("No DBeaver connection files found in {}", path.display()),
                        cx,
                    );
                    return;
                }
                let found = pgroove_core::import_dbeaver::load_projects(&projects);
                this.show_dbeaver_dialog(found, true, window, cx);
            })
            .ok();
        })
        .detach();
    }

    /// Opens the import dialog for what is new in `found`; false (after a notice when
    /// `report`) when there is nothing to import.
    fn show_dbeaver_dialog(
        &mut self,
        found: pgroove_core::import_dbeaver::Found,
        report: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let candidates =
            pgroove_core::import_dbeaver::only_new(found.candidates, self.store.profiles());
        if candidates.is_empty() {
            if report {
                window.push_notification("No new DBeaver connections found to import", cx);
            }
            return false;
        }
        let state = cx.new(|_| crate::dbeaver_dialog::ImportState::new(candidates));
        let this = cx.entity();
        window.open_dialog(cx, move |dialog, _, cx| {
            let (state, this) = (state.clone(), this.clone());
            dialog
                .title("Import from DBeaver")
                .w(px(520.))
                .child(crate::dbeaver_dialog::render(&state, cx))
                .actions("Import", true, move |_, window, cx| {
                    let picks = state.read(cx).picks();
                    let (done, errors) = this.update(cx, |ws, cx| {
                        let r = ws.import_connections(picks);
                        cx.notify();
                        r
                    });
                    let message = format!(
                        "Imported {done} connection{}",
                        if done == 1 { "" } else { "s" }
                    );
                    if errors.is_empty() {
                        window.push_notification(message, cx);
                    } else {
                        window.push_notification(
                            Notification::error(format!("{message}; {}", errors.join("; "))),
                            cx,
                        );
                    }
                    true
                })
        });
        true
    }

    /// Saves imported connections (and their passwords); returns how many went in and what failed.
    fn import_connections(
        &mut self,
        picks: Vec<pgroove_core::import_dbeaver::Candidate>,
    ) -> (usize, Vec<String>) {
        let (mut done, mut errors) = (0, Vec::new());
        for c in picks {
            let name = c.profile.name.clone();
            if let Err(e) = self.store.upsert(None, c.profile) {
                errors.push(format!("{name}: {e:#}"));
                continue;
            }
            done += 1;
            if let Some(pw) = c.password
                && let Err(e) = self.secrets.set(&name, &pw)
            {
                errors.push(format!("{name}: password not saved ({e:#})"));
            }
        }
        (done, errors)
    }

    pub(crate) fn open_form(
        &mut self,
        original: Option<ConnectionProfile>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let initial = original.clone().unwrap_or_default();
        let form = ConnectionForm::new(&initial, window, cx);
        let this = cx.entity();
        let original_name = original.map(|p| p.name);
        let title = if original_name.is_some() {
            "Edit connection"
        } else {
            "New connection"
        };
        let test = {
            let this = this.clone();
            let original = original_name.clone();
            Rc::new(
                move |values: crate::connections::FormValues, window: &mut Window, cx: &mut App| {
                    let original = original.clone();
                    this.update(cx, |ws, cx| {
                        ws.test_connection(values, original, window, cx)
                    });
                },
            )
        };
        window.open_dialog(cx, move |dialog, window, cx| {
            let form_ok = form.clone();
            let this = this.clone();
            let original_name = original_name.clone();
            dialog
                .title(title)
                .w(px(420.))
                .child(
                    // Scrolls when the window is too short for the whole form.
                    div()
                        .id("connection-form")
                        .max_h(window.viewport_size().height - px(220.))
                        .overflow_y_scroll()
                        .child(form.render(cx, test.clone())),
                )
                .actions("Save", true, move |_, window, cx| {
                    let values = form_ok.values(cx);
                    let result = values
                        .profile
                        .validate()
                        .map_err(anyhow::Error::msg)
                        .and_then(|_| {
                            this.update(cx, |ws, _| {
                                ws.save(
                                    original_name.as_deref(),
                                    values.profile.clone(),
                                    &values.password,
                                )
                            })
                        });
                    match result {
                        Ok(()) => {
                            this.update(cx, |ws, cx| {
                                // The "all databases" choice may have changed.
                                let ids: Vec<usize> = ws.ready_sessions().map(|s| s.id).collect();
                                for id in ids {
                                    ws.load_databases(id, cx);
                                }
                                cx.notify()
                            });
                            true
                        }
                        Err(e) => {
                            window.push_notification(Notification::error(format!("{e:#}")), cx);
                            false
                        }
                    }
                })
        });
    }
}
