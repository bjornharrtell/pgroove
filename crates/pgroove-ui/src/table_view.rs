//! Data viewer tab: paged, sortable, filterable table contents.

use std::sync::Arc;

use crate::dialogs::DialogActions;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::*;
use gpui_kit::*;
use pgroove_core::data::{self, PageRequest, Sort, TableRef};
use pgroove_core::edit::{self, EditSession, RowIdentity};
use tokio::runtime::Runtime;

use crate::grid_pane::{GridChanged, GridPane};
use crate::mode::ReadOnlyMode;
use crate::query_tab::format_limit;
use crate::structure_view::{OpenTable, StructureView};

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
    /// How rows are identified for edits; `None` means the relation can't be edited.
    identity: RowIdentity,
    notice: Option<(String, bool)>,
    structure: Option<Entity<StructureView>>,
    table: TableRef,
    client: Arc<pgroove_core::Client>,
    runtime: Arc<Runtime>,
    pane: Entity<GridPane>,
    filter: Entity<InputState>,
    page: u64,
    sort: Option<Sort>,
    applied_filter: Option<String>,
    load: Load,
    rows: usize,
    has_more: bool,
    /// Rows per page.
    page_size: u32,
    /// A whole-table export is running (clicking Export again cancels it).
    exporting: bool,
    /// Bumped per request so stale responses are ignored.
    generation: u64,
    _subs: Vec<Subscription>,
}

/// Human-readable row range for a page, e.g. `201–400`.
pub fn range_label(page: u64, page_size: u32, rows: usize) -> String {
    if rows == 0 {
        return "No rows".into();
    }
    let first = page * u64::from(page_size) + 1;
    format!("{}–{}", first, first + rows as u64 - 1)
}

impl EventEmitter<OpenTable> for TableView {}

/// Emitted when the user picks another page size, so it can be remembered.
pub struct PageSizeChanged(pub u32);

impl EventEmitter<PageSizeChanged> for TableView {}

/// Emitted when the user asks to leave read-only mode so they can edit this table.
pub struct AllowEditing;

impl EventEmitter<AllowEditing> for TableView {}

impl TableView {
    pub fn new(
        table: TableRef,
        client: Arc<pgroove_core::Client>,
        runtime: Arc<Runtime>,
        page_size: u32,
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
            identity: RowIdentity::None,
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
            page_size: pgroove_core::settings::clamp_row_limit(page_size),
            exporting: false,
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
        let rows = self.pane.read(cx).selected_rows(cx);
        if rows.is_empty() {
            self.notice = Some(("Select a cell in the row first.".into(), true));
            cx.notify();
            return;
        }
        self.notice = None;
        self.pane.update(cx, |p, cx| {
            p.edit_session(cx, |s| {
                // Delete every marked row; if they are all deleted already, bring them back.
                let restore = rows.iter().all(|r| s.is_deleted(*r));
                for r in rows {
                    if s.is_deleted(r) == restore {
                        s.toggle_delete(r);
                    }
                }
            })
        });
    }

    /// Opens the new-row dialog filled with the selected row's values.
    fn duplicate_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((row, _)) = self.pane.read(cx).selected() else {
            self.notice = Some(("Select a cell in the row first.".into(), true));
            cx.notify();
            return;
        };
        let values: Vec<Option<String>> = {
            let grid = self.pane.read(cx).grid().read(cx);
            let t = grid.delegate();
            (0..t.names().len())
                .map(|c| {
                    t.cell(row, c)
                        .filter(|v| !pgroove_core::query::is_clipped(v))
                        .map(str::to_string)
                })
                .collect()
        };
        self.notice = None;
        self.open_add_row(Some(values), window, cx);
    }

    fn discard(&mut self, cx: &mut Context<Self>) {
        self.notice = None;
        self.pane
            .update(cx, |p, cx| p.edit_session(cx, EditSession::discard));
    }

    fn open_add_row(
        &mut self,
        prefill: Option<Vec<Option<String>>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let names = self
            .pane
            .read(cx)
            .grid()
            .read(cx)
            .delegate()
            .names()
            .to_vec();
        let title = if prefill.is_some() {
            "Duplicate row"
        } else {
            "New row"
        };
        let inputs: Vec<Entity<InputState>> = names
            .iter()
            .enumerate()
            .map(|(i, _)| {
                let initial = prefill
                    .as_ref()
                    .and_then(|p| p.get(i).cloned().flatten())
                    .unwrap_or_default();
                cx.new(|cx| {
                    InputState::new(window, cx)
                        .placeholder("default")
                        .default_value(initial)
                })
            })
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
                .title(title)
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
        if self.identity == RowIdentity::None {
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
                bar.child(
                    div()
                        .text_color(theme.muted_foreground)
                        .child("Read-only mode blocks adding, changing and deleting rows."),
                )
                .child(
                    Button::new("allow-editing")
                        .outline()
                        .xsmall()
                        .icon(IconName::LockOpen)
                        .label("Allow editing")
                        .on_click(cx.listener(|_, _, _, cx| cx.emit(AllowEditing))),
                )
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
        let ctid_note = (self.identity == RowIdentity::Ctid).then(|| {
            div().text_color(theme.muted_foreground).child(
                "No primary key: rows are matched by position, and changed rows are refused.",
            )
        });
        Some(
            bar.child(
                Button::new("add-row")
                    .ghost()
                    .xsmall()
                    .icon(IconName::Plus)
                    .label("Add row")
                    .on_click(
                        cx.listener(|this, _, window, cx| this.open_add_row(None, window, cx)),
                    ),
            )
            .child(
                Button::new("duplicate-row")
                    .ghost()
                    .xsmall()
                    .icon(IconName::Copy)
                    .label("Duplicate row")
                    .tooltip("Start a new row from the selected row")
                    .on_click(
                        cx.listener(|this, _, window, cx| this.duplicate_selected(window, cx)),
                    ),
            )
            .child(
                Button::new("delete-row")
                    .ghost()
                    .xsmall()
                    .icon(IconName::Trash)
                    .label("Delete row")
                    .tooltip("Marks the selected row for deletion; select a cell in it first")
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
            .children(ctid_note)
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

    /// Uses another page size (from another tab's choice) and returns to the first page.
    pub(crate) fn apply_page_size(&mut self, size: u32, cx: &mut Context<Self>) {
        let size = pgroove_core::settings::clamp_row_limit(size);
        if size == self.page_size || self.pending(cx) > 0 {
            return;
        }
        self.page_size = size;
        self.page = 0;
        self.reload(cx);
    }

    fn pick_page_size(&mut self, size: u32, cx: &mut Context<Self>) {
        if self.blocked_by_pending(cx) {
            return;
        }
        self.apply_page_size(size, cx);
        cx.emit(PageSizeChanged(self.page_size));
    }

    /// Streams the whole table (with the current filter and sort) to a CSV file.
    /// While an export runs, clicking again cancels it.
    fn export_all(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.exporting {
            let client = self.client.clone();
            self.runtime.spawn(async move {
                if let Err(e) = data::cancel_running(&client).await {
                    log::warn!("cancel failed: {e:#}");
                }
            });
            return;
        }
        let sql = data::build_export_select(
            &self.table,
            self.sort.as_ref(),
            self.applied_filter.as_deref(),
        );
        let dir = std::env::current_dir().unwrap_or_default();
        let rx = cx.prompt_for_new_path(&dir, Some(&format!("{}.csv", self.table.name)));
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(path))) = rx.await else {
                return;
            };
            this.update_in(cx, |this, window, cx| {
                this.start_export(sql, path, window, cx)
            })
            .ok();
        })
        .detach();
    }

    fn start_export(
        &mut self,
        sql: String,
        path: std::path::PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.exporting = true;
        cx.notify();
        let client = self.client.clone();
        let dest = path.clone();
        let task = self
            .runtime
            .spawn(async move { pgroove_core::query::export_csv(&client, &sql, &dest).await });
        cx.spawn_in(window, async move |this, cx| {
            let result = match task.await {
                Ok(r) => r.map_err(|e| format!("{e:#}")),
                Err(e) => Err(e.to_string()),
            };
            this.update_in(cx, move |this, window, cx| {
                this.exporting = false;
                match result {
                    Ok(bytes) => window.push_notification(
                        format!(
                            "Exported {} to {}",
                            pgroove_core::structure::format_bytes(bytes),
                            path.display()
                        ),
                        cx,
                    ),
                    Err(e) => window.push_notification(
                        gpui_kit::component::notification::Notification::error(format!(
                            "Export failed: {e}"
                        )),
                        cx,
                    ),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn render_page_size_button(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let this = cx.entity();
        let current = self.page_size;
        Button::new("page-size")
            .ghost()
            .xsmall()
            .label(format!("{} rows/page", format_limit(current)))
            .tooltip("Rows per page")
            .dropdown_menu(move |menu, _, _| {
                pgroove_core::settings::ROW_LIMIT_PRESETS
                    .iter()
                    .fold(menu, |m, &n| {
                        let this = this.clone();
                        m.item(
                            PopupMenuItem::new(format!("{} rows", format_limit(n)))
                                .checked(n == current)
                                .on_click(move |_, _, cx| {
                                    this.update(cx, |t, cx| t.pick_page_size(n, cx))
                                }),
                        )
                    })
            })
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
        let mut req = PageRequest::page(self.page, u64::from(self.page_size));
        req.sort = self.sort.clone();
        req.filter = self.applied_filter.clone();
        let (client, table) = (self.client.clone(), self.table.clone());
        let task = self.runtime.spawn(async move {
            let identity = edit::row_identity(&client, &table)
                .await
                .unwrap_or(RowIdentity::None);
            req.row_ids = identity == RowIdentity::Ctid;
            let page = data::fetch_page(&client, &table, &req).await?;
            anyhow::Ok((page, identity))
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
                    Ok((mut page, identity)) => {
                        this.rows = page.data.rows.len();
                        this.notice = None;
                        this.has_more = page.has_more;
                        this.pane
                            .update(cx, |p, cx| p.set_data(Some(&page.data), cx));
                        // The grid keeps one copy of the rows; the edit session takes the
                        // original instead of a third copy, which matters for wide tables.
                        let rows = std::mem::take(&mut page.data.rows);
                        let columns = page.data.columns.clone();
                        let session = match &identity {
                            RowIdentity::PrimaryKey(pk) => EditSession::new(columns, pk, rows).ok(),
                            RowIdentity::Ctid => {
                                EditSession::with_row_ids(columns, rows, page.row_ids).ok()
                            }
                            RowIdentity::None => None,
                        };
                        this.identity = if session.is_some() {
                            identity
                        } else {
                            RowIdentity::None
                        };
                        this.pane.update(cx, |p, cx| p.set_session(session, cx));
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
        let editable = !cx.global::<ReadOnlyMode>().0 && self.identity != RowIdentity::None;
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
            _ => range_label(self.page, self.page_size, self.rows),
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
                        Button::new("export-all")
                            .ghost()
                            .xsmall()
                            .icon(if self.exporting {
                                IconName::X
                            } else {
                                IconName::FileDown
                            })
                            .label(if self.exporting {
                                "Cancel export"
                            } else {
                                "Export all…"
                            })
                            .tooltip(
                                "Stream every row of the table (with this filter and sort) to a CSV file",
                            )
                            .on_click(cx.listener(|this, _, window, cx| this.export_all(window, cx))),
                    )
                    .child(self.render_page_size_button(cx))
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
        assert_eq!(range_label(0, 200, 0), "No rows");
        assert_eq!(range_label(0, 200, 200), "1–200");
        assert_eq!(range_label(2, 200, 5), "401–405");
        assert_eq!(range_label(1, 1000, 1000), "1001–2000");
    }
}
