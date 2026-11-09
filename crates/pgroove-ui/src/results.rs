//! Result grid backed by gpui-kit's table.

use gpui_kit::component::ActiveTheme;
use gpui_kit::component::table::{Column, ColumnSort, TableDelegate, TableState};
use gpui_kit::*;
use pgroove_core::edit::EditSession;
use pgroove_core::query::ResultSet;

/// Characters of a cell drawn in the grid; the rest is in the inspector.
const MAX_CELL_CHARS: usize = 300;

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
    names: Vec<String>,
    columns: Vec<Column>,
    rows: Vec<Vec<Option<String>>>,
    /// Per column: every sampled value is a number, so it is right-aligned.
    numeric: Vec<bool>,
    sortable: bool,
    /// Column and direction currently applied, shown in the header.
    sorted: Option<(usize, bool)>,
    /// Sort requested by a header click, not yet handled.
    pending_sort: Option<Option<(usize, bool)>>,
    /// Staged edits; when present, cells are read through it.
    session: Option<EditSession>,
}

impl ResultsTable {
    /// A grid whose headers can be clicked to sort server-side.
    pub fn sortable() -> Self {
        Self {
            sortable: true,
            ..Self::default()
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
            .map_or(self.rows.len(), EditSession::row_count)
    }

    pub fn names(&self) -> &[String] {
        &self.names
    }

    pub fn rows(&self) -> &[Vec<Option<String>>] {
        &self.rows
    }

    pub fn column_name(&self, ix: usize) -> Option<&str> {
        self.names.get(ix).map(String::as_str)
    }

    /// Takes a pending header-click sort: `Some(None)` clears the sort.
    pub fn take_sort(&mut self) -> Option<Option<(usize, bool)>> {
        self.pending_sort.take()
    }

    pub fn set_sorted(&mut self, sorted: Option<(usize, bool)>) {
        self.sorted = sorted;
    }

    pub fn set(&mut self, set: Option<&ResultSet>) {
        match set {
            Some(s) => {
                self.session = None;
                self.names = s.columns.clone();
                self.numeric = (0..s.columns.len())
                    .map(|i| is_numeric(&s.rows, i))
                    .collect();
                self.columns = s
                    .columns
                    .iter()
                    .enumerate()
                    .map(|(i, name)| {
                        let w = column_width(name, &s.rows, i);
                        let mut c = Column::new(format!("c{i}"), name.clone()).width(px(w));
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
                self.rows = s.rows.clone();
            }
            None => {
                self.session = None;
                self.names.clear();
                self.columns.clear();
                self.numeric.clear();
                self.rows.clear();
            }
        }
    }

    pub fn cell(&self, row: usize, col: usize) -> Option<&str> {
        if let Some(s) = &self.session {
            return s.value(row, col);
        }
        self.rows.get(row)?.get(col)?.as_deref()
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
        let base = match self.cell(row_ix, col_ix) {
            Some(v) => div()
                .truncate()
                .child(pgroove_core::export::preview(v, MAX_CELL_CHARS)),
            None => div().italic().opacity(0.5).child("NULL"),
        };
        let base = if right { base.text_right() } else { base };
        if deleted {
            base.line_through().bg(theme.danger.opacity(0.18))
        } else if inserted {
            base.bg(theme.success.opacity(0.18))
        } else if edited {
            base.bg(theme.warning.opacity(0.22))
        } else {
            base
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ResultsTable;
    use pgroove_core::query::ResultSet;

    #[test]
    fn set_and_cell() {
        let mut t = ResultsTable::default();
        t.set(Some(&ResultSet {
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
        assert_eq!(super::column_width("a", &t.rows, 0), 80.0);
        t.set(None);
        assert!(t.columns.is_empty() && t.rows.is_empty());
        assert_eq!(t.column_name(0), None);
        assert_eq!(t.take_sort(), None);
        assert!(t.session().is_none());
        assert!(!super::is_numeric(&[vec![Some("x".into())]], 0));
        t.set_sorted(Some((0, true)));
        assert_eq!(t.sorted, Some((0, true)));
    }
}
