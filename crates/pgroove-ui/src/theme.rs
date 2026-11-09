//! Theme installation and switching: bundled Catppuccin flavors, user theme files, and the
//! appearance setting (follow the system, always light, always dark).

use gpui_kit::component::{Theme, ThemeMode, ThemeRegistry};
use gpui_kit::*;
use pgroove_core::settings::{Appearance, DEFAULT_DARK_THEME, DEFAULT_LIGHT_THEME, Settings};

/// The four Catppuccin flavors: Latte (light), Frappé, Macchiato and Mocha (dark).
/// See the README for the license.
const BUILTIN: &str = include_str!("../themes/catppuccin.json");

/// Corner radius of the application window.
const WINDOW_RADIUS: f32 = 10.;

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
    Theme::update(cx, |t| t.window_radius = px(WINDOW_RADIUS));
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
    Theme::update(cx, |t| t.window_radius = px(WINDOW_RADIUS));
}

#[cfg(test)]
mod tests {
    use super::BUILTIN;
    use gpui_kit::component::ThemeSet;
    use pgroove_core::settings::{DEFAULT_DARK_THEME, DEFAULT_LIGHT_THEME};

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
