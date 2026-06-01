//! One-shot "is there a newer release?" check against GitHub Releases.
//!
//! Runs once at startup on a background task. Any failure is swallowed (a
//! version check is never worth surfacing as an error); on success, if the
//! latest published release is newer than the running build, the UI shows an
//! unobtrusive badge in the status bar.

use std::time::Duration;

use anyhow::{Context, Result};

const RELEASES_API: &str = "https://api.github.com/repos/mario-chamuty/spotuify/releases/latest";
const RELEASES_PAGE: &str = "https://github.com/mario-chamuty/spotuify/releases/latest";

#[derive(Debug, Clone)]
pub struct UpdateInfo {
    /// The newer version without the `v` prefix, e.g. "0.1.4".
    pub latest: String,
    /// Web page for the release.
    pub url: String,
}

/// Return `Some` if GitHub's latest release is newer than this build, else
/// `None` (including on any network/parse error).
pub async fn check_latest() -> Option<UpdateInfo> {
    match fetch().await {
        Ok(info) => info,
        Err(e) => {
            tracing::debug!("update check failed: {e:#}");
            None
        }
    }
}

async fn fetch() -> Result<Option<UpdateInfo>> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(8))
        .user_agent(concat!("spotuify/", env!("CARGO_PKG_VERSION")))
        .build()?;
    let body = client
        .get(RELEASES_API)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .context("github releases request failed")?
        .error_for_status()
        .context("github releases returned an error")?
        .text()
        .await
        .context("reading github releases")?;

    let json: serde_json::Value =
        serde_json::from_str(&body).context("parsing release json")?;
    let tag = json
        .get("tag_name")
        .and_then(|t| t.as_str())
        .context("release has no tag_name")?;
    let url = json
        .get("html_url")
        .and_then(|u| u.as_str())
        .unwrap_or(RELEASES_PAGE)
        .to_string();

    let latest = tag.trim_start_matches('v').to_string();
    if is_newer(&latest, env!("CARGO_PKG_VERSION")) {
        Ok(Some(UpdateInfo { latest, url }))
    } else {
        Ok(None)
    }
}

/// Is dotted-numeric version `a` strictly newer than `b`?
fn is_newer(a: &str, b: &str) -> bool {
    parse(a) > parse(b)
}

/// Parse `major.minor.patch` into a comparable tuple, ignoring any pre-release
/// suffix (e.g. "0.2.0-rc1" -> (0, 2, 0)).
fn parse(v: &str) -> (u32, u32, u32) {
    let mut parts = v.split('.').map(|p| {
        p.chars()
            .take_while(|c| c.is_ascii_digit())
            .collect::<String>()
            .parse()
            .unwrap_or(0)
    });
    (
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_newer_versions() {
        assert!(is_newer("0.1.4", "0.1.3"));
        assert!(is_newer("0.2.0", "0.1.9"));
        assert!(is_newer("1.0.0", "0.9.9"));
        assert!(!is_newer("0.1.3", "0.1.3"));
        assert!(!is_newer("0.1.2", "0.1.3"));
    }

    #[test]
    fn parse_ignores_prefix_and_suffix() {
        assert_eq!(parse("0.1.3"), (0, 1, 3));
        assert_eq!(parse("0.2.0-rc1"), (0, 2, 0));
        assert_eq!(parse("1.2"), (1, 2, 0));
    }
}
