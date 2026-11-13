//! Command palette: the list of commands and how they are grouped.

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
    /// Opens the settings tab, which holds every preference.
    OpenSettings,
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
    let mut app = vec![
        entry(
            OpenSettings,
            "Open settings",
            &[
                "preferences",
                "options",
                "theme",
                "appearance",
                "font",
                "size",
                "row limit",
                "updates",
            ],
        ),
        entry(CheckUpdates, "Check for updates", &["upgrade", "version"]),
    ];
    if ctx.update_available {
        app.push(entry(
            InstallUpdate,
            "Install the available update",
            &["upgrade"],
        ));
    }
    groups.push(PaletteGroup {
        label: "Application",
        entries: app,
    });
    groups
}

/// Finds the command at a (section, row) position of the rendered palette.
pub fn command_at(groups: &[PaletteGroup], section: usize, row: usize) -> Option<PaletteCommand> {
    Some(groups.get(section)?.entries.get(row)?.command.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(connected: bool, read_only: bool, update: bool) -> PaletteContext {
        PaletteContext {
            connected,
            read_only,
            update_available: update,
        }
    }

    fn titles(g: &[PaletteGroup], i: usize) -> Vec<&str> {
        g[i].entries.iter().map(|e| e.title.as_str()).collect()
    }

    #[test]
    fn offline_palette_hides_server_commands() {
        let g = groups(&["a".to_string()], &ctx(false, true, false));
        assert_eq!(g.len(), 4);
        assert_eq!(titles(&g, 3), ["Open settings", "Check for updates"]);
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
        let g = groups(&[], &ctx(true, false, true));
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
            titles(&g, 3),
            [
                "Open settings",
                "Check for updates",
                "Install the available update"
            ]
        );
    }

    /// Preferences live in the settings tab: the palette no longer carries one command per
    /// preference, only the way to open it.
    #[test]
    fn preferences_are_not_palette_commands() {
        let g = groups(&["a".to_string()], &ctx(true, false, true));
        let all: Vec<&str> = g
            .iter()
            .flat_map(|g| g.entries.iter().map(|e| e.title.as_str()))
            .collect();
        for gone in [
            "Appearance",
            "Theme:",
            "size:",
            "automatic update checks",
            "SQL keywords",
        ] {
            assert!(
                !all.iter().any(|t| t.contains(gone)),
                "{gone} should be in the settings tab, not the palette"
            );
        }
        assert_eq!(
            command_at(&g, 3, 0),
            Some(PaletteCommand::OpenSettings),
            "first of the application commands"
        );
        let entry = &g[3].entries[0];
        assert!(entry.keywords.contains(&"theme"), "found by what it holds");
        assert!(entry.keywords.contains(&"font"));
    }

    #[test]
    fn lookup_by_position() {
        let g = groups(&["x".to_string()], &ctx(true, true, false));
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
