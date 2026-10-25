//! `pgui install-mcp`: register the MCP server with agent clients.
use std::path::{Path, PathBuf};

use clap::ValueEnum;
use serde_json::{Value, json};

use crate::domain::{AppError, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Target {
    Claude,
    Codex,
    Opencode,
    All,
}

const NAME: &str = "pgui";

impl Target {
    fn each(self) -> Vec<Target> {
        match self {
            Target::All => vec![Target::Claude, Target::Codex, Target::Opencode],
            one => vec![one],
        }
    }
}

fn exe_str(exe: &Path) -> String {
    exe.to_string_lossy().into_owned()
}

/// What `--write` would do, as text a person can run by hand.
pub fn snippet(target: Target, exe: &Path) -> String {
    let exe = exe_str(exe);
    match target {
        Target::Claude => format!("claude mcp add --scope user {NAME} -- {exe} mcp\n"),
        Target::Codex => format!("codex mcp add {NAME} -- {exe} mcp\n"),
        Target::Opencode => format!("# {}\n{}\n", opencode_config_path().display(), {
            let existing = std::fs::read_to_string(opencode_config_path())
                .ok()
                .and_then(|t| serde_json::from_str(&t).ok())
                .unwrap_or_else(|| json!({}));
            let entry = opencode_entry(&exe);
            let doc = if opencode_uses_servers_key(&existing, opencode_version().as_deref()) {
                json!({ "mcp": { "servers": { NAME: entry } } })
            } else {
                json!({ "mcp": { NAME: entry } })
            };
            serde_json::to_string_pretty(&doc).expect("json serializes")
        }),
        Target::All => Target::All
            .each()
            .into_iter()
            .map(|t| format!("== {t:?}\n{}", snippet(t, Path::new(&exe))))
            .collect(),
    }
}

fn opencode_entry(exe: &str) -> Value {
    json!({ "type": "local", "command": [exe, "mcp"], "enabled": true })
}

pub fn opencode_config_path() -> PathBuf {
    std::env::var_os("OPENCODE_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            dirs::home_dir()
                .unwrap_or_default()
                .join(".config/opencode/opencode.json")
        })
}

/// OpenCode 2.x keeps servers under `mcp.servers`; 1.x puts them directly under `mcp`.
/// An existing config decides; for a fresh one the installed version decides.
fn opencode_uses_servers_key(doc: &Value, version: Option<&str>) -> bool {
    match doc.get("mcp").and_then(Value::as_object) {
        Some(m) if m.contains_key("servers") => true,
        Some(m) if !m.is_empty() => false,
        _ => version
            .and_then(|v| {
                v.trim_start_matches('v')
                    .split('.')
                    .next()?
                    .parse::<u32>()
                    .ok()
            })
            .is_some_and(|major| major >= 2),
    }
}

fn opencode_version() -> Option<String> {
    run_client("opencode", &["--version"]).ok()
}

/// Adds the server to an opencode config document. Existing entries are left alone unless identical.
/// Returns false when the server was already registered with the same command.
pub fn merge_opencode(doc: &mut Value, exe: &str, version: Option<&str>) -> Result<bool> {
    let nested = opencode_uses_servers_key(doc, version);
    let root = doc
        .as_object_mut()
        .ok_or_else(|| AppError::invalid("opencode config", "top level is not an object"))?;
    let mcp = root
        .entry("mcp")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or_else(|| AppError::invalid("opencode config", "'mcp' is not an object"))?;
    let servers = if nested {
        mcp.entry("servers")
            .or_insert_with(|| json!({}))
            .as_object_mut()
            .ok_or_else(|| AppError::invalid("opencode config", "'mcp.servers' is not an object"))?
    } else {
        mcp
    };
    let entry = opencode_entry(exe);
    if let Some(existing) = servers.get(NAME) {
        if existing["command"] == entry["command"] {
            return Ok(false);
        }
        return Err(AppError::Conflict(format!(
            "opencode already has a different '{NAME}' server; edit it by hand"
        )));
    }
    servers.insert(NAME.into(), entry);
    Ok(true)
}

fn run_client(program: &str, args: &[&str]) -> Result<String> {
    let out = std::process::Command::new(program)
        .args(args)
        .output()
        .map_err(|e| {
            AppError::Storage(format!(
                "could not run `{program}`: {e} (is it installed and on PATH?)"
            ))
        })?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        Err(AppError::Storage(format!(
            "`{program} {}` failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        )))
    }
}

fn write_opencode(exe: &str) -> Result<String> {
    let path = opencode_config_path();
    let mut doc = match std::fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str(&text).map_err(|e| {
            AppError::Storage(format!(
                "{}: {e} (comments are not supported; edit by hand)",
                path.display()
            ))
        })?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            json!({ "$schema": "https://opencode.ai/config.json" })
        }
        Err(e) => return Err(AppError::Storage(format!("{}: {e}", path.display()))),
    };
    if !merge_opencode(&mut doc, exe, opencode_version().as_deref())? {
        return Ok(format!("opencode: already registered ({})", path.display()));
    }
    let mut backed_up = false;
    if path.exists() {
        let backup = path.with_extension("json.projectgui-bak");
        std::fs::copy(&path, &backup)
            .map_err(|e| AppError::Storage(format!("backup {}: {e}", backup.display())))?;
        backed_up = true;
    } else if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| AppError::Storage(format!("{}: {e}", dir.display())))?;
    }
    let text = serde_json::to_string_pretty(&doc).expect("json serializes");
    std::fs::write(&path, text + "\n")
        .map_err(|e| AppError::Storage(format!("{}: {e}", path.display())))?;
    let note = if backed_up {
        " (backup saved next to it)"
    } else {
        ""
    };
    Ok(format!("opencode: registered in {}{note}", path.display()))
}

pub fn write(target: Target, exe: &Path) -> Result<Vec<String>> {
    let exe = exe_str(exe);
    target
        .each()
        .into_iter()
        .map(|t| match t {
            Target::Claude => run_client(
                "claude",
                &["mcp", "add", "--scope", "user", NAME, "--", &exe, "mcp"],
            )
            .map(|_| "claude: registered (user scope)".to_string()),
            Target::Codex => run_client("codex", &["mcp", "add", NAME, "--", &exe, "mcp"])
                .map(|_| "codex: registered".to_string()),
            Target::Opencode => write_opencode(&exe),
            Target::All => unreachable!("expanded by each()"),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snippets_use_the_absolute_binary_and_the_mcp_subcommand() {
        let exe = Path::new("/opt/bin/pgui");
        assert_eq!(
            snippet(Target::Claude, exe),
            "claude mcp add --scope user pgui -- /opt/bin/pgui mcp\n"
        );
        assert!(snippet(Target::Codex, exe).contains("codex mcp add pgui -- /opt/bin/pgui mcp"));
        assert!(snippet(Target::Opencode, exe).contains("\"command\""));
        assert!(snippet(Target::All, exe).matches("== ").count() == 3);
    }

    #[test]
    fn opencode_merge_preserves_other_servers_and_is_idempotent() {
        let mut doc =
            json!({ "theme": "x", "mcp": { "other": { "type": "local", "command": ["o"] } } });
        assert!(
            merge_opencode(&mut doc, "/p/pgui", Some("v2.1.3")).unwrap(),
            "v1 layout is kept when entries already exist"
        );
        assert!(!merge_opencode(&mut doc, "/p/pgui", None).unwrap());
        assert_eq!(doc["mcp"]["other"]["command"][0], "o");
        assert_eq!(doc["theme"], "x");
        assert_eq!(doc["mcp"]["pgui"]["command"], json!(["/p/pgui", "mcp"]));
        assert!(merge_opencode(&mut doc, "/elsewhere/pgui", None).is_err());
        assert!(merge_opencode(&mut json!([]), "x", None).is_err());
        assert!(merge_opencode(&mut json!({ "mcp": 3 }), "x", None).is_err());
    }

    #[test]
    fn opencode_layout_follows_existing_config_then_version() {
        let mut v2 = json!({ "mcp": { "servers": { "a": { "command": ["a"] } } } });
        merge_opencode(&mut v2, "/p", None).unwrap();
        assert!(v2["mcp"]["servers"]["pgui"].is_object());
        let mut fresh_new = json!({});
        merge_opencode(&mut fresh_new, "/p", Some("v2.1.3")).unwrap();
        assert!(fresh_new["mcp"]["servers"]["pgui"].is_object());
        let mut fresh_old = json!({});
        merge_opencode(&mut fresh_old, "/p", Some("1.4.0")).unwrap();
        assert!(fresh_old["mcp"]["pgui"].is_object());
    }
}
