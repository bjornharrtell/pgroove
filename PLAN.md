# pgroove — PostgreSQL admin UI plan

Native desktop (and later wasm) PostgreSQL admin tool in Rust, UI on `gpui` + `gpui-kit`.

## Goals

- Fast, keyboard-driven, low-memory alternative to pgAdmin/DBeaver.
- Core flows: connect, browse schema, run SQL, view/edit data, inspect server activity.
- Safe by default: read-only toggle, confirm destructive ops, no secrets in logs.

## Non-goals (v1)

- Multi-DB-engine support, ER-diagram editor, backup/restore GUI, user-management wizards.

## Architecture

Cargo workspace:

| Crate | Role |
|---|---|
| `pgroove-core` | Pure logic: connection model, catalog queries, query execution, result types. No UI deps. |
| `pgroove-ui` | gpui views/components built on `gpui-kit`. |
| `pgroove-app` | Binary: window, settings, wiring, logging (`log` facade). |

Key decisions:

- **Driver:** `tokio-postgres` (+ `rustls` TLS). Runs on a tokio runtime; results bridged to gpui via channels/`cx.spawn`. Evaluate wasm story later (would need a proxy; out of scope for v1).
- **Catalog access:** SQL against `pg_catalog` / `information_schema`, versioned per server major (supported: 13+; features such as `pg_stat_statements` timing columns rely on this).
- **State:** one `Workspace` model per window holding connections, open tabs, selection; views observe it.
- **Result handling:** stream rows in batches into a virtualized table; cancel via `pg_cancel_backend`/cancel token; hard row cap with "load more".
- **Secrets:** OS keychain (`keyring` crate); connection config in TOML under XDG config dir.
- **Testing:** `pgroove-core` integration tests against a Postgres container (testcontainers); UI logic kept in testable models. 80% patch coverage per AGENTS.md.

## UI layout

- Left sidebar: connections → databases → schemas → tables/views/functions/etc. tree (lazy loaded).
- Center: tabbed workspace (SQL editor, table data, table structure, activity).
- Bottom: results / messages / explain panel.
- Top: command palette (Ctrl+K), connection switcher, read-only indicator.
- Prefer `gpui-kit` components (tree, tabs, table, inputs, dialogs, palette); custom only where missing (SQL editor, virtualized grid if kit's is insufficient).

## Milestones

1. **M0 – Skeleton (done):** workspace, window, theming from gpui-kit, logging, CI (fmt, clippy, test).
2. **M1 – Connections (done: TLS modes, URI import, test connection):** connection form (host/port/db/user/SSL/URI), test connection, keychain storage, connect/disconnect, saved list.
3. **M2 – Schema browser (done):** lazy tree for schemas/tables/views/matviews/sequences/functions/types/extensions; context menus; refresh.
4. **M3 – SQL editor (done: multiple tabs, history, snippets, error highlighting):** multi-tab editor, syntax highlight, run selection/all, cancel, multi-statement results, timing, error position, history, saved snippets.
5. **M4 – Data viewer (done):** virtualized grid, sort/filter/paginate, copy/export (CSV/JSON), cell inspector (json, bytea, long text).
6. **M5 – Table structure (done; size shown as table/index/total, DDL reconstructed):** columns, indexes, constraints, FKs (navigable), triggers, DDL view, size/stats.
7. **M6 – Data editing (done; inline edit via inspector, add/delete rows, review, atomic apply, server-side read-only switch):** inline edit/insert/delete for tables with PK, pending-changes review, transaction wrap, read-only guard.
8. **M7 – Query tooling (done: Explain/Analyze plan view, schema-aware completion, formatter):** `EXPLAIN (ANALYZE, BUFFERS)` viewer with plan tree + hot-node highlight, autocomplete (schema-aware), formatter.
9. **M8 – Server monitoring (done: sessions with cancel/terminate, blocking, statements, databases, tables, replication, settings; auto-refresh):** `pg_stat_activity` (cancel/terminate), locks/blocking tree, `pg_stat_statements` top queries, replication, DB/table sizes, settings browser.
10. **M9 – Polish:** command palette, keybindings config, session restore, packaging (Linux/macOS/Windows), docs.

## Later / ideas

- Schema diff, DDL generation/migrations, ER diagram, import wizard, SSH tunnels, role/privilege viewer, wasm build via proxy.

## Risks

- gpui API churn → pin versions, isolate behind `pgroove-ui` wrappers.
- Text editor component: gpui lacks a mature one; may need to build or embed (tree-sitter highlight). Spike early in M3.
- Large result sets and memory: enforce batching/caps from M3.
- Version differences across Postgres majors in catalog queries: cover with matrix tests (13–18).

## Open questions

- Which `gpui-kit` components exist for table/tree/editor? Audit before M0 ends.
- Single vs multiple connections per window?
- License choice.
