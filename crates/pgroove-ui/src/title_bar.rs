//! Application title bar: drag area, content, and a subtle close button.

use gpui_kit::assets::IconName;
use gpui_kit::component::{
    ActiveTheme, Icon, InteractiveElementExt as _, h_flex, window_corner_radii,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

pub const HEIGHT: f32 = 36.;

/// Left padding of the bar: on macOS the native window buttons ("traffic lights") sit there.
#[cfg(target_os = "macos")]
const LEFT_PADDING: f32 = 80.;
#[cfg(not(target_os = "macos"))]
const LEFT_PADDING: f32 = 12.;

struct DragState {
    should_move: bool,
}

impl Render for DragState {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

/// The close button: small, rounded, with a faint highlight on hover.
fn close_button(cx: &App) -> impl IntoElement {
    let theme = cx.theme();
    let hover = theme.foreground.opacity(0.08);
    let color = theme.muted_foreground;
    div()
        .id("window-close")
        .flex()
        .items_center()
        .justify_center()
        .size(px(24.))
        .mx_2()
        .rounded(px(6.))
        .text_color(color)
        .cursor_pointer()
        .hover(move |d| d.bg(hover))
        .child(Icon::new(IconName::X).size_3p5())
        .on_click(|_, window, _| window.remove_window())
}

/// Builds the title bar around `content`. Dragging moves the window and a
/// double click zooms it on Linux.
pub fn title_bar(content: impl IntoElement, window: &mut Window, cx: &mut App) -> impl IntoElement {
    let state = window.use_state(cx, |_, _| DragState { should_move: false });
    let radii = window_corner_radii(window, cx);
    let theme = cx.theme().clone();
    let client_decorated = matches!(window.window_decorations(), Decorations::Client { .. });
    h_flex()
        .id("title-bar")
        .flex_shrink_0()
        .h(px(HEIGHT))
        .items_center()
        .pl(px(LEFT_PADDING))
        .rounded_tl(radii.top_left)
        .rounded_tr(radii.top_right)
        .bg(theme.title_bar)
        .border_b_1()
        .border_color(theme.title_bar_border)
        .when(cfg!(target_os = "linux"), |d| {
            d.on_double_click(|_, window, _| window.zoom_window())
        })
        .on_mouse_down_out(window.listener_for(&state, |s, _, _, _| s.should_move = false))
        .on_mouse_down(
            MouseButton::Left,
            window.listener_for(&state, |s, _, _, _| s.should_move = true),
        )
        .on_mouse_up(
            MouseButton::Left,
            window.listener_for(&state, |s, _, _, _| s.should_move = false),
        )
        .on_mouse_move(window.listener_for(&state, |s, _, window, _| {
            if s.should_move {
                s.should_move = false;
                window.start_window_move();
            }
        }))
        .child(
            div()
                .flex_1()
                .h_full()
                .flex()
                .items_center()
                .window_control_area(WindowControlArea::Drag)
                // Keep the content (the palette button) clear of the close button.
                .pr_4()
                .child(content),
        )
        // Linux draws its own frame under client decorations; Windows has no frame at all once the
        // native title bar is hidden. macOS keeps its native window buttons.
        .when(
            cfg!(target_os = "windows") || (client_decorated && cfg!(target_os = "linux")),
            |d| d.child(close_button(cx)),
        )
}
