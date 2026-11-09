//! Result grid backed by gpui-kit's table.

use gpui_kit::component::table::{Column, ColumnSort, TableDelegate, TableState};
use gpui_kit::*;
use pgroove_core::query::ResultSet;

#[derive(Default)]
pub struct ResultsTable {
    names: Vec<String>,
    columns: Vec<Column>,
    rows: Vec<Vec<Option<String>>>,
    sortable: bool,
    /// Column and direction currently applied, shown in the header.
    sorted: Option<(usize, bool)>,
    /// Sort requested by a header click, not yet handled.
    pending_sort: Option<Option<(usize, bool)>>,
}

impl ResultsTable {
    /// A grid whose headers can be clicked to sort server-side.
    pub fn sortable() -> Self {
        Self {
            sortable: true,
            ..Self::default()
        }
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
                self.names = s.columns.clone();
                self.columns = s
                    .columns
                    .iter()
                    .enumerate()
                    .map(|(i, name)| {
                        let mut c = Column::new(format!("c{i}"), name.clone()).width(px(160.));
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
                self.names.clear();
                self.columns.clear();
                self.rows.clear();
            }
        }
    }

    pub fn cell(&self, row: usize, col: usize) -> Option<&str> {
        self.rows.get(row)?.get(col)?.as_deref()
    }
}

impl TableDelegate for ResultsTable {
    fn columns_count(&self, _: &App) -> usize {
        self.columns.len()
    }

    fn rows_count(&self, _: &App) -> usize {
        self.rows.len()
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
        _: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        match self.cell(row_ix, col_ix) {
            Some(v) => div().truncate().child(v.to_string()),
            None => div().italic().opacity(0.5).child("NULL"),
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
        t.set(None);
        assert!(t.columns.is_empty() && t.rows.is_empty());
        assert_eq!(t.column_name(0), None);
        assert_eq!(t.take_sort(), None);
        t.set_sorted(Some((0, true)));
        assert_eq!(t.sorted, Some((0, true)));
    }
}
