# Connecting agents (MCP)

`pgui mcp` is a stdio MCP server. It uses the same local database as the CLI and GUI, so
whatever an agent writes shows up immediately in `pgui status`.

## Register

```sh
pgui install-mcp all            # dry run: prints what would change
pgui install-mcp all --write    # Claude Code, Codex and OpenCode
```

| Client | What `--write` does |
|---|---|
| Claude Code | `claude mcp add --scope user pgui -- <path>/pgui mcp` |
| Codex | `codex mcp add pgui -- <path>/pgui mcp` |
| OpenCode | merges an entry into `opencode.json` (`mcp.servers` on 2.x, `mcp` on 1.x), keeps a `.projectgui-bak` copy |

The registered command is the absolute path of the binary that ran `install-mcp`; re-run it after
moving or reinstalling the binary.

## What the server exposes

**Tools.** Mutations take full 26-character ids; names, slugs and abbreviations are rejected.
`resolve_project` is the only fuzzy lookup and returns candidates with their ids.

| Read-only | Mutating | Destructive (server enforces confirmation) |
|---|---|---|
| `scan_projects` (lists `.project-status` files under a directory and what loading each would change), `list_projects` (filters: `priority`, `stage`, `min_progress`, `max_progress`, `done`, `include_archived`, `search`, `sort`, `reverse`), `get_project`, `find_project_for_path`, `resolve_project`, `get_summary` | `create_project`, `update_project`, `add_tasks`, `update_task`, `set_tasks_status`, `add_reference`, `sync_project_file` (loads the `.project-status` file in a directory; never deletes). Every mutating tool also writes the change back into the project's `.project-status` when it has one; if that file has unsynced edits it is left alone and the result carries a `notices` array to show the user | `delete_project` (needs `confirm_title` equal to the title), `remove_task` (needs `confirm: true`) |

Errors come back as tool results with `isError: true` and a JSON body
`{"code", "message", "field"?, "candidates"?}`. Codes: `not_found`, `ambiguous`, `invalid`,
`confirmation_required`, `conflict`, `busy`, `storage`. An unknown tool name is a protocol error.
The `destructiveHint`/`readOnlyHint` annotations are advisory only; clients may ignore them, so
the server never relies on them.

**Resources.** `pgui://projects`, `pgui://projects/{id}`, `pgui://summary`.

**Prompts.** `track_this_project`, `sync_progress`, `plan_tasks`, `weekly_review`. These are text
templates a client can offer to a user or hand to a model. They are not background agents: nothing
runs unless the model chooses to call the tools above.

## Guarantees

- stdout carries only JSON-RPC (logs go to stderr; set `PROJECTGUI_DEBUG=1` for more).
- The MCP server never touches the network and never prints update notices.
- Concurrent use with the CLI/GUI is safe (SQLite WAL, one transaction per write).

## Verified against real clients

Checked on Linux with these versions; other platforms are untested.

| Client | Version | Result |
|---|---|---|
| Claude Code | 2.1.310 | `claude mcp list` connected; headless run created a project, added tasks and marked one done |
| Codex CLI | (installed via asdf) | `codex exec` created a project and marked a task done |
| OpenCode | 2.1.3 | `opencode mcp list` shows `pgui connected` (no model run) |
