use gpui_kit::*;
use pgroove_ui::Workspace;

fn main() {
    env_logger::init();
    gpui_kit::application().run(|cx| {
        gpui_kit::init(cx);
        gpui_kit::open_window(WindowOptions::default(), cx, |_, cx| cx.new(|_| Workspace))
            .expect("failed to open window");
        log::info!("pgroove started");
    });
}
