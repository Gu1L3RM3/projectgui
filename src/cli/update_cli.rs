use std::io::{BufRead, IsTerminal, Write};

use serde_json::json;

use super::{Svc, io_err, out, out_json};
use crate::domain::{AppError, Result};
use crate::update::{self, check::Fetched, method::InstallMethod};

/// Passive notices only appear when a person is looking at the terminal.
/// `PROJECTGUI_NOTICE_FORCE_TTY` lets tests exercise that path.
pub fn interactive() -> bool {
    std::env::var_os("PROJECTGUI_NOTICE_FORCE_TTY").is_some() || std::io::stderr().is_terminal()
}

pub fn method_name() -> &'static str {
    update::detect_method().name()
}

pub fn latest_now() -> std::result::Result<Fetched, String> {
    let ep = update::check::endpoints().ok_or("update source is not configured")?;
    update::check::fetch(&ep)
}

pub fn describe_latest(l: std::result::Result<Fetched, String>) -> String {
    match l {
        Err(e) => format!("could not check for updates: {e}\n"),
        Ok(f) => match f.release {
            Some(r) if r.version > update::current_version() => {
                format!("latest: {} (update available: {})\n", r.version, r.url)
            }
            Some(r) => format!("latest: {} (up to date)\n", r.version),
            None => "latest: unknown\n".into(),
        },
    }
}

fn confirm(prompt: &str) -> Result<bool> {
    if !std::io::stdin().is_terminal() {
        return Err(AppError::ConfirmationRequired(
            "not a terminal; pass --yes to update".into(),
        ));
    }
    eprint!("{prompt} [y/N] ");
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut line)
        .map_err(|e| io_err("stdin", e))?;
    Ok(matches!(
        line.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

/// Exit code 10 with `--check` when an update is available, so scripts can branch on it.
pub fn update(svc: &mut Svc, check: bool, yes: bool, json: bool) -> Result<i32> {
    let current = update::current_version();
    let cache = update::refresh(svc.repo(), chrono::Utc::now())
        .map_err(|e| AppError::Storage(format!("could not check for updates: {e}")))?;
    let available = update::newer(&cache, &current);
    let method = update::detect_method();

    if json {
        out_json(&json!({
            "current": current.to_string(),
            "latest": cache.release.as_ref().map(|r| r.version.to_string()),
            "update_available": available.is_some(),
            "url": available.as_ref().map(|r| r.url.clone()),
            "install_method": method.name(),
            "notice": cache.notice,
        }));
    } else {
        match &available {
            Some(r) => out(&format!(
                "Update available: {current} → {} ({})\n",
                r.version, r.url
            )),
            None => out(&format!("projectgui {current} is up to date\n")),
        }
        if let Some(n) = &cache.notice {
            out(&format!(
                "[notice] {}{}\n",
                n.message,
                n.url.as_ref().map(|u| format!(" {u}")).unwrap_or_default()
            ));
        }
    }
    let Some(release) = available else {
        return Ok(0);
    };
    if check {
        return Ok(update::EXIT_UPDATE_AVAILABLE);
    }
    if method != InstallMethod::Installer {
        out(&format!(
            "Installed via {}: to upgrade, {}\n",
            method.name(),
            method.upgrade_hint()
        ));
        return Ok(0);
    }
    if !yes && !confirm(&format!("Update projectgui to {}?", release.version))? {
        return Err(AppError::ConfirmationRequired("update cancelled".into()));
    }
    out(&format!("{}\n", update::run_installer()?));
    Ok(0)
}
