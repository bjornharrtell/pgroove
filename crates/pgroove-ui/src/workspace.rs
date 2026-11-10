use std::sync::{Arc, Mutex};

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::status_bar::StatusBar;
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::window_corner_radii;
use gpui_kit::component::*;
use gpui_kit::*;
use pgroove_core::data::TableRef;
use pgroove_core::history::{History, Snippets};
use pgroove_core::secrets::SecretStore;
use pgroove_core::session::{ActiveTab, SessionExpanded, SessionState};
use pgroove_core::settings::Settings;
use pgroove_core::update;
use pgroove_core::{ConnectionProfile, ProfileStore};
use tokio::runtime::Runtime;

use crate::activity_view::ActivityView;
use crate::mode::ReadOnlyMode;
use crate::query_tab::{DatabaseChosen, EditorResized, QueryTab, RowLimitChanged};
use crate::sessions::{DbSession, PendingTable, SessionStatus};
use crate::sql_completion::SharedIndex;
use crate::tabs::TabKind;
use crate::updates::UpdateState;

actions!(
    workspace,
    [
        OpenPalette,
        ToggleFullscreen,
        SidebarUp,
        SidebarDown,
        SidebarActivate,
        SidebarExpand,
        SidebarCollapse
    ]
);

/// How long resize events pause before the session is written, so a dragged divider
/// costs one write instead of one per frame.
const SAVE_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(400);

/// Top-level window view: sidebar, main area, status bar.
pub struct Workspace {
    pub(crate) store: ProfileStore,
    pub(crate) secrets: Arc<dyn SecretStore>,
    pub(crate) runtime: Arc<Runtime>,
    pub(crate) sessions: Vec<DbSession>,
    pub(crate) next_session: usize,
    /// The most recently opened session, used when no tab says which one is meant.
    pub(crate) last_session: Option<usize>,
    pub(crate) tabs: Vec<TabKind>,
    pub(crate) active_tab: usize,
    pub(crate) history: Arc<Mutex<History>>,
    pub(crate) snippets: Arc<Mutex<Snippets>>,
    pub(crate) next_query: usize,
    session_path: Option<std::path::PathBuf>,
    window_size: Option<(f32, f32)>,
    pub(crate) pending_tables: Vec<PendingTable>,
    /// Activity monitors, expanded schemas and the front tab to restore once their databases connect.
    pub(crate) pending_activity: Vec<(String, String)>,
    pub(crate) pending_expanded: Vec<SessionExpanded>,
    /// Connection groups folded in the sidebar (lowercase names).
    pub(crate) collapsed_groups: std::collections::BTreeSet<String>,
    pub(crate) pending_active: Option<ActiveTab>,
    /// Keyboard focus and cursor of the sidebar tree.
    pub(crate) sidebar_focus: FocusHandle,
    pub(crate) sidebar_scroll: UniformListScrollHandle,
    pub(crate) cursor: Option<usize>,
    pub(crate) update: UpdateState,
    /// ETag of the last release answer, so a re-check can be conditional.
    pub(crate) update_etag: Option<String>,
    pub(crate) settings: Settings,
    pub(crate) settings_path: Option<std::path::PathBuf>,
    /// The one-time DBeaver import offer is still to be made (on the first render).
    pub(crate) dbeaver_offer_pending: bool,
    pub(crate) http: pgroove_core::update::HttpClient,
    /// Search box above the schema tree.
    pub(crate) tree_filter: Entity<InputState>,
    /// Sidebar and main area split; its sizes are remembered.
    main_split: Entity<ResizableState>,
    sidebar_width: f32,
    /// Height of the SQL editor pane (shared by new query tabs and remembered).
    pub(crate) editor_height: f32,
    /// The newest state written or still to be written, so a save on drop carries the
    /// final pane sizes.
    session_snapshot: Option<SessionState>,
    /// A debounced session save is scheduled.
    save_pending: bool,
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
        let dbeaver_offer_pending = !settings.dbeaver_import_offered;
        let (sidebar_width, editor_height) = (
            session.sidebar_width_or_default(),
            session.editor_height_or_default(),
        );
        let main_split = cx.new(|_| ResizableState::default());
        cx.subscribe(&main_split, |this, state, _: &ResizablePanelEvent, cx| {
            if let Some(w) = state.read(cx).sizes().first() {
                this.resized(Some(f32::from(*w)), None, cx);
            }
        })
        .detach();
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
                        settings.query_row_limit,
                        window,
                        cx,
                    )
                });
                cx.subscribe(&tab, |this, _, ev: &RowLimitChanged, cx| {
                    this.set_row_limit(ev.0, cx)
                })
                .detach();
                cx.subscribe(&tab, |this, tab, ev: &DatabaseChosen, cx| {
                    this.bind_query(&tab, Some(ev.0), cx)
                })
                .detach();
                cx.subscribe(&tab, |this, tab, ev: &DatabaseChosen, cx| {
                    this.bind_query(&tab, Some(ev.0), cx)
                })
                .detach();
                cx.subscribe(&tab, |this, _, ev: &EditorResized, cx| {
                    this.resized(None, Some(ev.0), cx);
                })
                .detach();
                tab.update(cx, |t, _| t.set_editor_height(editor_height));
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
        if let Some(text) = &session.tree_filter {
            tree_filter.update(cx, |i, cx| i.set_value(text.clone(), window, cx));
        }
        let mut this = Self {
            tabs,
            active_tab: 0,
            next_query,
            session_path,
            window_size: None,
            pending_activity: session.activity.clone(),
            pending_expanded: session.expanded.clone(),
            collapsed_groups: session.collapsed_groups.iter().cloned().collect(),
            dbeaver_offer_pending,
            pending_active: session.active.clone(),
            sidebar_focus: cx.focus_handle(),
            sidebar_scroll: UniformListScrollHandle::new(),
            cursor: None,
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
            update_etag: None,
            tree_filter,
            main_split,
            sidebar_width,
            editor_height,
            session_snapshot: Some(session.clone()),
            save_pending: false,
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

    /// Writes the restorable state (open databases, queries, tables, window size).
    /// Describes the front tab so it can be found again after a restart.
    fn active_descriptor(&self, cx: &App) -> Option<ActiveTab> {
        match self.tabs.get(self.active_tab)? {
            TabKind::Query(..) => {
                let index = self.tabs[..self.active_tab]
                    .iter()
                    .filter(|t| matches!(t, TabKind::Query(..)))
                    .count();
                Some(ActiveTab::Query { index })
            }
            TabKind::Table(v, sid) => {
                let s = self.session(*sid)?;
                let t = v.read(cx).table();
                Some(ActiveTab::Table {
                    connection: s.profile.name.clone(),
                    database: s.profile.database.clone(),
                    schema: t.schema.clone(),
                    name: t.name.clone(),
                })
            }
            TabKind::Activity(_, sid) => {
                let s = self.session(*sid)?;
                Some(ActiveTab::Activity {
                    connection: s.profile.name.clone(),
                    database: s.profile.database.clone(),
                })
            }
        }
    }

    /// Writes the restorable state: open databases, tabs, tree state, layout and window size.
    /// Items still waiting to be restored are kept so an early save doesn't lose them.
    pub(crate) fn save_session(&mut self, cx: &App) {
        if let Some(state) = self.session_state(cx) {
            self.write_session(state);
        }
    }

    /// Remembers pane sizes from a resize event and writes the session once the drag
    /// pauses, so a divider drag costs one write instead of one per frame.
    pub(crate) fn resized(
        &mut self,
        sidebar: Option<f32>,
        editor: Option<f32>,
        cx: &mut Context<Self>,
    ) {
        if let Some(w) = sidebar {
            self.sidebar_width = w;
        }
        if let Some(h) = editor {
            self.editor_height = h;
        }
        if let Some(state) = &mut self.session_snapshot {
            state.sidebar_width = Some(self.sidebar_width);
            state.editor_height = Some(self.editor_height);
        }
        if self.save_pending {
            return;
        }
        self.save_pending = true;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SAVE_DEBOUNCE).await;
            this.update(cx, |this, cx| {
                this.save_pending = false;
                this.save_session(cx);
            })
            .ok();
        })
        .detach();
    }

    /// Writes `state` and keeps it, so a save on drop of the workspace can repeat it.
    fn write_session(&mut self, state: SessionState) {
        let Some(path) = &self.session_path else {
            return;
        };
        if let Err(e) = state.save(path) {
            log::warn!("could not save session: {e:#}");
        }
        self.session_snapshot = Some(state);
    }

    /// The state to restore right now; `None` when the session is not persisted.
    fn session_state(&self, cx: &App) -> Option<SessionState> {
        self.session_path.as_ref()?;
        let mut tables: Vec<pgroove_core::session::SessionTable> = self
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
            .collect();
        tables.extend(
            self.pending_tables
                .iter()
                .map(|p| pgroove_core::session::SessionTable {
                    connection: p.connection.clone(),
                    database: p.database.clone(),
                    schema: p.table.schema.clone(),
                    name: p.table.name.clone(),
                }),
        );
        let mut activity: Vec<(String, String)> = self
            .tabs
            .iter()
            .filter_map(|t| match t {
                TabKind::Activity(_, sid) => {
                    let s = self.session(*sid)?;
                    Some((s.profile.name.clone(), s.profile.database.clone()))
                }
                _ => None,
            })
            .collect();
        activity.extend(self.pending_activity.iter().cloned());
        let mut expanded: Vec<SessionExpanded> = self
            .sessions
            .iter()
            .flat_map(|s| {
                s.tree
                    .expanded_schemas()
                    .into_iter()
                    .map(|schema| SessionExpanded {
                        connection: s.profile.name.clone(),
                        database: s.profile.database.clone(),
                        schema,
                    })
            })
            .collect();
        expanded.extend(self.pending_expanded.iter().cloned());
        let filter = self.tree_filter.read(cx).value().to_string();
        // Every saved connection gets a session at startup, so this list stays complete even
        // while databases are still connecting or have failed.
        let connections: Vec<(String, String)> = self
            .sessions
            .iter()
            .map(|s| (s.profile.name.clone(), s.profile.database.clone()))
            .collect();
        let state = SessionState {
            connections,
            queries: self
                .tabs
                .iter()
                .filter_map(|t| match t {
                    TabKind::Query(q, _) => Some(q.read(cx).text(cx)),
                    _ => None,
                })
                .collect(),
            tables,
            window: self.window_size,
            sidebar_width: Some(self.sidebar_width),
            editor_height: Some(self.editor_height),
            activity,
            expanded,
            active: self
                .pending_active
                .clone()
                .or_else(|| self.active_descriptor(cx)),
            tree_filter: (!filter.trim().is_empty()).then_some(filter),
            collapsed_groups: self.collapsed_groups.iter().cloned().collect(),
        };
        Some(state)
    }

    /// Remembers a new row limit and gives it to every query tab.
    pub(crate) fn set_row_limit(&mut self, limit: u32, cx: &mut Context<Self>) {
        let limit = pgroove_core::settings::clamp_row_limit(limit);
        self.settings.query_row_limit = limit;
        if let Some(path) = &self.settings_path
            && let Err(e) = self.settings.save(path)
        {
            log::warn!("could not save settings: {e:#}");
        }
        for t in &self.tabs {
            if let TabKind::Query(q, _) = t {
                q.update(cx, |q, cx| q.apply_limit(limit, cx));
            }
        }
    }

    /// Remembers a new data-viewer page size and gives it to every table tab.
    pub(crate) fn set_page_size(&mut self, size: u32, cx: &mut Context<Self>) {
        let size = pgroove_core::settings::clamp_row_limit(size);
        self.settings.table_page_size = size;
        if let Some(path) = &self.settings_path
            && let Err(e) = self.settings.save(path)
        {
            log::warn!("could not save settings: {e:#}");
        }
        for t in &self.tabs {
            if let TabKind::Table(v, _) = t {
                v.update(cx, |v, cx| v.apply_page_size(size, cx));
            }
        }
    }

    pub(crate) fn save_settings(&self) {
        if let Some(path) = &self.settings_path
            && let Err(e) = self.settings.save(path)
        {
            log::warn!("could not save settings: {e:#}");
        }
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
        let theme = cx.theme().clone();
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
                    crate::title_bar::drag_area("title-drag-name", window, cx)
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
                        .tooltip(
                            match self.focused_session(cx).and_then(|id| self.session(id)) {
                                Some(s) => format!("Sessions, locks and more for {}", s.label()),
                                None => "Sessions, locks, statements and more".to_string(),
                            },
                        )
                        .on_click(cx.listener(|this, _, window, cx| {
                            if let Some(sid) = this.focused_session(cx) {
                                this.open_activity(sid, window, cx);
                            }
                        })),
                )
                .children(self.update_chip(cx))
                .child(
                    crate::title_bar::drag_area("title-drag-fill", window, cx)
                        .flex_1()
                        .h_full(),
                )
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

    /// Hands a query tab the label and choices for its database picker.
    fn sync_query_databases(&self, q: &Entity<QueryTab>, cx: &mut Context<Self>) {
        let label = q
            .read(cx)
            .session()
            .and_then(|id| self.session(id))
            .map_or_else(|| "No database".to_string(), DbSession::label);
        let choices: Vec<(usize, String)> =
            self.ready_sessions().map(|s| (s.id, s.label())).collect();
        q.update(cx, |t, cx| {
            if t.set_databases(label, choices) {
                cx.notify();
            }
        });
    }

    /// Says which server and database the activity monitor shows, and lets you switch it.
    fn render_activity_server_bar(
        &self,
        view: &Entity<ActivityView>,
        sid: usize,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme().clone();
        let current = self.session(sid);
        let (label, detail) = match current {
            Some(s) => {
                let version = match &s.status {
                    SessionStatus::Ready { version } => format!(" · PostgreSQL {version}"),
                    _ => String::new(),
                };
                (s.label(), format!("{}{version}", s.detail()))
            }
            None => ("No database".to_string(), String::new()),
        };
        let choices: Vec<(usize, String)> =
            self.ready_sessions().map(|s| (s.id, s.label())).collect();
        let (this, view) = (cx.entity(), view.clone());
        h_flex()
            .px_3()
            .h(px(32.))
            .gap_2()
            .items_center()
            .border_b_1()
            .border_color(theme.border)
            .bg(theme.title_bar.opacity(0.5))
            .child(
                Icon::new(IconName::Server)
                    .size_4()
                    .text_color(theme.success),
            )
            .child(div().text_sm().font_weight(FontWeight::MEDIUM).child(label))
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(detail),
            )
            .child(div().flex_1())
            .child(
                Button::new("activity-switch")
                    .outline()
                    .xsmall()
                    .icon(IconName::ChevronsUpDown)
                    .label("Switch database")
                    .tooltip("Monitor another open database or server")
                    .dropdown_menu(move |menu, _, _| {
                        if choices.is_empty() {
                            return menu
                                .item(PopupMenuItem::new("No open databases").disabled(true));
                        }
                        choices.iter().fold(menu, |m, (id, label)| {
                            let (t, view, id) = (this.clone(), view.clone(), *id);
                            m.item(
                                PopupMenuItem::new(label.clone())
                                    .checked(id == sid)
                                    .on_click(move |_, _, cx| {
                                        t.update(cx, |ws, cx| ws.rebind_activity(&view, id, cx))
                                    }),
                            )
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
                this.save_session(cx);
                cx.notify();
            }));
        for (ix, kind) in self.tabs.iter().enumerate() {
            bar = bar.child(match kind {
                TabKind::Query(_, n) => Tab::new()
                    .label(format!("Query {n}"))
                    .prefix(
                        div()
                            .pl_3()
                            .pr_1()
                            .child(Icon::new(IconName::Code).size_4()),
                    )
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
                    .label(match self.session(*sid) {
                        Some(s) => format!("Activity · {}", s.label()),
                        None => "Activity".to_string(),
                    })
                    .prefix(
                        div()
                            .pl_3()
                            .pr_1()
                            .child(Icon::new(IconName::Activity).size_4()),
                    )
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
                    .prefix(
                        div()
                            .pl_3()
                            .pr_1()
                            .child(Icon::new(IconName::Table).size_4()),
                    )
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
            div().px_3().child(
                Button::new("new-query")
                    .ghost()
                    .xsmall()
                    .icon(IconName::Plus)
                    .tooltip("New query tab")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.new_query(window, cx);
                    })),
            ),
        )
    }

    fn render_main(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let content = match self.tabs.get(self.active_tab) {
            Some(TabKind::Table(v, _)) => {
                div().flex_1().min_h_0().child(v.clone()).into_any_element()
            }
            Some(TabKind::Activity(v, sid)) => {
                let (v, sid) = (v.clone(), *sid);
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .child(self.render_activity_server_bar(&v, sid, cx))
                    .child(div().flex_1().min_h_0().child(v))
                    .into_any_element()
            }
            Some(TabKind::Query(q, _)) => {
                let q = q.clone();
                self.sync_query_databases(&q, cx);
                div().flex_1().min_h_0().child(q).into_any_element()
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

/// "18.6 (Ubuntu 18.6-0ubuntu0.26.04.1)" -> "18.6".
fn short_version(version: &str) -> &str {
    version.split_whitespace().next().unwrap_or(version)
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
        self.restore_pending(window, cx);
        if std::mem::take(&mut self.dbeaver_offer_pending) {
            // Opening a dialog while rendering would not show it, so do it right after.
            cx.defer_in(window, |this, window, cx| {
                this.offer_dbeaver_import(true, window, cx)
            });
        }
        let (left, right) = match self.focused_session(cx).and_then(|id| self.session(id)) {
            Some(s) => match &s.status {
                SessionStatus::Ready { version } => (
                    format!("PostgreSQL {}", short_version(version)),
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
            .on_action(|_: &ToggleFullscreen, window, _| window.toggle_fullscreen())
            .on_action(|_: &crate::CloseWindow, window, _| window.remove_window())
            .rounded_tl(radii.top_left)
            .rounded_tr(radii.top_right)
            .rounded_bl(radii.bottom_left)
            .rounded_br(radii.bottom_right)
            .bg(theme.background)
            .text_color(theme.foreground)
            .child(self.render_toolbar(window, cx))
            .child(
                div().flex_1().min_h_0().child(
                    h_resizable("main-split")
                        .with_state(&self.main_split)
                        .child(
                            resizable_panel()
                                .size(px(self.sidebar_width))
                                .size_range(
                                    px(pgroove_core::session::SIDEBAR_WIDTH.1)
                                        ..px(pgroove_core::session::SIDEBAR_WIDTH.2),
                                )
                                .child(self.render_sidebar(cx)),
                        )
                        // Keep the toolbar and tabs readable however wide the sidebar gets.
                        .child(
                            resizable_panel()
                                .size_range(px(480.)..px(20_000.))
                                .child(self.render_main(cx)),
                        ),
                ),
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

impl Drop for Workspace {
    /// Writes a debounced save that is still pending; cheap when there is none.
    fn drop(&mut self) {
        if self.save_pending
            && let Some(state) = self.session_snapshot.clone()
        {
            self.write_session(state);
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::sessions::SessionStatus;

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
        assert_eq!(super::short_version("18.6 (Ubuntu 18.6-0ubuntu0)"), "18.6");
        assert_eq!(super::short_version(""), "");
        assert_eq!(
            SessionStatus::Failed("x".into()).text("a / b"),
            "Couldn't connect to a / b: x"
        );
    }
}
