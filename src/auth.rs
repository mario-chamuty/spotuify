//! Authentication. Two PKCE OAuth flows, each cached so the browser is only
//! needed on first launch:
//!
//! * **Playback** uses Spotify's official desktop ("keymaster") client id –
//!   librespot's `login5` only streams for Spotify's own client ids. No
//!   developer app required; the reusable credentials are cached by librespot.
//! * **Web API** (search, playlists, library, playback control) uses the user's
//!   own registered app id from the config. Spotify's 2026 changes rate-limit
//!   the official client id on `api.spotify.com` (it is shared by every
//!   librespot user), so the Web API must go through a per-user app.

use std::collections::HashSet;

use anyhow::{Context, Result};
use chrono::Utc;
use librespot::core::authentication::Credentials as LibrespotCredentials;
use librespot::core::cache::Cache;
use rspotify::clients::{BaseClient, OAuthClient};
use rspotify::{AuthCodePkceSpotify, Config as RspotifyConfig, Credentials, OAuth, Token};

use crate::config::{self, Config};
use crate::spotify::Spotify;

/// Spotify's official desktop ("keymaster") client id. librespot streams audio
/// via `login5`, which Spotify only authorises for its own client ids; a
/// development-mode app id is refused (HTTP 400 on every audio load) since the
/// 2026 API lockdown. Used only to bootstrap the playback session.
const STREAMING_CLIENT_ID: &str = "65b708073fc0480ea92a077233ca87bd";

/// The redirect URI registered for [`STREAMING_CLIENT_ID`] (librespot's
/// default). Fixed, since only this loopback URI is registered for the official
/// client.
const STREAMING_REDIRECT_URI: &str = "http://127.0.0.1:5588/login";

/// Scopes for the playback (keymaster) token. The token only bootstraps the
/// librespot session, so it just needs streaming/remote-control.
const STREAMING_SCOPES: &[&str] = &["streaming", "app-remote-control"];

/// Scopes for the Web API (user-app) token – everything the endpoints touch.
const WEB_API_SCOPES: &[&str] = &[
    "user-read-private",
    "user-read-email",
    "playlist-read-private",
    "playlist-read-collaborative",
    "playlist-modify-public",
    "playlist-modify-private",
    "user-library-read",
    "user-library-modify",
    "user-read-playback-state",
    "user-modify-playback-state",
    "user-read-currently-playing",
    "user-read-recently-played",
    "user-top-read",
];

/// What `authenticate` hands back: librespot credentials for the playback
/// session and a ready-to-use web API client.
pub struct Auth {
    pub librespot_credentials: LibrespotCredentials,
    pub spotify: Spotify,
    pub cache: Cache,
}

/// Authenticate both halves of the app, reusing cached credentials where
/// possible. On a clean install this runs two browser logins (playback, then
/// Web API); afterwards both are cached and no browser is needed.
pub async fn authenticate(config: &Config) -> Result<Auth> {
    let client_id = config.client_id.trim();
    if client_id.is_empty() {
        let path = config::config_path()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| "your config".to_string());
        anyhow::bail!(
            "Search and your library need a Spotify app client id.\n\n  \
             1. Create a free app: https://developer.spotify.com/dashboard\n  \
             2. Add this Redirect URI to it: {redirect}\n  \
             3. Put the app's Client ID into `client_id` in {path}\n\n\
             (Audio playback uses Spotify's official client and needs no app.)",
            redirect = config.redirect_uri,
        );
    }

    let cache = build_cache(config)?;
    let client = build_web_client(config)?;

    // ---- Web API token (user's own app) -----------------------------------
    let cached_web_token = client.read_token_cache(true).await.ok().flatten();
    if let Some(token) = cached_web_token.clone() {
        *client.token.lock().await.unwrap() = Some(token);
    }

    // ---- Playback credentials (official client) ----------------------------
    let lib_creds = match cache.credentials() {
        Some(creds) => {
            tracing::info!("Reusing cached playback credentials.");
            creds
        }
        None => {
            let token = run_oauth(
                "Log in to enable playback (Spotify's official client)…",
                STREAMING_CLIENT_ID,
                STREAMING_REDIRECT_URI,
                STREAMING_SCOPES,
            )
            .await?;
            LibrespotCredentials::with_access_token(token.access_token)
        }
    };

    // ---- Web API login, if not already cached ------------------------------
    if cached_web_token.is_none() {
        let token = run_oauth(
            "Log in to enable search & your library (your Spotify app)…",
            client_id,
            &config.redirect_uri,
            WEB_API_SCOPES,
        )
        .await
        .with_context(|| {
            format!(
                "Web API login failed. The most common cause is a Redirect URI \
                 mismatch: open developer.spotify.com/dashboard → your app → \
                 Settings → Redirect URIs and make sure it contains exactly `{}`.",
                config.redirect_uri
            )
        })?;
        *client.token.lock().await.unwrap() = Some(web_token_from(&token));
        client.write_token_cache().await.ok();
    } else {
        tracing::info!("Reusing cached Web API token.");
    }

    Ok(Auth {
        librespot_credentials: lib_creds,
        spotify: Spotify::new(client),
        cache,
    })
}

/// First-run guided setup for the Web API client id, printed on the normal
/// screen before the TUI starts (consistent with the OAuth prompts). Opens the
/// Spotify dashboard, shows the exact Redirect URI to register, reads the client
/// id from stdin and saves it to the config, so the very next step (OAuth) can
/// proceed without a restart. Leaving the id blank skips setup; `authenticate`
/// then prints the manual fallback instructions.
pub fn run_first_run_setup(config: &mut Config) -> Result<()> {
    use std::io::{self, Write};

    let redirect = config.redirect_uri.clone();
    println!("\n  Welcome to SpoTUIfy – one-time setup\n");
    println!("  Audio playback uses Spotify's official client and needs nothing.");
    println!("  Search and your library use the Web API, which Spotify now");
    println!("  rate-limits unless you use your own free app. Two quick steps:\n");
    println!("    1. Create a free app at https://developer.spotify.com/dashboard");
    println!("       (any name and description – it stays private to you).");
    println!("    2. In the app's settings, add this exact Redirect URI:\n");
    println!("         {redirect}\n");
    println!("       It must match character for character.\n");

    print!("  Open the dashboard in your browser now? [Y/n] ");
    io::stdout().flush().ok();
    let mut answer = String::new();
    io::stdin().read_line(&mut answer).ok();
    if !answer.trim().eq_ignore_ascii_case("n") {
        if let Err(e) = open::that("https://developer.spotify.com/dashboard") {
            println!("  (Couldn't open a browser automatically: {e})");
        }
    }

    println!("\n  Then copy the app's Client ID (Settings → Basic Information).");
    print!("  Paste Client ID here and press Enter (blank to skip): ");
    io::stdout().flush().ok();
    let mut id = String::new();
    io::stdin().read_line(&mut id).context("reading client id from stdin")?;
    let id = id.trim().to_string();
    if id.is_empty() {
        return Ok(()); // user skipped; authenticate() prints manual instructions
    }

    config.client_id = id;
    config.save().context("saving the client id to config")?;
    println!("\n  Saved your Client ID. Continuing to sign-in…\n");
    Ok(())
}

fn build_cache(config: &Config) -> Result<Cache> {
    let dir = config::librespot_cache_dir()?;
    let audio_dir = dir.join("audio");
    let size_limit = config.cache_size_mb.map(|mb| mb * 1024 * 1024);
    Cache::new(Some(&dir), Some(&dir), Some(&audio_dir), size_limit)
        .context("creating librespot cache")
}

fn build_web_client(config: &Config) -> Result<AuthCodePkceSpotify> {
    let creds = Credentials::new_pkce(config.client_id.trim());
    let oauth = OAuth {
        redirect_uri: config.redirect_uri.clone(),
        scopes: WEB_API_SCOPES.iter().map(|s| s.to_string()).collect(),
        ..Default::default()
    };
    let rconfig = RspotifyConfig {
        cache_path: config::web_token_path()?,
        token_cached: true,
        token_refreshing: true,
        ..Default::default()
    };
    Ok(AuthCodePkceSpotify::with_config(creds, oauth, rconfig))
}

/// Convert a librespot OAuth token into an rspotify [`Token`].
fn web_token_from(token: &OAuthTokenData) -> Token {
    let scopes: HashSet<String> = token.scopes.iter().cloned().collect();
    let expires_in = chrono::Duration::from_std(token.expires_in)
        .unwrap_or_else(|_| chrono::Duration::seconds(3600));
    Token {
        access_token: token.access_token.clone(),
        expires_in,
        expires_at: Some(Utc::now() + expires_in),
        refresh_token: (!token.refresh_token.is_empty()).then(|| token.refresh_token.clone()),
        scopes,
    }
}

/// Minimal owned copy of librespot's OAuth result, decoupled from `Instant`.
struct OAuthTokenData {
    access_token: String,
    refresh_token: String,
    expires_in: std::time::Duration,
    scopes: Vec<String>,
}

/// Run the blocking librespot OAuth helper on a blocking thread. It prints a
/// "Browse to: <url>" line and spins up a one-shot loopback listener to catch
/// the redirect, so the user just needs to open the URL and approve.
async fn run_oauth(
    purpose: &str,
    client_id: &str,
    redirect_uri: &str,
    scopes: &[&str],
) -> Result<OAuthTokenData> {
    let client_id = client_id.to_string();
    let redirect_uri = redirect_uri.to_string();
    let scopes: Vec<String> = scopes.iter().map(|s| s.to_string()).collect();

    println!("\n  {purpose}");
    println!("  Your browser should open; if not, copy the URL printed below.\n");

    tokio::task::spawn_blocking(move || {
        let now = std::time::Instant::now();
        let scope_refs: Vec<&str> = scopes.iter().map(String::as_str).collect();
        let client = librespot::oauth::OAuthClientBuilder::new(&client_id, &redirect_uri, scope_refs)
            .open_in_browser()
            .build()
            .map_err(|e| anyhow::anyhow!("OAuth setup failed: {e}"))?;
        let token = client
            .get_access_token()
            .map_err(|e| anyhow::anyhow!("OAuth failed: {e}"))?;
        // Convert the `Instant`-based expiry into a plain duration from now.
        let expires_in = token.expires_at.saturating_duration_since(now);
        Ok(OAuthTokenData {
            access_token: token.access_token,
            refresh_token: token.refresh_token,
            expires_in,
            scopes: token.scopes,
        })
    })
    .await
    .context("OAuth task panicked")?
}
