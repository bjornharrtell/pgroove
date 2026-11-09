use std::sync::Arc;

use gpui_kit::component::{Theme, TitleBar};
use gpui_kit::*;
use pgroove_core::ProfileStore;
use pgroove_core::secrets::KeyringStore;
use pgroove_ui::Workspace;

/// Corner radius of the application window.
const WINDOW_RADIUS: f32 = 10.;

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
    gpui_kit::application()
        .with_assets(gpui_kit::assets::AllAssets)
        .run(move |cx| {
            gpui_kit::init(cx);
            sync_theme(None, cx);
            pgroove_ui::init(cx);
            let options = WindowOptions {
                window_decorations: Some(WindowDecorations::Client),
                ..TitleBar::window_options()
            };
            gpui_kit::open_window(options, cx, move |window, cx| {
                window
                    .observe_window_appearance(|window, cx| sync_theme(Some(window), cx))
                    .detach();
                cx.new(|cx| Workspace::new(store, Arc::new(KeyringStore), runtime, window, cx))
            })
            .expect("failed to open window");
            log::info!("pgroove started");
        });
    Ok(())
}
