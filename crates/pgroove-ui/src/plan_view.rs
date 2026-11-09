//! Rendering of an EXPLAIN plan tree.

use gpui_kit::component::{Theme, h_flex, v_flex};
use gpui_kit::*;
use pgroove_core::explain::{FlatNode, Plan, PlanNode};

/// One-line numbers for a node, e.g. `cost 60.0 · rows 100 → 100 · 40.0 ms × 3`.
pub fn detail(n: &PlanNode) -> String {
    let mut parts = vec![format!("cost {:.1}", n.total_cost)];
    match n.actual_rows {
        Some(a) => parts.push(format!("rows {:.0} → {:.0}", n.plan_rows, a)),
        None => parts.push(format!("rows ~{:.0}", n.plan_rows)),
    }
    if let Some(ms) = n.exclusive_ms() {
        let mut t = format!("{ms:.2} ms self");
        if n.loops > 1. {
            t.push_str(&format!(" × {:.0} loops", n.loops));
        }
        parts.push(t);
    }
    if let (Some(h), Some(r)) = (n.shared_hit, n.shared_read) {
        parts.push(format!("buffers hit {h} read {r}"));
    }
    parts.join(" · ")
}

fn row(f: &FlatNode, theme: &Theme) -> Div {
    let n = f.node;
    let bar = (f.share.clamp(0., 1.) * 120.) as f32;
    let mut line = h_flex()
        .gap_3()
        .px_3()
        .py_1()
        .items_center()
        .child(div().w(px(f.depth as f32 * 16.)).flex_shrink_0())
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .child(div().text_sm().child(n.title()))
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(detail(n)),
                )
                .children(n.filter.as_ref().map(|f| {
                    div()
                        .text_xs()
                        .font_family(theme.mono_font_family.clone())
                        .text_color(theme.muted_foreground)
                        .child(format!("filter {f}"))
                })),
        )
        .child(
            h_flex()
                .w(px(170.))
                .flex_shrink_0()
                .gap_2()
                .items_center()
                .child(
                    div()
                        .w(px(120.))
                        .h(px(6.))
                        .rounded(px(3.))
                        .bg(theme.muted)
                        .child(div().w(px(bar)).h_full().rounded(px(3.)).bg(if f.hot {
                            theme.warning
                        } else {
                            theme.primary
                        })),
                )
                .child(div().text_xs().child(format!("{:.0}%", f.share * 100.))),
        );
    if f.hot {
        line = line.bg(theme.warning.opacity(0.12));
    }
    line
}

pub fn render(plan: Option<&Plan>, theme: &Theme) -> AnyElement {
    let Some(plan) = plan else {
        return div()
            .flex_1()
            .flex()
            .items_center()
            .justify_center()
            .text_sm()
            .text_color(theme.muted_foreground)
            .child("Use Explain or Analyze to see a query plan.")
            .into_any_element();
    };
    let header = if plan.analyzed {
        format!(
            "Planning {:.2} ms · execution {:.2} ms · share is time spent in the node itself",
            plan.planning_ms.unwrap_or(0.),
            plan.execution_ms.unwrap_or(0.)
        )
    } else {
        "Estimated costs only. Analyze runs the statement in a rolled-back transaction. Share is cost in the node itself."
            .to_string()
    };
    v_flex()
        .id("plan-scroll")
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .child(
            div()
                .px_3()
                .py_1()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(header),
        )
        .children(plan.flatten().iter().map(|f| row(f, theme)))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::detail;
    use pgroove_core::explain::parse;

    #[test]
    fn detail_lines() {
        let p = parse(
            r#"[{"Plan":{"Node Type":"Seq Scan","Relation Name":"t","Total Cost":10.0,
            "Plan Rows":5,"Actual Rows":7,"Actual Total Time":2.0,"Actual Loops":3,
            "Shared Hit Blocks":4,"Shared Read Blocks":1}}]"#,
        )
        .unwrap();
        assert_eq!(
            detail(&p.root),
            "cost 10.0 · rows 5 → 7 · 6.00 ms self × 3 loops · buffers hit 4 read 1"
        );
        let plain =
            parse(r#"[{"Plan":{"Node Type":"Result","Total Cost":0.01,"Plan Rows":1}}]"#).unwrap();
        assert_eq!(detail(&plain.root), "cost 0.0 · rows ~1");
    }
}
