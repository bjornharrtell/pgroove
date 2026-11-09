# pgroove

A fast native PostgreSQL admin UI written in Rust with [gpui-kit](https://gpui-kit.com).

Requires PostgreSQL 13 or newer on the server.

## Features

- **Connections:** saved profiles, passwords in the OS keychain, connection URI import,
  TLS modes (`disable`, `prefer`, `require`, `verify-full`), test connection.
- **Schema browser:** a search box finds tables, views and functions across every schema; lists every database on the server (click to open or close one; several can be open at once, across connections), then a lazy tree of schemas grouped into tables, views, sequences, types and functions, tables, views, sequences, types, functions and
  extensions, with context menus (open data or structure, copy name, show definition).
- **SQL editor:** Run always limits the rows it fetches (200 by default, adjustable up to 10 000 with the Limit button) and says so when the limit cuts a result. Full results go through **Export CSV…**, which streams one query to a file without loading it into memory. Several tabs, syntax highlighting, schema-aware completion, formatter,
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

## Updates

pgroove checks GitHub releases a few seconds after it starts and shows an **Update to x.y.z**
button in the title bar when a newer version exists. Clicking it shows the release notes;
**Install update** downloads the right file for your platform (the AppImage if you run one),
verifies its SHA-256 against the checksum published with the release, replaces the program,
and then offers **Restart to update**. Nothing is installed without your click, and a release
without a checksum file is refused.

Turn the startup check off from the command palette ("Turn off automatic update checks"),
by setting `check_for_updates = false` in `settings.toml`, or with the environment variable
`PGROOVE_NO_UPDATE_CHECK=1`. "Check for updates" in the palette checks on demand.
`PGROOVE_UPDATE_URL` points the check at another release feed.

## Files

| File | Location | Purpose |
|---|---|---|
| `connections.toml` | config dir `pgroove/` | saved connection profiles (no passwords) |
| `settings.toml` | config dir `pgroove/` | preferences (update checks, row limit) |
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

## Releasing

Push a tag like `v0.2.0` (after bumping `version` in the root `Cargo.toml`). The release workflow
builds every platform, publishes the archives with their checksums, and writes the release notes
from the Conventional Commit messages since the previous tag (`packaging/release-notes.sh`), so
keep commit subjects in the `type(scope): description` form.

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

## License

MIT, see [LICENSE](LICENSE).

The interface uses the [Catppuccin](https://github.com/catppuccin/catppuccin) Latte (light) and
Mocha (dark) palettes, which are MIT licensed. Copyright (c) 2021 Catppuccin.
