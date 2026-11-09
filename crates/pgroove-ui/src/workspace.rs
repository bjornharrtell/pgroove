use std::rc::Rc;
use std::sync::{Arc, Mutex};

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::command::{Command, CommandGroup, CommandItem, CommandState};
use gpui_kit::component::menu::{ContextMenuExt, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::status_bar::StatusBar;
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::*;
use gpui_kit::component::{TitleBar, window_corner_radii};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use pgroove_core::catalog::ObjectKind;
use pgroove_core::data::TableRef;
use pgroove_core::history::{History, Snippets};
use pgroove_core::secrets::SecretStore;
use pgroove_core::{ConnectionProfile, ProfileStore, connect::connect};
use tokio::runtime::Runtime;

use crate::activity_view::ActivityView;
use crate::connections::ConnectionForm;
use crate::mode::ReadOnlyMode;
use crate::palette::{self, PaletteCommand};
use crate::query_tab::QueryTab;
use crate::schema_tree::{Row, SchemaTree};
use crate::sql_completion::SharedIndex;
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

actions!(workspace, [OpenPalette]);

enum TabKind {
    Query(Entity<QueryTab>, usize),
    Activity(Entity<ActivityView>),
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
    tabs: Vec<TabKind>,
    active_tab: usize,
    completions: SharedIndex,
    history: Arc<Mutex<History>>,
    snippets: Arc<Mutex<Snippets>>,
    next_query: usize,
}

impl Workspace {
    pub fn new(
        store: ProfileStore,
        secrets: Arc<dyn SecretStore>,
        runtime: Arc<Runtime>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let completions: SharedIndex = Default::default();
        let history = History::default_path()
            .and_then(|p| {
                History::load(p)
                    .map_err(|e| log::warn!("history unavailable: {e:#}"))
                    .ok()
            })
            .unwrap_or_else(History::in_memory);
        let snippets = Snippets::default_path()
            .and_then(|p| {
                Snippets::load(p)
                    .map_err(|e| log::warn!("snippets unavailable: {e:#}"))
                    .ok()
            })
            .unwrap_or_else(Snippets::in_memory);
        let (history, snippets) = (
            Arc::new(Mutex::new(history)),
            Arc::new(Mutex::new(snippets)),
        );
        let first = cx.new(|cx| {
            QueryTab::new(
                runtime.clone(),
                completions.clone(),
                history.clone(),
                snippets.clone(),
                window,
                cx,
            )
        });
        Self {
            tabs: vec![TabKind::Query(first, 1)],
            active_tab: 0,
            next_query: 2,
            completions,
            history,
            snippets,
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
        self.sync_query_clients(cx);
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
                        this.sync_query_clients(cx);
                        this.status = Status::Connected { name, version };
                        this.load_schemas(cx);
                        this.load_completions(cx);
                        this.load_extensions(cx);
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

    fn load_extensions(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        let task = self
            .runtime
            .spawn(async move { pgroove_core::catalog::extensions(&client).await });
        cx.spawn(async move |this, cx| {
            let result = flatten(task.await);
            this.update(cx, |this, cx| {
                this.tree.set_extensions(result);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn fetch_objects(&mut self, schema: String, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            return;
        };
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

    /// Reloads schemas, open schemas' objects, extensions and the completion index.
    fn refresh_tree(&mut self, cx: &mut Context<Self>) {
        if self.client.is_none() {
            return;
        }
        let open = self.tree.refresh();
        self.load_schemas(cx);
        for schema in open {
            self.fetch_objects(schema, cx);
        }
        self.load_extensions(cx);
        self.load_completions(cx);
        cx.notify();
    }

    /// The query tab to use for inserted text: the active one, else any, else a new one.
    fn query_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Entity<QueryTab> {
        if let Some(TabKind::Query(q, _)) = self.tabs.get(self.active_tab) {
            return q.clone();
        }
        if let Some((ix, q)) = self.tabs.iter().enumerate().find_map(|(i, t)| match t {
            TabKind::Query(q, _) => Some((i, q.clone())),
            _ => None,
        }) {
            self.active_tab = ix;
            return q;
        }
        self.new_query(window, cx)
    }

    fn new_query(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Entity<QueryTab> {
        let (runtime, completions, history, snippets) = (
            self.runtime.clone(),
            self.completions.clone(),
            self.history.clone(),
            self.snippets.clone(),
        );
        let tab = cx.new(|cx| QueryTab::new(runtime, completions, history, snippets, window, cx));
        tab.update(cx, |t, cx| t.set_client(self.client.clone(), cx));
        self.tabs.push(TabKind::Query(tab.clone(), self.next_query));
        self.next_query += 1;
        self.active_tab = self.tabs.len() - 1;
        cx.notify();
        tab
    }

    fn append_to_editor(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        let tab = self.query_tab(window, cx);
        tab.update(cx, |t, cx| t.append(text, window, cx));
        cx.notify();
    }

    /// Gives every query tab the current connection (or none).
    fn sync_query_clients(&mut self, cx: &mut Context<Self>) {
        let client = self.client.clone();
        for t in &self.tabs {
            if let TabKind::Query(q, _) = t {
                q.update(cx, |q, cx| q.set_client(client.clone(), cx));
            }
        }
    }

    fn open_structure(&mut self, table: TableRef, window: &mut Window, cx: &mut Context<Self>) {
        self.open_table(table, window, cx);
        if let Some(TabKind::Table(v)) = self.tabs.get(self.active_tab) {
            v.update(cx, |v, cx| v.show_structure(cx));
        }
    }

    fn show_definition(
        &mut self,
        schema: String,
        name: String,
        kind: ObjectKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(client) = self.client.clone() else {
            return;
        };
        let task = self.runtime.spawn(async move {
            pgroove_core::catalog::definition(&client, &schema, &name, kind).await
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = flatten(task.await);
            this.update_in(cx, |this, window, cx| match result {
                Ok(text) => this.append_to_editor(&text, window, cx),
                Err(e) => window.push_notification(Notification::error(e), cx),
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
        self.tabs.retain(|t| matches!(t, TabKind::Query(..)));
        self.active_tab = 0;
    }

    fn open_table(&mut self, table: TableRef, window: &mut Window, cx: &mut Context<Self>) {
        let existing = self.tabs.iter().position(|t| match t {
            TabKind::Table(v) => v.read(cx).table() == &table,
            TabKind::Query(..) | TabKind::Activity(_) => false,
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

    fn open_activity(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(ix) = self
            .tabs
            .iter()
            .position(|t| matches!(t, TabKind::Activity(_)))
        {
            self.active_tab = ix;
        } else if let Some(client) = self.client.clone() {
            let runtime = self.runtime.clone();
            let view = cx.new(|cx| ActivityView::new(client, runtime, window, cx));
            self.tabs.push(TabKind::Activity(view));
            self.active_tab = self.tabs.len() - 1;
        }
        cx.notify();
    }

    fn close_tab(&mut self, ix: usize, cx: &mut Context<Self>) {
        if ix >= self.tabs.len() {
            return;
        }
        self.tabs.remove(ix);
        if self.active_tab >= ix {
            self.active_tab = self.active_tab.saturating_sub(1);
        }
        cx.notify();
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

    fn active_query(&self) -> Option<Entity<QueryTab>> {
        match self.tabs.get(self.active_tab) {
            Some(TabKind::Query(q, _)) => Some(q.clone()),
            _ => None,
        }
    }

    /// Runs a palette command; query commands act on the active (or a new) query tab.
    fn run_command(&mut self, cmd: PaletteCommand, window: &mut Window, cx: &mut Context<Self>) {
        use PaletteCommand::*;
        match cmd {
            NewQuery => {
                self.new_query(window, cx);
            }
            RunQuery | ExplainQuery | AnalyzeQuery | FormatSql | ShowHistory | ShowSnippets => {
                let tab = self
                    .active_query()
                    .unwrap_or_else(|| self.query_tab(window, cx));
                tab.update(cx, |t, cx| match cmd {
                    RunQuery => t.run_query(cx),
                    ExplainQuery => t.explain(false, cx),
                    AnalyzeQuery => t.explain(true, cx),
                    FormatSql => t.format_sql(window, cx),
                    ShowHistory => t.open_history(window, cx),
                    _ => t.open_snippets(window, cx),
                });
            }
            ToggleReadOnly => self.toggle_read_only(cx),
            OpenActivity => self.open_activity(window, cx),
            RefreshTree => self.refresh_tree(cx),
            NewConnection => self.open_form(None, window, cx),
            Connect(name) => {
                if let Some(p) = self
                    .store
                    .profiles()
                    .iter()
                    .find(|p| p.name == name)
                    .cloned()
                {
                    self.connect(p, cx);
                }
            }
            Disconnect => self.disconnect(cx),
        }
    }

    fn open_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let names: Vec<String> = self
            .store
            .profiles()
            .iter()
            .map(|p| p.name.clone())
            .collect();
        let groups = std::rc::Rc::new(palette::groups(
            &names,
            self.client.is_some(),
            cx.global::<ReadOnlyMode>().0,
        ));
        let state = cx.new(|cx| CommandState::new(window, cx));
        let this = cx.entity();
        let focus_on_mount = Rc::new(std::cell::Cell::new(true));
        window.open_dialog(cx, move |dialog, _, _| {
            let (state, groups, this) = (state.clone(), groups.clone(), this.clone());
            let focus_on_mount = focus_on_mount.clone();
            dialog
                .close_button(false)
                .p_0()
                .content(move |content, window, cx| {
                    if focus_on_mount.replace(false) {
                        let state = state.clone();
                        window.defer(cx, move |window, cx| {
                            state.read(cx).focus_handle(cx).focus(window, cx);
                        });
                    }
                    let mut command = Command::new(&state)
                        .bordered(false)
                        .placeholder("Type a command…");
                    for g in groups.iter() {
                        command = command.group(g.entries.iter().fold(
                            CommandGroup::new().label(g.label),
                            |grp, e| {
                                grp.item(
                                    CommandItem::new()
                                        .label(e.title.clone())
                                        .keywords(e.keywords.clone()),
                                )
                            },
                        ));
                    }
                    let (groups, this) = (groups.clone(), this.clone());
                    content.child(command.on_confirm(move |ix, window, cx| {
                        window.close_dialog(cx);
                        if let Some(cmd) = palette::command_at(&groups, ix.section, ix.row) {
                            this.update(cx, |ws, cx| ws.run_command(cmd, window, cx));
                        }
                    }))
                })
        });
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
        self.sync_query_clients(cx);
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
        window.open_dialog(cx, move |dialog, _, cx| {
            let form_ok = form.clone();
            let this = this.clone();
            let original_name = original_name.clone();
            dialog
                .title(title)
                .w(px(420.))
                .child(form.render(cx, test.clone()))
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
        let this = cx.entity();
        self.tree
            .rows()
            .into_iter()
            .enumerate()
            .map(|(i, row)| match row {
                Row::Schema { name, expanded } => {
                    let n = name.clone();
                    let (refresh_this, copy_name) = (this.clone(), name.clone());
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
                        .context_menu(move |menu, _, _| {
                            let (t, copy) = (refresh_this.clone(), copy_name.clone());
                            menu.item(PopupMenuItem::new("Refresh").on_click(move |_, _, cx| {
                                t.update(cx, |ws, cx| ws.refresh_tree(cx))
                            }))
                            .item(
                                PopupMenuItem::new("Copy name").on_click(move |_, _, cx| {
                                    cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()))
                                }),
                            )
                        })
                        .into_any_element()
                }
                Row::Object { schema, name, kind } => {
                    let icon = match kind {
                        ObjectKind::View | ObjectKind::MaterializedView => IconName::Eye,
                        ObjectKind::Sequence => IconName::Hash,
                        ObjectKind::Function | ObjectKind::Procedure | ObjectKind::Aggregate => {
                            IconName::Braces
                        }
                        ObjectKind::EnumType
                        | ObjectKind::CompositeType
                        | ObjectKind::DomainType => IconName::Type,
                        _ => IconName::Table,
                    };
                    let has_definition = matches!(
                        kind,
                        ObjectKind::Function | ObjectKind::Procedure | ObjectKind::EnumType
                    );
                    let (s, n) = (schema.clone(), name.clone());
                    let ctx_this = this.clone();
                    let row = self
                        .tree_row(("object", i), 3, cx)
                        .child(Icon::new(icon).size_4().text_color(muted))
                        .child(name.clone())
                        .tooltip(move |window, cx| Tooltip::new(kind.label()).build(window, cx));
                    let row = if kind.is_relation() {
                        let table = TableRef::new(schema.clone(), name.clone());
                        row.cursor_pointer()
                            .hover(move |d| d.bg(hover))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.open_table(table.clone(), window, cx)
                            }))
                    } else if has_definition {
                        row.cursor_pointer()
                            .hover(move |d| d.bg(hover))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.show_definition(s.clone(), n.clone(), kind, window, cx)
                            }))
                    } else {
                        row
                    };
                    let (s, n) = (schema.clone(), name.clone());
                    row.context_menu(move |menu, _, _| {
                        let qualified = format!(
                            "{}.{}",
                            pgroove_core::data::quote_ident(&s),
                            pgroove_core::data::quote_ident(&n)
                        );
                        let table = TableRef::new(s.clone(), n.clone());
                        let mut menu = menu;
                        if kind.is_relation() {
                            let (t1, t2, t3, tb1, tb2, q) = (
                                ctx_this.clone(),
                                ctx_this.clone(),
                                ctx_this.clone(),
                                table.clone(),
                                table.clone(),
                                qualified.clone(),
                            );
                            menu = menu
                                .item(PopupMenuItem::new("Open data").on_click(
                                    move |_, window, cx| {
                                        t1.update(cx, |ws, cx| {
                                            ws.open_table(tb1.clone(), window, cx)
                                        })
                                    },
                                ))
                                .item(PopupMenuItem::new("Open structure").on_click(
                                    move |_, window, cx| {
                                        t2.update(cx, |ws, cx| {
                                            ws.open_structure(tb2.clone(), window, cx)
                                        })
                                    },
                                ))
                                .item(PopupMenuItem::new("Select first 100 rows").on_click(
                                    move |_, window, cx| {
                                        let sql = format!("select * from {q} limit 100;");
                                        t3.update(cx, |ws, cx| {
                                            ws.append_to_editor(&sql, window, cx)
                                        })
                                    },
                                ));
                        }
                        if has_definition {
                            let (t, s2, n2) = (ctx_this.clone(), s.clone(), n.clone());
                            menu = menu.item(PopupMenuItem::new("Show definition").on_click(
                                move |_, window, cx| {
                                    t.update(cx, |ws, cx| {
                                        ws.show_definition(s2.clone(), n2.clone(), kind, window, cx)
                                    })
                                },
                            ));
                        }
                        let copy = if kind.is_relation() {
                            qualified
                        } else {
                            n.clone()
                        };
                        menu.item(PopupMenuItem::new("Copy name").on_click(move |_, _, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()))
                        }))
                    })
                    .into_any_element()
                }
                Row::ExtensionsHeader { expanded, count } => self
                    .tree_row(("extensions", i), 1, cx)
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
                    .child(Icon::new(IconName::Puzzle).size_4().text_color(muted))
                    .child(format!("Extensions ({})", count.unwrap_or(0)))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.tree.toggle_extensions();
                        cx.notify();
                    }))
                    .into_any_element(),
                Row::Extension { name, version } => self
                    .tree_row(("extension", i), 3, cx)
                    .child(Icon::new(IconName::Puzzle).size_4().text_color(muted))
                    .child(name)
                    .child(div().text_xs().text_color(muted).child(version))
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
                        h_flex()
                            .gap_0p5()
                            .child(
                                Button::new("refresh-tree")
                                    .ghost()
                                    .xsmall()
                                    .icon(IconName::RefreshCw)
                                    .tooltip("Refresh schema tree")
                                    .on_click(cx.listener(|this, _, _, cx| this.refresh_tree(cx))),
                            )
                            .child(
                                Button::new("add")
                                    .ghost()
                                    .xsmall()
                                    .icon(IconName::Plus)
                                    .tooltip("New connection")
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.open_form(None, window, cx)
                                    })),
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
        TitleBar::new().child(
            h_flex()
                .w_full()
                .gap_3()
                .items_center()
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
                .child(
                    Button::new("activity")
                        .ghost()
                        .xsmall()
                        .icon(IconName::Activity)
                        .label("Activity")
                        .tooltip("Sessions, locks, statements and more")
                        .on_click(
                            cx.listener(|this, _, window, cx| this.open_activity(window, cx)),
                        ),
                )
                .child(div().flex_1())
                .child(
                    Button::new("palette")
                        .outline()
                        .small()
                        .icon(IconName::Search)
                        .label("Command palette · Ctrl+K")
                        .on_click(cx.listener(|this, _, window, cx| this.open_palette(window, cx))),
                ),
        )
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
                TabKind::Query(_, n) => Tab::new()
                    .label(format!("Query {n}"))
                    .prefix(Icon::new(IconName::Code).size_4())
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
                TabKind::Activity(_) => Tab::new()
                    .label("Activity")
                    .prefix(Icon::new(IconName::Activity).size_4())
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
        bar.suffix(
            Button::new("new-query")
                .ghost()
                .xsmall()
                .icon(IconName::Plus)
                .tooltip("New query tab")
                .on_click(cx.listener(|this, _, window, cx| {
                    this.new_query(window, cx);
                })),
        )
    }

    fn render_main(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let content = match self.tabs.get(self.active_tab) {
            Some(TabKind::Table(v)) => div().flex_1().min_h_0().child(v.clone()).into_any_element(),
            Some(TabKind::Activity(v)) => {
                div().flex_1().min_h_0().child(v.clone()).into_any_element()
            }
            Some(TabKind::Query(q, _)) => {
                div().flex_1().min_h_0().child(q.clone()).into_any_element()
            }
            None => div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child("No tabs open. Use + to start a new query.")
                .into_any_element(),
        };
        v_flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .child(self.render_tab_bar(cx))
            .child(content)
    }
}

pub(crate) fn flatten<T>(
    r: Result<anyhow::Result<T>, tokio::task::JoinError>,
) -> Result<T, String> {
    match r {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(e)) => Err(format!("{e:#}")),
        Err(e) => Err(e.to_string()),
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let radii = window_corner_radii(window, cx);
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
            .on_action(
                cx.listener(|this, _: &OpenPalette, window, cx| this.open_palette(window, cx)),
            )
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
                    .rounded_bl(radii.bottom_left)
                    .rounded_br(radii.bottom_right)
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
