use gpui_kit::component::*;
use gpui_kit::*;

/// Top-level window view: sidebar, tabbed center, status bar.
pub struct Workspace;

impl Render for Workspace {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        div()
            .v_flex()
            .size_full()
            .child(
                div()
                    .h_flex()
                    .flex_1()
                    .child(
                        div()
                            .w(px(240.))
                            .h_full()
                            .border_r_1()
                            .border_color(theme.border)
                            .p_2()
                            .child("Connections"),
                    )
                    .child(div().flex_1().p_2().child("No connection open")),
            )
            .child(
                div()
                    .h_flex()
                    .px_2()
                    .py_1()
                    .border_t_1()
                    .border_color(theme.border)
                    .child("Not connected"),
            )
    }
}
