//! Data viewer tab: paged, sortable, filterable table contents.

use std::sync::Arc;

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::*;
use gpui_kit::*;
use pgroove_core::data::{self, PageRequest, Sort, TableRef};
use tokio::runtime::Runtime;

use crate::grid_pane::GridPane;
use crate::structure_view::{OpenTable, StructureView};

pub const PAGE_SIZE: u64 = 200;

enum Load {
    Loading,
    Ready,
    Failed(String),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Data,
    Structure,
}

pub struct TableView {
    mode: Mode,
    structure: Option<Entity<StructureView>>,
    table: TableRef,
    client: Arc<tokio_postgres::Client>,
    runtime: Arc<Runtime>,
    pane: Entity<GridPane>,
    filter: Entity<InputState>,
    page: u64,
    sort: Option<Sort>,
    applied_filter: Option<String>,
    load: Load,
    rows: usize,
    has_more: bool,
    /// Bumped per request so stale responses are ignored.
    generation: u64,
    _subs: Vec<Subscription>,
}

/// Human-readable row range for a page, e.g. `201–400`.
pub fn range_label(page: u64, rows: usize) -> String {
    if rows == 0 {
        return "No rows".into();
    }
    let first = page * PAGE_SIZE + 1;
    format!("{}–{}", first, first + rows as u64 - 1)
}

impl EventEmitter<OpenTable> for TableView {}

impl TableView {
    pub fn new(
        table: TableRef,
        client: Arc<tokio_postgres::Client>,
        runtime: Arc<Runtime>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let pane = cx.new(|cx| GridPane::new(true, window, cx));
        let grid = pane.read(cx).grid().clone();
        let filter =
            cx.new(|cx| InputState::new(window, cx).placeholder("Filter, e.g. total > 100"));
        let subs = vec![
            cx.observe(&grid, |this, grid, cx| {
                let req = grid.update(cx, |g, _| g.delegate_mut().take_sort());
                if let Some(req) = req {
                    this.set_sort(req, cx);
                }
            }),
            cx.subscribe_in(&filter, window, |this, _, ev: &InputEvent, _, cx| {
                if matches!(ev, InputEvent::PressEnter { .. }) {
                    this.apply_filter(cx);
                }
            }),
        ];
        let mut view = Self {
            mode: Mode::Data,
            structure: None,
            table,
            client,
            runtime,
            pane,
            filter,
            page: 0,
            sort: None,
            applied_filter: None,
            load: Load::Loading,
            rows: 0,
            has_more: false,
            generation: 0,
            _subs: subs,
        };
        view.reload(cx);
        view
    }

    fn set_mode(&mut self, mode: Mode, cx: &mut Context<Self>) {
        self.mode = mode;
        if mode == Mode::Structure && self.structure.is_none() {
            let (table, client, runtime) = (
                self.table.clone(),
                self.client.clone(),
                self.runtime.clone(),
            );
            let view = cx.new(|cx| StructureView::new(table, client, runtime, cx));
            self._subs
                .push(cx.subscribe(&view, |_, _, ev: &OpenTable, cx| {
                    cx.emit(OpenTable(ev.0.clone()))
                }));
            self.structure = Some(view);
        }
        cx.notify();
    }

    pub fn table(&self) -> &TableRef {
        &self.table
    }

    fn set_sort(&mut self, req: Option<(usize, bool)>, cx: &mut Context<Self>) {
        self.sort = req.and_then(|(ix, descending)| {
            let column = self
                .pane
                .read(cx)
                .grid()
                .read(cx)
                .delegate()
                .column_name(ix)?
                .to_string();
            Some(Sort { column, descending })
        });
        let shown = req.filter(|_| self.sort.is_some());
        let grid = self.pane.read(cx).grid().clone();
        grid.update(cx, |g, _| g.delegate_mut().set_sorted(shown));
        self.page = 0;
        self.reload(cx);
    }

    fn apply_filter(&mut self, cx: &mut Context<Self>) {
        let text = self.filter.read(cx).value().trim().to_string();
        self.applied_filter = (!text.is_empty()).then_some(text);
        self.page = 0;
        self.reload(cx);
    }

    fn go(&mut self, page: u64, cx: &mut Context<Self>) {
        self.page = page;
        self.reload(cx);
    }

    pub fn reload(&mut self, cx: &mut Context<Self>) {
        self.generation += 1;
        let generation = self.generation;
        self.load = Load::Loading;
        cx.notify();
        let mut req = PageRequest::page(self.page, PAGE_SIZE);
        req.sort = self.sort.clone();
        req.filter = self.applied_filter.clone();
        let (client, table) = (self.client.clone(), self.table.clone());
        let task = self
            .runtime
            .spawn(async move { data::fetch_page(&client, &table, &req).await });
        cx.spawn(async move |this, cx| {
            let result = match task.await {
                Ok(r) => r.map_err(|e| format!("{e:#}")),
                Err(e) => Err(e.to_string()),
            };
            this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                match result {
                    Ok(page) => {
                        this.rows = page.data.rows.len();
                        this.has_more = page.has_more;
                        this.pane
                            .update(cx, |p, cx| p.set_data(Some(&page.data), cx));
                        this.load = Load::Ready;
                    }
                    Err(e) => this.load = Load::Failed(e),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
}

impl Render for TableView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let page = self.page;
        let mode = self.mode;
        let mode_button = |id: &'static str, label: &'static str, which: Mode| {
            let b = Button::new(id).xsmall().label(label);
            let b = if mode == which {
                b.outline()
            } else {
                b.ghost()
            };
            b.on_click(cx.listener(move |this, _, _, cx| this.set_mode(which, cx)))
        };
        let mode_bar = h_flex()
            .gap_1()
            .px_3()
            .h(px(32.))
            .items_center()
            .border_b_1()
            .border_color(theme.border)
            .child(mode_button("mode-data", "Data", Mode::Data))
            .child(mode_button("mode-structure", "Structure", Mode::Structure))
            .child(div().flex_1())
            .child(
                Button::new("reload")
                    .ghost()
                    .xsmall()
                    .icon(IconName::RefreshCw)
                    .tooltip("Reload")
                    .on_click(
                        cx.listener(|this, _, _, cx| match (this.mode, &this.structure) {
                            (Mode::Structure, Some(s)) => s.update(cx, |s, cx| s.reload(cx)),
                            _ => this.reload(cx),
                        }),
                    ),
            );
        if mode == Mode::Structure
            && let Some(s) = &self.structure
        {
            return v_flex()
                .size_full()
                .child(mode_bar)
                .child(div().flex_1().min_h_0().child(s.clone()))
                .into_any_element();
        }
        let body = match &self.load {
            Load::Failed(e) => div()
                .p_3()
                .text_sm()
                .text_color(theme.danger)
                .child(e.clone())
                .into_any_element(),
            _ => div()
                .flex_1()
                .min_h_0()
                .child(self.pane.clone())
                .into_any_element(),
        };
        let status = match self.load {
            Load::Loading => "Loading…".to_string(),
            _ => range_label(self.page, self.rows),
        };
        v_flex()
            .size_full()
            .child(mode_bar)
            .child(
                h_flex()
                    .gap_2()
                    .px_3()
                    .h(px(38.))
                    .items_center()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(div().flex_1().child(Input::new(&self.filter)))
                    .child(self.pane.read(cx).actions())
                    .child(
                        Button::new("prev")
                            .ghost()
                            .xsmall()
                            .icon(IconName::ChevronLeft)
                            .tooltip("Previous page")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if page > 0 {
                                    this.go(page - 1, cx)
                                }
                            })),
                    )
                    .child(
                        div()
                            .min_w(px(80.))
                            .text_xs()
                            .text_center()
                            .text_color(theme.muted_foreground)
                            .child(status),
                    )
                    .child(
                        Button::new("next")
                            .ghost()
                            .xsmall()
                            .icon(IconName::ChevronRight)
                            .tooltip("Next page")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if this.has_more {
                                    this.go(page + 1, cx)
                                }
                            })),
                    ),
            )
            .child(body)
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::range_label;

    #[test]
    fn range_labels() {
        assert_eq!(range_label(0, 0), "No rows");
        assert_eq!(range_label(0, 200), "1–200");
        assert_eq!(range_label(2, 5), "401–405");
    }
}
