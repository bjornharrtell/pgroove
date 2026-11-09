use std::sync::Arc;

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use pgroove_core::secrets::SecretStore;
use pgroove_core::{ConnectionProfile, ProfileStore, connect::connect};
use tokio::runtime::Runtime;

use crate::connections::ConnectionForm;

/// Connection lifecycle shown in the status bar.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Status {
    Idle,
    Connecting(String),
    Connected { name: String, version: String },
    Failed(String),
}

impl Status {
    pub fn text(&self) -> String {
        match self {
            Status::Idle => "Not connected".into(),
            Status::Connecting(n) => format!("Connecting to {n}…"),
            Status::Connected { name, version } => format!("{name} · PostgreSQL {version}"),
            Status::Failed(e) => format!("Couldn't connect: {e}"),
        }
    }
}

/// Top-level window view: sidebar, main area, status bar.
pub struct Workspace {
    store: ProfileStore,
    secrets: Arc<dyn SecretStore>,
    runtime: Arc<Runtime>,
    status: Status,
    client: Option<Arc<tokio_postgres::Client>>,
}

impl Workspace {
    pub fn new(store: ProfileStore, secrets: Arc<dyn SecretStore>, runtime: Arc<Runtime>) -> Self {
        Self {
            store,
            secrets,
            runtime,
            status: Status::Idle,
            client: None,
        }
    }

    fn connect(&mut self, profile: ConnectionProfile, cx: &mut Context<Self>) {
        self.client = None;
        self.status = Status::Connecting(profile.name.clone());
        cx.notify();
        let secrets = self.secrets.clone();
        let task = self.runtime.spawn(async move {
            let pw = secrets.get(&profile.name)?;
            let (client, version) = connect(&profile, pw.as_deref()).await?;
            anyhow::Ok((profile.name, client, version))
        });
        cx.spawn(async move |this, cx| {
            let result = match task.await {
                Ok(r) => r,
                Err(e) => Err(e.into()),
            };
            this.update(cx, |this, cx| {
                match result {
                    Ok((name, client, version)) => {
                        this.client = Some(Arc::new(client));
                        this.status = Status::Connected { name, version };
                    }
                    Err(e) => {
                        log::warn!("connect failed: {e:#}");
                        this.status = Status::Failed(format!("{e:#}"));
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn disconnect(&mut self, cx: &mut Context<Self>) {
        self.client = None;
        self.status = Status::Idle;
        cx.notify();
    }

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

    fn delete(&mut self, name: &str, cx: &mut Context<Self>) {
        if let Err(e) = self
            .store
            .remove(name)
            .and_then(|_| self.secrets.delete(name))
        {
            log::error!("delete failed: {e:#}");
        }
        cx.notify();
    }

    fn open_form(
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
        window.open_dialog(cx, move |dialog, _, _| {
            let form_ok = form.clone();
            let this = this.clone();
            let original_name = original_name.clone();
            dialog
                .title(title)
                .w(px(420.))
                .child(form.render())
                .on_ok(move |_, window, cx| {
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
                            this.update(cx, |_, cx| cx.notify());
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

    fn render_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let active = match &self.status {
            Status::Connected { name, .. } | Status::Connecting(name) => Some(name.clone()),
            _ => None,
        };
        let rows = self
            .store
            .profiles()
            .iter()
            .cloned()
            .enumerate()
            .map(|(i, p)| {
                let is_active = active.as_deref() == Some(p.name.as_str());
                let (pc, pe, pd) = (p.clone(), p.clone(), p.name.clone());
                h_flex()
                    .id(("conn", i))
                    .gap_1()
                    .px_2()
                    .py_1()
                    .items_center()
                    .when(is_active, |d| d.bg(cx.theme().accent))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(div().text_sm().child(p.name.clone()))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(p.label()),
                            ),
                    )
                    .child(
                        Button::new(("connect", i))
                            .ghost()
                            .xsmall()
                            .icon(if is_active {
                                IconName::Unplug
                            } else {
                                IconName::Plug
                            })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if is_active {
                                    this.disconnect(cx)
                                } else {
                                    this.connect(pc.clone(), cx)
                                }
                            })),
                    )
                    .child(
                        Button::new(("edit", i))
                            .ghost()
                            .xsmall()
                            .icon(IconName::Settings)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.open_form(Some(pe.clone()), window, cx)
                            })),
                    )
                    .child(
                        Button::new(("delete", i))
                            .ghost()
                            .xsmall()
                            .icon(IconName::Delete)
                            .on_click(cx.listener(move |this, _, _, cx| this.delete(&pd, cx))),
                    )
            });
        v_flex()
            .w(px(260.))
            .h_full()
            .border_r_1()
            .border_color(cx.theme().border)
            .child(
                h_flex()
                    .px_2()
                    .py_1()
                    .justify_between()
                    .items_center()
                    .child(div().text_sm().child("Connections"))
                    .child(
                        Button::new("add")
                            .ghost()
                            .xsmall()
                            .icon(IconName::Plus)
                            .on_click(
                                cx.listener(|this, _, window, cx| this.open_form(None, window, cx)),
                            ),
                    ),
            )
            .children(rows)
    }
}

impl Render for Workspace {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let main = match &self.status {
            Status::Connected { name, .. } => format!("Connected to {name}"),
            _ => "No connection open".into(),
        };
        v_flex()
            .size_full()
            .child(
                h_flex()
                    .flex_1()
                    .child(self.render_sidebar(cx))
                    .child(div().flex_1().p_2().child(main)),
            )
            .child(
                h_flex()
                    .px_2()
                    .py_1()
                    .text_xs()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .child(self.status.text()),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::Status;

    #[test]
    fn status_text() {
        assert_eq!(Status::Idle.text(), "Not connected");
        assert_eq!(Status::Connecting("a".into()).text(), "Connecting to a…");
        let c = Status::Connected {
            name: "a".into(),
            version: "16.3".into(),
        };
        assert_eq!(c.text(), "a · PostgreSQL 16.3");
        assert_eq!(Status::Failed("x".into()).text(), "Couldn't connect: x");
    }
}
