//! One SQL editor tab: editor, run/explain, results, history and snippets.

use std::sync::{Arc, Mutex};

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::highlighter::{Diagnostic, DiagnosticSeverity};
use gpui_kit::component::input::{Editor, EditorState, Input, InputState, RopeExt};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::*;
use gpui_kit::*;
use pgroove_core::explain::Plan;
use pgroove_core::history::{self, History, HistoryEntry, Snippets};
use pgroove_core::query::{self, QueryError, QueryOutput};
use tokio::runtime::Runtime;

use crate::grid_pane::GridPane;
use crate::sql_completion::{SharedIndex, SqlCompletions};

actions!(query_tab, [RunQuery, CancelQuery, FormatSql]);

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum RunState {
    Idle,
    Running,
    Failed(String),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum BottomTab {
    Results,
    Messages,
    Plan,
}

/// Failure of a run: display text plus the error span in the executed SQL.
pub struct RunFailure {
    pub text: String,
    pub span: Option<std::ops::Range<usize>>,
}

/// Splits a task result into success or a failure with the error's source span.
pub fn split_result<T>(
    r: Result<anyhow::Result<T>, tokio::task::JoinError>,
    sql: &str,
) -> Result<T, RunFailure> {
    match r {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(e)) => {
            let span = e.downcast_ref::<QueryError>().and_then(|q| q.span_in(sql));
            Err(RunFailure {
                text: format!("{e:#}"),
                span,
            })
        }
        Err(e) => Err(RunFailure {
            text: e.to_string(),
            span: None,
        }),
    }
}

/// First line of a statement, shortened for list display.
pub fn one_line(sql: &str, max: usize) -> String {
    let first = sql
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim();
    if first.chars().count() > max {
        format!("{}…", first.chars().take(max).collect::<String>())
    } else {
        first.to_string()
    }
}

pub struct QueryTab {
    runtime: Arc<Runtime>,
    client: Option<Arc<tokio_postgres::Client>>,
    editor: Entity<EditorState>,
    results: Entity<GridPane>,
    run: RunState,
    output: Option<QueryOutput>,
    bottom: BottomTab,
    plan: Option<Plan>,
    history: Arc<Mutex<History>>,
    snippets: Arc<Mutex<Snippets>>,
}

impl QueryTab {
    pub fn new(
        runtime: Arc<Runtime>,
        completions: SharedIndex,
        history: Arc<Mutex<History>>,
        snippets: Arc<Mutex<Snippets>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let editor = cx.new(|cx| {
            EditorState::new(window, cx)
                .language("sql".to_string())
                .line_number(true)
                .soft_wrap(false)
                .placeholder("select now();")
        });
        editor.update(cx, |e, _| {
            e.lsp_mut().completion_provider =
                Some(std::rc::Rc::new(SqlCompletions { index: completions }));
        });
        let results = cx.new(|cx| GridPane::new(false, window, cx));
        Self {
            runtime,
            client: None,
            editor,
            results,
            run: RunState::Idle,
            output: None,
            bottom: BottomTab::Results,
            plan: None,
            history,
            snippets,
        }
    }

    pub fn set_client(
        &mut self,
        client: Option<Arc<tokio_postgres::Client>>,
        cx: &mut Context<Self>,
    ) {
        self.client = client;
        cx.notify();
    }

    /// Appends text to the editor, separated by a blank line when it isn't empty.
    pub fn append(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        let current = self.editor.read(cx).value().to_string();
        let joined = if current.trim().is_empty() {
            text.to_string()
        } else {
            format!("{}\n\n{}", current.trim_end(), text)
        };
        self.editor
            .update(cx, |e, cx| e.set_value(joined, window, cx));
    }

    /// The selection, or the whole text, with the byte offset where it starts.
    fn sql_to_run(&self, cx: &App) -> (String, usize) {
        let e = self.editor.read(cx);
        let sel = e.selected_range();
        let selected = e.selected_text().to_string();
        if selected.trim().is_empty() {
            (e.value().to_string(), 0)
        } else {
            (selected, sel.start)
        }
    }

    fn clear_diagnostics(&mut self, cx: &mut Context<Self>) {
        self.editor.update(cx, |e, cx| {
            if let Some(set) = e.diagnostics_mut() {
                set.clear();
            }
            cx.notify();
        });
    }

    fn mark_error(&mut self, span: std::ops::Range<usize>, text: &str, cx: &mut Context<Self>) {
        let message = text.lines().next().unwrap_or("Error").to_string();
        self.editor.update(cx, |e, cx| {
            let rope = e.text().clone();
            let (start, end) = (span.start.min(rope.len()), span.end.min(rope.len()));
            if let Some(set) = e.diagnostics_mut() {
                set.clear();
                set.push(
                    Diagnostic::new(
                        rope.offset_to_position(start)..rope.offset_to_position(end),
                        message,
                    )
                    .with_severity(DiagnosticSeverity::Error),
                );
            }
            cx.notify();
        });
    }

    fn record(&mut self, sql: &str, millis: u64, ok: bool) {
        let entry = HistoryEntry {
            sql: sql.trim().to_string(),
            at: history::now_secs(),
            millis,
            ok,
        };
        if let Ok(mut h) = self.history.lock()
            && let Err(e) = h.push(entry)
        {
            log::warn!("could not save history: {e:#}");
        }
    }

    fn not_connected(&mut self, cx: &mut Context<Self>) {
        self.run = RunState::Failed("Connect to a database first.".into());
        self.bottom = BottomTab::Messages;
        cx.notify();
    }

    pub(crate) fn run_query(&mut self, cx: &mut Context<Self>) {
        if self.run == RunState::Running {
            return;
        }
        let Some(client) = self.client.clone() else {
            return self.not_connected(cx);
        };
        let (sql, base) = self.sql_to_run(cx);
        if sql.trim().is_empty() {
            return;
        }
        self.clear_diagnostics(cx);
        self.run = RunState::Running;
        cx.notify();
        let run_sql = sql.clone();
        let task = self
            .runtime
            .spawn(async move { query::run(&client, &run_sql, query::MAX_ROWS).await });
        cx.spawn(async move |this, cx| {
            let result = split_result(task.await, &sql);
            this.update(cx, |this, cx| this.finish_query(&sql, base, result, cx))
                .ok();
        })
        .detach();
    }

    fn finish_query(
        &mut self,
        sql: &str,
        base: usize,
        result: Result<QueryOutput, RunFailure>,
        cx: &mut Context<Self>,
    ) {
        match result {
            Ok(out) => {
                self.record(sql, out.elapsed.as_millis() as u64, true);
                self.run = RunState::Idle;
                let set = out.last_rowset().cloned();
                self.bottom = if set.is_some() {
                    BottomTab::Results
                } else {
                    BottomTab::Messages
                };
                self.results
                    .update(cx, |p, cx| p.set_data(set.as_ref(), cx));
                self.output = Some(out);
            }
            Err(f) => {
                self.record(sql, 0, false);
                if let Some(span) = &f.span {
                    self.mark_error(span.start + base..span.end + base, &f.text, cx);
                }
                self.run = RunState::Failed(f.text);
                self.bottom = BottomTab::Messages;
            }
        }
        cx.notify();
    }

    fn cancel_query(&mut self, cx: &mut Context<Self>) {
        if self.run != RunState::Running {
            return;
        }
        if let Some(client) = self.client.clone() {
            self.runtime.spawn(async move {
                if let Err(e) = query::cancel(&client).await {
                    log::warn!("cancel failed: {e:#}");
                }
            });
        }
        cx.notify();
    }

    pub(crate) fn format_sql(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.editor.read(cx).value().to_string();
        if text.trim().is_empty() {
            return;
        }
        let formatted = pgroove_core::format::format_sql(&text);
        self.editor
            .update(cx, |e, cx| e.set_value(formatted, window, cx));
    }

    pub(crate) fn explain(&mut self, analyze: bool, cx: &mut Context<Self>) {
        if self.run == RunState::Running {
            return;
        }
        let Some(client) = self.client.clone() else {
            return self.not_connected(cx);
        };
        let (sql, _) = self.sql_to_run(cx);
        self.run = RunState::Running;
        cx.notify();
        let run_sql = sql.clone();
        let task = self
            .runtime
            .spawn(async move { pgroove_core::explain::run(&client, &run_sql, analyze).await });
        cx.spawn(async move |this, cx| {
            let result = split_result(task.await, &sql);
            this.update(cx, |this, cx| {
                match result {
                    Ok(plan) => {
                        this.run = RunState::Idle;
                        this.plan = Some(plan);
                        this.bottom = BottomTab::Plan;
                    }
                    Err(f) => {
                        this.run = RunState::Failed(f.text);
                        this.bottom = BottomTab::Messages;
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub(crate) fn open_history(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let this = cx.entity();
        let history = self.history.clone();
        window.open_dialog(cx, move |dialog, _, cx| {
            let theme = cx.theme().clone();
            let now = history::now_secs();
            let entries: Vec<HistoryEntry> = history
                .lock()
                .map(|h| h.recent().take(100).cloned().collect())
                .unwrap_or_default();
            let rows = entries.into_iter().enumerate().map(|(i, e)| {
                let (t, sql) = (this.clone(), e.sql.clone());
                v_flex()
                    .id(("history", i))
                    .px_2()
                    .py_1()
                    .cursor_pointer()
                    .hover(|d| d.bg(theme.list_hover))
                    .child(
                        div()
                            .text_sm()
                            .font_family(theme.mono_font_family.clone())
                            .child(one_line(&e.sql, 90)),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(if e.ok {
                                theme.muted_foreground
                            } else {
                                theme.danger
                            })
                            .child(format!(
                                "{} · {} ms{}",
                                history::relative_time(e.at, now),
                                e.millis,
                                if e.ok { "" } else { " · failed" }
                            )),
                    )
                    .on_click(move |_, window, cx| {
                        t.update(cx, |t, cx| t.append(&sql, window, cx));
                        window.close_dialog(cx);
                    })
            });
            let clear_history = history.clone();
            dialog
                .title("History")
                .w(px(640.))
                .child(
                    v_flex()
                        .id("history-list")
                        .max_h(px(420.))
                        .overflow_y_scroll()
                        .children(rows)
                        .child(
                            h_flex().justify_end().pt_2().child(
                                Button::new("clear-history")
                                    .ghost()
                                    .xsmall()
                                    .label("Clear history")
                                    .on_click(move |_, window, _| {
                                        if let Ok(mut h) = clear_history.lock() {
                                            let _ = h.clear();
                                        }
                                        window.refresh();
                                    }),
                            ),
                        ),
                )
                .on_ok(|_, _, _| true)
        });
    }

    pub(crate) fn open_snippets(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let this = cx.entity();
        let snippets = self.snippets.clone();
        let name = cx.new(|cx| InputState::new(window, cx).placeholder("Snippet name"));
        window.open_dialog(cx, move |dialog, _, cx| {
            let theme = cx.theme().clone();
            let items: Vec<_> = snippets
                .lock()
                .map(|s| s.items().to_vec())
                .unwrap_or_default();
            let rows = items.into_iter().enumerate().map(|(i, s)| {
                let (t, sql, del, name_del) = (
                    this.clone(),
                    s.sql.clone(),
                    snippets.clone(),
                    s.name.clone(),
                );
                h_flex()
                    .px_2()
                    .py_1()
                    .gap_2()
                    .items_center()
                    .hover(|d| d.bg(theme.list_hover))
                    .child(
                        v_flex()
                            .id(("snippet", i))
                            .flex_1()
                            .min_w_0()
                            .cursor_pointer()
                            .child(div().text_sm().child(s.name.clone()))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .font_family(theme.mono_font_family.clone())
                                    .child(one_line(&s.sql, 80)),
                            )
                            .on_click(move |_, window, cx| {
                                t.update(cx, |t, cx| t.append(&sql, window, cx));
                                window.close_dialog(cx);
                            }),
                    )
                    .child(
                        Button::new(("del-snippet", i))
                            .ghost()
                            .xsmall()
                            .icon(IconName::Trash)
                            .tooltip("Delete snippet")
                            .on_click(move |_, window, _| {
                                if let Ok(mut s) = del.lock() {
                                    let _ = s.remove(&name_del);
                                }
                                window.refresh();
                            }),
                    )
            });
            let (save_this, save_snips, save_name) = (this.clone(), snippets.clone(), name.clone());
            dialog
                .title("Snippets")
                .w(px(560.))
                .child(
                    v_flex()
                        .gap_2()
                        .child(
                            h_flex()
                                .gap_2()
                                .child(div().flex_1().child(Input::new(&name)))
                                .child(
                                    Button::new("save-snippet")
                                        .outline()
                                        .small()
                                        .label("Save current SQL")
                                        .on_click(move |_, window, cx| {
                                            let sql = save_this
                                                .read(cx)
                                                .editor
                                                .read(cx)
                                                .value()
                                                .to_string();
                                            let n = save_name.read(cx).value().to_string();
                                            let res = save_snips
                                                .lock()
                                                .map_err(|_| anyhow::anyhow!("snippets are locked"))
                                                .and_then(|mut s| s.save(&n, &sql));
                                            match res {
                                                Ok(()) => window.refresh(),
                                                Err(e) => window.push_notification(
                                                    Notification::error(format!("{e:#}")),
                                                    cx,
                                                ),
                                            }
                                        }),
                                ),
                        )
                        .child(
                            v_flex()
                                .id("snippet-list")
                                .max_h(px(320.))
                                .overflow_y_scroll()
                                .children(rows),
                        ),
                )
                .on_ok(|_, _, _| true)
        });
    }

    fn render_results_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let tab = |id: &'static str, label: &'static str, which: BottomTab| {
            let on = self.bottom == which;
            div()
                .id(id)
                .cursor_pointer()
                .text_color(if on {
                    theme.foreground
                } else {
                    theme.muted_foreground
                })
                .child(label)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.bottom = which;
                    cx.notify();
                }))
        };
        let summary = match (&self.run, &self.output) {
            (RunState::Running, _) => "Running…".to_string(),
            (_, Some(o)) => {
                let n = o.last_rowset().map(|s| s.count).unwrap_or(0);
                format!("{n} rows · {} ms", o.elapsed.as_millis())
            }
            _ => String::new(),
        };
        h_flex()
            .gap_4()
            .px_3()
            .h(px(34.))
            .items_center()
            .text_xs()
            .border_b_1()
            .border_color(theme.border)
            .child(tab("tab-results", "Results", BottomTab::Results))
            .child(tab("tab-messages", "Messages", BottomTab::Messages))
            .child(tab("tab-plan", "Plan", BottomTab::Plan))
            .child(div().flex_1())
            .child(div().text_color(theme.muted_foreground).child(summary))
            .child(self.results.read(cx).actions())
    }

    fn render_results_body(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let note = |text: String, color: Hsla| {
            div()
                .flex_1()
                .p_3()
                .text_sm()
                .text_color(color)
                .child(text)
                .into_any_element()
        };
        if self.bottom == BottomTab::Plan {
            return crate::plan_view::render(self.plan.as_ref(), &theme);
        }
        if self.bottom == BottomTab::Messages {
            if let RunState::Failed(e) = &self.run {
                return note(e.clone(), theme.danger);
            }
            let text = self
                .output
                .as_ref()
                .map(|o| o.messages().join("\n"))
                .unwrap_or_else(|| "No messages.".into());
            return note(text, theme.foreground);
        }
        if self.output.as_ref().and_then(|o| o.last_rowset()).is_some() {
            div()
                .flex_1()
                .min_h_0()
                .child(self.results.clone())
                .into_any_element()
        } else {
            div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child("Run a query to see results.")
                .into_any_element()
        }
    }
}

impl Render for QueryTab {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let strip = || {
            h_flex()
                .gap_2()
                .px_3()
                .h(px(34.))
                .items_center()
                .border_b_1()
                .border_color(theme.border)
        };
        v_flex()
            .size_full()
            .key_context("QueryTab")
            .on_action(cx.listener(|this, _: &RunQuery, _, cx| this.run_query(cx)))
            .on_action(cx.listener(|this, _: &CancelQuery, _, cx| this.cancel_query(cx)))
            .on_action(cx.listener(|this, _: &FormatSql, window, cx| this.format_sql(window, cx)))
            .child(
                strip()
                    .child(
                        Button::new("run")
                            .outline()
                            .xsmall()
                            .icon(IconName::Play)
                            .label("Run · Ctrl+Enter")
                            .loading(self.run == RunState::Running)
                            .on_click(cx.listener(|this, _, _, cx| this.run_query(cx))),
                    )
                    .child(
                        Button::new("explain")
                            .outline()
                            .xsmall()
                            .label("Explain")
                            .tooltip("Estimated plan; does not run the statement")
                            .on_click(cx.listener(|this, _, _, cx| this.explain(false, cx))),
                    )
                    .child(
                        Button::new("analyze")
                            .outline()
                            .xsmall()
                            .label("Analyze")
                            .tooltip("Runs the statement in a transaction that is rolled back")
                            .on_click(cx.listener(|this, _, _, cx| this.explain(true, cx))),
                    )
                    .child(
                        Button::new("format")
                            .ghost()
                            .xsmall()
                            .label("Format")
                            .tooltip("Format SQL (Ctrl+Shift+F)")
                            .on_click(
                                cx.listener(|this, _, window, cx| this.format_sql(window, cx)),
                            ),
                    )
                    .child(
                        Button::new("cancel")
                            .ghost()
                            .xsmall()
                            .label("Cancel")
                            .tooltip("Cancel the running statement (Esc)")
                            .on_click(cx.listener(|this, _, _, cx| this.cancel_query(cx))),
                    )
                    .child(div().flex_1())
                    .child(
                        Button::new("history")
                            .ghost()
                            .xsmall()
                            .icon(IconName::Clock)
                            .label("History")
                            .on_click(
                                cx.listener(|this, _, window, cx| this.open_history(window, cx)),
                            ),
                    )
                    .child(
                        Button::new("snippets")
                            .ghost()
                            .xsmall()
                            .icon(IconName::Bookmark)
                            .label("Snippets")
                            .on_click(
                                cx.listener(|this, _, window, cx| this.open_snippets(window, cx)),
                            ),
                    ),
            )
            .child(
                div()
                    .h(px(170.))
                    .flex_shrink_0()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(
                        Editor::new(&self.editor)
                            .bordered(false)
                            .p_0()
                            .h_full()
                            .font_family(theme.mono_font_family.clone())
                            .text_size(theme.mono_font_size),
                    ),
            )
            .child(self.render_results_header(cx))
            .child(self.render_results_body(cx))
    }
}

#[cfg(test)]
mod tests {
    use super::{one_line, split_result};
    use pgroove_core::query::QueryError;

    #[test]
    fn one_line_shortens() {
        assert_eq!(one_line("\n  select 1\nfrom t", 50), "select 1");
        assert_eq!(one_line("select 12345", 6), "select…");
        assert_eq!(one_line("", 5), "");
    }

    #[test]
    fn split_result_carries_span() {
        let ok: Result<anyhow::Result<u8>, tokio::task::JoinError> = Ok(Ok(1));
        assert_eq!(split_result(ok, "x").ok(), Some(1));
        let err = anyhow::Error::new(QueryError {
            message: "boom".into(),
            position: Some(8),
        });
        let r: Result<anyhow::Result<u8>, tokio::task::JoinError> = Ok(Err(err));
        let f = split_result(r, "select foo from t").err().unwrap();
        assert_eq!(f.text, "boom");
        assert_eq!(f.span, Some(7..10));
        let plain: Result<anyhow::Result<u8>, tokio::task::JoinError> =
            Ok(Err(anyhow::anyhow!("plain")));
        assert!(split_result(plain, "x").err().unwrap().span.is_none());
    }
}
