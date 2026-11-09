//! Data viewer tab: paged, sortable, filterable table contents.

use std::sync::Arc;

use crate::dialogs::DialogActions;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::*;
use gpui_kit::*;
use pgroove_core::data::{self, PageRequest, Sort, TableRef};
use pgroove_core::edit::{self, EditSession};
use tokio::runtime::Runtime;

use crate::grid_pane::{GridChanged, GridPane};
use crate::mode::ReadOnlyMode;
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
    /// Primary key columns when the relation is a keyed table.
    pk: Vec<String>,
    notice: Option<(String, bool)>,
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
            cx.subscribe(&pane, |_, _, _: &GridChanged, cx| cx.notify()),
            cx.subscribe_in(&filter, window, |this, _, ev: &InputEvent, _, cx| {
                if matches!(ev, InputEvent::PressEnter { .. }) {
                    this.apply_filter(cx);
                }
            }),
        ];
        let mut view = Self {
            mode: Mode::Data,
            pk: Vec::new(),
            notice: None,
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

    /// Switches the tab to the Structure view.
    pub fn show_structure(&mut self, cx: &mut Context<Self>) {
        self.set_mode(Mode::Structure, cx);
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

    fn delete_selected(&mut self, cx: &mut Context<Self>) {
        if let Some((row, _)) = self.pane.read(cx).selected() {
            self.pane
                .update(cx, |p, cx| p.edit_session(cx, |s| s.toggle_delete(row)));
        }
    }

    fn discard(&mut self, cx: &mut Context<Self>) {
        self.notice = None;
        self.pane
            .update(cx, |p, cx| p.edit_session(cx, EditSession::discard));
    }

    fn open_add_row(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let names = self
            .pane
            .read(cx)
            .grid()
            .read(cx)
            .delegate()
            .names()
            .to_vec();
        let inputs: Vec<Entity<InputState>> = names
            .iter()
            .map(|_| cx.new(|cx| InputState::new(window, cx).placeholder("default")))
            .collect();
        let this = cx.entity();
        window.open_dialog(cx, move |dialog, _, _| {
            let (inputs, this) = (inputs.clone(), this.clone());
            let fields = names.iter().zip(inputs.iter()).map(|(n, i)| {
                v_flex()
                    .gap_1()
                    .child(div().text_sm().child(n.clone()))
                    .child(Input::new(i))
            });
            dialog
                .title("New row")
                .w(px(420.))
                .child(
                    v_flex()
                        .id("new-row-form")
                        .max_h(px(420.))
                        .overflow_y_scroll()
                        .gap_2()
                        .child(
                            div()
                                .text_xs()
                                .child("Empty fields use the column default."),
                        )
                        .children(fields),
                )
                .actions("Add row", true, move |_, _, cx| {
                    let values: Vec<Option<String>> = inputs
                        .iter()
                        .map(|i| {
                            let v = i.read(cx).value().to_string();
                            (!v.is_empty()).then_some(v)
                        })
                        .collect();
                    this.update(cx, |tv, cx| {
                        tv.pane
                            .update(cx, |p, cx| p.edit_session(cx, |s| s.add_insert(values)))
                    });
                    true
                })
        });
    }

    fn open_review(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let sql: Vec<String> = self
            .pane
            .read(cx)
            .grid()
            .read(cx)
            .delegate()
            .session()
            .map(|s| {
                s.statements(&self.table)
                    .into_iter()
                    .map(|st| st.sql)
                    .collect()
            })
            .unwrap_or_default();
        if sql.is_empty() {
            return;
        }
        let mono = cx.theme().mono_font_family.clone();
        let this = cx.entity();
        window.open_dialog(cx, move |dialog, _, _| {
            let this = this.clone();
            dialog
                .title(format!("Apply {} change(s)?", sql.len()))
                .w(px(640.))
                .child(
                    v_flex()
                        .id("review-sql")
                        .max_h(px(360.))
                        .overflow_y_scroll()
                        .gap_1()
                        .font_family(mono.clone())
                        .text_xs()
                        .children(sql.iter().map(|s| div().child(format!("{s};")))),
                )
                .actions("Apply changes", true, move |_, _, cx| {
                    this.update(cx, |tv, cx| tv.apply(cx));
                    true
                })
        });
    }

    fn apply(&mut self, cx: &mut Context<Self>) {
        let statements = match self.pane.read(cx).grid().read(cx).delegate().session() {
            Some(s) => s.statements(&self.table),
            None => return,
        };
        let client = self.client.clone();
        let task = self
            .runtime
            .spawn(async move { edit::apply(&client, &statements).await });
        self.notice = Some(("Applying…".into(), false));
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = match task.await {
                Ok(r) => r.map_err(|e| format!("{e:#}")),
                Err(e) => Err(e.to_string()),
            };
            this.update(cx, |this, cx| match result {
                Ok(()) => {
                    this.reload(cx);
                    this.notice = Some(("Changes applied.".into(), false));
                }
                Err(e) => {
                    this.notice = Some((format!("Not applied: {e}"), true));
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    fn render_edit_bar(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let theme = cx.theme().clone();
        if self.pk.is_empty() {
            return None;
        }
        let read_only = cx.global::<ReadOnlyMode>().0;
        let pending = self.pending(cx);
        let bar = h_flex()
            .gap_2()
            .px_3()
            .h(px(32.))
            .items_center()
            .border_b_1()
            .border_color(theme.border)
            .text_xs();
        if read_only {
            return Some(
                bar.text_color(theme.muted_foreground)
                    .child("Read-only. Turn it off in the toolbar to edit rows.")
                    .into_any_element(),
            );
        }
        let notice = self.notice.clone().map(|(text, err)| {
            div()
                .text_color(if err {
                    theme.danger
                } else {
                    theme.muted_foreground
                })
                .child(text)
        });
        Some(
            bar.child(
                Button::new("add-row")
                    .ghost()
                    .xsmall()
                    .icon(IconName::Plus)
                    .label("Add row")
                    .on_click(cx.listener(|this, _, window, cx| this.open_add_row(window, cx))),
            )
            .child(
                Button::new("delete-row")
                    .ghost()
                    .xsmall()
                    .icon(IconName::Trash)
                    .label("Delete row")
                    .tooltip("Select a cell first")
                    .on_click(cx.listener(|this, _, _, cx| this.delete_selected(cx))),
            )
            .child(
                Button::new("review")
                    .outline()
                    .xsmall()
                    .label(format!("Review changes ({pending})"))
                    .on_click(cx.listener(|this, _, window, cx| this.open_review(window, cx))),
            )
            .child(
                Button::new("discard")
                    .ghost()
                    .xsmall()
                    .label("Discard")
                    .on_click(cx.listener(|this, _, _, cx| this.discard(cx))),
            )
            .children(notice)
            .into_any_element(),
        )
    }

    pub fn table(&self) -> &TableRef {
        &self.table
    }

    fn pending(&self, cx: &App) -> usize {
        self.pane
            .read(cx)
            .grid()
            .read(cx)
            .delegate()
            .session()
            .map_or(0, EditSession::pending_count)
    }

    /// True (with a notice) when unsaved edits would be lost by reloading.
    fn blocked_by_pending(&mut self, cx: &mut Context<Self>) -> bool {
        if self.pending(cx) > 0 {
            self.notice = Some(("Apply or discard pending changes first.".into(), true));
            cx.notify();
            return true;
        }
        false
    }

    fn set_sort(&mut self, req: Option<(usize, bool)>, cx: &mut Context<Self>) {
        if self.blocked_by_pending(cx) {
            return;
        }
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
        if self.blocked_by_pending(cx) {
            return;
        }
        let text = self.filter.read(cx).value().trim().to_string();
        self.applied_filter = (!text.is_empty()).then_some(text);
        self.page = 0;
        self.reload(cx);
    }

    fn go(&mut self, page: u64, cx: &mut Context<Self>) {
        if self.blocked_by_pending(cx) {
            return;
        }
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
        let task = self.runtime.spawn(async move {
            let page = data::fetch_page(&client, &table, &req).await?;
            let pk = edit::primary_key(&client, &table).await.unwrap_or_default();
            anyhow::Ok((page, pk))
        });
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
                    Ok((page, pk)) => {
                        this.rows = page.data.rows.len();
                        this.notice = None;
                        let session = (!pk.is_empty())
                            .then(|| {
                                EditSession::new(
                                    page.data.columns.clone(),
                                    &pk,
                                    page.data.rows.clone(),
                                )
                                .ok()
                            })
                            .flatten();
                        this.pk = if session.is_some() { pk } else { Vec::new() };
                        this.has_more = page.has_more;
                        this.pane.update(cx, |p, cx| {
                            p.set_data(Some(&page.data), cx);
                            p.set_session(session, cx);
                        });
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
        let editable = !cx.global::<ReadOnlyMode>().0 && !self.pk.is_empty();
        self.pane.update(cx, |p, cx| p.set_editable(editable, cx));
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
            .children(self.render_edit_bar(cx))
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
