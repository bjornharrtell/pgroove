# pgroove

A fast native PostgreSQL admin UI written in Rust with [gpui-kit](https://gpui-kit.com).

Requires PostgreSQL 13 or newer on the server.

## Features

- **Connections:** saved profiles, passwords in the OS keychain, connection URI import,
  TLS modes (`disable`, `prefer`, `require`, `verify-full`), test connection.
- **Schema browser:** lazy tree of schemas, tables, views, sequences, types, functions and
  extensions, with context menus (open data or structure, copy name, show definition).
- **SQL editor:** several tabs, syntax highlighting, schema-aware completion, formatter,
  history, snippets, error position highlighting, cancel, `EXPLAIN` and `EXPLAIN ANALYZE`
  with a plan tree that marks the hottest node.
- **Data viewer:** paged, server-side sorting, filter expression, copy or save as CSV or JSON,
  cell inspector with pretty-printed JSON.
- **Table structure:** columns, constraints (foreign keys are links), indexes, triggers,
  sizes and reconstructed DDL.
- **Editing:** stage edits, inserts and deletes, review the generated SQL, apply atomically.
  Writes are blocked by default through a server-side read-only session; switch it off in the
  title bar to edit.
- **Monitoring:** sessions (cancel and terminate), blocking locks, `pg_stat_statements`,
  database and table sizes, replication, settings.
- **Desktop integration:** follows the system light or dark theme, command palette
  (`Ctrl+K`), restores open tabs and window size.

## Build and run

```bash
cargo run --release -p pgroove-app
```

On Linux you need the usual GPUI dependencies (Vulkan, Wayland or X11 and xkbcommon headers,
fontconfig). See `.github/workflows/ci.yml` for the exact package list.

## Downloads

Tagged releases publish a Linux x86_64 AppImage and tarball, a macOS Apple silicon
(arm64) tarball and a Windows x86_64 zip. The AppImage relies on the system's graphics
libraries (Vulkan, Wayland or X11); make it executable and run it:

```bash
chmod +x pgroove-*.AppImage && ./pgroove-*.AppImage
```

## Files

| File | Location | Purpose |
|---|---|---|
| `connections.toml` | config dir `pgroove/` | saved connection profiles (no passwords) |
| `snippets.toml` | config dir `pgroove/` | saved snippets |
| `keybindings.toml` | config dir `pgroove/` | keybinding overrides |
| `history.toml` | local data dir `pgroove/` | query history (last 500) |
| `session.toml` | state dir `pgroove/` | open tabs and window size |

The config dir is `~/.config` on Linux, `~/Library/Application Support` on macOS and
`%APPDATA%` on Windows.

### Keybindings

Defaults can be replaced per command in `keybindings.toml`:

```toml
[keys]
run_query = "f5"                      # one key
open_palette = ["ctrl-p", "cmd-p"]    # several keys
format_sql = []                       # unbind
```

Commands: `run_query` (default `ctrl-enter`), `cancel_query` (`escape`),
`format_sql` (`ctrl-shift-f`), `open_palette` (`ctrl-k`).

## Development

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Tests that need a database are skipped unless configured:

```bash
# host:port:user:password:database
PGROOVE_TEST_PG=localhost:5432:postgres:secret:postgres cargo test --workspace
# a TLS-enabled server, for the TLS checks
PGROOVE_TEST_PG_TLS=localhost:5433:postgres:secret:postgres cargo test --workspace
# a running desktop keychain
PGROOVE_TEST_KEYRING=1 cargo test --workspace
```

Layout: `pgroove-core` (no UI: connections, catalog, queries, editing, monitoring),
`pgroove-ui` (gpui views), `pgroove-app` (the binary). The design notes live in
[PLAN.md](PLAN.md).
