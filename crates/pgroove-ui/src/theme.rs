//! Theme installation and switching: bundled Catppuccin flavors, user theme files, and the
//! appearance setting (follow the system, always light, always dark).

use gpui_kit::component::scroll::ScrollbarMode;
use gpui_kit::component::{Theme, ThemeConfig, ThemeMode, ThemeRegistry};
use gpui_kit::*;
use pgroove_core::settings::{
    Appearance, DEFAULT_DARK_THEME, DEFAULT_LIGHT_THEME, DEFAULT_SCALE, Settings, clamp_scale,
    scaled,
};

/// The four Catppuccin flavors: Latte (light), Frappé, Macchiato and Mocha (dark).
/// See the README for the license.
const BUILTIN: &str = include_str!("../themes/catppuccin.json");

/// Corner radius of the application window.
const WINDOW_RADIUS: f32 = 10.;

/// The kit's own text sizes, which a theme file may override (`font.size`, `mono_font.size`).
const KIT_FONT: f32 = 16.;
const KIT_MONO_FONT: f32 = 13.;

/// The sizes in force: the interface and grid scales (percent), set from the settings.
#[derive(Clone, Copy)]
struct Scale {
    ui: u32,
    grid: u32,
}

impl Global for Scale {}

/// The theme's own text sizes: what its file asks for, else the kit's. Never read back from
/// the live theme, which already carries the scale.
fn base_sizes(light: &ThemeConfig, dark: &ThemeConfig, mode: ThemeMode) -> (f32, f32) {
    let t = if mode.is_dark() { dark } else { light };
    (
        t.font_size.unwrap_or(KIT_FONT),
        t.mono_font_size.unwrap_or(KIT_MONO_FONT),
    )
}

/// The theme's text sizes at `percent` of normal, to whole pixels.
fn text_sizes(
    light: &ThemeConfig,
    dark: &ThemeConfig,
    mode: ThemeMode,
    percent: u32,
) -> (f32, f32) {
    let (font, mono) = base_sizes(light, dark, mode);
    (scaled(font, percent), scaled(mono, percent))
}

/// Re-applies the settings that must survive a theme change: the window radius, no kit
/// highlight on the table's clicked cell (the result grid draws its own selection, and the
/// kit's can go stale after a drag), scrollbars that stay visible, so a grid that is
/// wider or longer than its pane shows that there is more in every direction, and the
/// interface size, which scales the theme's text sizes (the window's rem size follows them,
/// so most of the interface does too).
fn tweak(cx: &mut App) {
    Theme::set_scrollbar_mode(ScrollbarMode::Always, cx);
    let percent = ui_scale(cx);
    let (font, mono) = {
        let t = Theme::global(cx);
        text_sizes(&t.light_theme, &t.dark_theme, t.mode, percent)
    };
    Theme::update(cx, |t| {
        t.window_radius = px(WINDOW_RADIUS);
        t.table_active = transparent_black();
        t.table_active_border = transparent_black();
        t.font_size = px(font);
        t.mono_font_size = px(mono);
    });
}

/// The interface size in force, in percent.
pub fn ui_scale(cx: &App) -> u32 {
    cx.try_global::<Scale>().map_or(DEFAULT_SCALE, |s| s.ui)
}

/// A length in pixels at the interface size in force: the fixed heights of bars and chips,
/// which would clip their text at a larger size.
pub fn ui_px(normal: f32, cx: &App) -> Pixels {
    px(scaled(normal, ui_scale(cx)))
}

/// The grid size in force, in percent.
pub fn grid_scale(cx: &App) -> u32 {
    cx.try_global::<Scale>().map_or(DEFAULT_SCALE, |s| s.grid)
}

/// The theme choice in force, kept so system appearance changes can be followed.
#[derive(Clone)]
struct Choice {
    appearance: Appearance,
}

impl Global for Choice {}

fn load(registry: &mut ThemeRegistry, source: &str, text: &str) {
    if let Err(e) = registry.load_themes_from_str(text) {
        log::warn!("could not load themes from {source}: {e:#}");
    }
}

/// Registers the bundled themes and any `*.json` theme files in the user's themes folder.
/// Calling it again picks up new or edited files (existing theme names are kept).
pub fn install(cx: &mut App) {
    let registry = ThemeRegistry::global_mut(cx);
    load(registry, "the built-in themes", BUILTIN);
    let Some(dir) = pgroove_core::settings::themes_dir() else {
        return;
    };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };
    let mut files: Vec<_> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .collect();
    files.sort();
    for path in files {
        match std::fs::read_to_string(&path) {
            Ok(text) => load(registry, &path.display().to_string(), &text),
            Err(e) => log::warn!("could not read {}: {e}", path.display()),
        }
    }
}

/// Names of the available themes with whether each is dark: light ones first, then by name.
pub fn available(cx: &App) -> Vec<(String, bool)> {
    let mut out: Vec<(String, bool)> = ThemeRegistry::global(cx)
        .themes()
        .values()
        .map(|t| (t.name.to_string(), t.mode.is_dark()))
        .collect();
    out.sort_by(|a, b| {
        a.1.cmp(&b.1)
            .then_with(|| a.0.to_lowercase().cmp(&b.0.to_lowercase()))
    });
    out
}

fn mode_for(appearance: Appearance, window: Option<&Window>, cx: &App) -> ThemeMode {
    match appearance {
        Appearance::Light => ThemeMode::Light,
        Appearance::Dark => ThemeMode::Dark,
        Appearance::System => window
            .map(|w| w.appearance())
            .unwrap_or_else(|| cx.window_appearance())
            .into(),
    }
}

/// Applies the settings: picks the light and dark themes by name (unknown names keep the
/// current theme), chooses the mode, and keeps the rounded window corners.
pub fn apply(settings: &Settings, window: Option<&mut Window>, cx: &mut App) {
    cx.set_global(Choice {
        appearance: settings.appearance,
    });
    cx.set_global(Scale {
        ui: clamp_scale(settings.ui_scale),
        grid: clamp_scale(settings.grid_scale),
    });
    let registry = ThemeRegistry::global(cx);
    // An unknown name (a deleted theme file, a typo) falls back to the app's own default.
    let pick = |name: &str, fallback: &str| {
        registry.themes().get(name).cloned().or_else(|| {
            log::warn!("unknown theme {name:?}, using {fallback:?}");
            registry.themes().get(fallback).cloned()
        })
    };
    let light = pick(&settings.light_theme, DEFAULT_LIGHT_THEME);
    let dark = pick(&settings.dark_theme, DEFAULT_DARK_THEME);
    Theme::update(cx, |t| {
        if let Some(l) = light {
            t.light_theme = l;
        }
        if let Some(d) = dark {
            t.dark_theme = d;
        }
    });
    let mode = mode_for(settings.appearance, window.as_deref(), cx);
    Theme::change(mode, window, cx);
    tweak(cx);
    cx.refresh_windows();
}

/// Follows the desktop's light/dark switch, unless the appearance is forced.
pub fn system_changed(window: &mut Window, cx: &mut App) {
    if cx
        .try_global::<Choice>()
        .is_some_and(|c| c.appearance != Appearance::System)
    {
        return;
    }
    let mode = mode_for(Appearance::System, Some(window), cx);
    Theme::change(mode, Some(window), cx);
    tweak(cx);
}

#[cfg(test)]
mod tests {
    use super::{BUILTIN, KIT_FONT, KIT_MONO_FONT, base_sizes, text_sizes};
    use gpui_kit::component::{ThemeMode, ThemeSet};
    use pgroove_core::settings::{DEFAULT_DARK_THEME, DEFAULT_LIGHT_THEME};

    #[test]
    fn base_sizes_come_from_the_theme_file_not_the_live_theme() {
        let set: ThemeSet = serde_json::from_str(BUILTIN).unwrap();
        let (light, dark) = (&set.themes[0], &set.themes[3]);
        // The bundled themes ask for no size, so the kit's apply.
        assert_eq!(
            base_sizes(light, dark, ThemeMode::Light),
            (KIT_FONT, KIT_MONO_FONT)
        );
        // One that does is followed, for its own mode only.
        let mut big = dark.clone();
        big.font_size = Some(18.);
        big.mono_font_size = Some(15.);
        assert_eq!(base_sizes(light, &big, ThemeMode::Dark), (18., 15.));
        assert_eq!(
            base_sizes(light, &big, ThemeMode::Light),
            (KIT_FONT, KIT_MONO_FONT)
        );
    }

    #[test]
    fn text_sizes_follow_the_interface_size() {
        let set: ThemeSet = serde_json::from_str(BUILTIN).unwrap();
        let (light, dark) = (&set.themes[0], &set.themes[3]);
        assert_eq!(text_sizes(light, dark, ThemeMode::Light, 100), (16., 13.));
        assert_eq!(text_sizes(light, dark, ThemeMode::Dark, 125), (20., 16.));
        assert_eq!(text_sizes(light, dark, ThemeMode::Dark, 75), (12., 10.));
        // Applying the same size twice gives the same result: it never compounds.
        let once = text_sizes(light, dark, ThemeMode::Dark, 75);
        assert_eq!(text_sizes(light, dark, ThemeMode::Dark, 75), once);
        // A theme file's own size is what is scaled.
        let mut big = dark.clone();
        big.font_size = Some(20.);
        assert_eq!(text_sizes(light, &big, ThemeMode::Dark, 75).0, 15.);
    }

    #[test]
    fn bundled_themes_include_the_defaults() {
        let set: ThemeSet = serde_json::from_str(BUILTIN).expect("valid theme file");
        let find = |name: &str| set.themes.iter().find(|t| t.name == name);
        assert!(
            !find(DEFAULT_LIGHT_THEME)
                .expect("default light theme")
                .mode
                .is_dark()
        );
        assert!(
            find(DEFAULT_DARK_THEME)
                .expect("default dark theme")
                .mode
                .is_dark()
        );
        let names: Vec<_> = set.themes.iter().map(|t| t.name.to_string()).collect();
        assert_eq!(
            names,
            [
                "Catppuccin Latte",
                "Catppuccin Frappe",
                "Catppuccin Macchiato",
                "Catppuccin Mocha"
            ]
        );
    }
}
