use std::sync::Arc;

use gpui_kit::*;
use pgroove_core::data::TableRef;
use pgroove_core::session::ActiveTab;
use pgroove_core::{ConnectionProfile, connect::connect};

use crate::mode::ReadOnlyMode;
use crate::query_tab::QueryTab;
use crate::schema_tree::SchemaTree;
use crate::tabs::TabKind;
use crate::workspace::{Workspace, flatten};

/// How far a session's connection has got.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) enum SessionStatus {
    Connecting,
    Ready { version: String },
    Failed(String),
}

impl SessionStatus {
    /// Status bar text for a session labelled `label`.
    pub(crate) fn text(&self, label: &str) -> String {
        match self {
            SessionStatus::Connecting => format!("Connecting to {label}…"),
            SessionStatus::Ready { version } => format!("{label} · PostgreSQL {version}"),
            SessionStatus::Failed(e) => format!("Couldn't connect to {label}: {e}"),
        }
    }
}

/// One open database: its own connection, schema tree and completion data.
pub(crate) struct DbSession {
    pub(crate) id: usize,
    pub(crate) profile: ConnectionProfile,
    pub(crate) client: Option<Arc<pgroove_core::Client>>,
    pub(crate) status: SessionStatus,
    pub(crate) tree: SchemaTree,
    /// Databases of the server, as listed by this session.
    pub(crate) databases: Option<Vec<String>>,
    pub(crate) index: Option<pgroove_core::completion::Index>,
}

impl DbSession {
    pub(crate) fn label(&self) -> String {
        format!("{} / {}", self.profile.name, self.profile.database)
    }

    /// Who connects where, for telling servers apart: `user@host:port`.
    pub(crate) fn detail(&self) -> String {
        format!(
            "{}@{}:{}",
            self.profile.user, self.profile.host, self.profile.port
        )
    }
}

/// A table tab to reopen once its database is connected.
pub(crate) struct PendingTable {
    pub(crate) connection: String,
    pub(crate) database: String,
    pub(crate) table: TableRef,
}

impl Workspace {
    pub(crate) fn session(&self, id: usize) -> Option<&DbSession> {
        self.sessions.iter().find(|s| s.id == id)
    }

    pub(crate) fn session_mut(&mut self, id: usize) -> Option<&mut DbSession> {
        self.sessions.iter_mut().find(|s| s.id == id)
    }

    pub(crate) fn client_of(&self, id: usize) -> Option<Arc<pgroove_core::Client>> {
        self.session(id).and_then(|s| s.client.clone())
    }

    pub(crate) fn ready_sessions(&self) -> impl Iterator<Item = &DbSession> {
        self.sessions.iter().filter(|s| s.client.is_some())
    }

    /// The session the user is looking at: the active tab's, else the latest opened.
    pub(crate) fn focused_session(&self, cx: &App) -> Option<usize> {
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

    /// Opens a database (or focuses it when already open).
    pub(crate) fn open_session(
        &mut self,
        profile: ConnectionProfile,
        cx: &mut Context<Self>,
    ) -> usize {
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
    pub(crate) fn close_session(&mut self, id: usize, cx: &mut Context<Self>) {
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
    pub(crate) fn close_profile(&mut self, name: &str, cx: &mut Context<Self>) {
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
    pub(crate) fn bind_query(
        &mut self,
        tab: &Entity<QueryTab>,
        id: Option<usize>,
        cx: &mut Context<Self>,
    ) {
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

    pub(crate) fn load_databases(&mut self, id: usize, cx: &mut Context<Self>) {
        let Some(client) = self.client_of(id) else {
            return;
        };
        let wanted = self
            .session(id)
            .and_then(|s| {
                self.store
                    .profiles()
                    .iter()
                    .find(|p| p.name == s.profile.name)
            })
            .is_none_or(|p| p.show_all_databases);
        if !wanted {
            if let Some(s) = self.session_mut(id) {
                s.databases = None;
            }
            return;
        }
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

    /// The id of a ready session for a connection profile and database.
    fn ready_session_for(&self, connection: &str, database: &str) -> Option<usize> {
        self.sessions
            .iter()
            .find(|s| {
                s.client.is_some() && s.profile.name == connection && s.profile.database == database
            })
            .map(|s| s.id)
    }

    /// Reopens what was open last time as its database connects: table tabs, activity
    /// monitors, expanded schemas, and finally the tab that was in front.
    pub(crate) fn restore_pending(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.pending_tables.is_empty()
            && self.pending_activity.is_empty()
            && self.pending_expanded.is_empty()
            && self.pending_active.is_none()
        {
            return;
        }
        for p in std::mem::take(&mut self.pending_tables) {
            match self.ready_session_for(&p.connection, &p.database) {
                Some(sid) => self.open_table(sid, p.table, window, cx),
                None => self.pending_tables.push(p),
            }
        }
        for (c, d) in std::mem::take(&mut self.pending_activity) {
            match self.ready_session_for(&c, &d) {
                Some(sid) => self.open_activity(sid, window, cx),
                None => self.pending_activity.push((c, d)),
            }
        }
        for e in std::mem::take(&mut self.pending_expanded) {
            match self.ready_session_for(&e.connection, &e.database) {
                Some(sid) => {
                    let open = self
                        .session(sid)
                        .is_some_and(|s| s.tree.expanded_schemas().contains(&e.schema));
                    if !open {
                        self.toggle_schema(sid, e.schema, cx);
                    }
                }
                None => self.pending_expanded.push(e),
            }
        }
        // Items whose database never connected would wait forever; give up once nothing is
        // still connecting.
        let connecting = self
            .sessions
            .iter()
            .any(|s| matches!(s.status, SessionStatus::Connecting));
        if !connecting {
            self.pending_tables.clear();
            self.pending_activity.clear();
            self.pending_expanded.clear();
        }
        if self.pending_tables.is_empty()
            && self.pending_activity.is_empty()
            && let Some(active) = self.pending_active.take()
        {
            if let Some(ix) = self.find_tab(&active, cx) {
                self.active_tab = ix;
            }
            self.save_session(cx);
            cx.notify();
        }
    }

    /// Finds the tab matching a saved descriptor.
    fn find_tab(&self, active: &ActiveTab, cx: &App) -> Option<usize> {
        match active {
            ActiveTab::Query { index } => self
                .tabs
                .iter()
                .enumerate()
                .filter(|(_, t)| matches!(t, TabKind::Query(..)))
                .nth(*index)
                .map(|(i, _)| i),
            ActiveTab::Table {
                connection,
                database,
                schema,
                name,
            } => {
                let sid = self.ready_session_for(connection, database)?;
                self.tabs.iter().position(|t| match t {
                    TabKind::Table(v, s) => {
                        let t = v.read(cx).table();
                        *s == sid && t.schema == *schema && t.name == *name
                    }
                    _ => false,
                })
            }
            ActiveTab::Activity {
                connection,
                database,
            } => {
                let sid = self.ready_session_for(connection, database)?;
                self.tabs
                    .iter()
                    .position(|t| matches!(t, TabKind::Activity(_, s) if *s == sid))
            }
        }
    }

    /// Applies the search box text to every open database.
    pub(crate) fn apply_tree_filter(&mut self, cx: &mut Context<Self>) {
        let text = self.tree_filter.read(cx).value().to_string();
        let ids: Vec<usize> = self.sessions.iter().map(|s| s.id).collect();
        for id in ids {
            self.sync_filter(id, &text, cx);
        }
        self.save_session(cx);
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
    pub(crate) fn refresh_session(&mut self, id: usize, cx: &mut Context<Self>) {
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

    pub(crate) fn refresh_all(&mut self, cx: &mut Context<Self>) {
        let ids: Vec<usize> = self.ready_sessions().map(|s| s.id).collect();
        for id in ids {
            self.refresh_session(id, cx);
        }
    }

    pub(crate) fn toggle_schema(&mut self, sid: usize, schema: String, cx: &mut Context<Self>) {
        let fetch = self
            .session_mut(sid)
            .is_some_and(|s| s.tree.toggle(&schema));
        if fetch {
            self.fetch_objects(sid, schema, cx);
        }
        self.save_session(cx);
        cx.notify();
    }
}
