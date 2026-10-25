//! Version check, passive update notices and in-place update. Never used by the MCP server.
pub mod check;
pub mod method;
pub mod notice;

use chrono::Duration;
use semver::Version;
use serde::{Deserialize, Serialize};

use crate::domain::{AppError, Result, Timestamp};
use crate::repo::Repository;
use check::Release;
use method::InstallMethod;
use notice::Notice;

const CACHE_KEY: &str = "update.cache";
const DISMISSED_KEY: &str = "update.dismissed";
const CHECK_EVERY_HOURS: i64 = 24;
pub const EXIT_UPDATE_AVAILABLE: i32 = 10;

pub fn current_version() -> Version {
    Version::parse(env!("CARGO_PKG_VERSION")).expect("crate version is valid semver")
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct Cache {
    pub checked_at: Option<Timestamp>,
    pub release: Option<Release>,
    pub notice: Option<Notice>,
}

fn load<R: Repository>(repo: &mut R) -> Cache {
    repo.kv_get(CACHE_KEY)
        .ok()
        .flatten()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save<R: Repository>(repo: &mut R, cache: &Cache) {
    if let Ok(text) = serde_json::to_string(cache) {
        let _ = repo.kv_set(CACHE_KEY, &text);
    }
}

fn dismissed<R: Repository>(repo: &mut R) -> Vec<String> {
    repo.kv_get(DISMISSED_KEY)
        .ok()
        .flatten()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn is_stale(cache: &Cache, now: Timestamp) -> bool {
    cache
        .checked_at
        .is_none_or(|t| now - t >= Duration::hours(CHECK_EVERY_HOURS))
}

pub fn env_disabled() -> bool {
    std::env::var_os("PROJECTGUI_NO_UPDATE_CHECK").is_some()
}

/// Fetches fresh data and stores it. Used by `update --check` and by the background refresh.
pub fn refresh<R: Repository>(repo: &mut R, now: Timestamp) -> std::result::Result<Cache, String> {
    let ep = check::endpoints()
        .ok_or("update source is not configured (set `repository` in Cargo.toml)")?;
    let fetched = check::fetch(&ep)?;
    let cache = Cache {
        checked_at: Some(now),
        release: fetched.release,
        notice: fetched.notice,
    };
    save(repo, &cache);
    Ok(cache)
}

pub fn newer(cache: &Cache, current: &Version) -> Option<Release> {
    cache.release.clone().filter(|r| &r.version > current)
}

/// Lines to print for an interactive command, plus the notice id to remember as shown.
pub fn passive_lines(
    cache: &Cache,
    dismissed: &[String],
    current: &Version,
    now: Timestamp,
) -> (Vec<String>, Option<String>) {
    let mut lines = vec![];
    if let Some(r) = newer(cache, current) {
        lines.push(format!(
            "projectgui {} is available (you have {current}). Run `projectgui update`. {}",
            r.version, r.url
        ));
    }
    let mut shown = None;
    if let Some(n) = cache
        .notice
        .as_ref()
        .filter(|n| notice::should_show(n, current, dismissed, now))
    {
        let tag = match n.severity {
            notice::Severity::Info => "notice",
            notice::Severity::Warn => "warning",
            notice::Severity::Critical => "IMPORTANT",
        };
        lines.push(format!(
            "[{tag}] {}{}",
            n.message,
            n.url.as_ref().map(|u| format!(" {u}")).unwrap_or_default()
        ));
        shown = Some(n.id.clone());
    }
    (lines, shown)
}

/// For an interactive command or the GUI: the lines to show from the cache and, if it is stale, a refresh in a
/// detached child so the caller never waits for the network.
pub fn passive<R: Repository>(repo: &mut R, now: Timestamp) -> Vec<String> {
    let cache = load(repo);
    let mut seen = dismissed(repo);
    let (lines, shown) = passive_lines(&cache, &seen, &current_version(), now);
    if let Some(id) = shown.filter(|id| !seen.contains(id)) {
        seen.push(id);
        seen.truncate(50);
        if let Ok(text) = serde_json::to_string(&seen) {
            let _ = repo.kv_set(DISMISSED_KEY, &text);
        }
    }
    if is_stale(&cache, now) && check::endpoints().is_some() {
        // Mark the attempt first so a failing network is retried in 24 h, not on every command.
        save(
            repo,
            &Cache {
                checked_at: Some(now),
                ..cache
            },
        );
        if let Ok(exe) = std::env::current_exe() {
            let _ = std::process::Command::new(exe)
                .arg("__refresh-update-cache")
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn();
        }
    }
    lines
}

pub fn detect_method() -> InstallMethod {
    // A receipt only counts when it describes this executable: a dev build or another copy is not the installed one.
    let mut updater = axoupdater::AxoUpdater::new_for("projectgui");
    let has_receipt = updater.load_receipt().is_ok()
        && updater
            .check_receipt_is_for_this_executable()
            .unwrap_or(false);
    match std::env::current_exe() {
        Ok(exe) => method::classify(&exe.canonicalize().unwrap_or(exe), has_receipt),
        Err(_) => InstallMethod::Unknown,
    }
}

/// Upgrades in place; only valid for `InstallMethod::Installer`.
pub fn run_installer() -> Result<String> {
    let mut updater = axoupdater::AxoUpdater::new_for("projectgui");
    updater
        .load_receipt()
        .map_err(|e| AppError::Storage(format!("install receipt not found: {e}")))?;
    match updater
        .run_sync()
        .map_err(|e| AppError::Storage(format!("update failed: {e}")))?
    {
        Some(r) => Ok(format!("Updated to {}", r.new_version)),
        None => Ok("Already up to date".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cache(latest: &str, notice_json: Option<&str>) -> Cache {
        Cache {
            checked_at: Some(chrono::Utc::now()),
            release: Some(Release {
                version: Version::parse(latest).unwrap(),
                url: "https://example.com/r".into(),
            }),
            notice: notice_json.and_then(notice::parse),
        }
    }

    #[test]
    fn throttle_is_24_hours() {
        let now = chrono::Utc::now();
        assert!(is_stale(&Cache::default(), now));
        assert!(!is_stale(
            &Cache {
                checked_at: Some(now - Duration::hours(23)),
                ..Default::default()
            },
            now
        ));
        assert!(is_stale(
            &Cache {
                checked_at: Some(now - Duration::hours(25)),
                ..Default::default()
            },
            now
        ));
    }

    #[test]
    fn newer_release_produces_a_hint_and_equal_or_older_does_not() {
        let now = chrono::Utc::now();
        let cur = Version::new(0, 1, 0);
        let (lines, _) = passive_lines(&cache("0.2.0", None), &[], &cur, now);
        assert!(lines[0].contains("0.2.0") && lines[0].contains("projectgui update"));
        assert!(
            passive_lines(&cache("0.1.0", None), &[], &cur, now)
                .0
                .is_empty()
        );
        assert!(
            passive_lines(&cache("0.0.9", None), &[], &cur, now)
                .0
                .is_empty()
        );
    }

    #[test]
    fn notice_lines_respect_dismissal() {
        let now = chrono::Utc::now();
        let cur = Version::new(0, 1, 0);
        let c = cache(
            "0.1.0",
            Some(r#"{"id":"n1","severity":"warn","message":"Heads up"}"#),
        );
        let (lines, shown) = passive_lines(&c, &[], &cur, now);
        assert_eq!(
            (lines, shown),
            (
                vec!["[warning] Heads up".to_string()],
                Some("n1".to_string())
            )
        );
        assert!(passive_lines(&c, &["n1".into()], &cur, now).0.is_empty());
    }
}
