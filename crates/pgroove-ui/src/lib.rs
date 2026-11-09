//! gpui views built on gpui-kit.

mod activity_view;
mod connections;
mod grid_pane;
mod mode;
mod plan_view;
mod query_tab;
mod results;
mod schema_tree;
mod sql_completion;
mod structure_view;
mod table_view;
mod workspace;

pub use workspace::Workspace;

use gpui_kit::{App, KeyBinding};

pub use query_tab::{CancelQuery, FormatSql, RunQuery};

/// Registers key bindings; call once at startup.
pub fn init(cx: &mut App) {
    cx.set_global(mode::ReadOnlyMode(true));
    cx.bind_keys([
        KeyBinding::new("ctrl-enter", RunQuery, None),
        KeyBinding::new("cmd-enter", RunQuery, None),
        KeyBinding::new("escape", CancelQuery, Some("QueryTab")),
        KeyBinding::new("ctrl-shift-f", FormatSql, None),
    ]);
}
