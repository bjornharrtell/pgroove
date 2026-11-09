//! Result grid with export actions and a cell inspector.

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::table::{DataTable, TableEvent, TableState};
use gpui_kit::component::*;
use gpui_kit::*;
use pgroove_core::export;
use pgroove_core::query::ResultSet;

use crate::results::ResultsTable;

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

pub struct GridPane {
    grid: Entity<TableState<ResultsTable>>,
    selected: Option<(usize, usize)>,
    _sub: Subscription,
}

impl GridPane {
    pub fn new(sortable: bool, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let delegate = if sortable {
            ResultsTable::sortable()
        } else {
            ResultsTable::default()
        };
        let grid = cx.new(|cx| TableState::new(delegate, window, cx).cell_selectable(true));
        let sub = cx.subscribe(&grid, |this, _, ev: &TableEvent, cx| {
            if let TableEvent::SelectCell(r, c) = ev {
                this.selected = Some((*r, *c));
                cx.notify();
            }
        });
        Self {
            grid,
            selected: None,
            _sub: sub,
        }
    }

    pub fn grid(&self) -> &Entity<TableState<ResultsTable>> {
        &self.grid
    }

    /// Replaces the shown data and clears the cell selection.
    pub fn set_data(&mut self, set: Option<&ResultSet>, cx: &mut Context<Self>) {
        self.selected = None;
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
        let theme = cx.theme().clone();
        let t = self.grid.read(cx).delegate();
        let name = t.column_name(c)?.to_string();
        let value = t.cell(r, c).map(export::pretty_value);
        Some(
            v_flex()
                .flex_shrink_0()
                .max_h(px(160.))
                .border_t_1()
                .border_color(theme.border)
                .child(
                    h_flex()
                        .px_3()
                        .h(px(26.))
                        .items_center()
                        .gap_2()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(format!("{name} · row {}", r + 1))
                        .child(div().flex_1())
                        .child(
                            Button::new("copy-cell")
                                .ghost()
                                .xsmall()
                                .icon(IconName::Copy)
                                .tooltip("Copy value")
                                .on_click({
                                    let value = value.clone();
                                    move |_, window, cx| {
                                        if let Some(v) = &value {
                                            cx.write_to_clipboard(ClipboardItem::new_string(
                                                v.clone(),
                                            ));
                                            window.push_notification("Copied value", cx);
                                        }
                                    }
                                }),
                        ),
                )
                .child(
                    div()
                        .id("inspector-value")
                        .px_3()
                        .pb_2()
                        .overflow_y_scroll()
                        .font_family(theme.mono_font_family.clone())
                        .text_sm()
                        .child(match value {
                            Some(v) => div().child(v),
                            None => div()
                                .italic()
                                .text_color(theme.muted_foreground)
                                .child("NULL"),
                        }),
                ),
        )
    }
}

impl Render for GridPane {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .child(div().flex_1().min_h_0().child(DataTable::new(&self.grid)))
            .children(self.render_inspector(cx))
    }
}

fn snapshot(
    grid: &Entity<TableState<ResultsTable>>,
    cx: &App,
) -> (Vec<String>, Vec<Vec<Option<String>>>) {
    let t = grid.read(cx).delegate();
    (t.names().to_vec(), t.rows().to_vec())
}

fn copy(grid: &Entity<TableState<ResultsTable>>, fmt: Format, window: &mut Window, cx: &mut App) {
    let (names, rows) = snapshot(grid, cx);
    if names.is_empty() {
        return;
    }
    cx.write_to_clipboard(ClipboardItem::new_string(fmt.render(&names, &rows)));
    window.push_notification(format!("Copied {} rows", rows.len()), cx);
}

fn save(grid: &Entity<TableState<ResultsTable>>, window: &mut Window, cx: &mut App) {
    let (names, rows) = snapshot(grid, cx);
    if names.is_empty() {
        return;
    }
    let dir = std::env::current_dir().unwrap_or_default();
    let rx = cx.prompt_for_new_path(&dir, Some("results.csv"));
    let handle = window.window_handle();
    cx.spawn(async move |cx| {
        let Ok(Ok(Some(path))) = rx.await else {
            return;
        };
        let text = Format::from_path(&path).render(&names, &rows);
        let msg = match std::fs::write(&path, text) {
            Ok(()) => format!("Saved {} rows to {}", rows.len(), path.display()),
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
