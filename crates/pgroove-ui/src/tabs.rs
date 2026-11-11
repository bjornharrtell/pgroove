use gpui_kit::component::notification::Notification;
use gpui_kit::component::*;
use gpui_kit::*;
use pgroove_core::catalog::ObjectKind;
use pgroove_core::data::TableRef;

use crate::activity_view::ActivityView;
use crate::mode::ReadOnlyMode;
use crate::query_tab::{EditorResized, QueryTab, RowLimitChanged};
use crate::sql_completion::SharedIndex;
use crate::structure_view::OpenTable;
use crate::table_view::{AllowEditing, PageSizeChanged, TableView};
use crate::workspace::{Workspace, flatten};

pub(crate) enum TabKind {
    Query(Entity<QueryTab>, usize),
    Activity(Entity<ActivityView>, usize),
    Table(Entity<TableView>, usize),
}

impl Workspace {
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

    pub(crate) fn append_to_editor(
        &mut self,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let tab = self.query_tab(window, cx);
        tab.update(cx, |t, cx| t.append(text, window, cx));
        cx.notify();
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

    /// Generates `statement` for `table` from its structure and shows it in the editor,
    /// as the object tree's "Generate SQL" submenu asks for.
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
                    Some(sql) => this.append_to_editor(&sql, window, cx),
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
                Ok(text) => this.append_to_editor(&text, window, cx),
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
