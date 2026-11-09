use std::ops::Range;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use crate::dialogs::DialogActions;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::command::{Command, CommandGroup, CommandItem, CommandState};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{ContextMenuExt, DropdownMenu, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::status_bar::StatusBar;
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::window_corner_radii;
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use pgroove_core::catalog::ObjectKind;
use pgroove_core::data::TableRef;
use pgroove_core::history::{History, Snippets};
use pgroove_core::secrets::SecretStore;
use pgroove_core::session::SessionState;
use pgroove_core::settings::Settings;
use pgroove_core::update::{self, Release};
use pgroove_core::{ConnectionProfile, ProfileStore, connect::connect};
use tokio::runtime::Runtime;

use crate::activity_view::ActivityView;
use crate::connections::ConnectionForm;
use crate::mode::ReadOnlyMode;
use crate::palette::{self, PaletteCommand};
use crate::query_tab::QueryTab;
use crate::schema_tree::{Category, Row, SchemaTree};
use crate::sql_completion::SharedIndex;
use crate::structure_view::OpenTable;
use crate::table_view::TableView;

/// How far a session's connection has got.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum SessionStatus {
    Connecting,
    Ready { version: String },
    Failed(String),
}

impl SessionStatus {
    /// Status bar text for a session labelled `label`.
    pub fn text(&self, label: &str) -> String {
        match self {
            SessionStatus::Connecting => format!("Connecting to {label}…"),
            SessionStatus::Ready { version } => format!("{label} · PostgreSQL {version}"),
            SessionStatus::Failed(e) => format!("Couldn't connect to {label}: {e}"),
        }
    }
}

/// One open database: its own connection, schema tree and completion data.
struct DbSession {
    id: usize,
    profile: ConnectionProfile,
    client: Option<Arc<pgroove_core::Client>>,
    status: SessionStatus,
    tree: SchemaTree,
    /// Databases of the server, as listed by this session.
    databases: Option<Vec<String>>,
    index: Option<pgroove_core::completion::Index>,
}

impl DbSession {
    fn label(&self) -> String {
        format!("{} / {}", self.profile.name, self.profile.database)
    }
}

actions!(workspace, [OpenPalette]);

/// Where the self-update process stands.
#[derive(Clone, Debug)]
enum UpdateState {
    Idle,
    Checking,
    Available(Release),
    Installing(pgroove_core::update::Version),
    Ready(pgroove_core::update::Version),
}

/// Left padding of the first tree level and the extra indent per level.
const TREE_BASE: f32 = 10.;
const INDENT_STEP: f32 = 16.;
/// Width of a row's chevron plus gap: leaf rows (no chevron) are indented by this
/// beyond their parent so their icon lines up with the parent's icon.
const CHEVRON_WIDTH: f32 = 22.;

/// Left padding for a leaf row under a node at `parent_depth`.
fn leaf_indent(parent_depth: usize) -> f32 {
    TREE_BASE + parent_depth as f32 * INDENT_STEP + CHEVRON_WIDTH
}

enum SidebarItem {
    Connection(usize, ConnectionProfile),
    Database {
        profile: ConnectionProfile,
        session: Option<usize>,
    },
    Tree(usize, usize, Row),
    Note(String),
}

enum TabKind {
    Query(Entity<QueryTab>, usize),
    Activity(Entity<ActivityView>, usize),
    Table(Entity<TableView>, usize),
}

/// A table tab to reopen once its database is connected.
struct PendingTable {
    connection: String,
    database: String,
    table: TableRef,
}

/// Top-level window view: sidebar, main area, status bar.
pub struct Workspace {
    store: ProfileStore,
    secrets: Arc<dyn SecretStore>,
    runtime: Arc<Runtime>,
    sessions: Vec<DbSession>,
    next_session: usize,
    /// The most recently opened session, used when no tab says which one is meant.
    last_session: Option<usize>,
    tabs: Vec<TabKind>,
    active_tab: usize,
    history: Arc<Mutex<History>>,
    snippets: Arc<Mutex<Snippets>>,
    next_query: usize,
    session_path: Option<std::path::PathBuf>,
    window_size: Option<(f32, f32)>,
    pending_tables: Vec<PendingTable>,
    update: UpdateState,
    settings: Settings,
    settings_path: Option<std::path::PathBuf>,
    http: pgroove_core::update::HttpClient,
    /// Search box above the schema tree.
    tree_filter: Entity<InputState>,
}

/// State loaded from disk before the window opens.
pub struct Startup {
    pub session: SessionState,
    pub session_path: Option<std::path::PathBuf>,
    pub settings: Settings,
    pub settings_path: Option<std::path::PathBuf>,
}

impl Workspace {
    pub fn new(
        store: ProfileStore,
        secrets: Arc<dyn SecretStore>,
        runtime: Arc<Runtime>,
        startup: Startup,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let Startup {
            session,
            session_path,
            settings,
            settings_path,
        } = startup;
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
        let texts = if session.queries.is_empty() {
            vec![String::new()]
        } else {
            session.queries.clone()
        };
        let tabs: Vec<TabKind> = texts
            .iter()
            .enumerate()
            .map(|(i, text)| {
                let tab = cx.new(|cx| {
                    QueryTab::new(
                        runtime.clone(),
                        SharedIndex::default(),
                        history.clone(),
                        snippets.clone(),
                        window,
                        cx,
                    )
                });
                if !text.is_empty() {
                    tab.update(cx, |t, cx| t.set_text(text, window, cx));
                }
                TabKind::Query(tab, i + 1)
            })
            .collect();
        let next_query = tabs.len() + 1;
        let tree_filter = cx
            .new(|cx| InputState::new(window, cx).placeholder("Search tables, views, functions…"));
        cx.subscribe(&tree_filter, |this, _, ev: &InputEvent, cx| {
            if matches!(ev, InputEvent::Change) {
                this.apply_tree_filter(cx);
            }
        })
        .detach();
        let mut this = Self {
            tabs,
            active_tab: 0,
            next_query,
            session_path,
            window_size: None,
            pending_tables: session
                .tables
                .iter()
                .map(|t| PendingTable {
                    connection: t.connection.clone(),
                    database: t.database.clone(),
                    table: TableRef::new(t.schema.clone(), t.name.clone()),
                })
                .collect(),
            history,
            snippets,
            store,
            secrets,
            runtime,
            sessions: Vec::new(),
            next_session: 1,
            last_session: None,
            update: UpdateState::Idle,
            tree_filter,
            settings,
            settings_path,
            http: update::http_client(),
        };
        cx.on_app_quit(|this, cx| {
            this.save_session(cx);
            async {}
        })
        .detach();
        if this.settings.check_for_updates && std::env::var_os("PGROOVE_NO_UPDATE_CHECK").is_none()
        {
            cx.spawn_in(window, async move |this, cx| {
                cx.background_executor()
                    .timer(std::time::Duration::from_secs(3))
                    .await;
                this.update_in(cx, |this, window, cx| this.check_updates(false, window, cx))
                    .ok();
            })
            .detach();
        }
        for (name, database) in &session.connections {
            if let Some(p) = this
                .store
                .profiles()
                .iter()
                .find(|p| &p.name == name)
                .cloned()
            {
                this.open_session(
                    ConnectionProfile {
                        database: database.clone(),
                        ..p
                    },
                    cx,
                );
            }
        }
        this
    }

    fn session(&self, id: usize) -> Option<&DbSession> {
        self.sessions.iter().find(|s| s.id == id)
    }

    fn session_mut(&mut self, id: usize) -> Option<&mut DbSession> {
        self.sessions.iter_mut().find(|s| s.id == id)
    }

    fn client_of(&self, id: usize) -> Option<Arc<pgroove_core::Client>> {
        self.session(id).and_then(|s| s.client.clone())
    }

    fn ready_sessions(&self) -> impl Iterator<Item = &DbSession> {
        self.sessions.iter().filter(|s| s.client.is_some())
    }

    /// The session the user is looking at: the active tab's, else the latest opened.
    fn focused_session(&self, cx: &App) -> Option<usize> {
        let from_tab = match self.tabs.get(self.active_tab) {
            Some(TabKind::Query(q, _)) => q.read(cx).session(),
            Some(TabKind::Table(_, s)) | Some(TabKind::Activity(_, s)) => Some(*s),
            None => None,
        };
        from_tab
            .filter(|id| self.session(*id).is_some())
            .or(self.last_session.filter(|id| self.session(*id).is_some()))
            .or_else(|| self.sessions.last().map(|s| s.id))
    }

    /// Writes the restorable state (open databases, queries, tables, window size).
    fn save_session(&self, cx: &App) {
        let Some(path) = &self.session_path else {
            return;
        };
        let state = SessionState {
            connections: self
                .sessions
                .iter()
                .map(|s| (s.profile.name.clone(), s.profile.database.clone()))
                .collect(),
            queries: self
                .tabs
                .iter()
                .filter_map(|t| match t {
                    TabKind::Query(q, _) => Some(q.read(cx).text(cx)),
                    _ => None,
                })
                .collect(),
            tables: self
                .tabs
                .iter()
                .filter_map(|t| match t {
                    TabKind::Table(v, sid) => {
                        let s = self.session(*sid)?;
                        let t = v.read(cx).table();
                        Some(pgroove_core::session::SessionTable {
                            connection: s.profile.name.clone(),
                            database: s.profile.database.clone(),
                            schema: t.schema.clone(),
                            name: t.name.clone(),
                        })
                    }
                    _ => None,
                })
                .collect(),
            window: self.window_size,
        };
        if let Err(e) = state.save(path) {
            log::warn!("could not save session: {e:#}");
        }
    }

    /// Opens a database (or focuses it when already open).
    fn open_session(&mut self, profile: ConnectionProfile, cx: &mut Context<Self>) -> usize {
        if let Some(s) = self
            .sessions
            .iter()
            .find(|s| s.profile.name == profile.name && s.profile.database == profile.database)
        {
            let id = s.id;
            self.last_session = Some(id);
            cx.notify();
            return id;
        }
        let id = self.next_session;
        self.next_session += 1;
        self.last_session = Some(id);
        self.sessions.push(DbSession {
            id,
            profile: profile.clone(),
            client: None,
            status: SessionStatus::Connecting,
            tree: SchemaTree::default(),
            databases: None,
            index: None,
        });
        cx.notify();
        let secrets = self.secrets.clone();
        let read_only = cx.global::<ReadOnlyMode>().0;
        let task = self.runtime.spawn(async move {
            let pw = secrets.get(&profile.name)?;
            let (client, version) = connect(&profile, pw.as_deref()).await?;
            pgroove_core::edit::set_read_only(&client, read_only).await?;
            anyhow::Ok((client, version))
        });
        cx.spawn(async move |this, cx| {
            let result = match task.await {
                Ok(r) => r,
                Err(e) => Err(e.into()),
            };
            this.update(cx, |this, cx| {
                let Some(s) = this.session_mut(id) else {
                    return; // closed while connecting
                };
                match result {
                    Ok((client, version)) => {
                        s.client = Some(Arc::new(client));
                        s.status = SessionStatus::Ready { version };
                        this.bind_unbound_queries(id, cx);
                        this.save_session(cx);
                        this.load_schemas(id, cx);
                        this.load_extensions(id, cx);
                        this.load_databases(id, cx);
                        this.load_completions(id, cx);
                        let text = this.tree_filter.read(cx).value().to_string();
                        this.sync_filter(id, &text, cx);
                    }
                    Err(e) => {
                        log::warn!("connect failed: {e:#}");
                        s.status = SessionStatus::Failed(format!("{e:#}"));
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        id
    }

    /// Closes one open database and the tabs that belong to it.
    fn close_session(&mut self, id: usize, cx: &mut Context<Self>) {
        self.sessions.retain(|s| s.id != id);
        self.tabs.retain(|t| match t {
            TabKind::Table(_, s) | TabKind::Activity(_, s) => *s != id,
            TabKind::Query(..) => true,
        });
        for t in &self.tabs {
            if let TabKind::Query(q, _) = t
                && q.read(cx).session() == Some(id)
            {
                q.update(cx, |q, cx| q.bind(None, None, None, cx));
            }
        }
        if self.last_session == Some(id) {
            self.last_session = None;
        }
        self.active_tab = self.active_tab.min(self.tabs.len().saturating_sub(1));
        self.save_session(cx);
        cx.notify();
    }

    /// Closes every open database of a connection profile.
    fn close_profile(&mut self, name: &str, cx: &mut Context<Self>) {
        let ids: Vec<usize> = self
            .sessions
            .iter()
            .filter(|s| s.profile.name == name)
            .map(|s| s.id)
            .collect();
        for id in ids {
            self.close_session(id, cx);
        }
    }

    /// Binds query tabs that have no database yet to a newly ready one.
    fn bind_unbound_queries(&mut self, id: usize, cx: &mut Context<Self>) {
        let (client, index) = match self.session(id) {
            Some(s) => (s.client.clone(), s.index.clone()),
            None => return,
        };
        for t in &self.tabs {
            if let TabKind::Query(q, _) = t
                && q.read(cx).session().is_none()
            {
                q.update(cx, |q, cx| {
                    q.bind(Some(id), client.clone(), index.clone(), cx)
                });
            }
        }
    }

    /// Points a query tab at an open database.
    fn bind_query(&mut self, tab: &Entity<QueryTab>, id: Option<usize>, cx: &mut Context<Self>) {
        let (client, index) = id
            .and_then(|id| self.session(id))
            .map_or((None, None), |s| (s.client.clone(), s.index.clone()));
        let id = id.filter(|_| client.is_some());
        tab.update(cx, |q, cx| q.bind(id, client, index, cx));
    }

    fn load_schemas(&mut self, id: usize, cx: &mut Context<Self>) {
        let Some(client) = self.client_of(id) else {
            return;
        };
        if let Some(s) = self.session_mut(id) {
            s.tree.start_loading_schemas();
        }
        let task = self
            .runtime
            .spawn(async move { pgroove_core::catalog::schemas(&client).await });
        cx.spawn(async move |this, cx| {
            let result = flatten(task.await);
            this.update(cx, |this, cx| {
                if let Some(s) = this.session_mut(id) {
                    s.tree.set_schemas(result);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn load_databases(&mut self, id: usize, cx: &mut Context<Self>) {
        let Some(client) = self.client_of(id) else {
            return;
        };
        let task = self
            .runtime
            .spawn(async move { pgroove_core::catalog::databases(&client).await });
        cx.spawn(async move |this, cx| {
            let result = flatten(task.await);
            this.update(cx, |this, cx| {
                match (this.session_mut(id), result) {
                    (Some(s), Ok(dbs)) => s.databases = Some(dbs),
                    (_, Err(e)) => log::warn!("could not list databases: {e}"),
                    _ => {}
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn load_extensions(&mut self, id: usize, cx: &mut Context<Self>) {
        let Some(client) = self.client_of(id) else {
            return;
        };
        let task = self
            .runtime
            .spawn(async move { pgroove_core::catalog::extensions(&client).await });
        cx.spawn(async move |this, cx| {
            let result = flatten(task.await);
            this.update(cx, |this, cx| {
                if let Some(s) = this.session_mut(id) {
                    s.tree.set_extensions(result);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn fetch_objects(&mut self, id: usize, schema: String, cx: &mut Context<Self>) {
        let Some(client) = self.client_of(id) else {
            return;
        };
        let name = schema.clone();
        let task = self
            .runtime
            .spawn(async move { pgroove_core::catalog::objects(&client, &name).await });
        cx.spawn(async move |this, cx| {
            let result = flatten(task.await);
            this.update(cx, |this, cx| {
                if let Some(s) = this.session_mut(id) {
                    s.tree.set_objects(&schema, result);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn load_completions(&mut self, id: usize, cx: &mut Context<Self>) {
        let Some(client) = self.client_of(id) else {
            return;
        };
        let task = self
            .runtime
            .spawn(async move { pgroove_core::completion::load(&client).await });
        cx.spawn(async move |this, cx| match flatten(task.await) {
            Ok(index) => {
                this.update(cx, |this, cx| {
                    if let Some(s) = this.session_mut(id) {
                        s.index = Some(index.clone());
                    }
                    for t in &this.tabs {
                        if let TabKind::Query(q, _) = t {
                            q.update(cx, |q, _| {
                                if q.session() == Some(id) {
                                    q.set_index(Some(index.clone()));
                                }
                            });
                        }
                    }
                })
                .ok();
            }
            Err(e) => log::warn!("could not load completion index: {e}"),
        })
        .detach();
    }

    /// Applies the search box text to every open database.
    fn apply_tree_filter(&mut self, cx: &mut Context<Self>) {
        let text = self.tree_filter.read(cx).value().to_string();
        let ids: Vec<usize> = self.sessions.iter().map(|s| s.id).collect();
        for id in ids {
            self.sync_filter(id, &text, cx);
        }
        cx.notify();
    }

    fn sync_filter(&mut self, id: usize, text: &str, cx: &mut Context<Self>) {
        let need = self.session_mut(id).is_some_and(|s| {
            s.tree.set_filter(text);
            s.tree.needs_all_objects()
        });
        if need {
            self.load_all_objects(id, cx);
        }
    }

    /// Loads every object of the database so a search can look across schemas.
    fn load_all_objects(&mut self, id: usize, cx: &mut Context<Self>) {
        let Some(client) = self.client_of(id) else {
            return;
        };
        if let Some(s) = self.session_mut(id) {
            s.tree.start_loading_all();
        }
        let task = self
            .runtime
            .spawn(async move { pgroove_core::catalog::all_objects(&client).await });
        cx.spawn(async move |this, cx| {
            let result = flatten(task.await);
            this.update(cx, |this, cx| {
                if let Some(s) = this.session_mut(id) {
                    s.tree.set_all_objects(result);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Reloads one database's schemas, open schemas' objects, extensions and completion data.
    fn refresh_session(&mut self, id: usize, cx: &mut Context<Self>) {
        let open = match self.session_mut(id) {
            Some(s) if s.client.is_some() => s.tree.refresh(),
            _ => return,
        };
        self.load_schemas(id, cx);
        for schema in open {
            self.fetch_objects(id, schema, cx);
        }
        self.load_extensions(id, cx);
        self.load_databases(id, cx);
        self.load_completions(id, cx);
        if self.session(id).is_some_and(|s| s.tree.needs_all_objects()) {
            self.load_all_objects(id, cx);
        }
        cx.notify();
    }

    fn refresh_all(&mut self, cx: &mut Context<Self>) {
        let ids: Vec<usize> = self.ready_sessions().map(|s| s.id).collect();
        for id in ids {
            self.refresh_session(id, cx);
        }
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
        let focused = self.focused_session(cx);
        let (runtime, history, snippets) = (
            self.runtime.clone(),
            self.history.clone(),
            self.snippets.clone(),
        );
        let tab = cx.new(|cx| {
            QueryTab::new(
                runtime,
                SharedIndex::default(),
                history,
                snippets,
                window,
                cx,
            )
        });
        self.bind_query(&tab, focused, cx);
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

    fn open_structure(
        &mut self,
        sid: usize,
        table: TableRef,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_table(sid, table, window, cx);
        if let Some(TabKind::Table(v, _)) = self.tabs.get(self.active_tab) {
            v.update(cx, |v, cx| v.show_structure(cx));
        }
    }

    fn show_definition(
        &mut self,
        sid: usize,
        schema: String,
        name: String,
        kind: ObjectKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(client) = self.client_of(sid) else {
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

    fn toggle_schema(&mut self, sid: usize, schema: String, cx: &mut Context<Self>) {
        let fetch = self
            .session_mut(sid)
            .is_some_and(|s| s.tree.toggle(&schema));
        if fetch {
            self.fetch_objects(sid, schema, cx);
        }
        cx.notify();
    }

    fn open_table(
        &mut self,
        sid: usize,
        table: TableRef,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let existing = self.tabs.iter().position(|t| match t {
            TabKind::Table(v, s) => *s == sid && v.read(cx).table() == &table,
            _ => false,
        });
        if let Some(ix) = existing {
            self.active_tab = ix;
        } else if let Some(client) = self.client_of(sid) {
            let runtime = self.runtime.clone();
            let view = cx.new(|cx| TableView::new(table, client, runtime, window, cx));
            cx.subscribe_in(&view, window, move |this, _, ev: &OpenTable, window, cx| {
                this.open_table(sid, ev.0.clone(), window, cx)
            })
            .detach();
            self.tabs.push(TabKind::Table(view, sid));
            self.active_tab = self.tabs.len() - 1;
        }
        self.last_session = Some(sid);
        self.save_session(cx);
        cx.notify();
    }

    fn open_activity(&mut self, sid: usize, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(ix) = self
            .tabs
            .iter()
            .position(|t| matches!(t, TabKind::Activity(_, s) if *s == sid))
        {
            self.active_tab = ix;
        } else if let Some(client) = self.client_of(sid) {
            let runtime = self.runtime.clone();
            let view = cx.new(|cx| ActivityView::new(client, runtime, window, cx));
            self.tabs.push(TabKind::Activity(view, sid));
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
        self.save_session(cx);
        cx.notify();
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
            OpenActivity => {
                if let Some(sid) = self.focused_session(cx) {
                    self.open_activity(sid, window, cx);
                }
            }
            RefreshTree => self.refresh_all(cx),
            NewConnection => self.open_form(None, window, cx),
            Connect(name) => {
                if let Some(p) = self
                    .store
                    .profiles()
                    .iter()
                    .find(|p| p.name == name)
                    .cloned()
                {
                    self.open_session(p, cx);
                }
            }
            Disconnect => {
                if let Some(sid) = self.focused_session(cx) {
                    self.close_session(sid, cx);
                }
            }
            CheckUpdates => self.check_updates(true, window, cx),
            InstallUpdate => self.open_update_dialog(window, cx),
            ToggleUpdateChecks => self.toggle_update_checks(cx),
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
            &palette::PaletteContext {
                connected: self.ready_sessions().next().is_some(),
                read_only: cx.global::<ReadOnlyMode>().0,
                update_available: matches!(self.update, UpdateState::Available(_)),
                auto_check_updates: self.settings.check_for_updates,
            },
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

    /// Looks for a newer release; `manual` checks report their outcome in a notification.
    fn check_updates(&mut self, manual: bool, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(
            self.update,
            UpdateState::Checking | UpdateState::Installing(_) | UpdateState::Ready(_)
        ) {
            return;
        }
        self.update = UpdateState::Checking;
        cx.notify();
        let http = self.http.clone();
        // PGROOVE_UPDATE_URL points the check at another release feed (for testing or mirrors).
        let url = std::env::var("PGROOVE_UPDATE_URL")
            .unwrap_or_else(|_| update::LATEST_RELEASE_URL.to_string());
        let task = self
            .runtime
            .spawn(async move { update::fetch_latest(&http, &url).await });
        cx.spawn_in(window, async move |this, cx| {
            let result = flatten(task.await);
            this.update_in(cx, move |this, window, cx| {
                this.update = UpdateState::Idle;
                match result {
                    Ok(rel) if rel.is_newer_than(&update::current_version()) => {
                        if manual {
                            window.push_notification(
                                format!("Version {} is available", rel.version),
                                cx,
                            );
                        }
                        this.update = UpdateState::Available(rel);
                    }
                    Ok(_) if manual => window.push_notification(
                        format!("You're up to date (version {})", update::current_version()),
                        cx,
                    ),
                    Ok(_) => {}
                    Err(e) => {
                        log::warn!("update check failed: {e}");
                        if manual {
                            window.push_notification(
                                Notification::error(format!("Couldn't check for updates: {e}")),
                                cx,
                            );
                        }
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Downloads, verifies and installs the available update.
    fn install_update(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let UpdateState::Available(release) = self.update.clone() else {
            return;
        };
        let Some((asset, package)) =
            update::pick_asset(&release, &update::current_target()).map(|(a, p)| (a.clone(), p))
        else {
            window.push_notification(
                Notification::error("There is no download for this platform."),
                cx,
            );
            return;
        };
        self.update = UpdateState::Installing(release.version.clone());
        cx.notify();
        let http = self.http.clone();
        let rel = release.clone();
        let task = self.runtime.spawn(async move {
            let bytes = update::download_verified(&http, &rel, &asset).await?;
            let binary = if cfg!(windows) {
                "pgroove.exe"
            } else {
                "pgroove"
            };
            let program = update::extract_binary(&bytes, package, binary)?;
            update::install(&program, package)?;
            anyhow::Ok(())
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = flatten(task.await);
            this.update_in(cx, move |this, window, cx| {
                match result {
                    Ok(()) => this.update = UpdateState::Ready(release.version.clone()),
                    Err(e) => {
                        log::warn!("update failed: {e}");
                        window.push_notification(
                            Notification::error(format!("Update failed: {e}")),
                            cx,
                        );
                        this.update = UpdateState::Available(release);
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Starts the updated program and quits this one.
    fn restart_app(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.save_session(cx);
        match update::restart() {
            Ok(()) => cx.quit(),
            Err(e) => {
                window.push_notification(Notification::error(format!("Couldn't restart: {e}")), cx)
            }
        }
    }

    fn toggle_update_checks(&mut self, cx: &mut Context<Self>) {
        self.settings.check_for_updates = !self.settings.check_for_updates;
        if let Some(path) = &self.settings_path
            && let Err(e) = self.settings.save(path)
        {
            log::warn!("could not save settings: {e:#}");
        }
        cx.notify();
    }

    fn open_update_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let UpdateState::Available(release) = self.update.clone() else {
            return;
        };
        let this = cx.entity();
        let notes: String = release.notes.chars().take(1500).collect();
        let title = format!("Version {} is available", release.version);
        let current = format!("You have version {}.", update::current_version());
        window.open_dialog(cx, move |dialog, _, _| {
            let this = this.clone();
            dialog
                .title(title.clone())
                .w(px(520.))
                .child(
                    v_flex()
                        .id("update-notes")
                        .max_h(px(300.))
                        .overflow_y_scroll()
                        .gap_2()
                        .child(div().text_sm().child(current.clone()))
                        .child(div().text_xs().child(notes.clone())),
                )
                .actions("Install update", true, move |_, window, cx| {
                    this.update(cx, |ws, cx| ws.install_update(window, cx));
                    true
                })
        });
    }

    /// The title bar control for the update state, if there is something to show.
    fn update_chip(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        match &self.update {
            UpdateState::Available(r) => Some(
                Button::new("update-available")
                    .xsmall()
                    .outline()
                    .icon(IconName::Download)
                    .label(format!("Update to {}", r.version))
                    .tooltip("A new version is available")
                    .on_click(
                        cx.listener(|this, _, window, cx| this.open_update_dialog(window, cx)),
                    )
                    .into_any_element(),
            ),
            UpdateState::Installing(v) => Some(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!("Updating to {v}…"))
                    .into_any_element(),
            ),
            UpdateState::Ready(v) => Some(
                Button::new("update-restart")
                    .xsmall()
                    .outline()
                    .icon(IconName::RefreshCw)
                    .label("Restart to update")
                    .tooltip(format!("Version {v} is installed"))
                    .on_click(cx.listener(|this, _, window, cx| this.restart_app(window, cx)))
                    .into_any_element(),
            ),
            UpdateState::Idle | UpdateState::Checking => None,
        }
    }

    fn toggle_read_only(&mut self, cx: &mut Context<Self>) {
        let read_only = !cx.global::<ReadOnlyMode>().0;
        cx.set_global(ReadOnlyMode(read_only));
        for client in self.ready_sessions().filter_map(|s| s.client.clone()) {
            self.runtime.spawn(async move {
                if let Err(e) = pgroove_core::edit::set_read_only(&client, read_only).await {
                    log::warn!("could not switch read-only mode: {e:#}");
                }
            });
        }
        cx.refresh_windows();
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

    /// A tree row for a node at `depth`; each level indents by `INDENT_STEP`.
    fn tree_row(
        &self,
        id: impl Into<ElementId>,
        depth: usize,
        cx: &Context<Self>,
    ) -> Stateful<Div> {
        self.tree_row_px(id, TREE_BASE + depth as f32 * INDENT_STEP, cx)
    }

    /// A tree row with an explicit left padding in pixels.
    fn tree_row_px(
        &self,
        id: impl Into<ElementId>,
        left: f32,
        cx: &Context<Self>,
    ) -> Stateful<Div> {
        h_flex()
            .id(id)
            .gap_1p5()
            .pl(px(left))
            .pr_2()
            .h(px(26.))
            .flex_shrink_0()
            .items_center()
            .text_sm()
            .text_color(cx.theme().sidebar_foreground)
            .overflow_hidden()
    }

    fn render_tree_item(
        &self,
        sid: usize,
        j: usize,
        row: Row,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        // Ids must stay unique across the open databases.
        let i = sid * 1_000_000 + j;
        let muted = cx.theme().muted_foreground;
        let hover = cx.theme().sidebar_accent;
        let this = cx.entity();
        match row {
            Row::Schema { name, expanded } => {
                let n = name.clone();
                let (refresh_this, copy_name) = (this.clone(), name.clone());
                self.tree_row(("schema", i), 2, cx)
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
                        cx.listener(move |this, _, _, cx| this.toggle_schema(sid, n.clone(), cx)),
                    )
                    .context_menu(move |menu, _, _| {
                        let (t, copy) = (refresh_this.clone(), copy_name.clone());
                        menu.item(PopupMenuItem::new("Refresh").on_click(move |_, _, cx| {
                            t.update(cx, |ws, cx| ws.refresh_session(sid, cx))
                        }))
                        .item(PopupMenuItem::new("Copy name").on_click(move |_, _, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()))
                        }))
                    })
                    .into_any_element()
            }
            Row::Category {
                schema,
                category,
                expanded,
                count,
            } => {
                let icon = match category {
                    Category::Tables => IconName::Table,
                    Category::Views => IconName::Eye,
                    Category::Sequences => IconName::Hash,
                    Category::Types => IconName::Type,
                    Category::Functions => IconName::Braces,
                };
                self.tree_row(("category", i), 3, cx)
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
                    .child(Icon::new(icon).size_4().text_color(muted))
                    .child(category.label())
                    .child(div().text_xs().text_color(muted).child(count.to_string()))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(s) = this.session_mut(sid) {
                            s.tree.toggle_category(&schema, category);
                        }
                        cx.notify();
                    }))
                    .into_any_element()
            }
            Row::Object { schema, name, kind } => {
                let icon = match kind {
                    ObjectKind::View | ObjectKind::MaterializedView => IconName::Eye,
                    ObjectKind::Sequence => IconName::Hash,
                    ObjectKind::Function | ObjectKind::Procedure | ObjectKind::Aggregate => {
                        IconName::Braces
                    }
                    ObjectKind::EnumType | ObjectKind::CompositeType | ObjectKind::DomainType => {
                        IconName::Type
                    }
                    _ => IconName::Table,
                };
                let has_definition = matches!(
                    kind,
                    ObjectKind::Function | ObjectKind::Procedure | ObjectKind::EnumType
                );
                let (s, n) = (schema.clone(), name.clone());
                let ctx_this = this.clone();
                let row = self
                    .tree_row_px(("object", i), leaf_indent(3), cx)
                    .child(Icon::new(icon).size_4().text_color(muted))
                    .child(name.clone())
                    .tooltip(move |window, cx| Tooltip::new(kind.label()).build(window, cx));
                let row = if kind.is_relation() {
                    let table = TableRef::new(schema.clone(), name.clone());
                    row.cursor_pointer()
                        .hover(move |d| d.bg(hover))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.open_table(sid, table.clone(), window, cx)
                        }))
                } else if has_definition {
                    row.cursor_pointer()
                        .hover(move |d| d.bg(hover))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.show_definition(sid, s.clone(), n.clone(), kind, window, cx)
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
                            .item(
                                PopupMenuItem::new("Open data").on_click(move |_, window, cx| {
                                    t1.update(cx, |ws, cx| {
                                        ws.open_table(sid, tb1.clone(), window, cx)
                                    })
                                }),
                            )
                            .item(PopupMenuItem::new("Open structure").on_click(
                                move |_, window, cx| {
                                    t2.update(cx, |ws, cx| {
                                        ws.open_structure(sid, tb2.clone(), window, cx)
                                    })
                                },
                            ))
                            .item(PopupMenuItem::new("Select first 100 rows").on_click(
                                move |_, window, cx| {
                                    let sql = format!("select * from {q} limit 100;");
                                    t3.update(cx, |ws, cx| ws.append_to_editor(&sql, window, cx))
                                },
                            ));
                    }
                    if has_definition {
                        let (t, s2, n2) = (ctx_this.clone(), s.clone(), n.clone());
                        menu = menu.item(PopupMenuItem::new("Show definition").on_click(
                            move |_, window, cx| {
                                t.update(cx, |ws, cx| {
                                    ws.show_definition(
                                        sid,
                                        s2.clone(),
                                        n2.clone(),
                                        kind,
                                        window,
                                        cx,
                                    )
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
                .tree_row(("extensions", i), 2, cx)
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
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(s) = this.session_mut(sid) {
                        s.tree.toggle_extensions();
                    }
                    cx.notify();
                }))
                .into_any_element(),
            Row::Extension { name, version } => self
                .tree_row_px(("extension", i), leaf_indent(2), cx)
                .child(Icon::new(IconName::Puzzle).size_4().text_color(muted))
                .child(name)
                .child(div().text_xs().text_color(muted).child(version))
                .into_any_element(),
            Row::Message(m) => div()
                .pl(px(leaf_indent(3)))
                .h(px(26.))
                .flex_shrink_0()
                .flex()
                .items_center()
                .text_xs()
                .text_color(muted)
                .child(m)
                .into_any_element(),
        }
    }

    fn connection_row(&self, i: usize, p: ConnectionProfile, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let is_active = self.sessions.iter().any(|s| s.profile.name == p.name);
        let connected = self
            .sessions
            .iter()
            .any(|s| s.profile.name == p.name && s.client.is_some());
        let (pc, pe, pd) = (p.clone(), p.clone(), p.name.clone());
        let hover = theme.sidebar_accent;
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
                    this.close_profile(&pc.name, cx)
                } else {
                    this.open_session(pc.clone(), cx);
                }
            }))
            .into_any_element()
    }

    fn database_row(
        &self,
        i: usize,
        profile: ConnectionProfile,
        session: Option<usize>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let hover = theme.sidebar_accent;
        let open = session.is_some();
        let name = profile.database.clone();
        let mut row = self
            .tree_row(("database", i), 1, cx)
            .cursor_pointer()
            .hover(move |d| d.bg(hover))
            .child(
                Icon::new(if open {
                    IconName::ChevronDown
                } else {
                    IconName::ChevronRight
                })
                .size_4()
                .text_color(theme.muted_foreground),
            )
            .child(
                Icon::new(IconName::Database)
                    .size_4()
                    .text_color(theme.muted_foreground),
            )
            .child(div().truncate().child(name))
            .tooltip(move |window, cx| {
                Tooltip::new(if open {
                    "Close this database"
                } else {
                    "Open this database"
                })
                .build(window, cx)
            })
            .on_click(cx.listener(move |this, _, _, cx| match session {
                Some(id) => this.close_session(id, cx),
                None => {
                    this.open_session(profile.clone(), cx);
                }
            }));
        if open {
            row = row.font_weight(FontWeight::MEDIUM);
        }
        row.into_any_element()
    }

    fn render_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let mut items: Vec<SidebarItem> = Vec::new();
        for (i, p) in self.store.profiles().iter().cloned().enumerate() {
            let open: Vec<&DbSession> = self
                .sessions
                .iter()
                .filter(|s| s.profile.name == p.name)
                .collect();
            items.push(SidebarItem::Connection(i, p.clone()));
            if open.is_empty() {
                continue;
            }
            let mut names: std::collections::BTreeSet<String> =
                open.iter().map(|s| s.profile.database.clone()).collect();
            for s in &open {
                if let Some(dbs) = &s.databases {
                    names.extend(dbs.iter().cloned());
                }
            }
            for name in names {
                let session = open.iter().find(|s| s.profile.database == name);
                items.push(SidebarItem::Database {
                    profile: ConnectionProfile {
                        database: name.clone(),
                        ..p.clone()
                    },
                    session: session.map(|s| s.id),
                });
                if let Some(s) = session {
                    match &s.status {
                        SessionStatus::Connecting => {
                            items.push(SidebarItem::Note("Connecting…".into()))
                        }
                        SessionStatus::Failed(e) => {
                            items.push(SidebarItem::Note(format!("Couldn't connect: {e}")))
                        }
                        SessionStatus::Ready { .. } => items.extend(
                            s.tree
                                .rows()
                                .into_iter()
                                .enumerate()
                                .map(|(j, r)| SidebarItem::Tree(s.id, j, r)),
                        ),
                    }
                }
            }
        }
        let empty = items.is_empty();
        let items = Rc::new(items);
        let count = items.len();
        let list = uniform_list(
            "sidebar-rows",
            count,
            cx.processor(move |this, range: Range<usize>, _, cx| {
                range
                    .map(|ix| match &items[ix] {
                        SidebarItem::Connection(i, p) => this.connection_row(*i, p.clone(), cx),
                        SidebarItem::Database { profile, session } => {
                            this.database_row(ix, profile.clone(), *session, cx)
                        }
                        SidebarItem::Tree(sid, j, r) => {
                            this.render_tree_item(*sid, *j, r.clone(), cx)
                        }
                        SidebarItem::Note(m) => div()
                            .pl(px(leaf_indent(2)))
                            .h(px(26.))
                            .flex_shrink_0()
                            .flex()
                            .items_center()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(m.clone())
                            .into_any_element(),
                    })
                    .collect::<Vec<_>>()
            }),
        )
        .size_full();
        let body = if empty {
            div()
                .p_3()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child("Add a connection to get started.")
                .into_any_element()
        } else {
            list.into_any_element()
        };
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
                                    .on_click(cx.listener(|this, _, _, cx| this.refresh_all(cx))),
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
                div()
                    .px_2()
                    .pb_1()
                    .child(Input::new(&self.tree_filter).small()),
            )
            .child(div().flex_1().min_h_0().child(body))
    }

    fn render_toolbar(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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
        let focused = self.focused_session(cx).and_then(|id| self.session(id));
        let theme = cx.theme();
        let (label, color) = match focused {
            Some(s) => {
                let mut label = s.label();
                if self.sessions.len() > 1 {
                    label.push_str(&format!(" +{}", self.sessions.len() - 1));
                }
                let color = match s.status {
                    SessionStatus::Ready { .. } => theme.success,
                    SessionStatus::Connecting => theme.muted_foreground,
                    SessionStatus::Failed(_) => theme.danger,
                };
                (label, color)
            }
            None => ("No connection".to_string(), theme.muted_foreground),
        };
        crate::title_bar::title_bar(
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
                        .on_click(cx.listener(|this, _, window, cx| {
                            if let Some(sid) = this.focused_session(cx) {
                                this.open_activity(sid, window, cx);
                            }
                        })),
                )
                .children(self.update_chip(cx))
                .child(div().flex_1())
                .child(
                    Button::new("palette")
                        .outline()
                        .small()
                        .icon(IconName::Search)
                        .label("Command palette · Ctrl+K")
                        .on_click(cx.listener(|this, _, window, cx| this.open_palette(window, cx))),
                ),
            window,
            cx,
        )
    }

    /// A tab title, with the database appended when several are open.
    fn tab_label(&self, title: &str, sid: usize) -> String {
        match self.session(sid) {
            Some(s) if self.sessions.len() > 1 => format!("{title} · {}", s.profile.database),
            _ => title.to_string(),
        }
    }

    /// The database picker above a query tab.
    fn render_query_session_bar(
        &self,
        q: &Entity<QueryTab>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme().clone();
        let label = q
            .read(cx)
            .session()
            .and_then(|id| self.session(id))
            .map_or_else(|| "No database".to_string(), DbSession::label);
        let choices: Vec<(usize, String)> =
            self.ready_sessions().map(|s| (s.id, s.label())).collect();
        let (this, tab) = (cx.entity(), q.clone());
        h_flex()
            .px_3()
            .h(px(28.))
            .gap_2()
            .items_center()
            .border_b_1()
            .border_color(theme.border)
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child("Database"),
            )
            .child(
                Button::new("query-database")
                    .ghost()
                    .xsmall()
                    .icon(IconName::Database)
                    .label(label)
                    .tooltip("Choose which open database this query runs against")
                    .dropdown_menu(move |menu, _, _| {
                        if choices.is_empty() {
                            return menu
                                .item(PopupMenuItem::new("No open databases").disabled(true));
                        }
                        choices.iter().fold(menu, |m, (id, label)| {
                            let (t, tab, id) = (this.clone(), tab.clone(), *id);
                            m.item(PopupMenuItem::new(label.clone()).on_click(move |_, _, cx| {
                                t.update(cx, |ws, cx| ws.bind_query(&tab, Some(id), cx))
                            }))
                        })
                    }),
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
                    .prefix(div().px_1().child(Icon::new(IconName::Code).size_4()))
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
                TabKind::Activity(_, sid) => Tab::new()
                    .label(self.tab_label("Activity", *sid))
                    .prefix(div().px_1().child(Icon::new(IconName::Activity).size_4()))
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
                TabKind::Table(v, sid) => Tab::new()
                    .label(self.tab_label(&v.read(cx).table().name, *sid))
                    .prefix(div().px_1().child(Icon::new(IconName::Table).size_4()))
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
            Some(TabKind::Table(v, _)) => {
                div().flex_1().min_h_0().child(v.clone()).into_any_element()
            }
            Some(TabKind::Activity(v, _)) => {
                div().flex_1().min_h_0().child(v.clone()).into_any_element()
            }
            Some(TabKind::Query(q, _)) => {
                let q = q.clone();
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .child(self.render_query_session_bar(&q, cx))
                    .child(div().flex_1().min_h_0().child(q))
                    .into_any_element()
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
        let size = window.bounds().size;
        self.window_size = Some((f32::from(size.width), f32::from(size.height)));
        let ready: Vec<(usize, usize)> = self
            .pending_tables
            .iter()
            .enumerate()
            .filter_map(|(ix, p)| {
                self.sessions
                    .iter()
                    .find(|s| {
                        s.client.is_some()
                            && s.profile.name == p.connection
                            && s.profile.database == p.database
                    })
                    .map(|s| (ix, s.id))
            })
            .collect();
        for (ix, sid) in ready.into_iter().rev() {
            let p = self.pending_tables.remove(ix);
            self.open_table(sid, p.table, window, cx);
        }
        let (left, right) = match self.focused_session(cx).and_then(|id| self.session(id)) {
            Some(s) => match &s.status {
                SessionStatus::Ready { version } => (
                    format!("PostgreSQL {version}"),
                    format!("Connected · {}", s.label()),
                ),
                other => (String::new(), other.text(&s.label())),
            },
            None => (String::new(), "Not connected".to_string()),
        };
        v_flex()
            .size_full()
            .key_context("Workspace")
            .on_action(
                cx.listener(|this, _: &OpenPalette, window, cx| this.open_palette(window, cx)),
            )
            .rounded_tl(radii.top_left)
            .rounded_tr(radii.top_right)
            .rounded_bl(radii.bottom_left)
            .rounded_br(radii.bottom_right)
            .bg(theme.background)
            .text_color(theme.foreground)
            .child(self.render_toolbar(window, cx))
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
    use super::{SessionStatus, leaf_indent};

    #[test]
    fn leaf_icons_align_with_the_parent_icon() {
        // A parent's icon sits after its chevron; its leaf rows start at the same x.
        assert_eq!(leaf_indent(3), 10. + 3. * 16. + 22.);
        assert!(leaf_indent(3) > leaf_indent(2));
    }

    #[test]
    fn status_text() {
        assert_eq!(
            SessionStatus::Connecting.text("a / b"),
            "Connecting to a / b…"
        );
        let r = SessionStatus::Ready {
            version: "16.3".into(),
        };
        assert_eq!(r.text("a / b"), "a / b · PostgreSQL 16.3");
        assert_eq!(
            SessionStatus::Failed("x".into()).text("a / b"),
            "Couldn't connect to a / b: x"
        );
    }
}
