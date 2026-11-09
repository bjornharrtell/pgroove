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
pub fn groups(connections: &[String], connected: bool, read_only: bool) -> Vec<PaletteGroup> {
    use PaletteCommand::*;
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
    groups
}

/// Finds the command at a (section, row) position of the rendered palette.
pub fn command_at(groups: &[PaletteGroup], section: usize, row: usize) -> Option<PaletteCommand> {
    Some(groups.get(section)?.entries.get(row)?.command.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn titles(g: &[PaletteGroup], i: usize) -> Vec<&str> {
        g[i].entries.iter().map(|e| e.title.as_str()).collect()
    }

    #[test]
    fn offline_palette_hides_server_commands() {
        let g = groups(&["a".to_string()], false, true);
        assert_eq!(g.len(), 3);
        assert!(!titles(&g, 0).contains(&"Run query"));
        assert_eq!(titles(&g, 1), ["Allow editing (turn off read-only)"]);
        assert_eq!(titles(&g, 2), ["New connection…", "Connect to a"]);
    }

    #[test]
    fn connected_palette_lists_everything() {
        let g = groups(&[], true, false);
        assert!(titles(&g, 0).contains(&"Run query"));
        assert!(titles(&g, 0).contains(&"Analyze query"));
        assert!(titles(&g, 1).contains(&"Open activity monitor"));
        assert!(titles(&g, 1).contains(&"Disconnect"));
        assert_eq!(titles(&g, 1)[0], "Block writes (turn on read-only)");
        assert_eq!(titles(&g, 2), ["New connection…"]);
    }

    #[test]
    fn lookup_by_position() {
        let g = groups(&["x".to_string()], true, true);
        assert_eq!(command_at(&g, 0, 0), Some(PaletteCommand::NewQuery));
        assert_eq!(
            command_at(&g, 2, 1),
            Some(PaletteCommand::Connect("x".into()))
        );
        assert_eq!(command_at(&g, 9, 0), None);
        assert_eq!(command_at(&g, 0, 99), None);
    }
}
