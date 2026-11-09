use std::sync::Arc;

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
        .with_assets(gpui_kit::assets::Assets)
        .run(move |cx| {
            gpui_kit::init(cx);
            gpui_kit::open_window(WindowOptions::default(), cx, move |_, cx| {
                cx.new(|_| Workspace::new(store, Arc::new(KeyringStore), runtime))
            })
            .expect("failed to open window");
            log::info!("pgroove started");
        });
    Ok(())
}
