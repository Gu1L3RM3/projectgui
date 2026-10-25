# The `.project-status` file

A single TOML file at the root of a project directory that describes the project. ProjectGui keeps it and its
database in step, both ways:

- **File -> ProjectGui:** edit the file, then run `pgui sync` (or `scan --apply`, or the GUI's Scan folder).
- **ProjectGui -> file:** a change made with the CLI, the GUI or an agent over MCP is written back into the file.

ProjectGui never creates the file on its own; only `pgui init-file` does, when you ask.

```toml
title = "Website redesign"        # required
description = "New marketing site"
priority = "high"                 # low | medium | high | critical
stage = "in-progress"             # idea | planning | in-progress | review | done | paused

[[task]]
title = "Write copy"              # required and unique in the project; tasks are matched by exact title
status = "done"                   # todo | in-progress | blocked | done (default todo)
description = "Landing and pricing pages"
priority = "medium"

[[reference]]
kind = "pr"                       # commit | pr | issue
value = "#12"
url = "https://github.com/acme/site/pull/12"
```

Keys that are not listed here are rejected, with the key named in the error. A file is at most 256 KiB and must be a
regular file (a symlink is refused).

## How a sync works

- **The directory identifies the project.** The project whose linked path equals the file's directory is updated; if
  there is none, a project is created and linked to that directory. Two projects linked to one directory is an error.
- **Keys you leave out are left alone.** An absent `description`, `priority`, `stage`, task `status`, and so on keep
  their stored value. The slug never changes with the title.
- **Tasks are matched by title.** A task in the file that is not stored is added (in file order); a stored one gets
  the status, description and priority the file gives.
- **A sync never deletes.** A task that is stored but no longer in the file is kept, reported as "not in the file",
  and written back into the file. To remove a task, use `pgui task rm` (or Remove in the GUI); the removal is then
  written to the file too.
- **References are added** when the `(kind, value)` pair is new, never removed.
- **It is idempotent.** Syncing the same file twice changes nothing.
- **It is all or nothing.** The file is validated first and every change is written in one database transaction, so
  a bad file, a crash or a concurrent writer never leaves half a sync behind.

## How changes are written back

- After any change to a project that has a `.project-status`, the file is regenerated from the database and replaced
  atomically: the new text is written next to it, the old file is moved aside and checked, and the new one is put in
  place only if no other file appeared meanwhile. Tasks only the database had are added to the file the same way after
  a sync. For that instant there is no file, so a sync running at exactly that moment reports it missing.
- **Unsynced edits are never overwritten.** ProjectGui remembers the file content it last read or wrote. If the file
  changed since then (you edited it and have not synced yet), it is left untouched and the command prints a note
  (`note: …` on stderr; a `notices` field over MCP; the message line in the GUI). Run `pgui sync` to load your
  edits; the file's values win, so repeat the change if it was undone.
- A project without a file never gets one, and a file that is a symlink is never written.
- A file that `sync` would reject (two tasks with the same title, left over from before titles had to be unique) is
  not written; the command prints a note instead.
- The file is regenerated, so comments and the order of keys are not kept.

## Commands

```sh
pgui init-file [PROJECT] [--force]     # write a starter file in the current directory
pgui sync [PATH] [--dry-run]           # load the file in PATH (default: current directory)
pgui scan [DIR] [--depth N] [--apply]  # find files under DIR; a dry run unless --apply
```

`scan` looks up to `--depth` levels down (default 3, maximum 6), skips hidden directories, `node_modules` and `target`,
does not follow symlinks, and stops after 200 files.

The GUI has a "Scan folder" form that does the same, and MCP clients get `scan_projects` and `sync_project_file`
(see `docs/agents.md`).
