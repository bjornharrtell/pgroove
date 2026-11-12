use std::ops::Range;
use std::rc::Rc;

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::Input;
use gpui_kit::component::menu::{ContextMenuExt, PopupMenu, PopupMenuItem};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use pgroove_core::ConnectionProfile;
use pgroove_core::catalog::{self, ObjectKind};
use pgroove_core::data::TableRef;
use pgroove_core::sqlgen;

use crate::schema_tree::Row;
use crate::sessions::{DbSession, SessionStatus};
use crate::tabs::TabKind;
use crate::workspace::{
    SidebarActivate, SidebarCollapse, SidebarDown, SidebarExpand, SidebarUp, Workspace,
};

/// Moves a list cursor by `delta` rows, staying inside `0..len`. With no cursor yet, moving
/// down selects the first row and moving up the last.
pub fn step_cursor(current: Option<usize>, len: usize, delta: isize) -> Option<usize> {
    if len == 0 {
        return None;
    }
    Some(match current {
        None if delta >= 0 => 0,
        None => len - 1,
        Some(c) => (c.min(len - 1) as isize + delta).clamp(0, len as isize - 1) as usize,
    })
}

/// Height of every sidebar row; the virtualized list needs them all to be the same.
const ROW_HEIGHT: f32 = 28.;

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

/// The size shown at the right edge of a relation row; empty without an estimate.
fn size_text(size: Option<i64>) -> String {
    size.map(catalog::format_size).unwrap_or_default()
}

/// Tooltip of an object row: its kind, plus the size when the catalog has one.
fn row_tip(kind: ObjectKind, size: Option<i64>) -> String {
    match size.map(catalog::format_size) {
        Some(text) => format!("{} · about {text}", kind.label()),
        None => kind.label().to_string(),
    }
}

#[derive(Clone)]
enum SidebarItem {
    /// Header of a group of connections (only when there are several groups).
    Group {
        /// Lowercase key, as stored in the session.
        key: String,
        /// Name to show; empty for connections without a group.
        label: String,
        count: usize,
        collapsed: bool,
    },
    Connection(usize, ConnectionProfile),
    Database {
        profile: ConnectionProfile,
        session: Option<usize>,
    },
    Tree(usize, usize, Row),
    Note(String),
}

impl Workspace {
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
            .h(px(ROW_HEIGHT))
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
                        let open = refresh_this.clone();
                        menu.item(PopupMenuItem::new("Open Tab Here").on_click(
                            move |_, window, cx| {
                                open.update(cx, |ws, cx| {
                                    ws.open_tab_here(sid, window, cx);
                                })
                            },
                        ))
                        .item(PopupMenuItem::new("Refresh").on_click(move |_, _, cx| {
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
            } => self
                .tree_row(("category", i), 3, cx)
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
                .child(category.label())
                .child(div().text_xs().text_color(muted).child(count.to_string()))
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(s) = this.session_mut(sid) {
                        s.tree.toggle_category(&schema, category);
                    }
                    cx.notify();
                }))
                .into_any_element(),
            Row::Object {
                schema,
                name,
                kind,
                size,
            } => {
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
                    .tree_row_px(("object", i), leaf_indent(4), cx)
                    .child(Icon::new(icon).size_4().text_color(muted))
                    // The name yields space so the trailing size sits at the right edge.
                    .child(div().flex_1().truncate().child(name.clone()))
                    .child(div().text_xs().text_color(muted).child(size_text(size)))
                    .tooltip(move |window, cx| Tooltip::new(row_tip(kind, size)).build(window, cx));
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
                // MERGE arrived in PostgreSQL 15, so older servers do not offer it.
                let merge = self.session(sid).is_some_and(|s| match &s.status {
                    SessionStatus::Ready { version } => sqlgen::supports_merge(version),
                    _ => false,
                });
                row.context_menu(move |menu, window, cx| {
                    let qualified = format!(
                        "{}.{}",
                        pgroove_core::data::pretty_ident(&s),
                        pgroove_core::data::pretty_ident(&n)
                    );
                    let table = TableRef::new(s.clone(), n.clone());
                    let mut menu = menu;
                    if kind.is_relation() {
                        let (t1, t2, tb1, tb2) = (
                            ctx_this.clone(),
                            ctx_this.clone(),
                            table.clone(),
                            table.clone(),
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
                            ));
                        let submenu = PopupMenu::build(window, cx, {
                            let this = ctx_this.clone();
                            let table = table.clone();
                            move |menu, _, _| {
                                let mut menu = menu;
                                for statement in sqlgen::Statement::ALL {
                                    if statement == sqlgen::Statement::Merge && !merge {
                                        continue;
                                    }
                                    let (this, table) = (this.clone(), table.clone());
                                    menu =
                                        menu.item(PopupMenuItem::new(statement.label()).on_click(
                                            move |_, window, cx| {
                                                this.update(cx, |ws, cx| {
                                                    ws.generate_sql(
                                                        sid,
                                                        table.clone(),
                                                        statement,
                                                        window,
                                                        cx,
                                                    )
                                                })
                                            },
                                        ));
                                }
                                menu
                            }
                        });
                        menu = menu.item(PopupMenuItem::submenu("Generate SQL", submenu));
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
                .h(px(ROW_HEIGHT))
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
            .group("conn-row")
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
            .children(p.color.map(|c| {
                div()
                    .size(px(8.))
                    .flex_shrink_0()
                    .rounded_full()
                    .bg(rgb(c.rgb()))
            }))
            .child(
                h_flex()
                    .invisible()
                    .group_hover("conn-row", |d| d.visible())
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
                            .on_click(cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.confirm_delete(pd.clone(), window, cx)
                            })),
                    ),
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

    /// The rows of the sidebar tree, top to bottom.
    fn sidebar_items(&self) -> Vec<SidebarItem> {
        let mut items: Vec<SidebarItem> = Vec::new();
        let profiles = self.store.profiles();
        let grouped = pgroove_core::profile::has_several_groups(profiles);
        let mut current: Option<String> = None;
        for (i, p) in pgroove_core::profile::grouped_order(profiles)
            .into_iter()
            .map(|(i, p)| (i, p.clone()))
        {
            if grouped {
                let key = p.group.trim().to_lowercase();
                if current.as_ref() != Some(&key) {
                    let count = profiles
                        .iter()
                        .filter(|q| q.group.trim().to_lowercase() == key)
                        .count();
                    items.push(SidebarItem::Group {
                        collapsed: self.collapsed_groups.contains(&key),
                        label: p.group.trim().to_string(),
                        count,
                        key: key.clone(),
                    });
                    current = Some(key);
                }
                if self
                    .collapsed_groups
                    .contains(&p.group.trim().to_lowercase())
                {
                    continue;
                }
            }
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
            for s in open.iter().filter(|_| p.show_all_databases) {
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
        items
    }

    /// The table shown in the front tab, with the database it belongs to.
    fn active_table(&self, cx: &App) -> Option<(usize, TableRef)> {
        match self.tabs.get(self.active_tab)? {
            TabKind::Table(v, sid) => Some((*sid, v.read(cx).table().clone())),
            _ => None,
        }
    }

    /// Whether a sidebar row is expanded (`None` for rows that can't expand).
    fn item_expanded(&self, item: &SidebarItem) -> Option<bool> {
        match item {
            SidebarItem::Group { collapsed, .. } => Some(!collapsed),
            SidebarItem::Connection(_, p) => {
                Some(self.sessions.iter().any(|s| s.profile.name == p.name))
            }
            SidebarItem::Database { session, .. } => Some(session.is_some()),
            SidebarItem::Tree(
                _,
                _,
                Row::Schema { expanded, .. }
                | Row::Category { expanded, .. }
                | Row::ExtensionsHeader { expanded, .. },
            ) => Some(*expanded),
            _ => None,
        }
    }

    /// Does what clicking the row does.
    fn activate_item(&mut self, item: &SidebarItem, window: &mut Window, cx: &mut Context<Self>) {
        match item {
            SidebarItem::Group { key, .. } => self.toggle_group(key.clone(), cx),
            SidebarItem::Connection(_, p) => {
                if self.sessions.iter().any(|s| s.profile.name == p.name) {
                    self.close_profile(&p.name, cx);
                } else {
                    self.open_session(p.clone(), cx);
                }
            }
            SidebarItem::Database { profile, session } => match session {
                Some(id) => self.close_session(*id, cx),
                None => {
                    self.open_session(profile.clone(), cx);
                }
            },
            SidebarItem::Tree(sid, _, row) => match row {
                Row::Schema { name, .. } => self.toggle_schema(*sid, name.clone(), cx),
                Row::Category {
                    schema, category, ..
                } => {
                    if let Some(s) = self.session_mut(*sid) {
                        s.tree.toggle_category(schema, *category);
                    }
                    cx.notify();
                }
                Row::ExtensionsHeader { .. } => {
                    if let Some(s) = self.session_mut(*sid) {
                        s.tree.toggle_extensions();
                    }
                    cx.notify();
                }
                Row::Object {
                    schema, name, kind, ..
                } => {
                    if kind.is_relation() {
                        let table = TableRef::new(schema.clone(), name.clone());
                        self.open_table(*sid, table, window, cx);
                    } else if matches!(
                        kind,
                        ObjectKind::Function | ObjectKind::Procedure | ObjectKind::EnumType
                    ) {
                        self.show_definition(*sid, schema.clone(), name.clone(), *kind, window, cx);
                    }
                }
                Row::Extension { .. } | Row::Message(_) => {}
            },
            SidebarItem::Note(_) => {}
        }
    }

    fn move_cursor(&mut self, delta: isize, cx: &mut Context<Self>) {
        let len = self.sidebar_items().len();
        self.cursor = step_cursor(self.cursor, len, delta);
        if let Some(c) = self.cursor {
            self.sidebar_scroll
                .scroll_to_item(c, ScrollStrategy::Nearest);
        }
        cx.notify();
    }

    /// Right and left arrows: expand a collapsed row, or collapse an expanded one.
    fn expand_cursor(&mut self, expand: bool, window: &mut Window, cx: &mut Context<Self>) {
        let items = self.sidebar_items();
        let Some(item) = self.cursor.and_then(|c| items.get(c)) else {
            return;
        };
        if self.item_expanded(item) == Some(!expand) {
            self.activate_item(item, window, cx);
        }
    }

    fn toggle_group(&mut self, key: String, cx: &mut Context<Self>) {
        if !self.collapsed_groups.remove(&key) {
            self.collapsed_groups.insert(key);
        }
        self.save_session(cx);
        cx.notify();
    }

    fn group_row(
        &self,
        ix: usize,
        key: &str,
        label: &str,
        count: usize,
        collapsed: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let key = key.to_string();
        let hover = theme.sidebar_accent;
        let label = if label.is_empty() { "Ungrouped" } else { label };
        // Looks like an accordion header (title left, chevron right, ruled between groups);
        // the rows stay in the virtualized list, which an accordion's content could not be.
        // The list assumes every row is the same height, so the header keeps that height; a
        // full-width tinted strip sets it apart.
        div()
            .h(px(ROW_HEIGHT))
            .flex_shrink_0()
            .child(
                h_flex()
                    .id(("group", ix))
                    .size_full()
                    .px_3()
                    .gap_2()
                    .items_center()
                    .bg(theme.foreground.opacity(0.06))
                    .cursor_pointer()
                    .hover(move |d| d.bg(hover))
                    .child(
                        div()
                            .flex_1()
                            .truncate()
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .child(label.to_string()),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(count.to_string()),
                    )
                    .child(
                        Icon::new(IconName::ChevronDown)
                            .xsmall()
                            .flex_none()
                            .text_color(theme.muted_foreground)
                            .rotate(percentage(if collapsed { 0. } else { 0.5 })),
                    )
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.toggle_group(key.clone(), cx)),
                    ),
            )
            .into_any_element()
    }

    pub(crate) fn render_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let items = self.sidebar_items();
        let empty = items.is_empty();
        let items = Rc::new(items);
        let count = items.len();
        let list = uniform_list(
            "sidebar-rows",
            count,
            cx.processor(move |this, range: Range<usize>, window, cx| {
                let theme = cx.theme().clone();
                let open_table = this.active_table(cx);
                let focused = this.sidebar_focus.contains_focused(window, cx);
                range
                    .map(|ix| {
                        let element = match &items[ix] {
                            SidebarItem::Group {
                                key,
                                label,
                                count,
                                collapsed,
                            } => this.group_row(ix, key, label, *count, *collapsed, cx),
                            SidebarItem::Connection(i, p) => this.connection_row(*i, p.clone(), cx),
                            SidebarItem::Database { profile, session } => {
                                this.database_row(ix, profile.clone(), *session, cx)
                            }
                            SidebarItem::Tree(sid, j, r) => {
                                this.render_tree_item(*sid, *j, r.clone(), cx)
                            }
                            SidebarItem::Note(m) => div()
                                .pl(px(leaf_indent(2)))
                                .h(px(ROW_HEIGHT))
                                .flex_shrink_0()
                                .flex()
                                .items_center()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(m.clone())
                                .into_any_element(),
                        };
                        // The table open in the front tab is marked; the keyboard cursor
                        // shows only while the sidebar has focus.
                        let is_open = matches!(
                            (&items[ix], &open_table),
                            (
                                SidebarItem::Tree(sid, _, Row::Object { schema, name, .. }),
                                Some((open_sid, t)),
                            ) if sid == open_sid && *schema == t.schema && *name == t.name
                        );
                        let is_cursor = focused && this.cursor == Some(ix);
                        div()
                            .w_full()
                            .h(px(ROW_HEIGHT))
                            .flex_shrink_0()
                            .border_l_2()
                            .border_color(if is_cursor {
                                theme.primary
                            } else {
                                gpui::transparent_black()
                            })
                            .when(is_open, |d| d.bg(theme.list_active))
                            .when(is_cursor, |d| d.bg(theme.sidebar_accent))
                            .child(element)
                    })
                    .collect::<Vec<_>>()
            }),
        )
        .track_scroll(&self.sidebar_scroll)
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
            .size_full()
            .track_focus(&self.sidebar_focus)
            .key_context("Sidebar")
            .on_action(cx.listener(|this, _: &SidebarUp, _, cx| this.move_cursor(-1, cx)))
            .on_action(cx.listener(|this, _: &SidebarDown, _, cx| this.move_cursor(1, cx)))
            .on_action(cx.listener(|this, _: &SidebarActivate, window, cx| {
                let items = this.sidebar_items();
                if let Some(item) = this.cursor.and_then(|c| items.get(c)) {
                    this.activate_item(item, window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &SidebarExpand, window, cx| {
                this.expand_cursor(true, window, cx)
            }))
            .on_action(cx.listener(|this, _: &SidebarCollapse, window, cx| {
                this.expand_cursor(false, window, cx)
            }))
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
                    .pb_2()
                    .child(Input::new(&self.tree_filter).small()),
            )
            .child(
                div()
                    .id("sidebar-list")
                    .relative()
                    .flex_1()
                    .min_h_0()
                    // Clicking the tree gives it keyboard focus (the search box keeps its own).
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, window, cx| {
                            window.focus(&this.sidebar_focus, cx);
                            cx.notify();
                        }),
                    )
                    .child(body)
                    // The list scrolls with the wheel; the bar shows how much there is.
                    .when(!empty, |d| {
                        use gpui_kit::component::scroll::{Scrollbar, ScrollbarMode};
                        d.child(
                            div()
                                .absolute()
                                .top_0()
                                .right_0()
                                .bottom_0()
                                .w(Scrollbar::width())
                                .child(
                                    Scrollbar::vertical(&self.sidebar_scroll)
                                        .mode(ScrollbarMode::Scrolling)
                                        .viewport_from_layout(),
                                ),
                        )
                    }),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::{leaf_indent, row_tip, size_text, step_cursor};

    #[test]
    fn object_rows_show_the_catalog_size() {
        assert_eq!(size_text(Some(1536)), "1.5 kB");
        assert_eq!(size_text(None), "", "no estimate, nothing to show");
        assert_eq!(
            row_tip(pgroove_core::catalog::ObjectKind::Table, Some(1536)),
            "table · about 1.5 kB"
        );
        assert_eq!(
            row_tip(pgroove_core::catalog::ObjectKind::View, None),
            "view"
        );
    }

    #[test]
    fn cursor_steps_stay_in_range() {
        assert_eq!(step_cursor(None, 0, 1), None, "an empty list has no cursor");
        assert_eq!(step_cursor(None, 5, 1), Some(0));
        assert_eq!(step_cursor(None, 5, -1), Some(4));
        assert_eq!(step_cursor(Some(0), 5, -1), Some(0), "clamped at the top");
        assert_eq!(step_cursor(Some(4), 5, 1), Some(4), "clamped at the bottom");
        assert_eq!(step_cursor(Some(2), 5, 1), Some(3));
        assert_eq!(step_cursor(Some(2), 5, -1), Some(1));
        assert_eq!(
            step_cursor(Some(9), 5, 0),
            Some(4),
            "a stale cursor is pulled back in"
        );
        assert_eq!(step_cursor(Some(3), 0, 1), None);
    }

    #[test]
    fn leaf_icons_align_with_the_parent_icon() {
        // A parent's icon sits after its chevron; its leaf rows start at the same x.
        assert_eq!(leaf_indent(3), 10. + 3. * 16. + 22.);
        assert!(leaf_indent(3) > leaf_indent(2));
    }
}
