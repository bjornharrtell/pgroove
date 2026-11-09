use std::sync::Arc;

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::status_bar::StatusBar;
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use pgroove_core::catalog::ObjectKind;
use pgroove_core::secrets::SecretStore;
use pgroove_core::{ConnectionProfile, ProfileStore, connect::connect};
use tokio::runtime::Runtime;

use crate::connections::ConnectionForm;
use crate::schema_tree::{Row, SchemaTree};

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
    tree: SchemaTree,
}

impl Workspace {
    pub fn new(store: ProfileStore, secrets: Arc<dyn SecretStore>, runtime: Arc<Runtime>) -> Self {
        Self {
            store,
            secrets,
            runtime,
            status: Status::Idle,
            client: None,
            tree: SchemaTree::default(),
        }
    }

    fn connect(&mut self, profile: ConnectionProfile, cx: &mut Context<Self>) {
        self.client = None;
        self.tree.reset();
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
                        this.load_schemas(cx);
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

    fn load_schemas(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        self.tree.start_loading_schemas();
        let task = self
            .runtime
            .spawn(async move { pgroove_core::catalog::schemas(&client).await });
        cx.spawn(async move |this, cx| {
            let result = flatten(task.await);
            this.update(cx, |this, cx| {
                this.tree.set_schemas(result);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn toggle_schema(&mut self, schema: String, cx: &mut Context<Self>) {
        if self.tree.toggle(&schema)
            && let Some(client) = self.client.clone()
        {
            {
                let name = schema.clone();
                let task = self
                    .runtime
                    .spawn(async move { pgroove_core::catalog::objects(&client, &name).await });
                cx.spawn(async move |this, cx| {
                    let result = flatten(task.await);
                    this.update(cx, |this, cx| {
                        this.tree.set_objects(&schema, result);
                        cx.notify();
                    })
                    .ok();
                })
                .detach();
            }
        }
        cx.notify();
    }

    fn disconnect(&mut self, cx: &mut Context<Self>) {
        self.client = None;
        self.tree.reset();
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

    fn tree_row(
        &self,
        id: impl Into<ElementId>,
        depth: usize,
        cx: &Context<Self>,
    ) -> Stateful<Div> {
        h_flex()
            .id(id)
            .gap_1p5()
            .pl(px(10. + depth as f32 * 14.))
            .pr_2()
            .h(px(26.))
            .items_center()
            .text_sm()
            .text_color(cx.theme().sidebar_foreground)
            .overflow_hidden()
    }

    fn render_tree_rows(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let muted = cx.theme().muted_foreground;
        let hover = cx.theme().sidebar_accent;
        self.tree
            .rows()
            .into_iter()
            .enumerate()
            .map(|(i, row)| match row {
                Row::Schema { name, expanded } => {
                    let n = name.clone();
                    self.tree_row(("schema", i), 1, cx)
                        .cursor_pointer()
                        .hover(move |d| d.bg(hover))
                        .child(
                            Icon::new(if expanded {
                                IconName::ChevronDown
                            } else {
                                IconName::ChevronRight
                            })
                            .size_4()
                            .text_color(muted),
                        )
                        .child(Icon::new(IconName::Folder).size_4().text_color(muted))
                        .child(name)
                        .on_click(
                            cx.listener(move |this, _, _, cx| this.toggle_schema(n.clone(), cx)),
                        )
                        .into_any_element()
                }
                Row::Object { name, kind, .. } => self
                    .tree_row(("object", i), 3, cx)
                    .child(
                        Icon::new(match kind {
                            ObjectKind::View | ObjectKind::MaterializedView => IconName::Eye,
                            ObjectKind::Sequence => IconName::Hash,
                            _ => IconName::Table,
                        })
                        .size_4()
                        .text_color(muted),
                    )
                    .child(name)
                    .tooltip(move |window, cx| Tooltip::new(kind.label()).build(window, cx))
                    .into_any_element(),
                Row::Message(m) => div()
                    .pl(px(52.))
                    .h(px(24.))
                    .flex()
                    .items_center()
                    .text_xs()
                    .text_color(muted)
                    .child(m)
                    .into_any_element(),
            })
            .collect()
    }

    fn render_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let active = match &self.status {
            Status::Connected { name, .. } | Status::Connecting(name) => Some(name.clone()),
            _ => None,
        };
        let mut items: Vec<AnyElement> = Vec::new();
        for (i, p) in self.store.profiles().iter().cloned().enumerate() {
            let is_active = active.as_deref() == Some(p.name.as_str());
            let connected = is_active && self.client.is_some();
            let (pc, pe, pd) = (p.clone(), p.clone(), p.name.clone());
            let hover = theme.sidebar_accent;
            items.push(
                self.tree_row(("conn", i), 0, cx)
                    .gap_1()
                    .cursor_pointer()
                    .hover(move |d| d.bg(hover))
                    .when(is_active, |d| d.bg(theme.sidebar_accent))
                    .child(
                        Icon::new(if connected {
                            IconName::ChevronDown
                        } else {
                            IconName::ChevronRight
                        })
                        .size_4()
                        .text_color(theme.muted_foreground),
                    )
                    .child(
                        Icon::new(IconName::Server)
                            .size_4()
                            .text_color(if connected {
                                theme.success
                            } else {
                                theme.muted_foreground
                            }),
                    )
                    .child(div().flex_1().truncate().child(p.name.clone()))
                    .child(
                        Button::new(("edit", i))
                            .ghost()
                            .xsmall()
                            .icon(IconName::Settings)
                            .tooltip("Edit connection")
                            .on_click(cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.open_form(Some(pe.clone()), window, cx)
                            })),
                    )
                    .child(
                        Button::new(("delete", i))
                            .ghost()
                            .xsmall()
                            .icon(IconName::Trash)
                            .tooltip("Delete connection")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.delete(&pd, cx)
                            })),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if is_active {
                            this.disconnect(cx)
                        } else {
                            this.connect(pc.clone(), cx)
                        }
                    }))
                    .into_any_element(),
            );
            if is_active {
                items.extend(self.render_tree_rows(cx));
            }
        }
        if items.is_empty() {
            items.push(
                div()
                    .p_3()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child("Add a connection to get started.")
                    .into_any_element(),
            );
        }
        v_flex()
            .w(px(260.))
            .flex_shrink_0()
            .h_full()
            .bg(theme.sidebar)
            .border_r_1()
            .border_color(theme.sidebar_border)
            .child(
                h_flex()
                    .px_3()
                    .h(px(32.))
                    .justify_between()
                    .items_center()
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child("Connections"),
                    )
                    .child(
                        Button::new("add")
                            .ghost()
                            .xsmall()
                            .icon(IconName::Plus)
                            .tooltip("New connection")
                            .on_click(
                                cx.listener(|this, _, window, cx| this.open_form(None, window, cx)),
                            ),
                    ),
            )
            .child(
                v_flex()
                    .id("sidebar-scroll")
                    .flex_1()
                    .overflow_y_scroll()
                    .children(items),
            )
    }

    fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (label, color) = match &self.status {
            Status::Connected { name, .. } => (name.clone(), theme.success),
            Status::Connecting(n) => (format!("{n}…"), theme.muted_foreground),
            Status::Failed(_) => ("Connection failed".into(), theme.danger),
            Status::Idle => ("No connection".into(), theme.muted_foreground),
        };
        h_flex()
            .h(px(40.))
            .px_3()
            .gap_3()
            .items_center()
            .bg(theme.title_bar)
            .border_b_1()
            .border_color(theme.border)
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .child("pgroove"),
            )
            .child(
                h_flex()
                    .gap_1p5()
                    .px_2()
                    .h(px(26.))
                    .items_center()
                    .rounded(theme.radius)
                    .border_1()
                    .border_color(theme.border)
                    .text_sm()
                    .child(Icon::new(IconName::Circle).size_3().text_color(color))
                    .child(label),
            )
            .child(div().flex_1())
            .child(
                Button::new("palette")
                    .outline()
                    .small()
                    .icon(IconName::Search)
                    .label("Command palette · Ctrl+K"),
            )
    }

    fn render_main(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let strip = || {
            h_flex()
                .gap_2()
                .px_3()
                .h(px(34.))
                .items_center()
                .border_b_1()
                .border_color(theme.border)
        };
        v_flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .child(
                TabBar::new("tabs").underline().selected_index(0).child(
                    Tab::new()
                        .label("Query 1")
                        .prefix(Icon::new(IconName::Code).size_4()),
                ),
            )
            .child(
                strip()
                    .child(
                        Button::new("run")
                            .outline()
                            .xsmall()
                            .icon(IconName::Play)
                            .label("Run · Ctrl+Enter"),
                    )
                    .child(Button::new("explain").outline().xsmall().label("Explain"))
                    .child(Button::new("cancel").ghost().xsmall().label("Cancel")),
            )
            .child(
                div()
                    .h(px(150.))
                    .flex_shrink_0()
                    .p_3()
                    .border_b_1()
                    .border_color(theme.border)
                    .font_family(theme.mono_font_family.clone())
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child("-- SQL editor arrives in the next milestone"),
            )
            .child(
                strip()
                    .gap_4()
                    .text_xs()
                    .child(div().text_color(theme.foreground).child("Results"))
                    .child(div().text_color(theme.muted_foreground).child("Messages"))
                    .child(div().text_color(theme.muted_foreground).child("Plan")),
            )
            .child(
                div()
                    .flex_1()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child("Run a query to see results."),
            )
    }
}

fn flatten<T>(r: Result<anyhow::Result<T>, tokio::task::JoinError>) -> Result<T, String> {
    match r {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(e)) => Err(format!("{e:#}")),
        Err(e) => Err(e.to_string()),
    }
}

impl Render for Workspace {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let (left, right) = match &self.status {
            Status::Connected { name, version } => (
                format!("PostgreSQL {version}"),
                format!("Connected · {name}"),
            ),
            s => (String::new(), s.text()),
        };
        v_flex()
            .size_full()
            .bg(theme.background)
            .text_color(theme.foreground)
            .child(self.render_toolbar(cx))
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .child(self.render_sidebar(cx))
                    .child(self.render_main(cx)),
            )
            .child(
                StatusBar::new()
                    .left(div().text_xs().child(left))
                    .right(div().text_xs().child(right)),
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
