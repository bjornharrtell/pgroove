use gpui_kit::component::notification::Notification;
use gpui_kit::component::*;
use gpui_kit::*;
use pgroove_core::catalog::ObjectKind;
use pgroove_core::data::TableRef;

use crate::activity_view::ActivityView;
use crate::mode::ReadOnlyMode;
use crate::query_tab::{EditorResized, QueryTab, RowLimitChanged};
use crate::settings_view::{ReloadThemesRequested, SettingsChanged, SettingsView, effects};
use crate::sql_completion::SharedIndex;
use crate::structure_view::OpenTable;
use crate::table_view::{AllowEditing, PageSizeChanged, TableView};
use crate::workspace::{Workspace, flatten};

pub(crate) enum TabKind {
    Query(Entity<QueryTab>, usize),
    Activity(Entity<ActivityView>, usize),
    Table(Entity<TableView>, usize),
    /// The settings pane; there is at most one.
    Settings(Entity<crate::settings_view::SettingsView>),
}

impl Workspace {
    /// Has every open grid rebuild its columns (the kit caches their widths).
    fn rescale_grids(&mut self, cx: &mut Context<Self>) {
        for tab in &self.tabs {
            match tab {
                TabKind::Query(q, _) => q.update(cx, |q, cx| q.rescale(cx)),
                TabKind::Table(t, _) => t.update(cx, |t, cx| t.rescale(cx)),
                TabKind::Activity(..) | TabKind::Settings(_) => {}
            }
        }
        cx.notify();
    }

    /// The query tab to use for inserted text: the active one, else any, else a new one.
    pub(crate) fn query_tab(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<QueryTab> {
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

    pub(crate) fn new_query(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<QueryTab> {
        let focused = self.focused_session(cx);
        let (runtime, history, snippets) = (
            self.runtime.clone(),
            self.history.clone(),
            self.snippets.clone(),
        );
        let limit = self.settings.query_row_limit;
        let tab = cx.new(|cx| {
            QueryTab::new(
                runtime,
                SharedIndex::default(),
                history,
                snippets,
                limit,
                window,
                cx,
            )
        });
        cx.subscribe(&tab, |this, _, ev: &RowLimitChanged, cx| {
            this.set_row_limit(ev.0, cx)
        })
        .detach();
        cx.subscribe(&tab, |this, _, ev: &EditorResized, cx| {
            this.resized(None, Some(ev.0), cx);
        })
        .detach();
        let height = self.editor_height;
        tab.update(cx, |t, _| t.set_editor_height(height));
        self.bind_query(&tab, focused, cx);
        self.tabs.push(TabKind::Query(tab.clone(), self.next_query));
        self.next_query += 1;
        self.active_tab = self.tabs.len() - 1;
        cx.notify();
        tab
    }

    /// A new query tab bound to database `sid`, as the object tree's "Open Tab Here" asks for.
    /// Always a new tab, unlike `query_tab_for`, which reuses one.
    pub(crate) fn open_tab_here(
        &mut self,
        sid: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<QueryTab> {
        let tab = self.new_query(window, cx);
        self.bind_query(&tab, Some(sid), cx);
        self.last_session = Some(sid);
        tab
    }

    /// The query tab to put generated text for database `sid` in: the active tab when it is
    /// bound to that database, else another tab bound to it, else a new tab bound to it.
    pub(crate) fn query_tab_for(
        &mut self,
        sid: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<QueryTab> {
        let found = {
            let bound = |t: &TabKind| match t {
                TabKind::Query(q, _) => q.read(cx).session() == Some(sid),
                _ => false,
            };
            let active = self.tabs.get(self.active_tab).filter(|t| bound(t));
            let ix = match active {
                Some(_) => Some(self.active_tab),
                None => self.tabs.iter().position(bound),
            };
            ix.and_then(|ix| match &self.tabs[ix] {
                TabKind::Query(q, _) => Some((ix, q.clone())),
                _ => None,
            })
        };
        if let Some((ix, tab)) = found {
            self.active_tab = ix;
            return tab;
        }
        let tab = self.new_query(window, cx);
        self.bind_query(&tab, Some(sid), cx);
        tab
    }

    pub(crate) fn open_structure(
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

    /// Generates `statement` for `table` from its structure and shows it in a query tab of
    /// the database it came from, as the object tree's "Generate SQL" submenu asks for.
    pub(crate) fn generate_sql(
        &mut self,
        sid: usize,
        table: TableRef,
        statement: pgroove_core::sqlgen::Statement,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(client) = self.client_of(sid) else {
            return;
        };
        let load_table = table.clone();
        let task = self
            .runtime
            .spawn(async move { pgroove_core::structure::load(&client, &load_table).await });
        cx.spawn_in(window, async move |this, cx| {
            let result = flatten(task.await);
            this.update_in(cx, |this, window, cx| match result {
                Ok(st) => match pgroove_core::sqlgen::generate(&table, &st, statement) {
                    Some(sql) => {
                        let sql = if this.settings.uppercase_generated_sql {
                            pgroove_core::sqlgen::uppercase_keywords(&sql)
                        } else {
                            sql
                        };
                        let tab = this.query_tab_for(sid, window, cx);
                        tab.update(cx, |t, cx| t.append(&sql, window, cx));
                        cx.notify();
                    }
                    None => window.push_notification(
                        Notification::info(format!(
                            "{} is not available for {}",
                            statement.label(),
                            table.qualified()
                        )),
                        cx,
                    ),
                },
                Err(e) => window.push_notification(Notification::error(e), cx),
            })
            .ok();
        })
        .detach();
    }

    pub(crate) fn show_definition(
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
                Ok(text) => {
                    let tab = this.query_tab_for(sid, window, cx);
                    tab.update(cx, |t, cx| t.append(&text, window, cx));
                    cx.notify();
                }
                Err(e) => window.push_notification(Notification::error(e), cx),
            })
            .ok();
        })
        .detach();
    }

    pub(crate) fn open_table(
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
            let page_size = self.settings.table_page_size;
            let view = cx.new(|cx| TableView::new(table, client, runtime, page_size, window, cx));
            cx.subscribe(&view, |this, _, ev: &PageSizeChanged, cx| {
                this.set_page_size(ev.0, cx)
            })
            .detach();
            cx.subscribe(&view, |this, _, _: &AllowEditing, cx| {
                if cx.global::<ReadOnlyMode>().0 {
                    this.toggle_read_only(cx);
                }
            })
            .detach();
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

    pub(crate) fn open_activity(
        &mut self,
        sid: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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

    /// Opens the settings tab, or brings the open one to the front.
    pub(crate) fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let themes = crate::theme::available(cx);
        if let Some(ix) = self
            .tabs
            .iter()
            .position(|t| matches!(t, TabKind::Settings(_)))
        {
            self.active_tab = ix;
            if let TabKind::Settings(v) = &self.tabs[ix] {
                let settings = self.settings.clone();
                v.update(cx, |v, cx| v.sync(settings, themes, cx));
            }
        } else {
            let view = cx.new(|_| SettingsView::new(self.settings.clone(), themes));
            cx.subscribe_in(
                &view,
                window,
                |this, _, ev: &SettingsChanged, window, cx| {
                    this.settings_edited(ev.0.clone(), window, cx)
                },
            )
            .detach();
            cx.subscribe_in(
                &view,
                window,
                |this, view, _: &ReloadThemesRequested, window, cx| {
                    crate::theme::install(cx);
                    crate::theme::apply(&this.settings, Some(window), cx);
                    let (settings, themes) = (this.settings.clone(), crate::theme::available(cx));
                    view.update(cx, |v, cx| v.sync(settings, themes, cx));
                },
            )
            .detach();
            self.tabs.push(TabKind::Settings(view));
            self.active_tab = self.tabs.len() - 1;
        }
        cx.notify();
    }

    /// Settings were changed in the pane: remember them and apply what differs, at once.
    fn settings_edited(
        &mut self,
        new: pgroove_core::settings::Settings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let fx = effects(&self.settings, &new);
        if !fx.save {
            return;
        }
        self.settings = new;
        self.save_settings();
        if fx.theme {
            crate::theme::apply(&self.settings, Some(window), cx);
        }
        if fx.grid {
            self.rescale_grids(cx);
        }
        if fx.row_limit {
            self.set_row_limit(self.settings.query_row_limit, cx);
        }
        if fx.page_size {
            self.set_page_size(self.settings.table_page_size, cx);
        }
        cx.notify();
    }

    pub(crate) fn close_tab(&mut self, ix: usize, cx: &mut Context<Self>) {
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

    /// Closes every tab for which `keep` is false, like single close (no
    /// unsaved-text prompt). Keeps the active tab if it survives, else the
    /// nearest earlier survivor.
    pub(crate) fn retain_tabs(&mut self, keep: impl Fn(usize) -> bool, cx: &mut Context<Self>) {
        let active = self.active_tab;
        let new_active = (0..=active)
            .rev()
            .find(|&i| keep(i))
            .map(|i| (0..i).filter(|&j| keep(j)).count())
            .or_else(|| (active..self.tabs.len()).find(|&i| keep(i)).map(|_| 0))
            .unwrap_or(0);
        let mut i = 0;
        self.tabs.retain(|_| {
            let k = keep(i);
            i += 1;
            k
        });
        self.active_tab = new_active.min(self.tabs.len().saturating_sub(1));
        self.save_session(cx);
        cx.notify();
    }

    pub(crate) fn active_query(&self) -> Option<Entity<QueryTab>> {
        match self.tabs.get(self.active_tab) {
            Some(TabKind::Query(q, _)) => Some(q.clone()),
            _ => None,
        }
    }

    /// Points an activity tab at another open database.
    pub(crate) fn rebind_activity(
        &mut self,
        view: &Entity<ActivityView>,
        sid: usize,
        cx: &mut Context<Self>,
    ) {
        let Some(client) = self.client_of(sid) else {
            return;
        };
        for t in &mut self.tabs {
            if let TabKind::Activity(v, s) = t
                && v == view
            {
                *s = sid;
            }
        }
        view.update(cx, |v, cx| v.rebind(client, cx));
        self.last_session = Some(sid);
        self.save_session(cx);
        cx.notify();
    }
}
