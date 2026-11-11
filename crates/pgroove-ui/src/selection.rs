//! Free-form selection of cells and rows in a result grid: rectangles that can be dragged
//! out, extended with Shift and added to with Ctrl/Cmd, kept as a list so the selection
//! stays small however many cells it covers.

use std::collections::BTreeSet;

/// An inclusive, normalized block of cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub r0: usize,
    pub r1: usize,
    pub c0: usize,
    pub c1: usize,
}

impl Rect {
    pub fn between(a: (usize, usize), b: (usize, usize)) -> Self {
        Self {
            r0: a.0.min(b.0),
            r1: a.0.max(b.0),
            c0: a.1.min(b.1),
            c1: a.1.max(b.1),
        }
    }

    fn contains(&self, row: usize, col: usize) -> bool {
        (self.r0..=self.r1).contains(&row) && (self.c0..=self.c1).contains(&col)
    }

    fn is_cell(&self, row: usize, col: usize) -> bool {
        self.r0 == row && self.r1 == row && self.c0 == col && self.c1 == col
    }
}

/// Which sides of a cell lie on the outline of the selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Edges {
    pub top: bool,
    pub right: bool,
    pub bottom: bool,
    pub left: bool,
}

/// How a cell is drawn: the sides to outline and whether to tint it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Marking {
    pub edges: Edges,
    /// The active cell (where the selection started) stays untinted so it stands out.
    pub tint: bool,
}

/// How a press or click combines with the existing selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Mods {
    /// Shift: extend the last block from the anchor.
    pub extend: bool,
    /// Ctrl or Cmd: add (or remove) a block, keeping the others.
    pub add: bool,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct CellSelection {
    ranges: Vec<Rect>,
    anchor: Option<(usize, usize)>,
    /// A drag is in progress: moving over cells reshapes the last block.
    dragging: bool,
}

impl CellSelection {
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    pub fn contains(&self, row: usize, col: usize) -> bool {
        self.ranges.iter().any(|r| r.contains(row, col))
    }

    /// Whether any selected block covers the row.
    pub fn row_marked(&self, row: usize) -> bool {
        self.ranges.iter().any(|r| (r.r0..=r.r1).contains(&row))
    }

    /// How to draw a cell, or `None` when it is not selected. The outline follows the
    /// outer border of the selected area, so touching or overlapping blocks read as one.
    pub fn marking(&self, row: usize, col: usize) -> Option<Marking> {
        if !self.contains(row, col) {
            return None;
        }
        if self.anchor == Some((row, col)) {
            let all = Edges {
                top: true,
                right: true,
                bottom: true,
                left: true,
            };
            return Some(Marking {
                edges: all,
                tint: false,
            });
        }
        let open = |r: Option<usize>, c: Option<usize>| match (r, c) {
            (Some(r), Some(c)) => !self.contains(r, c),
            _ => true,
        };
        Some(Marking {
            edges: Edges {
                top: open(row.checked_sub(1), Some(col)),
                right: open(Some(row), col.checked_add(1)),
                bottom: open(row.checked_add(1), Some(col)),
                left: open(Some(row), col.checked_sub(1)),
            },
            tint: true,
        })
    }

    /// A mouse press on a cell: starts a block (and a drag).
    pub fn press(&mut self, cell: (usize, usize), mods: Mods) {
        self.dragging = true;
        match (mods.extend, self.anchor) {
            (true, Some(anchor)) => {
                let block = Rect::between(anchor, cell);
                match self.ranges.last_mut() {
                    Some(last) => *last = block,
                    None => self.ranges.push(block),
                }
            }
            _ if mods.add => {
                // Ctrl-click on a lone selected cell takes it back out.
                if let Some(at) = self.ranges.iter().position(|r| r.is_cell(cell.0, cell.1)) {
                    self.ranges.remove(at);
                    self.dragging = false;
                } else {
                    self.ranges.push(Rect::between(cell, cell));
                }
                self.anchor = Some(cell);
            }
            _ => {
                self.ranges = vec![Rect::between(cell, cell)];
                self.anchor = Some(cell);
            }
        }
    }

    /// The pointer moved over a cell while the button is down.
    pub fn drag_to(&mut self, cell: (usize, usize)) {
        if !self.dragging {
            return;
        }
        if let (Some(anchor), Some(last)) = (self.anchor, self.ranges.last_mut()) {
            *last = Rect::between(anchor, cell);
        }
    }

    pub fn release(&mut self) {
        self.dragging = false;
    }

    /// Selects whole rows (all `cols` columns): a click on a row header.
    pub fn select_row(&mut self, row: usize, cols: usize, mods: Mods) {
        if cols == 0 {
            return;
        }
        let last = cols - 1;
        match (mods.extend, self.anchor) {
            (true, Some(anchor)) => {
                let block = Rect::between((anchor.0, 0), (row, last));
                match self.ranges.last_mut() {
                    Some(l) => *l = block,
                    None => self.ranges.push(block),
                }
            }
            _ if mods.add => {
                self.ranges.push(Rect::between((row, 0), (row, last)));
                self.anchor = Some((row, 0));
            }
            _ => {
                self.ranges = vec![Rect::between((row, 0), (row, last))];
                self.anchor = Some((row, 0));
            }
        }
        self.dragging = false;
    }

    pub fn select_all(&mut self, rows: usize, cols: usize) {
        self.dragging = false;
        if rows == 0 || cols == 0 {
            self.clear();
            return;
        }
        self.ranges = vec![Rect::between((0, 0), (rows - 1, cols - 1))];
        self.anchor = Some((0, 0));
    }

    /// Rows touched by the selection, ascending.
    pub fn rows(&self) -> BTreeSet<usize> {
        self.ranges.iter().flat_map(|r| r.r0..=r.r1).collect()
    }

    fn cols(&self) -> BTreeSet<usize> {
        self.ranges.iter().flat_map(|r| r.c0..=r.c1).collect()
    }

    /// Number of selected cells (blocks that overlap are counted once).
    pub fn cell_count(&self) -> usize {
        match self.ranges.as_slice() {
            [] => 0,
            [r] => (r.r1 - r.r0 + 1) * (r.c1 - r.c0 + 1),
            _ => {
                let cols = self.cols();
                self.rows()
                    .into_iter()
                    .map(|r| cols.iter().filter(|c| self.contains(r, **c)).count())
                    .sum()
            }
        }
    }

    /// The selection as tab-separated text: the rows and columns it touches, in grid order,
    /// with empty fields where a touched row and column cross outside the selection.
    /// `cell` gives a cell's text (`None` for NULL); `headers` adds the column names on top.
    pub fn to_tsv<'a>(
        &self,
        cell: impl Fn(usize, usize) -> Option<&'a str>,
        names: &[String],
        headers: bool,
    ) -> String {
        let cols: Vec<usize> = self.cols().into_iter().collect();
        let mut out = String::new();
        if headers {
            let line: Vec<String> = cols
                .iter()
                .map(|c| tsv_field(names.get(*c).map_or("", String::as_str)))
                .collect();
            out.push_str(&line.join("\t"));
            out.push('\n');
        }
        for row in self.rows() {
            let line: Vec<String> = cols
                .iter()
                .map(|c| {
                    if self.contains(row, *c) {
                        tsv_field(cell(row, *c).unwrap_or(""))
                    } else {
                        String::new()
                    }
                })
                .collect();
            out.push_str(&line.join("\t"));
            out.push('\n');
        }
        out
    }
}

/// Quotes a field the way spreadsheets expect when it holds a tab, line break or quote.
fn tsv_field(v: &str) -> String {
    if v.contains(['\t', '\n', '\r', '"']) {
        format!("\"{}\"", v.replace('"', "\"\""))
    } else {
        v.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLAIN: Mods = Mods {
        extend: false,
        add: false,
    };
    const SHIFT: Mods = Mods {
        extend: true,
        add: false,
    };
    const CTRL: Mods = Mods {
        extend: false,
        add: true,
    };

    #[test]
    fn click_drag_and_release() {
        let mut s = CellSelection::default();
        assert_eq!(s.cell_count(), 0);
        s.drag_to((3, 3));
        assert_eq!(s.cell_count(), 0, "no drag without a press");
        s.press((1, 1), PLAIN);
        s.drag_to((3, 2));
        assert!(s.contains(2, 2) && s.contains(1, 1) && s.contains(3, 2) && !s.contains(0, 0));
        assert_eq!(s.cell_count(), 6);
        s.release();
        s.drag_to((9, 9));
        assert_eq!(s.cell_count(), 6, "no drag after release");
        s.press((5, 5), PLAIN);
        assert_eq!((s.cell_count(), s.contains(1, 1)), (1, false));
    }

    #[test]
    fn shift_extends_from_the_anchor_and_ctrl_adds() {
        let mut s = CellSelection::default();
        s.press((1, 1), PLAIN);
        s.release();
        s.press((3, 0), SHIFT);
        s.release();
        assert_eq!(s.cell_count(), 6);
        s.press((1, 2), SHIFT);
        s.release();
        assert_eq!((s.cell_count(), s.contains(2, 2)), (2, false));
        s.press((7, 7), CTRL);
        s.release();
        assert_eq!(s.cell_count(), 3);
        assert!(s.contains(7, 7) && s.contains(1, 2));
        // Ctrl-click on a lone cell removes it again.
        s.press((7, 7), CTRL);
        assert_eq!(s.cell_count(), 2);
        s.release();
        s.clear();
        s.press((2, 2), SHIFT);
        assert_eq!(
            s.cell_count(),
            1,
            "shift without an anchor selects the cell"
        );
    }

    #[test]
    fn marking_outlines_the_selected_area() {
        let mut s = CellSelection::default();
        assert_eq!(s.marking(0, 0), None);
        s.press((1, 1), PLAIN);
        s.drag_to((2, 2));
        s.release();
        let m = |r, c| s.marking(r, c).expect("selected");
        // The anchor is outlined on every side and left untinted.
        let anchor = m(1, 1);
        assert!(!anchor.tint);
        assert!(anchor.edges.top && anchor.edges.right && anchor.edges.bottom && anchor.edges.left);
        // The opposite corner is outlined only on the outside.
        let corner = m(2, 2);
        assert!(corner.tint);
        assert_eq!(
            corner.edges,
            Edges {
                top: false,
                right: true,
                bottom: true,
                left: false
            }
        );
        assert_eq!(s.marking(3, 3), None);
        // Touching blocks share no inner outline.
        let mut s = CellSelection::default();
        s.press((0, 0), PLAIN);
        s.release();
        s.press((0, 1), CTRL);
        s.release();
        let left = s.marking(0, 0).expect("selected").edges;
        assert_eq!(
            left,
            Edges {
                top: true,
                right: false,
                bottom: true,
                left: true
            }
        );
    }

    #[test]
    fn rows_and_everything() {
        let mut s = CellSelection::default();
        s.select_row(2, 3, PLAIN);
        assert_eq!(s.cell_count(), 3);
        s.select_row(4, 3, CTRL);
        s.select_row(5, 3, SHIFT);
        assert_eq!(s.rows().into_iter().collect::<Vec<_>>(), [2, 4, 5]);
        s.select_row(0, 0, PLAIN);
        s.select_all(4, 2);
        assert_eq!(s.cell_count(), 8);
        s.select_row(1, 2, SHIFT);
        assert_eq!(s.cell_count(), 4, "shift from the corner anchor");
        s.select_all(0, 5);
        assert_eq!(s.cell_count(), 0);
    }

    #[test]
    fn copies_as_tsv() {
        let data = [
            [Some("a"), Some("b\tc"), None],
            [Some("d"), Some("say \"hi\""), Some("f")],
            [Some("g"), Some("h"), Some("i")],
        ];
        let cell = |r: usize, c: usize| data[r][c];
        let names: Vec<String> = ["x", "y", "z"].iter().map(|s| s.to_string()).collect();
        let mut s = CellSelection::default();
        s.press((0, 0), PLAIN);
        s.drag_to((1, 1));
        s.release();
        assert_eq!(
            s.to_tsv(cell, &names, false),
            "a\t\"b\tc\"\nd\t\"say \"\"hi\"\"\"\n"
        );
        assert!(s.to_tsv(cell, &names, true).starts_with("x\ty\na"));
        // Two separate cells: the grid they span, empty where nothing is selected.
        s.clear();
        s.press((0, 0), PLAIN);
        s.release();
        s.press((2, 2), CTRL);
        s.release();
        assert_eq!(s.to_tsv(cell, &names, false), "a\t\n\ti\n");
        assert_eq!(s.cell_count(), 2);
    }
}
