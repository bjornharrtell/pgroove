//! Application title bar: drag area, content, and a subtle close button.

use gpui_kit::assets::IconName;
use gpui_kit::component::{
    ActiveTheme, Icon, InteractiveElementExt as _, h_flex, window_corner_radii,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

pub const HEIGHT: f32 = 36.;

/// How much of a connection's colour is mixed into the bar's own colour. Chosen so text
/// keeps at least 4.5:1 contrast for every connection colour on the bundled themes (checked
/// by a test); the bar's bottom edge carries the colour at full strength.
const TINT: f32 = 0.22;

/// `base` with `accent` (`0xRRGGBB`) mixed in by `amount` (0 keeps `base`, 1 gives `accent`).
pub fn tinted(base: Hsla, accent: u32, amount: f32) -> Hsla {
    let (b, a) = (Rgba::from(base), rgb(accent));
    let mix = |from: f32, to: f32| from + (to - from) * amount.clamp(0., 1.);
    Rgba {
        r: mix(b.r, a.r),
        g: mix(b.g, a.g),
        b: mix(b.b, a.b),
        a: b.a,
    }
    .into()
}

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

/// A part of the title bar that drags the window and, on Linux, zooms it on a double click.
/// Only these parts react: buttons and chips in the bar keep their own clicks, so pressing
/// one never starts a window move or counts toward a double click.
pub fn drag_area(id: impl Into<ElementId>, window: &mut Window, cx: &mut App) -> Stateful<Div> {
    let state = window.use_state(cx, |_, _| DragState { should_move: false });
    div()
        .id(id)
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
        .window_control_area(WindowControlArea::Drag)
}

/// Background, border colour and border width of the bar, given the theme's own bar colours
/// and the focused connection's colour, if it has one.
fn bar_style(plain: Hsla, plain_border: Hsla, accent: Option<u32>) -> (Hsla, Hsla, f32) {
    match accent {
        Some(c) => (tinted(plain, c, TINT), rgb(c).into(), 2.),
        None => (plain, plain_border, 1.),
    }
}

/// Builds the title bar around `content`; put `drag_area`s in the content for the parts that
/// should move the window. `accent` (`0xRRGGBB`) tints the bar, as the focused connection's
/// colour does.
pub fn title_bar(
    content: impl IntoElement,
    accent: Option<u32>,
    window: &mut Window,
    cx: &mut App,
) -> impl IntoElement {
    let radii = window_corner_radii(window, cx);
    let theme = cx.theme().clone();
    let client_decorated = matches!(window.window_decorations(), Decorations::Client { .. });
    let (background, border, border_width) =
        bar_style(theme.title_bar, theme.title_bar_border, accent);
    h_flex()
        .id("title-bar")
        .flex_shrink_0()
        .h(crate::theme::ui_px(HEIGHT, cx))
        .items_center()
        .rounded_tl(radii.top_left)
        .rounded_tr(radii.top_right)
        .bg(background)
        .border_color(border)
        .border_b(px(border_width))
        // The left margin (clear of the macOS window buttons) drags too.
        .child(
            drag_area("title-drag-left", window, cx)
                .w(px(LEFT_PADDING))
                .h_full()
                .flex_shrink_0(),
        )
        .child(
            div()
                .flex_1()
                .h_full()
                .flex()
                .items_center()
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

#[cfg(test)]
mod tests {
    use super::{TINT, bar_style, tinted};
    use gpui_kit::{Rgba, rgb};
    use pgroove_core::ProfileColor;

    fn luminance(c: Rgba) -> f32 {
        let lin = |v: f32| {
            if v <= 0.03928 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * lin(c.r) + 0.7152 * lin(c.g) + 0.0722 * lin(c.b)
    }

    fn contrast(a: Rgba, b: Rgba) -> f32 {
        let (a, b) = (luminance(a), luminance(b));
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }

    fn hex(s: &str) -> Rgba {
        rgb(u32::from_str_radix(s.trim_start_matches('#'), 16).unwrap())
    }

    #[test]
    fn bar_style_follows_the_connection_color() {
        let (plain, edge): (gpui_kit::Hsla, gpui_kit::Hsla) =
            (rgb(0x202020).into(), rgb(0x404040).into());
        // No colour: the theme's own bar, with its thin border.
        assert_eq!(bar_style(plain, edge, None), (plain, edge, 1.0));
        // A colour: a tinted bar, the colour itself as a thicker border.
        let (bg, border, width) = bar_style(plain, edge, Some(0xE5484D));
        assert_ne!(bg, plain);
        assert_eq!(Rgba::from(border).r, rgb(0xE5484D).r);
        assert_eq!(width, 2.0);
        assert_ne!(border, edge);
    }

    #[test]
    fn mixing_endpoints_and_clamping() {
        let base: gpui_kit::Hsla = rgb(0x102030).into();
        let same = Rgba::from(tinted(base, 0xFFFFFF, 0.0));
        assert!((same.r - Rgba::from(base).r).abs() < 1e-4);
        let full = Rgba::from(tinted(base, 0xFF8000, 1.0));
        assert!((full.r - 1.0).abs() < 1e-3 && (full.g - 128. / 255.).abs() < 1e-3);
        // Out of range amounts are clamped to the endpoints.
        let over = Rgba::from(tinted(base, 0xFF8000, 7.0));
        assert!((over.r - full.r).abs() < 1e-4);
        let under = Rgba::from(tinted(base, 0xFF8000, -3.0));
        assert!((under.r - same.r).abs() < 1e-4);
    }

    /// The title text stays readable on every theme for every connection colour.
    #[test]
    fn tinted_bars_keep_text_readable() {
        let set: serde_json::Value =
            serde_json::from_str(include_str!("../themes/catppuccin.json")).unwrap();
        let themes = set["themes"].as_array().unwrap();
        assert_eq!(themes.len(), 4);
        for theme in themes {
            let colors = &theme["colors"];
            let bar = hex(colors["title_bar.background"].as_str().unwrap());
            let text = hex(colors["foreground"].as_str().unwrap());
            for color in ProfileColor::ALL {
                let tinted = Rgba::from(tinted(bar.into(), color.rgb(), TINT));
                let ratio = contrast(tinted, text);
                assert!(
                    ratio >= 4.5,
                    "{} on {}: {ratio:.2}:1",
                    color.label(),
                    theme["name"]
                );
            }
        }
    }
}
