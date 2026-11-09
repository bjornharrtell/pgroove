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
    Connect(String),
    Disconnect,
    CheckUpdates,
    InstallUpdate,
    ToggleUpdateChecks,
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
    let mut conns = vec![entry(NewConnection, "New connection…", &["add"])];
    conns.extend(
        connections
            .iter()
            .map(|n| entry(Connect(n.clone()), format!("Connect to {n}"), &["open"])),
    );
    groups.push(PaletteGroup {
        label: "Connections",
        entries: conns,
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

    fn ctx(connected: bool, read_only: bool, update: bool, auto: bool) -> PaletteContext {
        PaletteContext {
            connected,
            read_only,
            update_available: update,
            auto_check_updates: auto,
        }
    }

    fn titles(g: &[PaletteGroup], i: usize) -> Vec<&str> {
        g[i].entries.iter().map(|e| e.title.as_str()).collect()
    }

    #[test]
    fn offline_palette_hides_server_commands() {
        let g = groups(&["a".to_string()], &ctx(false, true, false, true));
        assert_eq!(g.len(), 4);
        assert_eq!(
            titles(&g, 3),
            ["Check for updates", "Turn off automatic update checks"]
        );
        assert!(!titles(&g, 0).contains(&"Run query"));
        assert_eq!(titles(&g, 1), ["Allow editing (turn off read-only)"]);
        assert_eq!(titles(&g, 2), ["New connection…", "Connect to a"]);
    }

    #[test]
    fn connected_palette_lists_everything() {
        let g = groups(&[], &ctx(true, false, true, false));
        assert!(titles(&g, 0).contains(&"Run query"));
        assert!(titles(&g, 0).contains(&"Analyze query"));
        assert!(titles(&g, 1).contains(&"Open activity monitor"));
        assert!(titles(&g, 1).contains(&"Disconnect"));
        assert_eq!(titles(&g, 1)[0], "Block writes (turn on read-only)");
        assert_eq!(titles(&g, 2), ["New connection…"]);
        assert_eq!(
            titles(&g, 3),
            [
                "Check for updates",
                "Install the available update",
                "Turn on automatic update checks"
            ]
        );
    }

    #[test]
    fn lookup_by_position() {
        let g = groups(&["x".to_string()], &ctx(true, true, false, true));
        assert_eq!(command_at(&g, 0, 0), Some(PaletteCommand::NewQuery));
        assert_eq!(
            command_at(&g, 2, 1),
            Some(PaletteCommand::Connect("x".into()))
        );
        assert_eq!(command_at(&g, 9, 0), None);
        assert_eq!(command_at(&g, 0, 99), None);
    }
}
