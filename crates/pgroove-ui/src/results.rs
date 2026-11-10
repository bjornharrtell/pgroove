//! Result grid backed by gpui-kit's table.

use std::sync::Arc;

use gpui_kit::component::ActiveTheme;
use gpui_kit::component::table::{Column, ColumnSort, TableDelegate, TableState};
use gpui_kit::*;
use pgroove_core::edit::{EditSession, InsertValue};
use pgroove_core::query::ResultSet;

use crate::selection::{CellSelection, Mods};

/// Characters of a cell drawn in the grid; the rest is in the inspector.
const MAX_CELL_CHARS: usize = 300;

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
    /// Per column: every sampled value is a number, so it is right-aligned.
    numeric: Vec<bool>,
    sortable: bool,
    /// Column and direction currently applied, shown in the header.
    sorted: Option<(usize, bool)>,
    /// Sort requested by a header click, not yet handled.
    pending_sort: Option<Option<(usize, bool)>>,
    /// Staged edits; when present, cells are read through it.
    session: Option<EditSession>,
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

    /// The kit selected a cell. After a press that is the click itself; otherwise the keyboard
    /// moved, and the selection follows to that single cell.
    pub fn cell_selected(&mut self, cell: (usize, usize)) {
        if std::mem::take(&mut self.press_pending) {
            return;
        }
        self.selection.press(cell, Mods::default());
        self.selection.release();
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

    /// Rows in the shared result set (staged inserts excluded).
    fn row_count(&self) -> usize {
        self.set.as_ref().map_or(0, |s| s.rows.len())
    }

    pub fn names(&self) -> &[String] {
        self.set.as_deref().map_or(&[], |s| s.columns.as_slice())
    }

    pub fn rows(&self) -> &[Vec<Option<String>>] {
        self.set.as_deref().map_or(&[], |s| s.rows.as_slice())
    }

    /// The shared data, for a copy or save that must not clone the rows.
    pub fn data(&self) -> Option<Arc<ResultSet>> {
        self.set.clone()
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
                self.numeric = (0..s.columns.len())
                    .map(|i| is_numeric(&s.rows, i))
                    .collect();
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
                self.set = None;
                self.columns.clear();
                self.numeric.clear();
            }
        }
    }

    pub fn cell(&self, row: usize, col: usize) -> Option<&str> {
        if let Some(s) = &self.session {
            return s.value(row, col);
        }
        self.rows().get(row)?.get(col)?.as_deref()
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
        self.columns.len()
    }

    fn rows_count(&self, _: &App) -> usize {
        self.row_total()
    }

    fn column(&self, col_ix: usize, _: &App) -> Column {
        self.columns[col_ix].clone()
    }

    fn perform_sort(
        &mut self,
        col_ix: usize,
        sort: ColumnSort,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) {
        self.pending_sort = Some(match sort {
            ColumnSort::Default => None,
            ColumnSort::Ascending => Some((col_ix, false)),
            ColumnSort::Descending => Some((col_ix, true)),
        });
        cx.notify();
    }

    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
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
        let selected = self.selection.contains(row_ix, col_ix);
        let base = match self.shown(row_ix, col_ix) {
            Shown::Default => div().italic().opacity(0.5).child("default"),
            Shown::Null => div().italic().opacity(0.5).child("NULL"),
            Shown::Text(v) => div()
                .truncate()
                .child(pgroove_core::export::preview(v, MAX_CELL_CHARS)),
        };
        let base = if right { base.text_right() } else { base };
        let base = base
            .w_full()
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
            .on_mouse_move(cx.listener(move |state, ev: &MouseMoveEvent, _, cx| {
                if ev.pressed_button == Some(MouseButton::Left) {
                    state.delegate_mut().selection.drag_to((row_ix, col_ix));
                    cx.notify();
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|state, _: &MouseUpEvent, _, _| {
                    state.delegate_mut().selection.release();
                }),
            );
        if deleted {
            base.line_through().bg(theme.danger.opacity(0.18))
        } else if inserted {
            base.bg(theme.success.opacity(0.18))
        } else if edited {
            base.bg(theme.warning.opacity(0.22))
        } else if selected {
            base.bg(theme.primary.opacity(0.28))
        } else {
            base
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ResultsTable;
    use pgroove_core::query::ResultSet;
    use std::sync::Arc;

    fn shared(set: ResultSet) -> Option<Arc<ResultSet>> {
        Some(Arc::new(set))
    }

    #[test]
    fn set_and_cell() {
        let mut t = ResultsTable::default();
        t.set(shared(ResultSet {
            columns: vec!["a".into(), "b".into()],
            rows: vec![vec![Some("1".into()), None]],
            count: 1,
            truncated: false,
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
        }));
        assert_eq!(t.copy_text(false), None);
        t.press((0, 0), Mods::default());
        t.selection_mut().drag_to((1, 1));
        t.selection_mut().release();
        assert_eq!(t.copy_text(true), Some(("a\tb\n1\tx\n2\t\n".into(), 4)));
        // The click's own SelectCell keeps the marked block; a keyboard move collapses it.
        t.cell_selected((1, 1));
        assert_eq!(t.selection().cell_count(), 4);
        t.cell_selected((0, 1));
        assert_eq!(t.selection().cell_count(), 1);
        // New data clears the marks.
        t.set(None);
        assert_eq!(t.selection().cell_count(), 0);
    }
}
