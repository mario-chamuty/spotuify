//! Diagnostic: connect with the cached librespot credentials and ask Spotify
//! for a few tracks' audio files — the exact step that failed on librespot 0.6
//! ("no audio files"). Prints whether 0.8 now returns playable files. No audio
//! is produced. Run: `cargo run --example probe`.

use anyhow::{Context, Result};
use librespot::core::cache::Cache;
use librespot::core::{Session, SessionConfig, SpotifyUri};
use librespot::metadata::audio::AudioItem;

const TEST_URIS: &[&str] = &[
    "spotify:track:5YnMOGu7F9sn2ODpwDvpHP", // one that failed in the logs
    "spotify:track:4cOdK2wGLETKBW3PvgPWqT", // Rick Astley — Never Gonna Give You Up
];

#[tokio::main]
async fn main() -> Result<()> {
    rustls::crypto::ring::default_provider().install_default().ok();

    let dir = dirs::cache_dir()
        .context("no cache dir")?
        .join("spotuify")
        .join("librespot");
    let cache = Cache::new(Some(&dir), Some(&dir), Some(&dir.join("audio")), None)
        .context("opening cache")?;
    let creds = cache
        .credentials()
        .context("no cached credentials — run the app and log in first")?;

    let session = Session::new(SessionConfig::default(), Some(cache));
    session.connect(creds, false).await.context("connect")?;
    println!("connected: user={} country={}", session.username(), session.country());

    for uri_str in TEST_URIS {
        let uri = SpotifyUri::from_uri(uri_str).context("parse uri")?;
        match AudioItem::get_file(&session, uri).await {
            Ok(item) => println!(
                "\n{uri_str}\n  name        : {}\n  availability: {:?}\n  files       : {} format(s)\n  alternatives: {}",
                item.name,
                item.availability,
                item.files.len(),
                item.alternatives.as_ref().map_or(0, |a| a.len()),
            ),
            Err(e) => println!("\n{uri_str}\n  get_file ERROR: {e}"),
        }
    }
    Ok(())
}
