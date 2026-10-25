# Updates and remote notices

```sh
projectgui --version
projectgui version --check     # also looks up the latest release
projectgui update --check      # exit code 0 = up to date, 10 = update available
projectgui update              # updates in place when installed with the official installer
```

`update` only modifies the binary when it was installed by the cargo-dist installer (an install
receipt exists). For Homebrew, `cargo install`, distro packages or manual installs it prints the
right command instead (`brew upgrade projectgui`, `cargo install projectgui --locked --force`, ...).

## Passive notice

After interactive commands (stderr is a terminal, no `--json`) ProjectGui prints, from a local
cache, a one-line hint when a newer release exists and any current remote notice. The cache is
refreshed at most once per 24 hours by a detached background process, so commands never wait on
the network. The MCP server and GUI-less/machine modes never check.

Opt out: `projectgui config set update.check false` or `PROJECTGUI_NO_UPDATE_CHECK=1`.
Requests are plain HTTPS GETs with no identifiers or telemetry; failures are silent.

## `notice.json`

Hosted at `notice.json` in the repository's default branch (`raw.githubusercontent.com/<owner>/<repo>/main/notice.json`).
Lets the maintainer tell users something without shipping a new binary.

```json
{
  "id": "2026-11-sec-1",
  "severity": "info | warn | critical",
  "message": "Please update to 0.3.1: fixes a data-loss bug.",
  "min_supported": "0.3.0",
  "url": "https://github.com/<owner>/projectgui/releases/tag/v0.3.1",
  "expires": "2026-12-31T00:00:00Z"
}
```

- `id` and `message` are required; the rest is optional.
- A notice is shown once per `id`, unless `severity` is `critical` or the running version is below
  `min_supported`, in which case it shows every time until the user updates. `expires` stops it.
- It is display-only text: control characters and escape sequences are stripped, the message is
  capped at 200 characters, `url` must be `https://`. Nothing in it is ever executed or downloaded.

## Source

Lookups go to the GitHub repository in `Cargo.toml` `repository` (releases API and `notice.json` on `main`).
Until the first release is published there, checks find nothing. `PROJECTGUI_UPDATE_BASE` redirects the
lookups to another server (`{base}/latest`, `{base}/notice.json`); the test suite uses it.
