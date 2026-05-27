//! Web-player access tokens, minted from the user's `sp_dc` cookie.
//!
//! Spotify's private GraphQL API ("pathfinder") — the only source of Daily
//! Mixes, Discover Weekly, Release Radar and the genre/mood shelves on Home —
//! accepts **only** the web player's own access tokens. Developer-app tokens get
//! `403 RBAC: access denied`, and librespot's keymaster tokens can't be minted
//! anymore. The web player itself obtains a token by calling
//! `open.spotify.com/api/token` with the logged-in `sp_dc` cookie plus a **TOTP**
//! (time-based one-time code) derived from a secret baked into the player bundle.
//!
//! That secret rotates, so it is fetched at runtime from a community-maintained
//! gist (the same self-healing idea as the GraphQL query-hash registry). Nothing
//! is hardcoded: with no cookie, an invalid cookie, or an unreachable gist we
//! simply return an error and the caller falls back to the public shelves.
//!
//! Recipe mirrors github.com/FrancescoGrazioso/Meld (`SpotifyAuth.kt`) and
//! sonic-liberation/spotube-plugin-spotify.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use hmac::{Hmac, Mac};
use serde::Deserialize;
use sha1::Sha1;
use tokio::sync::Mutex;

const TOKEN_URL: &str = "https://open.spotify.com/api/token";
const SERVER_TIME_URL: &str = "https://open.spotify.com/api/server-time";
const GIST_URL: &str = "https://api.github.com/gists/22ed9c6ba463899e933427f7de1f0eef";
const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
    (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";

type HmacSha1 = Hmac<Sha1>;

/// Mints and caches a web-player access token from an `sp_dc` cookie.
pub struct WebToken {
    sp_dc: String,
    http: reqwest::Client,
    cached: Mutex<Option<Cached>>,
}

struct Cached {
    token: String,
    /// Unix epoch milliseconds at which the token expires.
    expires_at_ms: u128,
}

impl WebToken {
    pub fn new(sp_dc: String) -> Arc<Self> {
        Arc::new(Self {
            sp_dc: sp_dc.trim().to_string(),
            http: reqwest::Client::new(),
            cached: Mutex::new(None),
        })
    }

    pub fn has_cookie(&self) -> bool {
        !self.sp_dc.is_empty()
    }

    /// A valid access token, minting (and caching) a fresh one when the cached
    /// token is missing or within a minute of expiring.
    pub async fn access_token(&self) -> Result<String> {
        if !self.has_cookie() {
            bail!("no sp_dc cookie configured");
        }
        {
            let guard = self.cached.lock().await;
            if let Some(c) = guard.as_ref() {
                if c.expires_at_ms > now_ms() + 60_000 {
                    return Ok(c.token.clone());
                }
            }
        }
        let fresh = self.mint().await?;
        let token = fresh.token.clone();
        *self.cached.lock().await = Some(fresh);
        Ok(token)
    }

    async fn mint(&self) -> Result<Cached> {
        let (secret, ver) = self.fetch_secret().await.context("fetching TOTP secret")?;
        let server_time = self
            .fetch_server_time()
            .await
            .context("fetching Spotify server time")?;
        let totp = generate_totp(&secret, server_time)?;

        let url = format!(
            "{TOKEN_URL}?reason=transport&productType=web-player\
             &totp={totp}&totpServer={totp}&totpVer={ver}"
        );
        let resp = self
            .http
            .get(&url)
            .header("User-Agent", UA)
            .header("Accept", "application/json")
            .header("Cookie", format!("sp_dc={}", self.sp_dc))
            .send()
            .await
            .context("token request failed")?;
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            bail!("token endpoint HTTP {status}: {}", truncate(&body, 200));
        }
        let tok: TokenResponse = serde_json::from_str(&body)
            .with_context(|| format!("parsing token response: {}", truncate(&body, 200)))?;
        if tok.is_anonymous || tok.access_token.is_empty() {
            bail!("Spotify returned an anonymous token — the sp_dc cookie is invalid or expired");
        }
        Ok(Cached {
            token: tok.access_token,
            expires_at_ms: tok
                .access_token_expiration_timestamp_ms
                .unwrap_or_else(|| now_ms() + 3_600_000),
        })
    }

    /// Fetch the current TOTP secret + version from the community gist, picking
    /// the highest version available.
    async fn fetch_secret(&self) -> Result<(String, u32)> {
        let body = self
            .http
            .get(GIST_URL)
            .header("User-Agent", UA)
            .header("Accept", "application/json")
            .send()
            .await?
            .error_for_status()?
            .text()
            .await?;
        let gist: Gist = serde_json::from_str(&body).context("parsing gist")?;
        let content = &gist
            .files
            .values()
            .next()
            .context("gist has no files")?
            .content;
        let mut nuances: Vec<Nuance> =
            serde_json::from_str(content).context("parsing TOTP secret list")?;
        nuances.sort_by_key(|n| n.v);
        let best = nuances.pop().context("gist has no TOTP secret")?;
        Ok((best.s, best.v))
    }

    async fn fetch_server_time(&self) -> Result<u64> {
        let body = self
            .http
            .get(SERVER_TIME_URL)
            .header("User-Agent", UA)
            .header("Accept", "application/json")
            .send()
            .await?
            .error_for_status()?
            .text()
            .await?;
        let r: ServerTime = serde_json::from_str(&body).context("parsing server time")?;
        Ok(r.server_time)
    }
}

#[derive(Deserialize)]
struct TokenResponse {
    #[serde(rename = "accessToken", default)]
    access_token: String,
    #[serde(rename = "accessTokenExpirationTimestampMs")]
    access_token_expiration_timestamp_ms: Option<u128>,
    #[serde(rename = "isAnonymous", default)]
    is_anonymous: bool,
}

#[derive(Deserialize)]
struct Gist {
    files: HashMap<String, GistFile>,
}

#[derive(Deserialize)]
struct GistFile {
    content: String,
}

#[derive(Deserialize)]
struct Nuance {
    s: String,
    v: u32,
}

#[derive(Deserialize)]
struct ServerTime {
    #[serde(rename = "serverTime")]
    server_time: u64,
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn truncate(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// 6-digit HMAC-SHA1 TOTP (RFC 6238, 30-second step) over a base32 secret,
/// using Spotify's server time as the clock.
fn generate_totp(secret_b32: &str, server_time_sec: u64) -> Result<String> {
    let key = base32_decode(secret_b32);
    if key.is_empty() {
        bail!("empty TOTP secret");
    }
    let counter = server_time_sec / 30;
    let mut mac = HmacSha1::new_from_slice(&key).context("invalid HMAC key")?;
    mac.update(&counter.to_be_bytes());
    let hash = mac.finalize().into_bytes();

    let offset = (hash[hash.len() - 1] & 0x0f) as usize;
    let code = ((hash[offset] as u32 & 0x7f) << 24)
        | ((hash[offset + 1] as u32) << 16)
        | ((hash[offset + 2] as u32) << 8)
        | (hash[offset + 3] as u32);
    Ok(format!("{:06}", code % 1_000_000))
}

/// RFC 4648 base32 decode (uppercase, tolerant of padding and stray chars).
fn base32_decode(input: &str) -> Vec<u8> {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
    let mut out = Vec::new();
    let mut buffer: u32 = 0;
    let mut bits_left: u32 = 0;
    for c in input.to_ascii_uppercase().into_bytes() {
        if c == b'=' {
            continue;
        }
        let Some(val) = ALPHABET.iter().position(|&a| a == c) else {
            continue;
        };
        buffer = (buffer << 5) | val as u32;
        bits_left += 5;
        if bits_left >= 8 {
            bits_left -= 8;
            out.push(((buffer >> bits_left) & 0xff) as u8);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base32_roundtrip_known() {
        // "JBSWY3DPEHPK3PXP" is base32 for "Hello!\xde\xad\xbe\xef" prefix bytes;
        // here just verify a known RFC 4648 vector: "MZXW6===" -> "foo".
        assert_eq!(base32_decode("MZXW6"), b"foo");
        assert_eq!(base32_decode("MZXW6==="), b"foo");
    }

    #[test]
    fn totp_is_six_digits() {
        // Deterministic given a fixed secret + time; just assert the shape.
        let code = generate_totp("MZXW6YTBOI======", 1_700_000_000).unwrap();
        assert_eq!(code.len(), 6);
        assert!(code.chars().all(|c| c.is_ascii_digit()));
    }
}
