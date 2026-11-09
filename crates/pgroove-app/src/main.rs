use std::sync::Arc;

use gpui_kit::component::TitleBar;
use gpui_kit::*;
use pgroove_core::ProfileStore;
use pgroove_core::secrets::KeyringStore;
use pgroove_core::session::{MIN_WINDOW, SessionState};
use pgroove_core::settings::Settings;
use pgroove_ui::{Startup, Workspace};

fn main() -> anyhow::Result<()> {
    env_logger::init();
    let runtime = Arc::new(tokio::runtime::Runtime::new()?);
    let path = ProfileStore::default_path().ok_or_else(|| anyhow::anyhow!("no config dir"))?;
    let store = ProfileStore::load(path)?;
    let settings_path = Settings::default_path();
    let settings = settings_path
        .as_deref()
        .map(Settings::load)
        .unwrap_or_default();
    let session_path = SessionState::default_path();
    let session = session_path
        .as_deref()
        .map(SessionState::load)
        .unwrap_or_default();
    gpui_kit::application()
        .with_assets(gpui_kit::assets::AllAssets)
        .run(move |cx| {
            gpui_kit::init(cx);
            pgroove_ui::theme::install(cx);
            pgroove_ui::theme::apply(&settings, None, cx);
            pgroove_ui::init(cx);
            #[allow(unused_mut)]
            let mut options = WindowOptions {
                window_bounds: session.usable_window().map(|(w, h)| {
                    WindowBounds::Windowed(Bounds::centered(None, size(px(w), px(h)), cx))
                }),
                // Rounded corners need an alpha surface so the corners and shadow margin
                // are see-through; every themed surface inside the frame is opaque.
                #[cfg(target_os = "linux")]
                window_background: WindowBackgroundAppearance::Transparent,
                window_min_size: Some(size(px(MIN_WINDOW.0), px(MIN_WINDOW.1))),
                window_decorations: Some(WindowDecorations::Client),
                ..TitleBar::window_options()
            };
            // Center the native window buttons in the 36px bar.
            #[cfg(target_os = "macos")]
            if let Some(t) = options.titlebar.as_mut() {
                t.traffic_light_position = Some(point(px(12.), px(11.)));
            }
            gpui_kit::open_window(options, cx, move |window, cx| {
                window
                    .observe_window_appearance(|window, cx| {
                        pgroove_ui::theme::system_changed(window, cx)
                    })
                    .detach();
                cx.new(|cx| {
                    Workspace::new(
                        store,
                        Arc::new(KeyringStore),
                        runtime,
                        Startup {
                            session,
                            session_path,
                            settings,
                            settings_path,
                        },
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
