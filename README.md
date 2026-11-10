# pgroove

A fast native PostgreSQL admin UI written in Rust with [gpui-kit](https://gpui-kit.com).

Requires PostgreSQL 13 or newer on the server.

## Features

- **Connections:** saved profiles, passwords in the OS keychain, connection URI import,
  TLS modes (`disable`, `prefer`, `require`, `verify-full`), test connection. Connections can
  have a group and a marker color (for example red for production); once there is more than
  one group, the sidebar lists them under collapsible group headers.
- **DBeaver import:** on the first start, if DBeaver connections are found, pgroove offers to
  import them (with their saved passwords, if you tick that). DBeaver folders become groups and
  its prod/test connection types become colors. Run it again any time from the command palette
  ("Import connections from DBeaver…"), or point it at a DBeaver project or workspace folder
  anywhere on disk ("Import connections from a DBeaver project folder…"). DBeaver's files are
  only read.
- **Schema browser:** a search box finds tables, views and functions across every schema; lists every database on the server (optional per connection; click to open or close one; several can be open at once, across connections), then a lazy tree of schemas: relations (tables, views and sequences), then types, then functions, plus
  extensions, with context menus (open data or structure, copy name, show definition).
- **SQL editor:** Run always limits the rows it fetches (200 by default, adjustable up to 10 000 with the Limit button) and says so when the limit cuts a result. Full results go through **Export CSV…**, which streams one query to a file without loading it into memory. Several tabs, syntax highlighting, schema-aware completion, formatter,
  history, snippets, error position highlighting, cancel, `EXPLAIN` and `EXPLAIN ANALYZE`
  with a plan tree that marks the hottest node.
- **Data viewer:** paged (50 to 10 000 rows per page, remembered), server-side sorting, filter expression, copy or save the page as CSV or JSON, **Export all…** to stream the whole table (with the current filter and sort) to a CSV file,
  cell inspector with pretty-printed JSON. Select cells freely in any result grid: drag to mark
  a block, Shift-click to extend it, Ctrl/Cmd-click to add more blocks, click a row number for a
  whole row, `Ctrl+A` for everything. `Ctrl+C` copies the marked cells as tab-separated text
  (paste into a spreadsheet) and `Ctrl+Shift+C` adds the column names. Deleting rows in the data
  viewer acts on every marked row.
- **Table structure:** columns, constraints (foreign keys are links), indexes, triggers,
  sizes and reconstructed DDL.
- **Editing:** add, duplicate, change and delete rows in any ordinary table, including tables without a primary key (rows are then matched by physical position and version, and a row changed in the meantime is refused). Edits are staged, you review the generated SQL, and it is applied atomically.
  Writes are blocked by default through a server-side read-only session; switch it off in the
  title bar to edit.
- **Monitoring:** sessions (cancel and terminate), blocking locks, `pg_stat_statements`,
  database and table sizes, replication, settings.
- **Desktop integration:** follows the system light or dark theme by default (Catppuccin Latte and Mocha), with the appearance and theme switchable from the command palette
  (`Ctrl+K`), and restores your open databases, tabs (queries, tables, activity monitors, the one in front), expanded schemas, search text and window layout.

## Build and run

Building needs a current stable Rust toolchain; the workspace uses edition 2024.

```bash
cargo run --release -p pgroove-app
```

On Linux you need the usual GPUI dependencies (Vulkan, Wayland or X11 and xkbcommon headers,
fontconfig). See `.github/workflows/ci.yml` for the exact package list.

## Downloads

Tagged releases publish a Linux x86_64 AppImage and tarball, a macOS Apple silicon
(arm64) disk image (`.dmg` with `pgroove.app`) and tarball, and a Windows x86_64 zip.

The macOS app is not notarized, so Gatekeeper blocks the first launch. Drag it to
Applications, then either right-click it and choose Open, or clear the download flag:
`xattr -cr /Applications/pgroove.app`. If macOS says the app is "damaged", it is that flag
(older builds also lacked a signature); the same command fixes it.

The AppImage relies on the system's graphics libraries (Vulkan, Wayland or X11); make it executable and run it. On first start it installs a launcher entry and icon under
`~/.local/share` (set `PGROOVE_NO_INTEGRATION=1` to skip), so it appears in menus and can be
pinned to a dock:

```bash
chmod +x pgroove-*.AppImage && ./pgroove-*.AppImage
```

## Themes

The command palette (`Ctrl+K`) has an **Appearance** section: follow the system, always light, or
always dark, and a list of themes. Picking a theme uses it for its mode and switches to that mode.
The bundled themes are the four Catppuccin flavors (Latte; Frappé, Macchiato and Mocha).

To add your own, put theme files (`*.json`, the format of the files in
[gpui-kit's `themes` folder](https://github.com/longbridge/gpui-kit/tree/main/themes)) into the
`themes` folder next to `settings.toml`, then run **Load new theme files** (or restart). The choice is
stored in `settings.toml`:

```toml
appearance = "system"              # or "light" / "dark"
light_theme = "Catppuccin Latte"
dark_theme = "Catppuccin Mocha"
```

Edits to a theme that is already loaded need a restart; an unknown theme name falls back to the default.

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

## Keyboard

Click the schema tree (or press Down from the search box) and use the arrow keys: Up and Down move
the cursor, Right and Left expand and collapse, Enter does what a click does (open a table, expand
a schema, show a function's definition). The table open in the front tab is highlighted in the tree.

## Layout

Drag the dividers to resize the sidebar, the editor and results panes, and the cell inspector below
a grid. The sidebar width and editor height are remembered in `session.toml`.

## Files

| File | Location | Purpose |
|---|---|---|
| `connections.toml` | config dir `pgroove/` | saved connection profiles (no passwords) |
| `settings.toml` | config dir `pgroove/` | preferences (update checks, row limit, page size, theme) |
| `themes/*.json` | config dir `pgroove/` | your own themes |
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
`format_sql` (`ctrl-shift-f`), `open_palette` (`ctrl-k`), `toggle_fullscreen` (`f11`).

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
