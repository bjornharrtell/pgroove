//! Structure tab content: columns, constraints, indexes, triggers, size and DDL.

use std::sync::Arc;

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use pgroove_core::data::TableRef;
use pgroove_core::structure::{self, Structure};
use tokio::runtime::Runtime;

/// Emitted when the user follows a foreign key.
pub struct OpenTable(pub TableRef);

enum Load {
    Loading,
    Ready(Box<Structure>),
    Failed(String),
}

pub struct StructureView {
    table: TableRef,
    client: Arc<pgroove_core::Client>,
    runtime: Arc<Runtime>,
    load: Load,
    generation: u64,
}

impl EventEmitter<OpenTable> for StructureView {}

impl StructureView {
    pub fn new(
        table: TableRef,
        client: Arc<pgroove_core::Client>,
        runtime: Arc<Runtime>,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut v = Self {
            table,
            client,
            runtime,
            load: Load::Loading,
            generation: 0,
        };
        v.reload(cx);
        v
    }

    pub fn reload(&mut self, cx: &mut Context<Self>) {
        self.generation += 1;
        let generation = self.generation;
        self.load = Load::Loading;
        cx.notify();
        let (client, table) = (self.client.clone(), self.table.clone());
        let task = self
            .runtime
            .spawn(async move { structure::load(&client, &table).await });
        cx.spawn(async move |this, cx| {
            let result = match task.await {
                Ok(r) => r.map_err(|e| format!("{e:#}")),
                Err(e) => Err(e.to_string()),
            };
            this.update(cx, |this, cx| {
                if this.generation == generation {
                    this.load = match result {
                        Ok(s) => Load::Ready(Box::new(s)),
                        Err(e) => Load::Failed(e),
                    };
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }
}

fn section(title: &str, count: usize, border: Hsla, muted: Hsla) -> Div {
    v_flex().gap_1().child(
        h_flex()
            .gap_2()
            .pb_1()
            .border_b_1()
            .border_color(border)
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .child(title.to_string()),
            )
            .child(div().text_xs().text_color(muted).child(count.to_string())),
    )
}

/// Collapses line breaks and runs of whitespace so text fits on one line.
pub fn single_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A one-line cell: the text never wraps, overflow is clipped with an ellipsis, and the
/// full original text shows in a tooltip. `width` fixes the cell width; `None` fills the row.
fn clip(id: impl Into<ElementId>, width: Option<f32>, text: impl AsRef<str>) -> Stateful<Div> {
    let full: SharedString = text.as_ref().to_string().into();
    let cell = div()
        .id(id)
        .min_w_0()
        .truncate()
        .whitespace_nowrap()
        .child(single_line(text.as_ref()));
    let cell = match width {
        Some(w) => cell.w(px(w)).flex_shrink_0(),
        None => cell.flex_1(),
    };
    if full.trim().is_empty() {
        cell
    } else {
        cell.tooltip(move |window, cx| Tooltip::new(full.clone()).build(window, cx))
    }
}

impl Render for StructureView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let (border, muted) = (theme.border, theme.muted_foreground);
        let st = match &self.load {
            Load::Loading => {
                return div()
                    .p_3()
                    .text_sm()
                    .text_color(muted)
                    .child("Loading…")
                    .into_any_element();
            }
            Load::Failed(e) => {
                return div()
                    .p_3()
                    .text_sm()
                    .text_color(theme.danger)
                    .child(e.clone())
                    .into_any_element();
            }
            Load::Ready(s) => s,
        };
        let mono = theme.mono_font_family.clone();

        let summary = format!(
            "{} · {} total (table {}, indexes {}) · {} rows",
            st.kind.label(),
            structure::format_bytes(st.stats.total_bytes),
            structure::format_bytes(st.stats.table_bytes),
            structure::format_bytes(st.stats.index_bytes),
            structure::format_rows(st.stats.estimated_rows),
        );

        let columns = st.columns.iter().enumerate().fold(
            section("Columns", st.columns.len(), border, muted),
            |sec, (i, c)| {
                let mut flags = Vec::new();
                if c.not_null {
                    flags.push("not null");
                }
                match c.identity {
                    Some('a') => flags.push("identity always"),
                    Some(_) => flags.push("identity"),
                    None => {}
                }
                if c.generated {
                    flags.push("generated");
                }
                sec.child(
                    h_flex()
                        .gap_3()
                        .text_sm()
                        .child(clip(("col-name", i), Some(180.), &c.name))
                        .child(clip(("col-type", i), Some(160.), &c.data_type).text_color(muted))
                        .child(
                            clip(("col-flags", i), Some(150.), flags.join(", ")).text_color(muted),
                        )
                        .child(
                            clip(
                                ("col-default", i),
                                Some(220.),
                                c.default
                                    .clone()
                                    .filter(|_| !c.generated)
                                    .unwrap_or_default(),
                            )
                            .font_family(mono.clone()),
                        )
                        .child(
                            clip(
                                ("col-comment", i),
                                None,
                                c.comment.clone().unwrap_or_default(),
                            )
                            .text_color(muted),
                        ),
                )
            },
        );

        let constraints = st.constraints.iter().enumerate().fold(
            section("Constraints", st.constraints.len(), border, muted),
            |sec, (i, c)| {
                let link = c.references.clone().map(|target| {
                    Button::new(("fk", i))
                        .ghost()
                        .xsmall()
                        .icon(IconName::ArrowRight)
                        .label(format!("{}.{}", target.schema, target.name))
                        .tooltip("Open referenced table")
                        .on_click(
                            cx.listener(move |_, _, _, cx| cx.emit(OpenTable(target.clone()))),
                        )
                });
                sec.child(
                    h_flex()
                        .gap_3()
                        .items_center()
                        .text_sm()
                        .child(clip(("con-kind", i), Some(100.), c.kind_label()).text_color(muted))
                        .child(clip(("con-name", i), Some(200.), &c.name))
                        .child(clip(("con-def", i), None, &c.definition).font_family(mono.clone()))
                        .children(link),
                )
            },
        );

        let indexes = st.indexes.iter().enumerate().fold(
            section("Indexes", st.indexes.len(), border, muted),
            |sec, (n, i)| {
                let tag = if i.primary {
                    "primary"
                } else if i.unique {
                    "unique"
                } else {
                    ""
                };
                sec.child(
                    h_flex()
                        .gap_3()
                        .text_sm()
                        .child(clip(("idx-name", n), Some(200.), &i.name))
                        .child(clip(("idx-tag", n), Some(70.), tag).text_color(muted))
                        .child(
                            clip(("idx-def", n), None, &i.definition)
                                .font_family(mono.clone())
                                .text_color(muted),
                        ),
                )
            },
        );

        let triggers = st.triggers.iter().enumerate().fold(
            section("Triggers", st.triggers.len(), border, muted),
            |sec, (n, t)| {
                sec.child(
                    h_flex()
                        .gap_3()
                        .text_sm()
                        .child(clip(("trg-name", n), Some(200.), &t.name))
                        .child(
                            clip(("trg-def", n), None, &t.definition)
                                .font_family(mono.clone())
                                .text_color(muted),
                        ),
                )
            },
        );

        let ddl = structure::ddl(&self.table, st);
        let ddl_copy = ddl.clone();
        let is_table = st.view_definition.is_none();

        v_flex()
            .id("structure-scroll")
            .size_full()
            .overflow_y_scroll()
            .p_3()
            .gap_4()
            .child(div().text_xs().text_color(muted).child(summary))
            .when(is_table, |d| {
                d.child(columns)
                    .child(constraints)
                    .child(indexes)
                    .child(triggers)
            })
            .child(
                section("DDL", 0, border, muted)
                    .child(
                        h_flex().justify_end().child(
                            Button::new("copy-ddl")
                                .ghost()
                                .xsmall()
                                .icon(IconName::Copy)
                                .label("Copy")
                                .on_click(move |_, window, cx| {
                                    cx.write_to_clipboard(ClipboardItem::new_string(
                                        ddl_copy.clone(),
                                    ));
                                    window.push_notification("Copied DDL", cx);
                                }),
                        ),
                    )
                    .child(
                        div()
                            .font_family(mono)
                            .text_sm()
                            .whitespace_normal()
                            .child(ddl),
                    ),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::single_line;

    #[test]
    fn comments_collapse_to_one_line() {
        assert_eq!(
            single_line("first line\nsecond\r\n  third\tfourth"),
            "first line second third fourth"
        );
        assert_eq!(single_line("  padded  "), "padded");
        assert_eq!(single_line("\n\n"), "");
        assert_eq!(single_line("plain"), "plain");
    }
}
