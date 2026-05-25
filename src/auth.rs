//! Authentication. A single OAuth (PKCE) flow yields one token that authorises
//! both the librespot playback session and the rspotify web API. Both sides
//! cache credentials, so the browser flow only runs on first launch.

use std::collections::HashSet;

use anyhow::{Context, Result};
use chrono::Utc;
use librespot::core::authentication::Credentials as LibrespotCredentials;
use librespot::core::cache::Cache;
use rspotify::clients::{BaseClient, OAuthClient};
use rspotify::{AuthCodePkceSpotify, Config as RspotifyConfig, Credentials, OAuth, Token};

use crate::config::{self, Config};
use crate::spotify::Spotify;

/// Scopes requested in the OAuth flow. Includes playback (`streaming`) which is
/// required by librespot plus everything the web API endpoints need.
const REQUESTED_SCOPES: &[&str] = &[
    "streaming",
    "app-remote-control",
    "user-read-private",
    "user-read-email",
    "playlist-read-private",
    "playlist-read-collaborative",
    "user-library-read",
    "user-library-modify",
    "user-read-playback-state",
    "user-modify-playback-state",
    "user-read-currently-playing",
    "user-read-recently-played",
];

/// Subset of [`REQUESTED_SCOPES`] the web API actually uses. Kept separate so a
/// cached token only needs to be a superset of *these*, not the playback-only
/// scopes which Spotify may not echo back for custom apps.
const WEB_API_SCOPES: &[&str] = &[
    "user-read-private",
    "playlist-read-private",
    "playlist-read-collaborative",
    "user-library-read",
    "user-library-modify",
    "user-read-playback-state",
    "user-modify-playback-state",
    "user-read-currently-playing",
    "user-read-recently-played",
];

/// What `authenticate` hands back: librespot credentials for the playback
/// session and a ready-to-use web API client.
pub struct Auth {
    pub librespot_credentials: LibrespotCredentials,
    pub spotify: Spotify,
    pub cache: Cache,
}

/// Authenticate, reusing cached credentials where possible and otherwise
/// running the interactive browser flow exactly once.
pub async fn authenticate(config: &Config) -> Result<Auth> {
    let cache = build_cache(config)?;
    let client = build_web_client(config)?;

    let cached_web_token = client.read_token_cache(true).await.ok().flatten();
    let cached_lib_creds = cache.credentials();

    let lib_creds = match (cached_lib_creds, &cached_web_token) {
        (Some(creds), Some(_)) => {
            tracing::info!("Reusing cached credentials; skipping browser login.");
            creds
        }
        _ => {
            tracing::info!("No usable cached credentials; starting OAuth flow.");
            let token = run_oauth(config).await?;
            *client.token.lock().await.unwrap() = Some(web_token_from(&token));
            LibrespotCredentials::with_access_token(token.access_token)
        }
    };

    if let Some(token) = cached_web_token {
        *client.token.lock().await.unwrap() = Some(token);
    }
    // Persist whatever token we ended up with for next time.
    client.write_token_cache().await.ok();

    Ok(Auth {
        librespot_credentials: lib_creds,
        spotify: Spotify::new(client),
        cache,
    })
}

fn build_cache(config: &Config) -> Result<Cache> {
    let dir = config::librespot_cache_dir()?;
    let audio_dir = dir.join("audio");
    let size_limit = config.cache_size_mb.map(|mb| mb * 1024 * 1024);
    Cache::new(Some(&dir), Some(&dir), Some(&audio_dir), size_limit)
        .context("creating librespot cache")
}

fn build_web_client(config: &Config) -> Result<AuthCodePkceSpotify> {
    let creds = Credentials::new_pkce(&config.client_id);
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
async fn run_oauth(config: &Config) -> Result<OAuthTokenData> {
    let client_id = config.client_id.clone();
    let redirect_uri = config.redirect_uri.clone();

    println!("\n  SpoTUIfy needs to connect to your Spotify account.");
    println!("  A browser URL will be printed below — open it and approve access.\n");

    tokio::task::spawn_blocking(move || {
        let now = std::time::Instant::now();
        let token = librespot::oauth::get_access_token(
            &client_id,
            &redirect_uri,
            REQUESTED_SCOPES.to_vec(),
        )
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
