//! gpui views built on gpui-kit.

mod connections;
mod grid_pane;
mod mode;
mod results;
mod schema_tree;
mod structure_view;
mod table_view;
mod workspace;

pub use workspace::Workspace;

use gpui_kit::{App, KeyBinding};

pub use workspace::{CancelQuery, RunQuery};

/// Registers key bindings; call once at startup.
pub fn init(cx: &mut App) {
    cx.set_global(mode::ReadOnlyMode(true));
    cx.bind_keys([
        KeyBinding::new("ctrl-enter", RunQuery, None),
        KeyBinding::new("cmd-enter", RunQuery, None),
        KeyBinding::new("escape", CancelQuery, Some("Workspace")),
    ]);
}
