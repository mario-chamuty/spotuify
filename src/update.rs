//! One-shot "is there a newer release?" check against GitHub Releases.
//!
//! Runs once at startup on a background task. Any failure is swallowed (a
//! version check is never worth surfacing as an error); on success, if the
//! latest published release is newer than the running build, the UI shows an
//! unobtrusive badge in the status bar.

use std::time::Duration;

use anyhow::{Context, Result};

const REPO: &str = "mario-chamuty/spotuify";
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

/// The release-asset platform slug for the running build, or `None` if we don't
/// publish a prebuilt binary for it (so in-app update can't be offered).
fn platform_slug() -> Option<&'static str> {
    Some(match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", "x86_64") => "windows-x86_64",
        ("macos", "aarch64") => "macos-arm64",
        ("macos", "x86_64") => "macos-x86_64",
        ("linux", "x86_64") => "linux-x86_64",
        _ => return None,
    })
}

/// Whether this build can replace itself in place (a matching release asset
/// exists for its platform). The status-bar badge uses this to decide whether to
/// offer the in-app update key or just a link.
pub fn can_self_update() -> bool {
    platform_slug().is_some()
}

/// Release asset file name for `version` on this platform (e.g.
/// `spotuify-v0.1.7-windows-x86_64.zip`).
fn asset_name(version: &str) -> Option<String> {
    let slug = platform_slug()?;
    let ext = if cfg!(target_os = "windows") { "zip" } else { "tar.gz" };
    Some(format!("spotuify-v{version}-{slug}.{ext}"))
}

/// Download the release asset for this platform and replace the running binary
/// in place. Returns the installed version. Prints simple progress on the normal
/// screen, so call it only after the TUI has been torn down.
pub async fn download_and_install(info: &UpdateInfo) -> Result<String> {
    let asset = asset_name(&info.latest).context("no prebuilt binary for this platform")?;
    let url = format!("https://github.com/{REPO}/releases/download/v{}/{asset}", info.latest);

    println!("  Downloading {asset} …");
    let client = reqwest::Client::builder()
        .user_agent(concat!("spotuify/", env!("CARGO_PKG_VERSION")))
        .build()?;
    let bytes = client
        .get(&url)
        .send()
        .await
        .context("download request failed")?
        .error_for_status()
        .context("download returned an error")?
        .bytes()
        .await
        .context("reading the downloaded archive")?;

    println!("  Installing …");
    // Extraction + the executable swap are blocking filesystem work.
    tokio::task::spawn_blocking(move || install_blob(&bytes))
        .await
        .context("install task panicked")??;
    Ok(info.latest.clone())
}

/// Extract the binary from the downloaded archive and swap it over the running
/// executable.
fn install_blob(bytes: &[u8]) -> Result<()> {
    let exe = std::env::current_exe().context("locating the running executable")?;
    let dir = exe.parent().context("the executable has no parent directory")?;
    let binary = extract_binary(bytes)?;

    // Write next to the target so the final swap is a same-volume operation.
    let tmp = dir.join(".spotuify-update.tmp");
    std::fs::write(&tmp, &binary).with_context(|| format!("writing {}", tmp.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))
            .context("setting executable permission")?;
    }

    self_replace::self_replace(&tmp).context("replacing the running executable")?;
    let _ = std::fs::remove_file(&tmp);
    Ok(())
}

/// Pull the `spotuify.exe` member out of the Windows release zip.
#[cfg(target_os = "windows")]
fn extract_binary(bytes: &[u8]) -> Result<Vec<u8>> {
    use std::io::Read;
    let mut zip =
        zip::ZipArchive::new(std::io::Cursor::new(bytes)).context("opening the downloaded zip")?;
    for i in 0..zip.len() {
        let mut file = zip.by_index(i)?;
        if file.name().ends_with("spotuify.exe") {
            let mut out = Vec::with_capacity(file.size() as usize);
            file.read_to_end(&mut out)?;
            return Ok(out);
        }
    }
    anyhow::bail!("spotuify.exe not found in the downloaded archive")
}

/// Pull the `spotuify` member out of the Unix release tarball.
#[cfg(unix)]
fn extract_binary(bytes: &[u8]) -> Result<Vec<u8>> {
    use std::io::Read;
    let gz = flate2::read::GzDecoder::new(std::io::Cursor::new(bytes));
    let mut archive = tar::Archive::new(gz);
    for entry in archive.entries().context("reading the downloaded tarball")? {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        if path.file_name().and_then(|n| n.to_str()) == Some("spotuify") {
            let mut out = Vec::new();
            entry.read_to_end(&mut out)?;
            return Ok(out);
        }
    }
    anyhow::bail!("spotuify binary not found in the downloaded archive")
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
