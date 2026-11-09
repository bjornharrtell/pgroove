use std::sync::Arc;

use gpui_kit::component::{Theme, ThemeRegistry, TitleBar};
use gpui_kit::*;
use pgroove_core::ProfileStore;
use pgroove_core::secrets::KeyringStore;
use pgroove_core::session::SessionState;
use pgroove_ui::Workspace;

/// Corner radius of the application window.
const WINDOW_RADIUS: f32 = 10.;

/// Catppuccin Latte (light) and Mocha (dark); see the README for the license.
const CATPPUCCIN: &str = include_str!("../themes/catppuccin.json");

/// Registers the Catppuccin themes and makes them the light and dark themes.
fn install_themes(cx: &mut App) {
    let registry = ThemeRegistry::global_mut(cx);
    if let Err(e) = registry.load_themes_from_str(CATPPUCCIN) {
        log::warn!("could not load the Catppuccin themes: {e:#}");
        return;
    }
    let latte = registry.themes().get("Catppuccin Latte").cloned();
    let mocha = registry.themes().get("Catppuccin Mocha").cloned();
    Theme::update(cx, |t| {
        if let Some(latte) = latte {
            t.light_theme = latte;
        }
        if let Some(mocha) = mocha {
            t.dark_theme = mocha;
        }
    });
}

/// Syncs the theme with the desktop and applies the window style on top of it.
fn sync_theme(window: Option<&mut Window>, cx: &mut App) {
    Theme::sync_system_appearance(window, cx);
    Theme::update(cx, |t| t.window_radius = px(WINDOW_RADIUS));
}

fn main() -> anyhow::Result<()> {
    env_logger::init();
    let runtime = Arc::new(tokio::runtime::Runtime::new()?);
    let path = ProfileStore::default_path().ok_or_else(|| anyhow::anyhow!("no config dir"))?;
    let store = ProfileStore::load(path)?;
    let session_path = SessionState::default_path();
    let session = session_path
        .as_deref()
        .map(SessionState::load)
        .unwrap_or_default();
    gpui_kit::application()
        .with_assets(gpui_kit::assets::AllAssets)
        .run(move |cx| {
            gpui_kit::init(cx);
            install_themes(cx);
            sync_theme(None, cx);
            pgroove_ui::init(cx);
            let options = WindowOptions {
                window_bounds: session.usable_window().map(|(w, h)| {
                    WindowBounds::Windowed(Bounds::centered(None, size(px(w), px(h)), cx))
                }),
                // Rounded corners need an alpha surface so the corners and shadow margin
                // are see-through; every themed surface inside the frame is opaque.
                #[cfg(target_os = "linux")]
                window_background: WindowBackgroundAppearance::Transparent,
                window_decorations: Some(WindowDecorations::Client),
                ..TitleBar::window_options()
            };
            gpui_kit::open_window(options, cx, move |window, cx| {
                window
                    .observe_window_appearance(|window, cx| sync_theme(Some(window), cx))
                    .detach();
                cx.new(|cx| {
                    Workspace::new(
                        store,
                        Arc::new(KeyringStore),
                        runtime,
                        session,
                        session_path,
                        window,
                        cx,
                    )
                })
            })
            .expect("failed to open window");
            log::info!("pgroove started");
        });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::CATPPUCCIN;
    use gpui_kit::component::ThemeSet;

    #[test]
    fn embedded_themes_are_latte_and_mocha() {
        let set: ThemeSet = serde_json::from_str(CATPPUCCIN).expect("valid theme file");
        let names: Vec<_> = set.themes.iter().map(|t| t.name.to_string()).collect();
        assert_eq!(names, ["Catppuccin Latte", "Catppuccin Mocha"]);
        assert!(!set.themes[0].mode.is_dark(), "Latte is the light theme");
        assert!(set.themes[1].mode.is_dark(), "Mocha is the dark theme");
    }
}
