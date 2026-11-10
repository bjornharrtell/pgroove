//! Server monitoring tab: sessions, locks, statements, sizes, replication, settings.

use std::sync::Arc;
use std::time::Duration;

use crate::dialogs::DialogActions;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::*;
use gpui_kit::*;
use pgroove_core::monitor::{self, View};
use pgroove_core::query::ResultSet;
use tokio::runtime::Runtime;

use crate::grid_pane::GridPane;

const REFRESH: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
    Cancel,
    Terminate,
}

impl Action {
    pub fn title(self, pid: i32) -> String {
        match self {
            Action::Cancel => format!("Cancel the running query of session {pid}?"),
            Action::Terminate => format!("Terminate session {pid}? Its connection is closed."),
        }
    }

    pub fn done_text(self, pid: i32, sent: bool) -> String {
        match (self, sent) {
            (Action::Cancel, true) => format!("Cancel signal sent to {pid}."),
            (Action::Terminate, true) => format!("Terminated session {pid}."),
            (_, false) => format!("Session {pid} was not found or you lack permission."),
        }
    }
}

enum Load {
    Loading,
    Ready,
    Failed(String),
}

pub struct ActivityView {
    client: Arc<pgroove_core::Client>,
    runtime: Arc<Runtime>,
    view: View,
    pane: Entity<GridPane>,
    data: Arc<ResultSet>,
    load: Load,
    auto: bool,
    notice: Option<(String, bool)>,
    generation: u64,
    _poll: Task<()>,
}

impl ActivityView {
    pub fn new(
        client: Arc<pgroove_core::Client>,
        runtime: Arc<Runtime>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let pane = cx.new(|cx| GridPane::new(false, window, cx));
        let poll = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(REFRESH).await;
                let alive = this.update(cx, |t, cx| {
                    if t.auto && !matches!(t.load, Load::Loading) {
                        t.refresh(cx);
                    }
                });
                if alive.is_err() {
                    break;
                }
            }
        });
        let mut v = Self {
            client,
            runtime,
            view: View::Sessions,
            pane,
            data: Arc::default(),
            load: Load::Loading,
            auto: true,
            notice: None,
            generation: 0,
            _poll: poll,
        };
        v.refresh(cx);
        v
    }

    fn set_view(&mut self, view: View, cx: &mut Context<Self>) {
        if self.view != view {
            self.view = view;
            self.notice = None;
            self.data = Arc::default();
            self.pane.update(cx, |p, cx| p.set_data(None, cx));
            self.refresh(cx);
        }
    }

    /// The pid of the selected row, if the view supports session actions.
    fn selected_pid(&self, cx: &App) -> Option<i32> {
        let (row, _) = self.pane.read(cx).selected()?;
        monitor::row_pid(self.view, &self.data, row)
    }

    /// Points the monitor at another database's connection and reloads.
    pub(crate) fn rebind(&mut self, client: Arc<pgroove_core::Client>, cx: &mut Context<Self>) {
        self.client = client;
        self.data = Arc::default();
        self.notice = None;
        self.load = Load::Loading;
        self.pane.update(cx, |p, cx| p.set_data(None, cx));
        self.refresh(cx);
    }

    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        self.generation += 1;
        let generation = self.generation;
        let keep = self.selected_pid(cx);
        let view = self.view;
        if matches!(self.load, Load::Failed(_)) {
            self.load = Load::Loading;
        }
        let client = self.client.clone();
        let task = self
            .runtime
            .spawn(async move { monitor::fetch(&client, view).await });
        cx.spawn(async move |this, cx| {
            let result = match task.await {
                Ok(r) => r.map_err(|e| format!("{e:#}")),
                Err(e) => Err(e.to_string()),
            };
            this.update(cx, |this, cx| {
                if this.generation != generation || this.view != view {
                    return;
                }
                match result {
                    Ok(set) => {
                        this.pane
                            .update(cx, |p, cx| p.set_data(Some(set.clone()), cx));
                        this.data = set;
                        this.load = Load::Ready;
                        this.reselect(keep, cx);
                    }
                    Err(e) => this.load = Load::Failed(e),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// Restores the selection to the row with the same pid after a refresh.
    fn reselect(&mut self, pid: Option<i32>, cx: &mut Context<Self>) {
        let Some(pid) = pid else { return };
        let row = (0..self.data.rows.len())
            .find(|&r| monitor::row_pid(self.view, &self.data, r) == Some(pid));
        if let Some(row) = row {
            let grid = self.pane.read(cx).grid().clone();
            grid.update(cx, |g, cx| g.set_selected_cell(row, 0, cx));
        }
    }

    fn confirm(&mut self, action: Action, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pid) = self.selected_pid(cx) else {
            self.notice = Some(("Select a session first.".into(), true));
            cx.notify();
            return;
        };
        let this = cx.entity();
        window.open_dialog(cx, move |dialog, _, _| {
            let this = this.clone();
            dialog
                .title(action.title(pid))
                .w(px(460.))
                .actions("Confirm", true, move |_, _, cx| {
                    this.update(cx, |t, cx| t.run_action(action, pid, cx));
                    true
                })
        });
    }

    fn run_action(&mut self, action: Action, pid: i32, cx: &mut Context<Self>) {
        let client = self.client.clone();
        let task = self.runtime.spawn(async move {
            match action {
                Action::Cancel => monitor::cancel_backend(&client, pid).await,
                Action::Terminate => monitor::terminate_backend(&client, pid).await,
            }
        });
        cx.spawn(async move |this, cx| {
            let result = match task.await {
                Ok(r) => r.map_err(|e| format!("{e:#}")),
                Err(e) => Err(e.to_string()),
            };
            this.update(cx, |this, cx| {
                this.notice = Some(match result {
                    Ok(sent) => (action.done_text(pid, sent), !sent),
                    Err(e) => (format!("Failed: {e}"), true),
                });
                this.refresh(cx);
            })
            .ok();
        })
        .detach();
    }
}

impl Render for ActivityView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let current = self.view;
        let tabs = View::ALL.iter().map(|&v| {
            let b = Button::new(("view", v as usize)).xsmall().label(v.label());
            let b = if v == current { b.outline() } else { b.ghost() };
            b.on_click(cx.listener(move |this, _, _, cx| this.set_view(v, cx)))
        });
        let bar = h_flex()
            .gap_1()
            .px_3()
            .h(px(34.))
            .items_center()
            .border_b_1()
            .border_color(theme.border)
            .children(tabs)
            .child(div().flex_1())
            .child({
                let b = Button::new("auto")
                    .xsmall()
                    .icon(IconName::Timer)
                    .label("Auto-refresh")
                    .tooltip("Refresh every 5 seconds");
                let b = if self.auto { b.outline() } else { b.ghost() };
                b.on_click(cx.listener(|this, _, _, cx| {
                    this.auto = !this.auto;
                    cx.notify();
                }))
            })
            .child(
                Button::new("refresh")
                    .ghost()
                    .xsmall()
                    .icon(IconName::RefreshCw)
                    .tooltip("Refresh now")
                    .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
            );
        let actions = current.pid_column().map(|_| {
            h_flex()
                .gap_2()
                .px_3()
                .h(px(32.))
                .items_center()
                .border_b_1()
                .border_color(theme.border)
                .text_xs()
                .child(
                    Button::new("cancel-query")
                        .outline()
                        .xsmall()
                        .label("Cancel query")
                        .tooltip("Select a row first")
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.confirm(Action::Cancel, window, cx)
                        })),
                )
                .child(
                    Button::new("terminate")
                        .outline()
                        .xsmall()
                        .label("Terminate session")
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.confirm(Action::Terminate, window, cx)
                        })),
                )
                .children(self.notice.clone().map(|(text, err)| {
                    div()
                        .text_color(if err {
                            theme.danger
                        } else {
                            theme.muted_foreground
                        })
                        .child(text)
                }))
        });
        let body = match &self.load {
            Load::Failed(e) => div()
                .p_3()
                .text_sm()
                .text_color(theme.danger)
                .child(e.clone())
                .into_any_element(),
            Load::Loading if self.data.columns.is_empty() => div()
                .p_3()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child("Loading…")
                .into_any_element(),
            _ => div()
                .flex_1()
                .min_h_0()
                .child(self.pane.clone())
                .into_any_element(),
        };
        v_flex()
            .size_full()
            .child(bar)
            .children(actions)
            .child(body)
    }
}

#[cfg(test)]
mod tests {
    use super::Action;

    #[test]
    fn action_texts() {
        assert!(Action::Cancel.title(5).contains("session 5"));
        assert!(Action::Terminate.title(5).contains("Terminate session 5"));
        assert_eq!(
            Action::Cancel.done_text(5, true),
            "Cancel signal sent to 5."
        );
        assert_eq!(
            Action::Terminate.done_text(5, true),
            "Terminated session 5."
        );
        assert!(Action::Cancel.done_text(5, false).contains("not found"));
    }
}
