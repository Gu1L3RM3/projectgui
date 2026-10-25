use std::time::Duration;

use semver::Version;
use serde::{Deserialize, Serialize};

use super::notice::{self, Notice};

const TIMEOUT: Duration = Duration::from_secs(3);
const MAX_BODY: u64 = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Release {
    pub version: Version,
    pub url: String,
}

pub struct Endpoints {
    pub latest: String,
    pub notice: String,
    pub releases_page: String,
}

/// `PROJECTGUI_UPDATE_BASE` redirects both lookups (`{base}/latest`, `{base}/notice.json`); meant for tests and mirrors.
pub fn endpoints() -> Option<Endpoints> {
    if let Ok(base) = std::env::var("PROJECTGUI_UPDATE_BASE") {
        let base = base.trim_end_matches('/');
        return Some(Endpoints {
            latest: format!("{base}/latest"),
            notice: format!("{base}/notice.json"),
            releases_page: format!("{base}/releases"),
        });
    }
    let repo = env!("CARGO_PKG_REPOSITORY").trim_end_matches('/');
    let path = repo.strip_prefix("https://github.com/")?;
    if path.starts_with("OWNER/") {
        return None; // repository not configured yet
    }
    Some(Endpoints {
        latest: format!("https://api.github.com/repos/{path}/releases/latest"),
        notice: format!("https://raw.githubusercontent.com/{path}/main/notice.json"),
        releases_page: format!("{repo}/releases"),
    })
}

fn get(url: &str) -> Result<String, String> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(TIMEOUT))
        .user_agent(concat!("projectgui/", env!("CARGO_PKG_VERSION")))
        .build()
        .into();
    let mut resp = agent.get(url).call().map_err(|e| e.to_string())?;
    resp.body_mut()
        .with_config()
        .limit(MAX_BODY)
        .read_to_string()
        .map_err(|e| e.to_string())
}

/// Accepts GitHub's release shape: `{"tag_name": "v1.2.3", "html_url": "https://..."}`.
pub fn parse_release(json: &str, fallback_url: &str) -> Option<Release> {
    #[derive(Deserialize)]
    struct Wire {
        tag_name: String,
        html_url: Option<String>,
        #[serde(default)]
        prerelease: bool,
    }
    let w: Wire = serde_json::from_str(json).ok()?;
    if w.prerelease {
        return None;
    }
    Some(Release {
        version: Version::parse(w.tag_name.trim().trim_start_matches('v')).ok()?,
        url: w
            .html_url
            .and_then(|u| notice::https_url(&u))
            .unwrap_or_else(|| fallback_url.to_string()),
    })
}

#[derive(Debug, Clone, Serialize)]
pub struct Fetched {
    pub release: Option<Release>,
    pub notice: Option<Notice>,
}

/// Network errors are returned as `Err` only when *nothing* could be fetched.
pub fn fetch(ep: &Endpoints) -> Result<Fetched, String> {
    let release = get(&ep.latest).map(|b| parse_release(&b, &ep.releases_page));
    let notice = get(&ep.notice).ok().and_then(|b| notice::parse(&b));
    match release {
        Err(e) if notice.is_none() => Err(e),
        r => Ok(Fetched {
            release: r.ok().flatten(),
            notice,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_github_releases_and_ignores_prereleases_and_garbage() {
        let r = parse_release(
            r#"{"tag_name":"v1.2.3","html_url":"https://github.com/x/y/releases/tag/v1.2.3"}"#,
            "fb",
        )
        .unwrap();
        assert_eq!(r.version, Version::new(1, 2, 3));
        assert_eq!(
            parse_release(r#"{"tag_name":"1.0.0","html_url":"javascript:x"}"#, "fb")
                .unwrap()
                .url,
            "fb"
        );
        assert!(parse_release(r#"{"tag_name":"v2.0.0-rc1","prerelease":true}"#, "fb").is_none());
        assert!(parse_release(r#"{"tag_name":"latest"}"#, "fb").is_none());
        assert!(parse_release("<html>", "fb").is_none());
    }
}
