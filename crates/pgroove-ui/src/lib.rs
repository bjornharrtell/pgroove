//! gpui views built on gpui-kit.

mod activity_view;
mod connections;
mod dbeaver_dialog;
mod dialogs;
mod grid_pane;
mod mode;
mod palette;
mod plan_view;
mod query_tab;
mod results;
mod schema_tree;
mod sql_completion;
mod structure_view;
mod table_view;
pub mod theme;
mod title_bar;
mod workspace;

pub use workspace::Workspace;

use gpui_kit::{App, KeyBinding};

pub use query_tab::{CancelQuery, FormatSql, RunQuery};
pub use workspace::{OpenPalette, Startup, ToggleFullscreen, step_cursor};

/// The action and key context for a command name from `pgroove_core::keys`.
fn action_for(name: &str) -> Option<(Box<dyn gpui_kit::Action>, Option<&'static str>)> {
    Some(match name {
        "run_query" => (Box::new(RunQuery), None),
        "cancel_query" => (Box::new(CancelQuery), Some("QueryTab")),
        "format_sql" => (Box::new(FormatSql), None),
        "open_palette" => (Box::new(OpenPalette), None),
        "toggle_fullscreen" => (Box::new(ToggleFullscreen), None),
        _ => return None,
    })
}

/// Builds a binding; `None` when the command is unknown or the keystroke is invalid.
fn binding(name: &str, keystroke: &str, cx: &App) -> Option<KeyBinding> {
    let (action, context) = action_for(name)?;
    let predicate = match context {
        Some(c) => Some(std::rc::Rc::new(
            gpui_kit::KeyBindingContextPredicate::parse(c).ok()?,
        )),
        None => None,
    };
    KeyBinding::load(keystroke, action, predicate, false, None, cx).ok()
}

gpui_kit::actions!(app, [Quit, CloseWindow]);

/// Registers key bindings (defaults plus the user's `keybindings.toml`); call once at startup.
pub fn init(cx: &mut App) {
    cx.set_global(mode::ReadOnlyMode(true));
    let overrides = pgroove_core::keys::default_path()
        .map(|p| {
            pgroove_core::keys::load_overrides(&p).unwrap_or_else(|e| {
                log::warn!("ignoring keybindings: {e:#}");
                Default::default()
            })
        })
        .unwrap_or_default();
    let (pairs, unknown) = pgroove_core::keys::resolve(&overrides);
    for name in unknown {
        log::warn!("keybindings: unknown command {name}");
    }
    let bindings: Vec<KeyBinding> = pairs
        .iter()
        .filter_map(|(name, key)| {
            let b = binding(name, key, cx);
            if b.is_none() {
                log::warn!("keybindings: invalid keystroke {key:?} for {name}");
            }
            b
        })
        .collect();
    cx.bind_keys(bindings);
    // Always a way to leave, whatever the window frame shows.
    cx.on_action(|_: &Quit, cx| cx.quit());
    cx.bind_keys([
        KeyBinding::new("cmd-q", Quit, None),
        KeyBinding::new("ctrl-q", Quit, None),
        KeyBinding::new("cmd-w", CloseWindow, None),
    ]);
    cx.set_menus([gpui_kit::Menu {
        name: "pgroove".into(),
        items: vec![gpui_kit::MenuItem::action("Quit pgroove", Quit)],
        disabled: false,
    }]);
    // Arrow keys move through the sidebar tree while it has focus.
    cx.bind_keys([
        KeyBinding::new("up", workspace::SidebarUp, Some("Sidebar")),
        KeyBinding::new("down", workspace::SidebarDown, Some("Sidebar")),
        KeyBinding::new("enter", workspace::SidebarActivate, Some("Sidebar")),
        KeyBinding::new("right", workspace::SidebarExpand, Some("Sidebar")),
        KeyBinding::new("left", workspace::SidebarCollapse, Some("Sidebar")),
    ]);
}

#[cfg(test)]
mod tests {
    use super::action_for;

    #[test]
    fn every_default_command_has_an_action() {
        for (name, _) in pgroove_core::keys::DEFAULTS {
            assert!(action_for(name).is_some(), "{name}");
        }
        assert!(action_for("nope").is_none());
        assert_eq!(action_for("cancel_query").unwrap().1, Some("QueryTab"));
    }
}
