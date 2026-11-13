//! Result grid with export actions and a cell inspector.

use std::sync::Arc;

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{InputEvent, Textarea, TextareaState};
use gpui_kit::component::table::{DataTable, TableEvent, TableState};
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use pgroove_core::edit::EditSession;
use pgroove_core::export;
use pgroove_core::query;
use pgroove_core::query::ResultSet;

use crate::results::{Jump, ResultsTable};

actions!(
    result_grid,
    [
        CopyCells,
        CopyCellsWithHeaders,
        SelectAllCells,
        JumpFirst,
        JumpLast,
        JumpTop,
        JumpBottom,
        JumpRowStart,
        JumpRowEnd
    ]
);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Format {
    Csv,
    Json,
}

impl Format {
    fn render(self, names: &[String], rows: &[Vec<Option<String>>]) -> String {
        match self {
            Format::Csv => export::to_csv(names, rows),
            Format::Json => export::to_json(names, rows),
        }
    }

    /// Chooses the format from a file name; anything but `.json` is CSV.
    fn from_path(path: &std::path::Path) -> Self {
        match path.extension().and_then(|e| e.to_str()) {
            Some(e) if e.eq_ignore_ascii_case("json") => Format::Json,
            _ => Format::Csv,
        }
    }
}

/// Emitted when staged edits change.
pub struct GridChanged;

/// The selected cell, prepared once for the inspector.
struct Inspected {
    /// Display text; `None` is SQL NULL.
    text: Option<String>,
    /// Why the shown value differs from the stored one, if it does.
    note: Option<&'static str>,
    /// Whether the value can be edited in the input below.
    can_edit: bool,
}

pub struct GridPane {
    grid: Entity<TableState<ResultsTable>>,
    selected: Option<(usize, usize)>,
    inspected: Option<Inspected>,
    /// Grid above, cell inspector below; the divider can be dragged.
    split: Entity<ResizableState>,
    editable: bool,
    edit_input: Entity<TextareaState>,
    _subs: Vec<Subscription>,
}

impl EventEmitter<GridChanged> for GridPane {}

impl GridPane {
    pub fn new(sortable: bool, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let delegate = if sortable {
            ResultsTable::sortable()
        } else {
            ResultsTable::default()
        };
        // The result grid draws its own row-number gutter and marks rows itself, so the
        // kit's blank row header and its row selection are off. Columns stay in query order:
        // dragging a header reorders them and makes the widths look wrong.
        let grid = cx.new(|cx| {
            TableState::new(delegate, window, cx)
                .cell_selectable(true)
                .row_header(false)
                .row_selectable(false)
                .col_movable(false)
        });
        let edit_input = cx.new(|cx| TextareaState::new(window, cx).placeholder("Value"));
        let subs = vec![
            cx.subscribe_in(&grid, window, |this, _, ev: &TableEvent, window, cx| {
                if let TableEvent::SelectCell(r, c) = ev {
                    let (r, c) = this
                        .grid
                        .update(cx, |g, _| g.delegate_mut().cell_selected((*r, *c)));
                    this.selected = Some((r, c));
                    this.refresh_inspected(cx);
                    if this.editable && this.inspected.as_ref().is_some_and(|i| i.can_edit) {
                        let text = this
                            .grid
                            .read(cx)
                            .delegate()
                            .cell(r, c)
                            .unwrap_or_default()
                            .to_string();
                        this.edit_input
                            .update(cx, |i, cx| i.set_value(text, window, cx));
                    }
                    cx.notify();
                }
            }),
            cx.subscribe(&edit_input, |this, input, ev: &InputEvent, cx| {
                // Enter inserts a line break; Ctrl/Cmd+Enter stages the value.
                if matches!(
                    ev,
                    InputEvent::PressEnter {
                        secondary: true,
                        ..
                    }
                ) {
                    let v = input.read(cx).value().to_string();
                    this.stage(Some(v), cx);
                }
            }),
        ];
        Self {
            grid,
            selected: None,
            inspected: None,
            split: cx.new(|_| ResizableState::default()),
            editable: false,
            edit_input,
            _subs: subs,
        }
    }

    /// Prepares the selected cell for the inspector (capped, pretty-printed once).
    fn refresh_inspected(&mut self, cx: &App) {
        let Some((r, c)) = self.selected else {
            self.inspected = None;
            return;
        };
        let value = self.grid.read(cx).delegate().cell(r, c);
        self.inspected = Some(match value {
            None => Inspected {
                text: None,
                note: None,
                can_edit: true,
            },
            Some(v) => {
                let clipped = query::is_clipped(v);
                let shown = export::inspect_text(v);
                let note = if clipped {
                    Some("Longer than 64 KiB: the value was cut when it was loaded.")
                } else if shown.shortened {
                    Some("Showing the start of a long value.")
                } else {
                    None
                };
                Inspected {
                    text: Some(shown.text),
                    note,
                    can_edit: note.is_none(),
                }
            }
        });
    }

    pub fn selected(&self) -> Option<(usize, usize)> {
        self.selected
    }

    /// The rows touched by the marked cells, or else the row of the selected cell.
    pub fn selected_rows(&self, cx: &App) -> Vec<usize> {
        let marked = self.grid.read(cx).delegate().selection().rows();
        if marked.is_empty() {
            self.selected.map(|(r, _)| r).into_iter().collect()
        } else {
            marked.into_iter().collect()
        }
    }

    /// Copies the marked cells (or the selected cell) as tab-separated text.
    fn copy_cells(&mut self, headers: bool, window: &mut Window, cx: &mut Context<Self>) {
        let copied = {
            let t = self.grid.read(cx).delegate();
            t.copy_text(headers).or_else(|| {
                let (r, c) = self.selected?;
                t.cell(r, c).map(|v| (v.to_string(), 1))
            })
        };
        if let Some((text, count)) = copied {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
            window.push_notification(
                format!("Copied {count} cell{}", if count == 1 { "" } else { "s" }),
                cx,
            );
        }
    }

    fn select_all_cells(&mut self, cx: &mut Context<Self>) {
        self.grid.update(cx, |g, cx| {
            let (rows, cols) = (g.delegate().row_total(), g.delegate().names().len());
            g.delegate_mut().selection_mut().select_all(rows, cols);
            cx.notify();
        });
    }

    /// Moves the selection to the cell a jump key names (Ctrl+Home, Ctrl+End, Ctrl+Up, ...).
    /// The kit scrolls to it and reports it like any keyboard move.
    fn jump(&mut self, jump: Jump, cx: &mut Context<Self>) {
        self.grid.update(cx, |g, cx| {
            if let Some((row, col)) = g.delegate().jump_target(jump, g.selected_cell()) {
                g.set_selected_cell(row, col, cx);
            }
        });
    }

    pub fn set_editable(&mut self, editable: bool, cx: &mut Context<Self>) {
        if self.editable != editable {
            self.editable = editable;
            cx.notify();
        }
    }

    /// Re-reads the grid size: the kit caches column widths, so they are rebuilt.
    pub fn rescale(&mut self, cx: &mut Context<Self>) {
        self.grid.update(cx, |g, cx| g.refresh(cx));
        cx.notify();
    }

    /// Installs (or clears) the staged-edit session for the shown data.
    pub fn set_session(&mut self, session: Option<EditSession>, cx: &mut Context<Self>) {
        self.grid.update(cx, |g, cx| {
            g.delegate_mut().set_session(session);
            g.refresh(cx);
        });
        cx.notify();
    }

    /// Runs `f` on the session, if any, and notifies listeners.
    pub fn edit_session<R>(
        &mut self,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut EditSession) -> R,
    ) -> Option<R> {
        let out = self.grid.update(cx, |g, cx| {
            let r = g.delegate_mut().session_mut().map(f);
            cx.notify();
            r
        });
        cx.emit(GridChanged);
        cx.notify();
        out
    }

    fn stage(&mut self, value: Option<String>, cx: &mut Context<Self>) {
        if let Some((r, c)) = self.selected {
            // On an inserted row, clearing the input means an explicit NULL.
            self.edit_session(cx, |s| s.set_value(r, c, value));
            self.refresh_inspected(cx);
        }
    }

    /// Makes the selected inserted cell use the column default again.
    fn stage_default(&mut self, cx: &mut Context<Self>) {
        if let Some((r, c)) = self.selected {
            self.edit_session(cx, |s| s.set_insert_default(r, c));
            self.refresh_inspected(cx);
        }
    }

    pub fn grid(&self) -> &Entity<TableState<ResultsTable>> {
        &self.grid
    }

    /// Replaces the shown data and clears the cell selection. The set is shared, not copied.
    pub fn set_data(&mut self, set: Option<Arc<ResultSet>>, cx: &mut Context<Self>) {
        self.selected = None;
        self.inspected = None;
        self.grid.update(cx, |g, cx| {
            g.delegate_mut().set(set);
            g.refresh(cx);
        });
        cx.notify();
    }

    /// Copy and save buttons for the current data.
    pub fn actions(&self) -> impl IntoElement {
        let button =
            |id: &'static str,
             icon: IconName,
             tip: &'static str,
             f: fn(&Entity<TableState<ResultsTable>>, &mut Window, &mut App)| {
                let grid = self.grid.clone();
                Button::new(id)
                    .ghost()
                    .xsmall()
                    .icon(icon)
                    .tooltip(tip)
                    .on_click(move |_, window, cx| f(&grid, window, cx))
            };
        h_flex()
            .gap_0p5()
            .child(button(
                "copy-csv",
                IconName::Copy,
                "Copy as CSV",
                |g, w, cx| copy(g, Format::Csv, w, cx),
            ))
            .child(button(
                "copy-json",
                IconName::Braces,
                "Copy as JSON",
                |g, w, cx| copy(g, Format::Json, w, cx),
            ))
            .child(button(
                "save",
                IconName::Download,
                "Save as CSV or JSON…",
                save,
            ))
    }

    fn render_inspector(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let (r, c) = self.selected?;
        let inspected = self.inspected.as_ref()?;
        let theme = cx.theme().clone();
        let name = self.grid.read(cx).delegate().column_name(c)?.to_string();
        let inserted = self
            .grid
            .read(cx)
            .delegate()
            .session()
            .is_some_and(|s| s.is_inserted(r));
        let grid = self.grid.clone();
        let editable = self.editable && inspected.can_edit;
        let note = inspected.note;
        let text = inspected.text.clone();
        Some(
            v_flex()
                .size_full()
                .border_t_1()
                .border_color(theme.border)
                .child(
                    h_flex()
                        .px_3()
                        .h(crate::theme::ui_px(26., cx))
                        .items_center()
                        .gap_2()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(format!("{name} · row {}", r + 1))
                        .children(note.map(|n| div().text_color(theme.warning).child(n)))
                        .child(div().flex_1())
                        .child(
                            Button::new("copy-cell")
                                .ghost()
                                .xsmall()
                                .icon(IconName::Copy)
                                .tooltip("Copy value")
                                .on_click(move |_, window, cx| {
                                    // Read at click time; the inspector only holds a preview.
                                    let value =
                                        grid.read(cx).delegate().cell(r, c).map(str::to_string);
                                    if let Some(v) = value {
                                        cx.write_to_clipboard(ClipboardItem::new_string(v));
                                        window.push_notification("Copied value", cx);
                                    }
                                }),
                        ),
                )
                .when(editable, |d| {
                    d.child(
                        h_flex()
                            .px_3()
                            .pb_2()
                            .gap_2()
                            .items_center()
                            .items_start()
                            .child(
                                div()
                                    .flex_1()
                                    .child(Textarea::new(&self.edit_input).h(px(96.))),
                            )
                            .child(
                                Button::new("stage-value")
                                    .outline()
                                    .xsmall()
                                    .label("Stage")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        let v = this.edit_input.read(cx).value().to_string();
                                        this.stage(Some(v), cx);
                                    })),
                            )
                            .child(
                                Button::new("stage-null")
                                    .ghost()
                                    .xsmall()
                                    .label("Set NULL")
                                    .tooltip(if inserted {
                                        "Insert an explicit NULL in this column"
                                    } else {
                                        "Set this column to NULL"
                                    })
                                    .on_click(cx.listener(|this, _, _, cx| this.stage(None, cx))),
                            )
                            .when(inserted, |d| {
                                d.child(
                                    Button::new("use-default")
                                        .ghost()
                                        .xsmall()
                                        .label("Use default")
                                        .tooltip("Omit this column when the row is inserted")
                                        .on_click(
                                            cx.listener(|this, _, _, cx| this.stage_default(cx)),
                                        ),
                                )
                            }),
                    )
                })
                .when(!editable, |d| {
                    d.child(
                        div()
                            .id("inspector-value")
                            .flex_1()
                            .min_h_0()
                            .px_3()
                            .pb_2()
                            .overflow_y_scroll()
                            .font_family(theme.mono_font_family.clone())
                            .text_sm()
                            .child(match text {
                                Some(v) => div().child(v),
                                None => div()
                                    .italic()
                                    .text_color(theme.muted_foreground)
                                    .child("NULL"),
                            }),
                    )
                }),
        )
    }
}

impl Render for GridPane {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let on_copy =
            cx.listener(|this, _: &CopyCells, window, cx| this.copy_cells(false, window, cx));
        let on_copy_headers = cx.listener(|this, _: &CopyCellsWithHeaders, window, cx| {
            this.copy_cells(true, window, cx)
        });
        let on_select_all =
            cx.listener(|this, _: &SelectAllCells, _, cx| this.select_all_cells(cx));
        let on_first = cx.listener(|this, _: &JumpFirst, _, cx| this.jump(Jump::First, cx));
        let on_last = cx.listener(|this, _: &JumpLast, _, cx| this.jump(Jump::Last, cx));
        let on_top = cx.listener(|this, _: &JumpTop, _, cx| this.jump(Jump::Top, cx));
        let on_bottom = cx.listener(|this, _: &JumpBottom, _, cx| this.jump(Jump::Bottom, cx));
        let on_row_start =
            cx.listener(|this, _: &JumpRowStart, _, cx| this.jump(Jump::RowStart, cx));
        let on_row_end = cx.listener(|this, _: &JumpRowEnd, _, cx| this.jump(Jump::RowEnd, cx));
        // The grid's size: the kit takes a row height, and the text follows through the
        // pixel text size of this subtree, so a changed size shows at once.
        let percent = crate::theme::grid_scale(cx);
        let row_height = crate::results::row_height(cx);
        let inspector = self.render_inspector(cx);
        let show_inspector = inspector.is_some();
        let grid = self.grid.clone();
        let table =
            DataTable::new(&self.grid).with_size(gpui_kit::component::Size::Size(row_height));
        div()
            .size_full()
            .key_context("ResultGrid")
            .on_action(on_copy)
            .on_action(on_copy_headers)
            .on_action(on_select_all)
            .on_action(on_first)
            .on_action(on_last)
            .on_action(on_top)
            .on_action(on_bottom)
            .on_action(on_row_start)
            .on_action(on_row_end)
            // A drag that ends outside the cells still ends.
            .on_mouse_up(MouseButton::Left, move |_, _, cx| {
                grid.update(cx, |g, _| g.delegate_mut().selection_mut().release());
            })
            .child(
                v_resizable("grid-split")
                    .with_state(&self.split)
                    .child(
                        resizable_panel().child(
                            div()
                                .size_full()
                                .text_size(px(crate::results::cell_text(percent)))
                                .child(table),
                        ),
                    )
                    .child(
                        resizable_panel()
                            .size(px(160.))
                            .size_range(px(70.)..px(900.))
                            .visible(show_inspector)
                            .children(inspector),
                    ),
            )
    }
}

fn snapshot(grid: &Entity<TableState<ResultsTable>>, cx: &App) -> Option<Arc<ResultSet>> {
    grid.read(cx).delegate().data()
}

fn copy(grid: &Entity<TableState<ResultsTable>>, fmt: Format, window: &mut Window, cx: &mut App) {
    let Some(set) = snapshot(grid, cx) else {
        return;
    };
    if set.columns.is_empty() {
        return;
    }
    cx.write_to_clipboard(ClipboardItem::new_string(
        fmt.render(&set.columns, &set.rows),
    ));
    window.push_notification(format!("Copied {} rows", set.rows.len()), cx);
}

fn save(grid: &Entity<TableState<ResultsTable>>, window: &mut Window, cx: &mut App) {
    let Some(set) = snapshot(grid, cx) else {
        return;
    };
    if set.columns.is_empty() {
        return;
    }
    let dir = std::env::current_dir().unwrap_or_default();
    let rx = cx.prompt_for_new_path(&dir, Some("results.csv"));
    let handle = window.window_handle();
    cx.spawn(async move |cx| {
        let Ok(Ok(Some(path))) = rx.await else {
            return;
        };
        let text = Format::from_path(&path).render(&set.columns, &set.rows);
        let msg = match std::fs::write(&path, text) {
            Ok(()) => format!("Saved {} rows to {}", set.rows.len(), path.display()),
            Err(e) => format!("Couldn't save: {e}"),
        };
        cx.update(|cx| {
            handle
                .update(cx, |_, window, cx| window.push_notification(msg, cx))
                .ok();
        });
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use super::Format;
    use std::path::Path;

    #[test]
    fn format_from_extension() {
        assert_eq!(Format::from_path(Path::new("a.json")), Format::Json);
        assert_eq!(Format::from_path(Path::new("a.JSON")), Format::Json);
        assert_eq!(Format::from_path(Path::new("a.csv")), Format::Csv);
        assert_eq!(Format::from_path(Path::new("a")), Format::Csv);
    }

    #[test]
    fn render_dispatch() {
        let names = vec!["a".to_string()];
        let rows = vec![vec![Some("1".to_string())]];
        assert_eq!(Format::Csv.render(&names, &rows), "a\r\n1\r\n");
        assert!(Format::Json.render(&names, &rows).contains("\"a\": \"1\""));
    }
}
