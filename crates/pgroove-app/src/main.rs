use std::sync::Arc;

use gpui_kit::component::Theme;
use gpui_kit::*;
use pgroove_core::ProfileStore;
use pgroove_core::secrets::KeyringStore;
use pgroove_ui::Workspace;

fn main() -> anyhow::Result<()> {
    env_logger::init();
    let runtime = Arc::new(tokio::runtime::Runtime::new()?);
    let path = ProfileStore::default_path().ok_or_else(|| anyhow::anyhow!("no config dir"))?;
    let store = ProfileStore::load(path)?;
    gpui_kit::application()
        .with_assets(gpui_kit::assets::AllAssets)
        .run(move |cx| {
            gpui_kit::init(cx);
            Theme::sync_system_appearance(None, cx);
            pgroove_ui::init(cx);
            gpui_kit::open_window(WindowOptions::default(), cx, move |window, cx| {
                window
                    .observe_window_appearance(|window, cx| {
                        Theme::sync_system_appearance(Some(window), cx)
                    })
                    .detach();
                cx.new(|cx| Workspace::new(store, Arc::new(KeyringStore), runtime, window, cx))
            })
            .expect("failed to open window");
            log::info!("pgroove started");
        });
    Ok(())
}
