//! Remote `notice.json`: a short message the maintainer can publish, e.g. "please update".
//! It is display-only data from the network, so everything is sanitised and nothing in it is ever executed.
use chrono::DateTime;
use semver::Version;
use serde::Deserialize;

use crate::domain::Timestamp;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Info,
    Warn,
    Critical,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, Deserialize)]
pub struct Notice {
    pub id: String,
    pub severity: Severity,
    pub message: String,
    pub min_supported: Option<Version>,
    pub url: Option<String>,
    pub expires: Option<Timestamp>,
}

/// Raw wire shape; lenient so that one bad field drops that field, not the whole notice.
#[derive(Deserialize)]
struct Wire {
    id: Option<String>,
    severity: Option<String>,
    message: Option<String>,
    min_supported: Option<String>,
    url: Option<String>,
    expires: Option<String>,
}

const MAX_MESSAGE: usize = 200;
const MAX_ID: usize = 64;

/// Removes control characters (ANSI escapes, newlines, ...) and caps the length.
pub fn sanitize(text: &str, max: usize) -> String {
    let flat: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let flat = flat.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        flat
    } else {
        format!("{}…", flat.chars().take(max - 1).collect::<String>())
    }
}

pub fn https_url(url: &str) -> Option<String> {
    let url = url.trim();
    (url.starts_with("https://")
        && url.len() <= 300
        && !url.chars().any(|c| c.is_control() || c.is_whitespace()))
    .then(|| url.to_string())
}

/// `None` when the document is unusable (not JSON, no id, no message after sanitising).
pub fn parse(json: &str) -> Option<Notice> {
    let w: Wire = serde_json::from_str(json).ok()?;
    let id = sanitize(&w.id?, MAX_ID);
    let message = sanitize(&w.message?, MAX_MESSAGE);
    if id.is_empty() || message.is_empty() {
        return None;
    }
    Some(Notice {
        id,
        severity: match w.severity.as_deref() {
            Some("critical") => Severity::Critical,
            Some("warn") => Severity::Warn,
            _ => Severity::Info,
        },
        message,
        min_supported: w
            .min_supported
            .and_then(|v| Version::parse(v.trim_start_matches('v')).ok()),
        url: w.url.and_then(|u| https_url(&u)),
        expires: w
            .expires
            .and_then(|e| DateTime::parse_from_rfc3339(&e).ok())
            .map(|d| d.with_timezone(&chrono::Utc)),
    })
}

/// Each notice is shown once, unless it is critical or this build is older than `min_supported`.
pub fn should_show(n: &Notice, current: &Version, dismissed: &[String], now: Timestamp) -> bool {
    if n.expires.is_some_and(|e| e <= now) {
        return false;
    }
    let forced =
        n.severity == Severity::Critical || n.min_supported.as_ref().is_some_and(|m| current < m);
    forced || !dismissed.contains(&n.id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    #[test]
    fn parses_a_full_notice() {
        let n = parse(r#"{"id":"2026-11-sec-1","severity":"warn","message":"Update soon","min_supported":"v0.3.0","url":"https://example.com/x","expires":"2030-01-01T00:00:00Z"}"#).unwrap();
        assert_eq!(
            (n.severity, n.min_supported, n.url.as_deref()),
            (
                Severity::Warn,
                Some(v("0.3.0")),
                Some("https://example.com/x")
            )
        );
        assert!(n.expires.is_some());
    }

    #[test]
    fn strips_escape_sequences_and_caps_length() {
        let n = parse(&serde_json::json!({ "id": "a\u{1b}[2Jb", "message": format!("\u{1b}]0;pwn\u{7}Hi\nthere {}", "x".repeat(500)) }).to_string()).unwrap();
        assert!(
            !n.id.contains('\u{1b}')
                && !n.message.contains('\u{1b}')
                && !n.message.contains('\n')
                && !n.message.contains('\u{7}')
        );
        assert_eq!(n.message.chars().count(), MAX_MESSAGE);
        assert!(n.message.starts_with("]0;pwn Hi there"));
    }

    #[test]
    fn rejects_unusable_documents_and_unsafe_urls() {
        assert!(parse("not json").is_none());
        assert!(parse(r#"{"id":"x"}"#).is_none());
        assert!(parse(r#"{"id":"x","message":"\u001b\u0007"}"#).is_none());
        let n = parse(r#"{"id":"x","message":"m","url":"http://insecure.example","min_supported":"nope","expires":"yesterday"}"#).unwrap();
        assert_eq!(
            (n.url, n.min_supported, n.expires, n.severity),
            (None, None, None, Severity::Info)
        );
        assert!(
            https_url("https://a.b/c d").is_none() && https_url("javascript:alert(1)").is_none()
        );
    }

    #[test]
    fn shown_once_unless_critical_or_below_min_supported() {
        let now = chrono::Utc::now();
        let mut n = parse(r#"{"id":"n1","message":"m"}"#).unwrap();
        assert!(should_show(&n, &v("1.0.0"), &[], now));
        assert!(!should_show(&n, &v("1.0.0"), &["n1".into()], now));
        n.min_supported = Some(v("2.0.0"));
        assert!(
            should_show(&n, &v("1.0.0"), &["n1".into()], now),
            "below min_supported always shown"
        );
        assert!(!should_show(&n, &v("2.0.0"), &["n1".into()], now));
        n.min_supported = None;
        n.severity = Severity::Critical;
        assert!(should_show(&n, &v("9.9.9"), &["n1".into()], now));
        n.expires = Some(now - chrono::Duration::days(1));
        assert!(
            !should_show(&n, &v("1.0.0"), &[], now),
            "expired notices never show"
        );
    }
}
