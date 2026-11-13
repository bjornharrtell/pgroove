//! Command palette: the list of commands and how they are grouped.

use pgroove_core::settings::Appearance;

/// Something the palette can run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PaletteCommand {
    NewQuery,
    RunQuery,
    ExplainQuery,
    AnalyzeQuery,
    FormatSql,
    ShowHistory,
    ShowSnippets,
    ToggleReadOnly,
    OpenActivity,
    RefreshTree,
    NewConnection,
    ImportDbeaver,
    ImportDbeaverFrom,
    Connect(String),
    Disconnect,
    CheckUpdates,
    InstallUpdate,
    ToggleUpdateChecks,
    ToggleUppercaseSql,
    SetAppearance(Appearance),
    UseTheme(String),
    ReloadThemes,
    /// Step the interface size (`true` is larger) or put it back to normal.
    StepUiScale(bool),
    ResetUiScale,
    /// The same for the result grid.
    StepGridScale(bool),
    ResetGridScale,
}

pub struct PaletteEntry {
    pub command: PaletteCommand,
    pub title: String,
    pub keywords: Vec<&'static str>,
}

pub struct PaletteGroup {
    pub label: &'static str,
    pub entries: Vec<PaletteEntry>,
}

fn entry(
    command: PaletteCommand,
    title: impl Into<String>,
    keywords: &[&'static str],
) -> PaletteEntry {
    PaletteEntry {
        command,
        title: title.into(),
        keywords: keywords.to_vec(),
    }
}

/// Builds the palette contents for the current state.
/// What the palette needs to know to decide which commands to offer.
pub struct PaletteContext {
    pub connected: bool,
    pub read_only: bool,
    /// A newer version has been found and can be installed.
    pub update_available: bool,
    pub auto_check_updates: bool,
    pub uppercase_generated_sql: bool,
    /// Interface and grid size in force, in percent.
    pub ui_scale: u32,
    pub grid_scale: u32,
    pub appearance: Appearance,
    pub light_theme: String,
    pub dark_theme: String,
    /// Available themes as `(name, is_dark)`.
    pub themes: Vec<(String, bool)>,
}

pub fn groups(connections: &[String], ctx: &PaletteContext) -> Vec<PaletteGroup> {
    use PaletteCommand::*;
    let (connected, read_only) = (ctx.connected, ctx.read_only);
    let mut query = vec![entry(NewQuery, "New query tab", &["tab", "editor"])];
    if connected {
        query.extend([
            entry(RunQuery, "Run query", &["execute"]),
            entry(ExplainQuery, "Explain query", &["plan"]),
            entry(AnalyzeQuery, "Analyze query", &["plan", "explain analyze"]),
        ]);
    }
    query.extend([
        entry(FormatSql, "Format SQL", &["beautify", "pretty"]),
        entry(ShowHistory, "Show history", &["recent", "past"]),
        entry(ShowSnippets, "Show snippets", &["saved", "bookmarks"]),
    ]);
    let mut server = vec![entry(
        ToggleReadOnly,
        if read_only {
            "Allow editing (turn off read-only)"
        } else {
            "Block writes (turn on read-only)"
        },
        &["write", "lock", "editable"],
    )];
    if connected {
        server.extend([
            entry(
                OpenActivity,
                "Open activity monitor",
                &["sessions", "locks", "stats"],
            ),
            entry(RefreshTree, "Refresh schema tree", &["reload"]),
            entry(Disconnect, "Disconnect", &["close"]),
        ]);
    }
    let mut groups = vec![
        PaletteGroup {
            label: "Query",
            entries: query,
        },
        PaletteGroup {
            label: "Server",
            entries: server,
        },
    ];
    let mut conns = vec![
        entry(NewConnection, "New connection…", &["add"]),
        entry(
            ImportDbeaver,
            "Import connections from DBeaver…",
            &["dbeaver", "migrate"],
        ),
        entry(
            ImportDbeaverFrom,
            "Import connections from a DBeaver project folder…",
            &["dbeaver", "migrate", "workspace", "folder"],
        ),
    ];
    conns.extend(
        connections
            .iter()
            .map(|n| entry(Connect(n.clone()), format!("Connect to {n}"), &["open"])),
    );
    groups.push(PaletteGroup {
        label: "Connections",
        entries: conns,
    });
    let mark = |title: String, current: bool| {
        if current {
            format!("{title} (current)")
        } else {
            title
        }
    };
    let mut look: Vec<PaletteEntry> = Appearance::ALL
        .iter()
        .map(|&a| {
            entry(
                SetAppearance(a),
                mark(format!("Appearance: {}", a.label()), ctx.appearance == a),
                &["theme", "light", "dark", "mode"],
            )
        })
        .collect();
    look.extend(ctx.themes.iter().map(|(name, dark)| {
        let current = if *dark {
            *name == ctx.dark_theme
        } else {
            *name == ctx.light_theme
        };
        entry(
            UseTheme(name.clone()),
            mark(
                format!("Theme: {name} ({})", if *dark { "dark" } else { "light" }),
                current,
            ),
            &["colors", "appearance"],
        )
    }));
    look.push(entry(
        ReloadThemes,
        "Load new theme files",
        &["custom", "themes folder"],
    ));
    look.extend(scale_entries(
        "Interface",
        ctx.ui_scale,
        [StepUiScale(true), StepUiScale(false), ResetUiScale],
        &["font", "size", "zoom", "density", "compact"],
    ));
    look.extend(scale_entries(
        "Grid",
        ctx.grid_scale,
        [StepGridScale(true), StepGridScale(false), ResetGridScale],
        &["font", "size", "zoom", "density", "table", "results"],
    ));
    groups.push(PaletteGroup {
        label: "Appearance",
        entries: look,
    });
    let mut app = vec![entry(
        CheckUpdates,
        "Check for updates",
        &["upgrade", "version"],
    )];
    if ctx.update_available {
        app.push(entry(
            InstallUpdate,
            "Install the available update",
            &["upgrade"],
        ));
    }
    app.push(entry(
        ToggleUpdateChecks,
        if ctx.auto_check_updates {
            "Turn off automatic update checks"
        } else {
            "Turn on automatic update checks"
        },
        &["upgrade", "settings"],
    ));
    app.push(entry(
        ToggleUppercaseSql,
        if ctx.uppercase_generated_sql {
            "Write generated SQL keywords in lower case"
        } else {
            "Write generated SQL keywords in upper case"
        },
        &["settings", "keywords", "caps", "generate"],
    ));
    groups.push(PaletteGroup {
        label: "Application",
        entries: app,
    });
    groups
}

/// The size commands for one area: larger and smaller (each left out at its limit) and reset
/// (left out at the normal size). `commands` is larger, smaller, reset.
fn scale_entries(
    area: &str,
    percent: u32,
    commands: [PaletteCommand; 3],
    keywords: &[&'static str],
) -> Vec<PaletteEntry> {
    use pgroove_core::settings::{DEFAULT_SCALE, MAX_SCALE, MIN_SCALE, clamp_scale};
    let now = clamp_scale(percent);
    let [larger, smaller, reset] = commands;
    let mut out = Vec::new();
    if now < MAX_SCALE {
        out.push(entry(
            larger,
            format!("{area} size: larger (now {now}%)"),
            keywords,
        ));
    }
    if now > MIN_SCALE {
        out.push(entry(
            smaller,
            format!("{area} size: smaller (now {now}%)"),
            keywords,
        ));
    }
    if now != DEFAULT_SCALE {
        out.push(entry(
            reset,
            format!("{area} size: back to normal (now {now}%)"),
            keywords,
        ));
    }
    out
}

/// The interface and grid sizes after `cmd`, or `None` when it is not a size command.
pub fn resized(cmd: &PaletteCommand, ui: u32, grid: u32) -> Option<(u32, u32)> {
    use pgroove_core::settings::{DEFAULT_SCALE, step_scale};
    Some(match cmd {
        PaletteCommand::StepUiScale(up) => (step_scale(ui, *up), grid),
        PaletteCommand::ResetUiScale => (DEFAULT_SCALE, grid),
        PaletteCommand::StepGridScale(up) => (ui, step_scale(grid, *up)),
        PaletteCommand::ResetGridScale => (ui, DEFAULT_SCALE),
        _ => return None,
    })
}

/// Finds the command at a (section, row) position of the rendered palette.
pub fn command_at(groups: &[PaletteGroup], section: usize, row: usize) -> Option<PaletteCommand> {
    Some(groups.get(section)?.entries.get(row)?.command.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(connected: bool, read_only: bool, update: bool, auto: bool) -> PaletteContext {
        PaletteContext {
            connected,
            read_only,
            update_available: update,
            auto_check_updates: auto,
            uppercase_generated_sql: false,
            ui_scale: 100,
            grid_scale: 100,
            appearance: Appearance::System,
            light_theme: "Light One".into(),
            dark_theme: "Dark Two".into(),
            themes: vec![
                ("Light One".into(), false),
                ("Dark One".into(), true),
                ("Dark Two".into(), true),
            ],
        }
    }

    fn titles(g: &[PaletteGroup], i: usize) -> Vec<&str> {
        g[i].entries.iter().map(|e| e.title.as_str()).collect()
    }

    #[test]
    fn offline_palette_hides_server_commands() {
        let g = groups(&["a".to_string()], &ctx(false, true, false, true));
        assert_eq!(g.len(), 5);
        assert_eq!(
            titles(&g, 4),
            [
                "Check for updates",
                "Turn off automatic update checks",
                "Write generated SQL keywords in upper case"
            ]
        );
        assert!(!titles(&g, 0).contains(&"Run query"));
        assert_eq!(titles(&g, 1), ["Allow editing (turn off read-only)"]);
        assert_eq!(
            titles(&g, 2),
            [
                "New connection…",
                "Import connections from DBeaver…",
                "Import connections from a DBeaver project folder…",
                "Connect to a"
            ]
        );
    }

    #[test]
    fn connected_palette_lists_everything() {
        let g = groups(&[], &ctx(true, false, true, false));
        assert!(titles(&g, 0).contains(&"Run query"));
        assert!(titles(&g, 0).contains(&"Analyze query"));
        assert!(titles(&g, 1).contains(&"Open activity monitor"));
        assert!(titles(&g, 1).contains(&"Disconnect"));
        assert_eq!(titles(&g, 1)[0], "Block writes (turn on read-only)");
        assert_eq!(
            titles(&g, 2),
            [
                "New connection…",
                "Import connections from DBeaver…",
                "Import connections from a DBeaver project folder…"
            ]
        );
        assert_eq!(
            titles(&g, 4),
            [
                "Check for updates",
                "Install the available update",
                "Turn on automatic update checks",
                "Write generated SQL keywords in upper case"
            ]
        );
        // Once on, the same entry offers to turn it off again.
        let mut on = ctx(true, false, true, false);
        on.uppercase_generated_sql = true;
        assert_eq!(
            titles(&groups(&[], &on), 4).last(),
            Some(&"Write generated SQL keywords in lower case")
        );
    }

    #[test]
    fn size_commands_follow_the_current_size_and_its_limits() {
        let appearance = |ui: u32, grid: u32| {
            let mut c = ctx(false, true, false, true);
            c.ui_scale = ui;
            c.grid_scale = grid;
            let g = groups(&[], &c);
            titles(&g, 3)
                .into_iter()
                .filter(|t| t.contains(" size: "))
                .map(String::from)
                .collect::<Vec<_>>()
        };
        // Normal: larger and smaller, no reset.
        assert_eq!(
            appearance(100, 100),
            [
                "Interface size: larger (now 100%)",
                "Interface size: smaller (now 100%)",
                "Grid size: larger (now 100%)",
                "Grid size: smaller (now 100%)"
            ]
        );
        // At the top the larger step is gone and reset appears; at the bottom the reverse.
        assert_eq!(
            appearance(125, 75),
            [
                "Interface size: smaller (now 125%)",
                "Interface size: back to normal (now 125%)",
                "Grid size: larger (now 75%)",
                "Grid size: back to normal (now 75%)"
            ]
        );
        // A stray stored value is shown as the size actually in force.
        assert!(appearance(500, 100)[0].contains("now 125%"));
        // The commands carry what they say.
        let mut c = ctx(false, true, false, true);
        c.ui_scale = 90;
        let g = groups(&[], &c);
        let cmds: Vec<_> = g[3].entries.iter().map(|e| e.command.clone()).collect();
        assert!(cmds.contains(&PaletteCommand::StepUiScale(true)));
        assert!(cmds.contains(&PaletteCommand::StepUiScale(false)));
        assert!(cmds.contains(&PaletteCommand::ResetUiScale));
        assert!(!cmds.contains(&PaletteCommand::ResetGridScale));
    }

    #[test]
    fn size_commands_change_one_size_and_keep_the_other() {
        use PaletteCommand::*;
        assert_eq!(resized(&StepUiScale(true), 100, 90), Some((105, 90)));
        assert_eq!(resized(&StepUiScale(false), 100, 90), Some((95, 90)));
        assert_eq!(resized(&StepGridScale(true), 100, 90), Some((100, 95)));
        assert_eq!(resized(&StepGridScale(false), 100, 75), Some((100, 75)));
        assert_eq!(resized(&ResetUiScale, 120, 90), Some((100, 90)));
        assert_eq!(resized(&ResetGridScale, 120, 90), Some((120, 100)));
        assert_eq!(resized(&StepUiScale(true), 125, 90), Some((125, 90)));
        assert_eq!(resized(&ReloadThemes, 100, 100), None);
        assert_eq!(resized(&NewQuery, 100, 100), None);
    }

    #[test]
    fn appearance_and_themes_are_listed() {
        let g = groups(&[], &ctx(false, true, false, true));
        assert_eq!(g[3].label, "Appearance");
        let t = titles(&g, 3);
        assert_eq!(t[0], "Appearance: follow the system (current)");
        assert_eq!(t[1], "Appearance: always light");
        assert!(t.contains(&"Theme: Light One (light) (current)"));
        assert!(t.contains(&"Theme: Dark One (dark)"));
        assert!(t.contains(&"Theme: Dark Two (dark) (current)"));
        assert!(t.contains(&"Load new theme files"));
        // The size commands come after the themes.
        let load = t.iter().position(|s| *s == "Load new theme files").unwrap();
        assert!(t[load + 1].starts_with("Interface size"));
        assert_eq!(
            command_at(&g, 3, 3),
            Some(PaletteCommand::UseTheme("Light One".into()))
        );
        assert_eq!(
            command_at(&g, 3, 1),
            Some(PaletteCommand::SetAppearance(Appearance::Light))
        );
    }

    #[test]
    fn lookup_by_position() {
        let g = groups(&["x".to_string()], &ctx(true, true, false, true));
        assert_eq!(command_at(&g, 0, 0), Some(PaletteCommand::NewQuery));
        assert_eq!(command_at(&g, 2, 1), Some(PaletteCommand::ImportDbeaver));
        assert_eq!(
            command_at(&g, 2, 2),
            Some(PaletteCommand::ImportDbeaverFrom)
        );
        assert_eq!(
            command_at(&g, 2, 3),
            Some(PaletteCommand::Connect("x".into()))
        );
        assert_eq!(command_at(&g, 9, 0), None);
        assert_eq!(command_at(&g, 0, 99), None);
    }
}
