use std::sync::Arc;

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Editor, EditorState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::status_bar::StatusBar;
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use pgroove_core::catalog::ObjectKind;
use pgroove_core::data::TableRef;
use pgroove_core::explain::Plan;
use pgroove_core::query::{self, QueryOutput};
use pgroove_core::secrets::SecretStore;
use pgroove_core::{ConnectionProfile, ProfileStore, connect::connect};
use tokio::runtime::Runtime;

use crate::connections::ConnectionForm;
use crate::grid_pane::GridPane;
use crate::mode::ReadOnlyMode;
use crate::schema_tree::{Row, SchemaTree};
use crate::sql_completion::{SharedIndex, SqlCompletions};
use crate::structure_view::OpenTable;
use crate::table_view::TableView;

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

actions!(workspace, [RunQuery, CancelQuery, FormatSql]);

#[derive(Clone, PartialEq, Eq, Debug)]
enum RunState {
    Idle,
    Running,
    Failed(String),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum BottomTab {
    Results,
    Messages,
    Plan,
}

enum TabKind {
    Query,
    Table(Entity<TableView>),
}

/// Top-level window view: sidebar, main area, status bar.
pub struct Workspace {
    store: ProfileStore,
    secrets: Arc<dyn SecretStore>,
    runtime: Arc<Runtime>,
    status: Status,
    client: Option<Arc<tokio_postgres::Client>>,
    tree: SchemaTree,
    editor: Entity<EditorState>,
    results: Entity<GridPane>,
    run: RunState,
    output: Option<QueryOutput>,
    bottom: BottomTab,
    tabs: Vec<TabKind>,
    active_tab: usize,
    plan: Option<Plan>,
    completions: SharedIndex,
}

impl Workspace {
    pub fn new(
        store: ProfileStore,
        secrets: Arc<dyn SecretStore>,
        runtime: Arc<Runtime>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let editor = cx.new(|cx| {
            EditorState::new(window, cx)
                .language("sql".to_string())
                .line_number(true)
                .soft_wrap(false)
                .placeholder("select now();")
        });
        let results = cx.new(|cx| GridPane::new(false, window, cx));
        let completions: SharedIndex = Default::default();
        editor.update(cx, |e, _| {
            e.lsp_mut().completion_provider = Some(std::rc::Rc::new(SqlCompletions {
                index: completions.clone(),
            }));
        });
        Self {
            editor,
            results,
            run: RunState::Idle,
            output: None,
            bottom: BottomTab::Results,
            tabs: vec![TabKind::Query],
            active_tab: 0,
            plan: None,
            completions,
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
        self.close_table_tabs();
        self.tree.reset();
        if let Ok(mut s) = self.completions.write() {
            *s = None;
        }
        self.status = Status::Connecting(profile.name.clone());
        cx.notify();
        let secrets = self.secrets.clone();
        let read_only = cx.global::<ReadOnlyMode>().0;
        let task = self.runtime.spawn(async move {
            let pw = secrets.get(&profile.name)?;
            let (client, version) = connect(&profile, pw.as_deref()).await?;
            pgroove_core::edit::set_read_only(&client, read_only).await?;
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
                        this.load_completions(cx);
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

    fn close_table_tabs(&mut self) {
        self.tabs.retain(|t| matches!(t, TabKind::Query));
        self.active_tab = 0;
    }

    fn open_table(&mut self, table: TableRef, window: &mut Window, cx: &mut Context<Self>) {
        let existing = self.tabs.iter().position(|t| match t {
            TabKind::Table(v) => v.read(cx).table() == &table,
            TabKind::Query => false,
        });
        if let Some(ix) = existing {
            self.active_tab = ix;
        } else if let Some(client) = self.client.clone() {
            let runtime = self.runtime.clone();
            let view = cx.new(|cx| TableView::new(table, client, runtime, window, cx));
            cx.subscribe_in(&view, window, |this, _, ev: &OpenTable, window, cx| {
                this.open_table(ev.0.clone(), window, cx)
            })
            .detach();
            self.tabs.push(TabKind::Table(view));
            self.active_tab = self.tabs.len() - 1;
        }
        cx.notify();
    }

    fn close_tab(&mut self, ix: usize, cx: &mut Context<Self>) {
        if ix == 0 || ix >= self.tabs.len() {
            return;
        }
        self.tabs.remove(ix);
        if self.active_tab >= ix {
            self.active_tab = self.active_tab.saturating_sub(1);
        }
        cx.notify();
    }

    fn run_query(&mut self, cx: &mut Context<Self>) {
        if self.run == RunState::Running {
            return;
        }
        let Some(client) = self.client.clone() else {
            self.run = RunState::Failed("Connect to a database first.".into());
            self.bottom = BottomTab::Messages;
            cx.notify();
            return;
        };
        let editor = self.editor.read(cx);
        let selected = editor.selected_text().to_string();
        let sql = if selected.trim().is_empty() {
            editor.value().to_string()
        } else {
            selected
        };
        if sql.trim().is_empty() {
            return;
        }
        self.run = RunState::Running;
        cx.notify();
        let task = self
            .runtime
            .spawn(async move { query::run(&client, &sql, query::MAX_ROWS).await });
        cx.spawn(async move |this, cx| {
            let result = flatten(task.await);
            this.update(cx, |this, cx| {
                this.finish_query(result, cx);
            })
            .ok();
        })
        .detach();
    }

    fn finish_query(&mut self, result: Result<QueryOutput, String>, cx: &mut Context<Self>) {
        match result {
            Ok(out) => {
                self.run = RunState::Idle;
                let set = out.last_rowset().cloned();
                self.bottom = if set.is_some() {
                    BottomTab::Results
                } else {
                    BottomTab::Messages
                };
                self.results
                    .update(cx, |p, cx| p.set_data(set.as_ref(), cx));
                self.output = Some(out);
            }
            Err(e) => {
                self.run = RunState::Failed(e);
                self.bottom = BottomTab::Messages;
            }
        }
        cx.notify();
    }

    fn cancel_query(&mut self, cx: &mut Context<Self>) {
        if self.run != RunState::Running {
            return;
        }
        if let Some(client) = self.client.clone() {
            self.runtime.spawn(async move {
                if let Err(e) = query::cancel(&client).await {
                    log::warn!("cancel failed: {e:#}");
                }
            });
        }
        cx.notify();
    }

    fn format_sql(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.editor.read(cx).value().to_string();
        if text.trim().is_empty() {
            return;
        }
        let formatted = pgroove_core::format::format_sql(&text);
        self.editor
            .update(cx, |e, cx| e.set_value(formatted, window, cx));
    }

    fn explain(&mut self, analyze: bool, cx: &mut Context<Self>) {
        if self.run == RunState::Running {
            return;
        }
        let Some(client) = self.client.clone() else {
            self.run = RunState::Failed("Connect to a database first.".into());
            self.bottom = BottomTab::Messages;
            cx.notify();
            return;
        };
        let editor = self.editor.read(cx);
        let selected = editor.selected_text().to_string();
        let sql = if selected.trim().is_empty() {
            editor.value().to_string()
        } else {
            selected
        };
        self.run = RunState::Running;
        cx.notify();
        let task = self
            .runtime
            .spawn(async move { pgroove_core::explain::run(&client, &sql, analyze).await });
        cx.spawn(async move |this, cx| {
            let result = flatten(task.await);
            this.update(cx, |this, cx| {
                match result {
                    Ok(plan) => {
                        this.run = RunState::Idle;
                        this.plan = Some(plan);
                        this.bottom = BottomTab::Plan;
                    }
                    Err(e) => {
                        this.run = RunState::Failed(e);
                        this.bottom = BottomTab::Messages;
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn load_completions(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        let slot = self.completions.clone();
        let task = self
            .runtime
            .spawn(async move { pgroove_core::completion::load(&client).await });
        cx.spawn(async move |_, _| match flatten(task.await) {
            Ok(index) => {
                if let Ok(mut s) = slot.write() {
                    *s = Some(index);
                }
            }
            Err(e) => log::warn!("could not load completion index: {e}"),
        })
        .detach();
    }

    fn toggle_read_only(&mut self, cx: &mut Context<Self>) {
        let read_only = !cx.global::<ReadOnlyMode>().0;
        cx.set_global(ReadOnlyMode(read_only));
        if let Some(client) = self.client.clone() {
            self.runtime.spawn(async move {
                if let Err(e) = pgroove_core::edit::set_read_only(&client, read_only).await {
                    log::warn!("could not switch read-only mode: {e:#}");
                }
            });
        }
        cx.refresh_windows();
    }

    fn disconnect(&mut self, cx: &mut Context<Self>) {
        self.client = None;
        self.close_table_tabs();
        self.tree.reset();
        if let Ok(mut s) = self.completions.write() {
            *s = None;
        }
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
                Row::Object { schema, name, kind } => self
                    .tree_row(("object", i), 3, cx)
                    .when(kind != ObjectKind::Sequence, |d| {
                        let table = TableRef::new(schema.clone(), name.clone());
                        d.cursor_pointer()
                            .hover(move |d| d.bg(hover))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.open_table(table.clone(), window, cx)
                            }))
                    })
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
        let read_only = cx.global::<ReadOnlyMode>().0;
        let mode_chip = Button::new("read-only")
            .xsmall()
            .outline()
            .icon(if read_only {
                IconName::Lock
            } else {
                IconName::LockOpen
            })
            .label(if read_only { "Read-only" } else { "Editable" })
            .tooltip(if read_only {
                "Writes are blocked. Click to allow editing."
            } else {
                "Editing is allowed. Click to block writes."
            })
            .on_click(cx.listener(|this, _, _, cx| this.toggle_read_only(cx)));
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
            .child(mode_chip)
            .child(div().flex_1())
            .child(
                Button::new("palette")
                    .outline()
                    .small()
                    .icon(IconName::Search)
                    .label("Command palette · Ctrl+K"),
            )
    }

    fn render_results_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let tab = |id: &'static str, label: &'static str, which: BottomTab| {
            let on = self.bottom == which;
            div()
                .id(id)
                .cursor_pointer()
                .text_color(if on {
                    theme.foreground
                } else {
                    theme.muted_foreground
                })
                .child(label)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.bottom = which;
                    cx.notify();
                }))
        };
        let summary = match (&self.run, &self.output) {
            (RunState::Running, _) => "Running…".to_string(),
            (_, Some(o)) => {
                let n = o.last_rowset().map(|s| s.count).unwrap_or(0);
                format!("{n} rows · {} ms", o.elapsed.as_millis())
            }
            _ => String::new(),
        };
        h_flex()
            .gap_4()
            .px_3()
            .h(px(34.))
            .items_center()
            .text_xs()
            .border_b_1()
            .border_color(theme.border)
            .child(tab("tab-results", "Results", BottomTab::Results))
            .child(tab("tab-messages", "Messages", BottomTab::Messages))
            .child(tab("tab-plan", "Plan", BottomTab::Plan))
            .child(div().flex_1())
            .child(div().text_color(theme.muted_foreground).child(summary))
            .child(self.results.read(cx).actions())
    }

    fn render_results_body(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let note = |text: String, color: Hsla| {
            div()
                .flex_1()
                .p_3()
                .text_sm()
                .text_color(color)
                .child(text)
                .into_any_element()
        };
        if self.bottom == BottomTab::Plan {
            return crate::plan_view::render(self.plan.as_ref(), &theme);
        }
        if self.bottom == BottomTab::Messages {
            if let RunState::Failed(e) = &self.run {
                return note(e.clone(), theme.danger);
            }
            let text = self
                .output
                .as_ref()
                .map(|o| o.messages().join("\n"))
                .unwrap_or_else(|| "No messages.".into());
            return note(text, theme.foreground);
        }
        if self.output.as_ref().and_then(|o| o.last_rowset()).is_some() {
            div()
                .flex_1()
                .min_h_0()
                .child(self.results.clone())
                .into_any_element()
        } else {
            div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child("Run a query to see results.")
                .into_any_element()
        }
    }

    fn render_tab_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut bar = TabBar::new("tabs")
            .underline()
            .selected_index(self.active_tab)
            .on_click(cx.listener(|this, ix: &usize, _, cx| {
                this.active_tab = *ix;
                cx.notify();
            }));
        for (ix, kind) in self.tabs.iter().enumerate() {
            bar = bar.child(match kind {
                TabKind::Query => Tab::new()
                    .label("Query 1")
                    .prefix(Icon::new(IconName::Code).size_4()),
                TabKind::Table(v) => Tab::new()
                    .label(v.read(cx).table().name.clone())
                    .prefix(Icon::new(IconName::Table).size_4())
                    .suffix(
                        Button::new(("close", ix))
                            .ghost()
                            .xsmall()
                            .icon(IconName::X)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.close_tab(ix, cx)
                            })),
                    ),
            });
        }
        bar
    }

    fn render_main(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let content = match self.tabs.get(self.active_tab) {
            Some(TabKind::Table(v)) => div().flex_1().min_h_0().child(v.clone()).into_any_element(),
            _ => self.render_query_tab(cx).into_any_element(),
        };
        v_flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .child(self.render_tab_bar(cx))
            .child(content)
    }

    fn render_query_tab(&self, cx: &mut Context<Self>) -> impl IntoElement {
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
            .min_h_0()
            .child(
                strip()
                    .child(
                        Button::new("run")
                            .outline()
                            .xsmall()
                            .icon(IconName::Play)
                            .label("Run · Ctrl+Enter")
                            .loading(self.run == RunState::Running)
                            .on_click(cx.listener(|this, _, _, cx| this.run_query(cx))),
                    )
                    .child(
                        Button::new("explain")
                            .outline()
                            .xsmall()
                            .label("Explain")
                            .tooltip("Estimated plan; does not run the statement")
                            .on_click(cx.listener(|this, _, _, cx| this.explain(false, cx))),
                    )
                    .child(
                        Button::new("analyze")
                            .outline()
                            .xsmall()
                            .label("Analyze")
                            .tooltip("Runs the statement in a transaction that is rolled back")
                            .on_click(cx.listener(|this, _, _, cx| this.explain(true, cx))),
                    )
                    .child(
                        Button::new("format")
                            .ghost()
                            .xsmall()
                            .label("Format")
                            .tooltip("Format SQL (Ctrl+Shift+F)")
                            .on_click(
                                cx.listener(|this, _, window, cx| this.format_sql(window, cx)),
                            ),
                    )
                    .child(
                        Button::new("cancel")
                            .ghost()
                            .xsmall()
                            .label("Cancel")
                            .on_click(cx.listener(|this, _, _, cx| this.cancel_query(cx))),
                    ),
            )
            .child(
                div()
                    .h(px(170.))
                    .flex_shrink_0()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(
                        Editor::new(&self.editor)
                            .bordered(false)
                            .p_0()
                            .h_full()
                            .font_family(theme.mono_font_family.clone())
                            .text_size(theme.mono_font_size),
                    ),
            )
            .child(self.render_results_header(cx))
            .child(self.render_results_body(cx))
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
            .key_context("Workspace")
            .on_action(cx.listener(|this, _: &RunQuery, _, cx| this.run_query(cx)))
            .on_action(cx.listener(|this, _: &CancelQuery, _, cx| this.cancel_query(cx)))
            .on_action(cx.listener(|this, _: &FormatSql, window, cx| this.format_sql(window, cx)))
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
