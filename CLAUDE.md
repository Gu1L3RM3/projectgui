# CLAUDE.md

Guidance for working in this repository. User-facing documentation is `README.md` and `docs/`; keep them in step
with the code.

## What this is

ProjectGui: a local project tracker. Projects have a priority, a stage, tasks and a progress bar derived from finished
tasks. The same SQLite database is used by a CLI, an MCP server for coding agents (Claude Code, Codex, OpenCode), an
iced GUI and a Linux tray indicator. No daemon, no cloud; the network is used only for the optional update check. One Rust package and one binary named `projectgui`, plus a separate COSMIC panel applet crate in `applet/`. Everything is written in English: code, CLI
output, MCP descriptions, docs, commit messages.

## Commands

```sh
cargo build
cargo test                                  # unit + integration (tests/ spawns the real binary)
cargo fmt --check
cargo clippy --all-targets -- -D warnings
```

Run these for every feature combination before calling work done:

```sh
cargo test --no-default-features
cargo test --no-default-features --features mcp
cargo test --no-default-features --features update
cargo test --no-default-features --features gui
```

Try the CLI against a throwaway database, never the user's real one:

```sh
export PROJECTGUI_DB=/tmp/pg.db PROJECTGUI_CONFIG=/tmp/pg.toml
cargo run -- add "Demo" -p high -t "first task"
cargo run -- status
```

## Layout

```
src/main.rs            entry: calls cli::run()
src/config.rs          config file + database path resolution
src/domain/            pure types and rules (no I/O, no SQL)
  enums.rs             Priority, Stage, TaskStatus, RefKind (string enums via string_enum!)
  model.rs             Project, Task, Reference, Progress, slugify, validation, project/task lookup
  error.rs             AppError: stable codes, CLI exit codes, JSON form for MCP
src/service/           use cases; the only place business rules live
  mod.rs               Service<R: Repository>: create/update/delete, tasks, references
  query.rs             Filter, sorting, project_for_path, Summary
  backup.rs            backup, JSON export/import
  projectfile.rs       `.project-status` (TOML): parse, plan, sync_dir (one write_batch), scan, mirror (DB -> file); docs/project-file.md
src/repo/              Repository trait + SqliteRepository, migrations/*.sql
src/cli/               clap args, rendering, colors, install-mcp, update command
src/gui/               iced window: projects, tasks, references, update banner; theme.rs = palettes; tray.rs = Linux tray (ksni) + single-instance socket (feature `gui`, run with `projectgui gui`)
src/mcp/               rmcp server: tools, resources, prompts, strict argument parsing
src/update/            release check, notice.json parsing, install-method detection (feature `update`)
tests/                 cli_e2e, mcp_protocol, repo_concurrency, update (+ tests/common helpers)
assets/                app icon (color + symbolic SVG) and the launcher .desktop entry
applet/                COSMIC panel applet (own crate and Cargo.lock, libcosmic pinned to COSMIC 1.0.0); reads and
                       changes data only through the `projectgui` CLI (`--json list`, `task done/reopen`); install.sh
docs/                  agents.md (MCP setup), update.md (update + notice.json), project-file.md (.project-status), images/ (README screenshots)
.github/workflows/     ci.yml (fmt, clippy, tests on Linux/macOS/Windows x feature sets), release.yml (cargo-dist, on tag v*)
dist-workspace.toml    cargo-dist config: targets, installers, release features
```

## Architecture rules

- Dependencies point down: adapters (cli, mcp, gui, tray) -> service -> domain. `repo` implements the trait the
  service uses. `update` is independent of domain, service and repo (it only needs `kv_get`/`kv_set`).
- Adapters stay thin: parse input, call the service, render output. No business rules in `cli/`, `mcp/` or a GUI.
- `Repository` is one concrete sync trait. Do not add generic layers, async, or a second implementation without a
  concrete need.
- Only MCP uses tokio, and it calls the service through `spawn_blocking`.
- Progress is always derived (`Project::progress`). Never store a percentage.
- Cargo features gate optional adapters (`mcp`, `update`, `gui`; `gui` is opt-in, not in `default`). Code used only by a feature carries
  `#[cfg(feature = "...")]` so every feature combination builds without warnings.

## Data and identity

- Ids are ULIDs and never change. Slugs are human lookups and may change; old slugs stay as aliases.
- Timestamps are UTC, stored as RFC 3339 text with millisecond precision.
- Schema changes are new numbered files in `src/repo/migrations/` plus a new entry in `MIGRATIONS`
  (`src/repo/migrations.rs`). Never edit an applied migration. Add a test with an older-schema fixture.
- Every mutation runs in one `BEGIN IMMEDIATE` transaction. CLI, MCP and GUI may write at the same time from
  different processes, so never hold state in memory between operations.

## Rules that must not regress

- **Mutations require exact ids, slugs or aliases.** Only read-only lookups may be fuzzy. MCP mutations accept only
  the full 26-character id.
- **`.project-status` is kept in step both ways.** File -> DB (`sync_dir`) is one transaction (`write_batch` for a new
  project, `modify_project` for an existing one) and never deletes (tasks missing from the file are kept, reported, and
  written back). DB -> file (`mirror`) runs after every service mutation:
  it rewrites an existing file atomically (temp file, move the old one aside and re-check it, then a no-replace hard link),
  never creates one (only `init-file` does), never writes text `parse` would reject, and never overwrites a
  file edited since ProjectGui last read or wrote it (content kept in `kv` as `project_file:<id>`); that case becomes a notice.
  Adapters drain `Service::take_notices()`: CLI on stderr, MCP as a `notices` field, GUI in the message line.
  `sync_project_file` and `scan_projects` take a directory instead of an id (the file defines the project); they read only a
  regular file literally named `.project-status` and the walk is bounded.
- **Read-modify-write goes through `Repository::modify_project`**, which reads and writes in one transaction. Never write
  back a `Project`/`Task` copy read in an earlier transaction: another process's change in between would be reverted.
- **Destructive actions are confirmed in the service**, not in the adapter: project deletion needs the exact title,
  task removal needs `confirm`. MCP `destructiveHint` is advisory and must never be the only protection.
- **MCP stdout is the protocol channel.** Nothing else may print to stdout in `mcp` mode (`#![deny(clippy::print_stdout)]`
  in `src/mcp`). Logs go to stderr. The MCP server never touches the network or prints update notices.
- Validate input at the service boundary (lengths, control characters, absolute paths, enum values). Errors name the
  field and list valid options.
- Update notices are untrusted network text: strip control characters, cap length, https-only URLs, never execute or
  download anything because of them. Passive checks run at most once per 24 h, in a detached child, and stay silent in
  `--json`, non-TTY and `mcp` modes and when opted out.
- Do not claim a platform works unless a test or CI runner on that platform passed.

## Conventions

- Errors: return `AppError` with a stable code; never panic on user input. CLI exit codes are in `AppError::exit_code`
  and documented in `--help`.
- CLI output: colors only on a TTY and when `NO_COLOR` is unset; every data command supports `--json`; tables use
  `cli/render.rs`. Closed pipes (`| head`) must not error.
- Keep comments to the why. A known limit gets a `shortcut:` comment stating the limit and when to upgrade it.
- Prefer extending an existing helper (`resolve_project`, `Filter`, `render::*`, `tests/common`) over adding a new one.

## Testing

- Write the failing test first for domain, service, repository and update logic.
- Unit tests sit next to the code; use `SqliteRepository::open_in_memory()` and the fixtures in
  `domain::model::fixtures`.
- Integration tests spawn the real binary with `tests/common::cmd`, which isolates `PROJECTGUI_DB`,
  `PROJECTGUI_CONFIG` and sets `NO_COLOR`. Never point tests at a real user database.
- `tests/update.rs` uses a local `tiny_http` stub through `PROJECTGUI_UPDATE_BASE`; tests never hit the internet.
  `PROJECTGUI_NOTICE_FORCE_TTY` makes passive notices testable.
- `tests/repo_concurrency.rs` runs several OS processes against one database; keep it green and non-flaky.
- Verify MCP changes against `tests/mcp_protocol.rs`, then manually with a real client when tool schemas change
  (`docs/agents.md` lists what was checked and with which versions).

## Environment variables

| Variable | Effect |
|---|---|
| `PROJECTGUI_DB` | database file path |
| `PROJECTGUI_CONFIG` | config file path |
| `NO_COLOR` | disable colors |
| `PROJECTGUI_NO_UPDATE_CHECK` | disable passive update checks |
| `PROJECTGUI_UPDATE_BASE` | redirect update lookups (`{base}/latest`, `{base}/notice.json`); for tests and mirrors |
| `PROJECTGUI_NOTICE_FORCE_TTY` | test hook: treat stderr as a terminal for passive notices |
| `PROJECTGUI_DEBUG` | verbose MCP logging on stderr |
| `COLORTERM` | `truecolor`/`24bit` makes the CLI use the GUI's exact palette; otherwise 16-color ANSI |
| `OPENCODE_CONFIG` | OpenCode config path used by `install-mcp` |

## MCP workflow when changing tools

1. Edit `src/mcp/tools.rs` (schema in `definitions()`, behavior in `call()`); keep schemas strict
   (`additionalProperties: false`) and argument parsing in `src/mcp/args.rs`.
2. Update prompts in `src/mcp/prompts.rs` if they mention the tool (a test checks that referenced tools exist).
3. Add or adjust tests in `src/mcp/tools.rs` and `tests/mcp_protocol.rs`.
4. Update `docs/agents.md`.

## What works and known gaps

- CLI, MCP server (`projectgui mcp`, `install-mcp`), `.project-status` sync, backup/export/import, update check and
  `notice.json`: done and tested.
- GUI (`projectgui gui`, feature `gui`): project and task CRUD, search/sort/filter, references, 5 themes (frosted
  translucent default, slate, paper, dusk, contrast; `gui_theme`, `gui_opacity` in the config), live reload via
  `data_version`, update banner. Linux tray (ksni) with top projects, update line, Open and Quit; a second
  `projectgui gui` focuses the running window through a socket in `$XDG_RUNTIME_DIR`. Closing the window quits
  (hide-to-tray needs `iced::daemon`).
- Tested by hand on Linux (COSMIC). macOS and Windows run the test suite in CI only; their GUI is unverified and they
  have no tray.
- COSMIC panel applet (`applet/`): panel icon with overall progress, popup with open projects, task checkboxes and
  Open ProjectGui. Panel icon checked on COSMIC; popup interaction awaits a human check; not built in CI. Build and check it with `cargo build --release`,
  `cargo clippy --release -- -D warnings` and `cargo test --release` inside `applet/`.
- Self-update (`projectgui update`) has not yet gone from one real release to the next.

## Releasing

Bump `version` in `Cargo.toml`, commit, then push a `vX.Y.Z` tag. `release.yml` (generated by `dist init`; regenerate
with `dist generate` after changing `dist-workspace.toml`, never hand-edit) builds the archives, shell/PowerShell
installers and checksums and publishes the GitHub Release. The installer receipt is what `projectgui update` uses.

## Working agreements

- Do not commit, push, publish or tag unless the user asks.
- Do not write to the user's real data directory, shell config, or agent configs (`~/.claude`, `~/.codex`,
  OpenCode) while testing; use temporary `HOME`, `CODEX_HOME`, `OPENCODE_CONFIG`, `PROJECTGUI_DB`.
- Before a commit: run the full check list above. Keep commits focused and describe the why.
