//! Result grid backed by gpui-kit's table.

use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::component::menu::{ContextMenuExt as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::table::{Column, ColumnSort, TableDelegate, TableState};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme, WindowExt as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use pgroove_core::edit::{EditSession, InsertValue};
use pgroove_core::query::ResultSet;
use pgroove_core::settings::scaled;

use crate::selection::{CellSelection, Marking, Mods};

/// Characters of a cell drawn in the grid; the rest is in the inspector.
const MAX_CELL_CHARS: usize = 300;

/// The formats the marked cells can be copied in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyAs {
    Tsv,
    TsvWithHeaders,
    Csv,
    Json,
    Markdown,
}

/// Asks for the next batch of rows of a result that has more.
pub type MoreLoader = Rc<dyn Fn(&mut App)>;

/// What the grid draws in a cell.
#[derive(Debug, PartialEq, Eq)]
enum Shown<'a> {
    /// An inserted cell that omits the column.
    Default,
    /// A stored or explicitly inserted NULL.
    Null,
    Text(&'a str),
}

/// Rows looked at when sizing and typing a column.
const SAMPLE_ROWS: usize = 100;

/// Columns the kit draws before the data: the row-number gutter. The kit's own column
/// indexes are the data's plus this; everything outside this module uses data indexes,
/// except where a kit index is passed in (`cell_selected`) or out (`set_selected_cell`).
pub const GUTTER: usize = 1;

/// Pixel width of the row-number gutter for `rows` rows.
fn gutter_width(rows: usize) -> f32 {
    let digits = rows.max(1).to_string().len() as f32;
    (digits * 8.0 + 24.0).max(44.0)
}

/// The kit's own table text size (`text_sm`, 14 px), which the grid size scales. Setting it
/// explicitly is needed because the kit sets none for a custom row height. In pixels, not
/// rems: the grid size is its own, so text, rows and columns all follow the one setting.
const CELL_TEXT: f32 = 14.;

/// Row height of the kit's medium table row, which the grid size scales.
pub const ROW_HEIGHT: f32 = 32.;

/// The cell text size at `percent` of normal, in pixels.
pub fn cell_text(percent: u32) -> f32 {
    scaled(CELL_TEXT, percent)
}

/// The row height of the grid at `percent` of normal, in pixels.
fn row_px(percent: u32) -> f32 {
    scaled(ROW_HEIGHT, percent)
}

/// The row height of the grid at the size in force.
pub fn row_height(cx: &App) -> Pixels {
    px(row_px(crate::theme::grid_scale(cx)))
}

/// Where a jump key sends the selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Jump {
    /// First row, first column.
    First,
    /// Last row, last column.
    Last,
    /// First row, same column.
    Top,
    /// Last row, same column.
    Bottom,
    /// Same row, first column.
    RowStart,
    /// Same row, last column.
    RowEnd,
}

/// Pixel width for a column: wide enough for its header and sampled values.
fn column_width(name: &str, rows: &[Vec<Option<String>>], col: usize) -> f32 {
    let longest = rows
        .iter()
        .take(SAMPLE_ROWS)
        .filter_map(|r| r.get(col)?.as_deref())
        .map(|v| v.chars().take(MAX_CELL_CHARS).count())
        .max()
        .unwrap_or(0)
        .max(name.chars().count() + 3);
    (longest as f32 * 7.5 + 28.0).clamp(80.0, 360.0)
}

/// Tooltip of a column header: the full name, which a narrow column clips, and the type.
fn header_tip(name: &str, type_name: Option<&str>) -> String {
    match type_name {
        Some(t) => format!("{name}\n{t}"),
        None => name.to_string(),
    }
}

/// True when the sampled values of a column are all numbers (and not all NULL).
fn is_numeric(rows: &[Vec<Option<String>>], col: usize) -> bool {
    let mut any = false;
    for v in rows
        .iter()
        .take(SAMPLE_ROWS)
        .filter_map(|r| r.get(col)?.as_deref())
    {
        if v.parse::<f64>().is_err() || v.eq_ignore_ascii_case("nan") || v.contains("inf") {
            return false;
        }
        any = true;
    }
    any
}

#[derive(Default)]
pub struct ResultsTable {
    /// The shown result set, shared with the tab that produced it; no rows are copied.
    set: Option<Arc<ResultSet>>,
    columns: Vec<Column>,
    /// Pixel width of the row-number gutter.
    gutter: f32,
    /// Per column: every sampled value is a number, so it is right-aligned.
    numeric: Vec<bool>,
    sortable: bool,
    /// Column and direction currently applied, shown in the header.
    sorted: Option<(usize, bool)>,
    /// Sort requested by a header click, not yet handled.
    pending_sort: Option<Option<(usize, bool)>>,
    /// Staged edits; when present, cells are read through it.
    session: Option<EditSession>,
    /// Rows fetched after the shared set (infinite scroll), in order.
    extra: Vec<Vec<Option<String>>>,
    /// Asks for the next batch of rows; set while the result has more to fetch.
    more: Option<MoreLoader>,
    /// The result has more rows than loaded.
    has_more: bool,
    /// A batch has been asked for and has not arrived yet.
    loading_more: bool,
    /// The cells the user has marked (drag, Shift, Ctrl/Cmd).
    selection: CellSelection,
    /// A mouse press just changed the selection; the click's `SelectCell` event that follows
    /// must not reset it (only keyboard moves should).
    press_pending: bool,
}

impl ResultsTable {
    /// A grid whose headers can be clicked to sort server-side.
    pub fn sortable() -> Self {
        Self {
            sortable: true,
            ..Self::default()
        }
    }

    pub fn selection(&self) -> &CellSelection {
        &self.selection
    }

    pub fn selection_mut(&mut self) -> &mut CellSelection {
        &mut self.selection
    }

    /// A mouse press on a cell.
    pub fn press(&mut self, cell: (usize, usize), mods: Mods) {
        self.selection.press(cell, mods);
        self.press_pending = true;
    }

    /// A mouse press on a row number: marks the whole row (Shift extends, Ctrl/Cmd adds).
    pub fn press_row(&mut self, row: usize, mods: Mods) {
        self.selection.select_row(row, self.columns.len(), mods);
        self.press_pending = true;
    }

    /// A press on the corner above the row numbers: marks everything.
    pub fn select_all(&mut self) {
        self.selection
            .select_all(self.row_total(), self.columns.len());
    }

    /// The kit selected a cell (by its own column index, gutter included); returns the data
    /// cell. After a press that is the click itself; otherwise the keyboard moved, and the
    /// selection follows to that single cell, or to the whole row when it landed in the gutter.
    pub fn cell_selected(&mut self, (row, kit_col): (usize, usize)) -> (usize, usize) {
        let cell = (row, kit_col.saturating_sub(GUTTER));
        // The click itself, or the active cell following a drag: keep the marked block.
        if std::mem::take(&mut self.press_pending) || self.selection.is_dragging() {
            return cell;
        }
        if kit_col < GUTTER {
            self.selection
                .select_row(row, self.columns.len(), Mods::default());
        } else {
            self.selection.press(cell, Mods::default());
            self.selection.release();
        }
        cell
    }

    /// The marked cells as tab-separated text and how many there are; `None` when none are.
    pub fn copy_text(&self, headers: bool) -> Option<(String, usize)> {
        let count = self.selection.cell_count();
        (count > 0).then(|| {
            (
                self.selection
                    .to_tsv(|r, c| self.cell(r, c), self.names(), headers),
                count,
            )
        })
    }

    /// The marked cells in one of the copy formats, and how many cells that is.
    pub fn copy_as(&self, how: CopyAs) -> Option<(String, usize)> {
        match how {
            CopyAs::Tsv => self.copy_text(false),
            CopyAs::TsvWithHeaders => self.copy_text(true),
            _ => {
                let count = self.selection.cell_count();
                if count == 0 {
                    return None;
                }
                let (names, rows) = self
                    .selection
                    .to_table(|r, c| self.cell(r, c), self.names());
                let text = match how {
                    CopyAs::Csv => pgroove_core::export::to_csv(&names, &rows),
                    CopyAs::Json => pgroove_core::export::to_json(&names, &rows),
                    _ => pgroove_core::export::to_markdown(&names, &rows),
                };
                Some((text, count))
            }
        }
    }

    pub fn session(&self) -> Option<&EditSession> {
        self.session.as_ref()
    }

    pub fn session_mut(&mut self) -> Option<&mut EditSession> {
        self.session.as_mut()
    }

    pub fn set_session(&mut self, session: Option<EditSession>) {
        self.session = session;
    }

    pub fn row_total(&self) -> usize {
        self.session
            .as_ref()
            .map_or(self.row_count(), EditSession::row_count)
    }

    /// Rows loaded: the shared result set plus later batches (staged inserts excluded).
    fn row_count(&self) -> usize {
        self.set.as_ref().map_or(0, |s| s.rows.len()) + self.extra.len()
    }

    /// Sets (or clears) how the grid asks for more rows when scrolled near the end.
    pub fn set_loader(&mut self, loader: Option<MoreLoader>) {
        self.more = loader;
    }

    /// Marks that more rows exist (or not) without adding any.
    pub fn set_has_more(&mut self, more: bool) {
        self.has_more = more;
        self.loading_more = false;
    }

    /// Adds a fetched batch below the loaded rows. Marked cells and the scroll position stay.
    /// `ids` are the rows' `(ctid, xmin)` pairs, for a staged-edit session that needs them.
    /// Returns true when the row-number column had to widen (the kit must rebuild its columns).
    pub fn append(
        &mut self,
        rows: Vec<Vec<Option<String>>>,
        ids: Vec<(String, String)>,
        more: bool,
    ) -> bool {
        // With a session, it is the one that holds (and shows) the rows.
        let mut more = more;
        match self.session.as_mut() {
            Some(session) => {
                if let Err(e) = session.extend(rows, ids) {
                    log::warn!("could not add the next rows to the edit session: {e:#}");
                    more = false;
                }
            }
            None => self.extra.extend(rows),
        }
        self.has_more = more;
        self.loading_more = false;
        let gutter = gutter_width(self.row_total());
        let widened = (gutter - self.gutter).abs() > f32::EPSILON;
        self.gutter = gutter;
        widened
    }

    /// The loaded rows as one result set, in the shape a copy or save needs.
    fn merged(&self) -> Option<Arc<ResultSet>> {
        let base = self.set.as_ref()?;
        if self.extra.is_empty() {
            return Some(base.clone());
        }
        let mut all = (**base).clone();
        all.rows.extend(self.extra.iter().cloned());
        all.count = all.rows.len() as u64;
        Some(Arc::new(all))
    }

    pub fn names(&self) -> &[String] {
        self.set.as_deref().map_or(&[], |s| s.columns.as_slice())
    }

    pub fn rows(&self) -> &[Vec<Option<String>>] {
        self.set.as_deref().map_or(&[], |s| s.rows.as_slice())
    }

    /// The loaded data, for a copy or save. A single batch is shared, not cloned; rows loaded
    /// later are merged into a copy.
    pub fn data(&self) -> Option<Arc<ResultSet>> {
        self.merged()
    }

    pub fn column_name(&self, ix: usize) -> Option<&str> {
        self.names().get(ix).map(String::as_str)
    }

    /// Takes a pending header-click sort: `Some(None)` clears the sort.
    pub fn take_sort(&mut self) -> Option<Option<(usize, bool)>> {
        self.pending_sort.take()
    }

    pub fn set_sorted(&mut self, sorted: Option<(usize, bool)>) {
        self.sorted = sorted;
    }

    pub fn set(&mut self, set: Option<Arc<ResultSet>>) {
        match set {
            Some(s) => {
                self.session = None;
                self.selection.clear();
                self.extra.clear();
                self.has_more = false;
                self.loading_more = false;
                self.numeric = (0..s.columns.len())
                    .map(|i| is_numeric(&s.rows, i))
                    .collect();
                self.gutter = gutter_width(s.rows.len());
                self.columns = s
                    .columns
                    .iter()
                    .enumerate()
                    .map(|(i, name)| {
                        let w = column_width(name, &s.rows, i);
                        let mut c = Column::new(format!("c{i}"), name.as_str()).width(px(w));
                        if self.numeric[i] {
                            c = c.text_right();
                        }
                        if self.sortable {
                            c = match self.sorted {
                                Some((ix, true)) if ix == i => c.descending(),
                                Some((ix, false)) if ix == i => c.ascending(),
                                _ => c.sortable(),
                            };
                        }
                        c
                    })
                    .collect();
                self.set = Some(s);
            }
            None => {
                self.session = None;
                self.selection.clear();
                self.extra.clear();
                self.has_more = false;
                self.loading_more = false;
                self.set = None;
                self.columns.clear();
                self.numeric.clear();
            }
        }
    }

    /// The kit cell (gutter included) a jump key moves to from `current`, or `None` when the
    /// grid has no cells. With nothing selected a jump starts from the first cell.
    pub fn jump_target(
        &self,
        jump: Jump,
        current: Option<(usize, usize)>,
    ) -> Option<(usize, usize)> {
        let (rows, cols) = (self.row_total(), self.columns.len());
        if rows == 0 || cols == 0 {
            return None;
        }
        let (row, col) = current.unwrap_or((0, GUTTER));
        let (row, col) = (row.min(rows - 1), col.clamp(GUTTER, GUTTER + cols - 1));
        let (first, last) = (GUTTER, GUTTER + cols - 1);
        Some(match jump {
            Jump::First => (0, first),
            Jump::Last => (rows - 1, last),
            Jump::Top => (0, col),
            Jump::Bottom => (rows - 1, col),
            Jump::RowStart => (row, first),
            Jump::RowEnd => (row, last),
        })
    }

    pub fn cell(&self, row: usize, col: usize) -> Option<&str> {
        if let Some(s) = &self.session {
            return s.value(row, col);
        }
        let base = self.rows();
        let line = match base.get(row) {
            Some(line) => line,
            None => self.extra.get(row - base.len())?,
        };
        line.get(col)?.as_deref()
    }

    /// The server's type name of data column `col`, when the result was described.
    fn type_of(&self, col: usize) -> Option<&str> {
        self.set.as_ref()?.types.get(col).map(String::as_str)
    }

    /// What the grid draws in a cell: an inserted cell keeps a default apart from a NULL.
    fn shown(&self, row: usize, col: usize) -> Shown<'_> {
        match self.session.as_ref().and_then(|s| s.insert_value(row, col)) {
            Some(InsertValue::Default) => Shown::Default,
            Some(InsertValue::Null) => Shown::Null,
            _ => self.cell(row, col).map_or(Shown::Null, Shown::Text),
        }
    }
}

impl TableDelegate for ResultsTable {
    fn columns_count(&self, _: &App) -> usize {
        if self.columns.is_empty() {
            0
        } else {
            self.columns.len() + GUTTER
        }
    }

    fn rows_count(&self, _: &App) -> usize {
        self.row_total()
    }

    fn has_more(&self, _: &App) -> bool {
        self.has_more && !self.loading_more && self.more.is_some()
    }

    /// Ask for the next batch while this many rows are still below the visible ones.
    fn load_more_threshold(&self) -> usize {
        50
    }

    fn load_more(&mut self, _: &mut Window, cx: &mut Context<TableState<Self>>) {
        if let Some(more) = self.more.clone() {
            self.loading_more = true;
            more(cx);
        }
    }

    fn column(&self, col_ix: usize, cx: &App) -> Column {
        match col_ix.checked_sub(GUTTER) {
            Some(i) => {
                let mut column = self.columns[i].clone();
                column.width = px(scaled(column.width.as_f32(), crate::theme::grid_scale(cx)));
                column
            }
            None => Column::new("row-numbers", "")
                .width(px(scaled(self.gutter, crate::theme::grid_scale(cx))))
                .fixed_left()
                .resizable(false)
                .movable(false)
                .selectable(false),
        }
    }

    fn render_th(
        &mut self,
        col_ix: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        // The corner above the row numbers marks everything.
        match col_ix.checked_sub(GUTTER) {
            Some(i) => {
                let tip = header_tip(&self.columns[i].name, self.type_of(i));
                div()
                    .id(SharedString::from(format!("th-{i}")))
                    .w_full()
                    .child(self.columns[i].name.clone())
                    .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                    .into_any_element()
            }
            None => div()
                .w_full()
                .h_full()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|state, _: &MouseDownEvent, _, cx| {
                        state.delegate_mut().select_all();
                        cx.notify();
                    }),
                )
                .into_any_element(),
        }
    }

    fn perform_sort(
        &mut self,
        col_ix: usize,
        sort: ColumnSort,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) {
        let col_ix = col_ix.saturating_sub(GUTTER);
        self.pending_sort = Some(match sort {
            ColumnSort::Default => None,
            ColumnSort::Ascending => Some((col_ix, false)),
            ColumnSort::Descending => Some((col_ix, true)),
        });
        cx.notify();
    }

    /// The menu for a right-click on a row number (the kit only opens the delegate's menu for
    /// those; cells carry their own, see `render_td`).
    fn context_menu(
        &mut self,
        _: usize,
        menu: PopupMenu,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> PopupMenu {
        copy_menu(menu, &cx.entity())
    }

    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        match col_ix.checked_sub(GUTTER) {
            // The kit opens the delegate's menu only for rows, and a right-click on a cell
            // clears that, so each data cell carries its own context menu.
            Some(col) => {
                let state = cx.entity();
                self.data_cell(row_ix, col, cx)
                    .id(("cell", row_ix * 4096 + col))
                    .context_menu(move |menu, _, _| copy_menu(menu, &state))
                    .into_any_element()
            }
            None => self.gutter_cell(row_ix, cx).into_any_element(),
        }
    }
}

/// The right-click menu: copy the marked cells in one of the formats.
fn copy_menu(menu: PopupMenu, state: &Entity<TableState<ResultsTable>>) -> PopupMenu {
    let item = |label: &'static str, how: CopyAs| {
        let state = state.clone();
        PopupMenuItem::new(label).on_click(move |_, window, cx| {
            let copied = state.read(cx).delegate().copy_as(how);
            if let Some((text, count)) = copied {
                cx.write_to_clipboard(ClipboardItem::new_string(text));
                window.push_notification(
                    format!("Copied {count} cell{}", if count == 1 { "" } else { "s" }),
                    cx,
                );
            }
        })
    };
    menu.item(item("Copy", CopyAs::Tsv))
        .item(item("Copy with column names", CopyAs::TsvWithHeaders))
        .separator()
        .item(item("Copy as CSV", CopyAs::Csv))
        .item(item("Copy as JSON", CopyAs::Json))
        .item(item("Copy as Markdown table", CopyAs::Markdown))
}

impl ResultsTable {
    /// The row number; it marks the row (Shift extends, Ctrl/Cmd adds) and lights up while
    /// any cell of the row is marked.
    fn gutter_cell(&self, row_ix: usize, cx: &mut Context<TableState<Self>>) -> Div {
        let theme = cx.theme();
        let marked = self.selection.row_marked(row_ix);
        div()
            .absolute()
            .inset_0()
            .px_2()
            .py_1()
            .text_right()
            .text_color(if marked {
                theme.foreground
            } else {
                theme.muted_foreground
            })
            .bg(theme.tokens.table_head)
            .child((row_ix + 1).to_string())
            .when(marked, |d| {
                d.child(div().absolute().inset_0().bg(theme.primary.opacity(0.3)))
            })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |state, ev: &MouseDownEvent, _, cx| {
                    let mods = Mods {
                        extend: ev.modifiers.shift,
                        add: ev.modifiers.secondary(cx),
                    };
                    state.delegate_mut().press_row(row_ix, mods);
                    cx.notify();
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |state, _: &MouseDownEvent, _, cx| {
                    if !state.delegate().selection.row_marked(row_ix) {
                        state.delegate_mut().press_row(row_ix, Mods::default());
                        cx.notify();
                    }
                }),
            )
    }

    fn data_cell(&self, row_ix: usize, col_ix: usize, cx: &mut Context<TableState<Self>>) -> Div {
        let theme = cx.theme();
        let (inserted, deleted, edited) =
            self.session.as_ref().map_or((false, false, false), |s| {
                (
                    s.is_inserted(row_ix),
                    s.is_deleted(row_ix),
                    s.is_edited(row_ix, col_ix),
                )
            });
        let right = self.numeric.get(col_ix).copied().unwrap_or(false);
        let marking = self.selection.marking(row_ix, col_ix);
        let base = match self.shown(row_ix, col_ix) {
            Shown::Default => div().italic().opacity(0.5).child("default"),
            Shown::Null => div().italic().opacity(0.5).child("NULL"),
            Shown::Text(v) => div()
                .truncate()
                .child(pgroove_core::export::preview(v, MAX_CELL_CHARS)),
        };
        let base = if right { base.text_right() } else { base };
        let base = if deleted { base.line_through() } else { base };
        // The wrapper covers the whole cell (padding included) so fills and the selection
        // outline are seamless and presses or drags register anywhere in the cell.
        let cell = div()
            .absolute()
            .inset_0()
            .px_2()
            .py_1()
            .child(base.w_full())
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |state, ev: &MouseDownEvent, _, cx| {
                    let mods = Mods {
                        extend: ev.modifiers.shift,
                        add: ev.modifiers.secondary(cx),
                    };
                    state.delegate_mut().press((row_ix, col_ix), mods);
                    cx.notify();
                }),
            )
            // Right-clicking outside the marked cells marks the one under the pointer first,
            // so the context menu copies what was clicked.
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |state, _: &MouseDownEvent, _, cx| {
                    let d = state.delegate_mut();
                    if !d.selection.contains(row_ix, col_ix) {
                        d.press((row_ix, col_ix), Mods::default());
                        d.selection.release();
                        cx.notify();
                    }
                }),
            )
            .on_mouse_move(cx.listener(move |state, ev: &MouseMoveEvent, _, cx| {
                if ev.pressed_button == Some(MouseButton::Left)
                    && state.delegate_mut().selection.drag_to((row_ix, col_ix))
                {
                    // The cell under the pointer becomes the active one (shown in the
                    // inspector), not the one the drag started on.
                    state.set_selected_cell(row_ix, col_ix + GUTTER, cx);
                    cx.notify();
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|state, _: &MouseUpEvent, _, _| {
                    state.delegate_mut().selection.release();
                }),
            );
        let cell = if deleted {
            cell.bg(theme.danger.opacity(0.18))
        } else if inserted {
            cell.bg(theme.success.opacity(0.18))
        } else if edited {
            cell.bg(theme.warning.opacity(0.22))
        } else {
            cell
        };
        // Drawn over the edit colors so a selected edited cell shows both.
        cell.children(marking.map(|m| selection_layer(m, theme.primary)))
    }
}

/// The selection paint for one cell: a tint and an outline along the selected area's
/// border. The active cell gets a heavier frame and no tint.
fn selection_layer(m: Marking, color: Hsla) -> Div {
    let layer = div().absolute().inset_0().border_color(color);
    if !m.tint {
        return layer.border_2();
    }
    let e = m.edges;
    layer
        .bg(color.opacity(0.16))
        .when(e.top, |d| d.border_t_1())
        .when(e.right, |d| d.border_r_1())
        .when(e.bottom, |d| d.border_b_1())
        .when(e.left, |d| d.border_l_1())
}

#[cfg(test)]
mod tests {
    use super::{CopyAs, Jump, ResultsTable, header_tip};
    use pgroove_core::query::ResultSet;
    use std::sync::Arc;

    #[test]
    fn jumps_stay_inside_the_grid() {
        let mut t = ResultsTable::default();
        assert_eq!(t.jump_target(Jump::First, None), None, "no cells");
        t.set(shared(ResultSet {
            columns: vec!["a".into(), "b".into(), "c".into()],
            rows: vec![vec![None; 3]; 5],
            count: 5,
            truncated: false,
            types: vec![],
        }));
        // Kit columns count the row-number gutter, so data starts at 1.
        let from = Some((2, 2));
        assert_eq!(t.jump_target(Jump::First, from), Some((0, 1)));
        assert_eq!(t.jump_target(Jump::Last, from), Some((4, 3)));
        assert_eq!(t.jump_target(Jump::Top, from), Some((0, 2)));
        assert_eq!(t.jump_target(Jump::Bottom, from), Some((4, 2)));
        assert_eq!(t.jump_target(Jump::RowStart, from), Some((2, 1)));
        assert_eq!(t.jump_target(Jump::RowEnd, from), Some((2, 3)));
        // Nothing selected starts from the first cell; a stale cell is pulled back in.
        assert_eq!(t.jump_target(Jump::RowEnd, None), Some((0, 3)));
        assert_eq!(t.jump_target(Jump::Bottom, Some((99, 99))), Some((4, 3)));
        assert_eq!(t.jump_target(Jump::RowStart, Some((1, 0))), Some((1, 1)));
    }

    #[test]
    fn header_tip_names_the_type() {
        assert_eq!(
            header_tip("created_at", Some("timestamptz")),
            "created_at\ntimestamptz"
        );
        assert_eq!(header_tip("created_at", None), "created_at");
    }

    #[test]
    fn types_follow_the_result_set() {
        let mut t = ResultsTable::default();
        assert_eq!(t.type_of(0), None);
        t.set(shared(ResultSet {
            columns: vec!["a".into(), "b".into()],
            types: vec!["int4".into(), "text".into()],
            rows: vec![vec![Some("1".into()), None]],
            count: 1,
            truncated: false,
        }));
        assert_eq!(t.type_of(0), Some("int4"));
        assert_eq!(t.type_of(1), Some("text"));
        assert_eq!(t.type_of(2), None);
    }

    fn shared(set: ResultSet) -> Option<Arc<ResultSet>> {
        Some(Arc::new(set))
    }

    #[test]
    fn later_batches_extend_the_result() {
        let mut t = ResultsTable::default();
        let first = ResultSet {
            columns: vec!["a".into()],
            rows: (1..=3).map(|n| vec![Some(n.to_string())]).collect(),
            count: 3,
            truncated: true,
            types: vec![],
        };
        let base = Arc::new(first);
        t.set(Some(base.clone()));
        t.has_more = true;
        assert_eq!((t.row_count(), t.row_total()), (3, 3));
        assert!(
            Arc::ptr_eq(&t.data().unwrap(), &base),
            "one batch is shared, not copied"
        );
        // A second batch continues the numbering; reading crosses the seam.
        let widened = t.append(vec![vec![Some("4".into())], vec![None]], vec![], true);
        assert!(!widened && t.has_more && !t.loading_more);
        assert_eq!((t.row_count(), t.row_total()), (5, 5));
        assert_eq!(t.cell(2, 0), Some("3"));
        assert_eq!(t.cell(3, 0), Some("4"));
        assert_eq!(t.cell(4, 0), None, "a NULL in the later batch");
        assert_eq!(t.cell(5, 0), None, "past the end");
        // Copy and save see every loaded row.
        let all = t.data().unwrap();
        assert_eq!(all.rows.len(), 5);
        assert_eq!(all.count, 5);
        // Marked cells survive an append; the end of the result clears the flag.
        t.press((3, 0), crate::selection::Mods::default());
        t.append(vec![], vec![], false);
        assert!(!t.has_more);
        assert_eq!(t.selection().cell_count(), 1);
        // The row-number column widens when the row count gains a digit.
        let many: Vec<Vec<Option<String>>> = vec![vec![None]; 1000];
        assert!(t.append(many, vec![], false));
        // New data starts over.
        t.set(Some(base));
        assert_eq!(t.row_count(), 3);
    }

    #[test]
    fn set_and_cell() {
        let mut t = ResultsTable::default();
        t.set(shared(ResultSet {
            columns: vec!["a".into(), "b".into()],
            rows: vec![vec![Some("1".into()), None]],
            count: 1,
            truncated: false,
            types: vec![],
        }));
        assert_eq!(t.columns.len(), 2);
        assert_eq!(t.cell(0, 0), Some("1"));
        assert_eq!(t.cell(0, 1), None);
        assert_eq!(t.cell(5, 0), None);
        assert_eq!(t.column_name(1), Some("b"));
        assert_eq!(t.numeric, vec![true, false]);
        assert_eq!(t.row_total(), 1);
        assert_eq!(super::column_width("a", t.rows(), 0), 80.0);
        t.set(None);
        assert!(t.columns.is_empty() && t.rows().is_empty() && t.names().is_empty());
        assert_eq!(t.row_total(), 0);
        assert_eq!(t.column_name(0), None);
        assert_eq!(t.take_sort(), None);
        assert!(t.session().is_none());
        assert!(!super::is_numeric(&[vec![Some("x".into())]], 0));
        t.set_sorted(Some((0, true)));
        assert_eq!(t.sorted, Some((0, true)));
    }

    #[test]
    fn inserted_cell_states() {
        use super::Shown;
        use pgroove_core::edit::{EditSession, InsertValue};
        let mut t = ResultsTable::default();
        t.set(shared(ResultSet {
            columns: vec!["a".into(), "b".into()],
            rows: vec![vec![Some("1".into()), None]],
            count: 1,
            truncated: false,
            types: vec![],
        }));
        assert_eq!(t.shown(0, 0), Shown::Text("1"));
        assert_eq!(t.shown(0, 1), Shown::Null, "a stored NULL stays NULL");
        let mut e = EditSession::new(
            vec!["a".into(), "b".into()],
            &["a".to_string()],
            vec![vec![Some("1".into()), None]],
        )
        .unwrap();
        e.add_insert(vec![InsertValue::Default, InsertValue::Null]);
        t.set_session(Some(e));
        assert_eq!(t.shown(1, 0), Shown::Default);
        assert_eq!(t.shown(1, 1), Shown::Null);
        t.session_mut().unwrap().set_value(1, 1, Some("v".into()));
        assert_eq!(t.shown(1, 1), Shown::Text("v"));
    }

    /// The grid reads the very allocation the caller holds, cells included.
    #[test]
    fn data_is_shared_not_copied() {
        let mut t = ResultsTable::default();
        let set = Arc::new(ResultSet {
            columns: vec!["a".into(), "b".into()],
            rows: vec![
                vec![Some("1".into()), Some("x".into())],
                vec![Some("2".into()), None],
            ],
            count: 2,
            truncated: false,
            types: vec![],
        });
        t.set(Some(set.clone()));
        assert_eq!(Arc::strong_count(&set), 2, "the tab and the grid share it");
        let held = t.data().expect("the grid keeps the set");
        assert!(Arc::ptr_eq(&set, &held), "same allocation, not a copy");
        assert!(std::ptr::eq(t.rows().as_ptr(), set.rows.as_ptr()));
        assert!(std::ptr::eq(t.names().as_ptr(), set.columns.as_ptr()));
        assert_eq!(t.cell(0, 0), Some("1"));
        assert_eq!(t.cell(1, 1), None);
        assert_eq!(t.cell(1, 0), Some("2"));
        assert_eq!(t.row_total(), 2);
    }

    #[test]
    fn rows_and_columns_scale_with_the_grid_size() {
        assert_eq!(super::row_px(100), 32.);
        assert_eq!(super::row_px(125), 40.);
        assert_eq!(super::row_px(75), 24.);
        assert_eq!(super::row_px(0), 24., "clamped");
        // Column widths and the gutter use the same rounding, so they stay whole pixels.
        assert_eq!(pgroove_core::settings::scaled(360., 125), 450.);
        assert_eq!(pgroove_core::settings::scaled(80., 75), 60.);
        assert_eq!(pgroove_core::settings::scaled(44., 125), 55.);
    }

    #[test]
    fn cell_text_scales_with_the_grid_size() {
        assert_eq!(super::cell_text(100), 14.);
        assert_eq!(super::cell_text(125), 18.);
        assert_eq!(super::cell_text(75), 11.);
        // A stray value is held to the allowed range.
        assert_eq!(super::cell_text(1), super::cell_text(75));
        assert_eq!(super::cell_text(9999), super::cell_text(125));
    }

    #[test]
    fn gutter_fits_the_row_count() {
        assert_eq!(super::gutter_width(0), 44.0);
        assert_eq!(super::gutter_width(999), 48.0);
        assert_eq!(super::gutter_width(1_000_000), 80.0);
    }

    #[test]
    fn marking_and_copying_cells() {
        use crate::selection::Mods;
        let mut t = ResultsTable::default();
        t.set(shared(ResultSet {
            columns: vec!["a".into(), "b".into()],
            rows: vec![
                vec![Some("1".into()), Some("x".into())],
                vec![Some("2".into()), None],
            ],
            count: 2,
            truncated: false,
            types: vec![],
        }));
        assert_eq!(t.copy_text(false), None);
        t.press((0, 0), Mods::default());
        assert!(t.selection_mut().drag_to((1, 1)));
        t.selection_mut().release();
        assert_eq!(t.copy_text(true), Some(("a\tb\n1\tx\n2\tNULL\n".into(), 4)));
        // The same cells in each copy format (what the right-click menu offers).
        assert_eq!(t.copy_as(CopyAs::Tsv), Some(("1\tx\n2\tNULL\n".into(), 4)));
        assert_eq!(
            t.copy_as(CopyAs::Csv),
            Some(("a,b\r\n1,x\r\n2,\r\n".into(), 4))
        );
        let (json, _) = t.copy_as(CopyAs::Json).unwrap();
        assert!(json.contains("\"b\": null") && json.contains("\"a\": \"2\""));
        assert_eq!(
            t.copy_as(CopyAs::Markdown),
            Some((
                "| a | b |\n| --- | --- |\n| 1 | x |\n| 2 | NULL |\n".into(),
                4
            ))
        );
        // The click's own SelectCell keeps the marked block; a keyboard move collapses it.
        // The kit's column index counts the row-number gutter.
        assert_eq!(t.cell_selected((1, 2)), (1, 1));
        assert_eq!(t.selection().cell_count(), 4);
        assert_eq!(t.cell_selected((0, 2)), (0, 1));
        assert_eq!(t.selection().cell_count(), 1);
        // Landing in the gutter by keyboard marks the row; a press on a row number does too.
        assert_eq!(t.cell_selected((1, 0)), (1, 0));
        assert_eq!(t.selection().cell_count(), 2);
        t.press_row(0, Mods::default());
        assert_eq!(t.cell_selected((0, 0)), (0, 0));
        assert_eq!(t.selection().rows().into_iter().collect::<Vec<_>>(), [0]);
        assert!(t.selection().row_marked(0) && !t.selection().row_marked(1));
        t.select_all();
        assert_eq!(t.selection().cell_count(), 4);
        // New data clears the marks.
        t.set(None);
        assert_eq!(t.selection().cell_count(), 0);
    }
}
