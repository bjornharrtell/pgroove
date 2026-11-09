//! Result grid backed by gpui-kit's table.

use gpui_kit::component::table::{Column, TableDelegate, TableState};
use gpui_kit::*;
use pgroove_core::query::ResultSet;

#[derive(Default)]
pub struct ResultsTable {
    columns: Vec<Column>,
    rows: Vec<Vec<Option<String>>>,
}

impl ResultsTable {
    pub fn set(&mut self, set: Option<&ResultSet>) {
        match set {
            Some(s) => {
                self.columns = s
                    .columns
                    .iter()
                    .enumerate()
                    .map(|(i, name)| Column::new(format!("c{i}"), name.clone()).width(px(160.)))
                    .collect();
                self.rows = s.rows.clone();
            }
            None => {
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
        t.set(None);
        assert!(t.columns.is_empty() && t.rows.is_empty());
    }
}
