//! gpui views built on gpui-kit.

mod activity_view;
mod connections;
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
mod workspace;

pub use workspace::Workspace;

use gpui_kit::{App, KeyBinding};

pub use query_tab::{CancelQuery, FormatSql, RunQuery};
pub use workspace::OpenPalette;

/// The action and key context for a command name from `pgroove_core::keys`.
fn action_for(name: &str) -> Option<(Box<dyn gpui_kit::Action>, Option<&'static str>)> {
    Some(match name {
        "run_query" => (Box::new(RunQuery), None),
        "cancel_query" => (Box::new(CancelQuery), Some("QueryTab")),
        "format_sql" => (Box::new(FormatSql), None),
        "open_palette" => (Box::new(OpenPalette), None),
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
