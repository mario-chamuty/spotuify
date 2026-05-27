//! Diagnostic: connect with the cached librespot credentials and ask Spotify
//! for a few tracks' audio files — the exact step that failed on librespot 0.6
//! ("no audio files"). Prints whether 0.8 now returns playable files. No audio
//! is produced. Run: `cargo run --example probe`.

use anyhow::{Context, Result};
use librespot::core::cache::Cache;
use librespot::core::{Session, SessionConfig, SpotifyId, SpotifyUri};
use librespot::metadata::audio::AudioItem;
use librespot::metadata::{Lyrics, Metadata};

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
        match AudioItem::get_file(&session, uri.clone()).await {
            Ok(item) => println!(
                "\n{uri_str}\n  name        : {}\n  availability: {:?}\n  files       : {} format(s)\n  alternatives: {}",
                item.name,
                item.availability,
                item.files.len(),
                item.alternatives.as_ref().map_or(0, |a| a.len()),
            ),
            Err(e) => println!("\n{uri_str}\n  get_file ERROR: {e}"),
        }
        if let Ok(id) = SpotifyId::try_from(&uri) {
            match Lyrics::get(&session, &id).await {
                Ok(l) => println!(
                    "  lyrics      : {} lines, sync={:?}, provider={}",
                    l.lyrics.lines.len(),
                    l.lyrics.sync_type,
                    l.lyrics.provider_display_name
                ),
                Err(e) => println!("  lyrics      : none ({e})"),
            }
        }
    }

    // Playlist via the internal context API (the Web API /tracks is 403 for
    // development-mode apps in 2026). Test a small + large + editorial playlist.
    for pl in [
        "spotify:playlist:7ETnL4fbHNcIdV1JmRuCZd", // editorial (403 on the Web API)
    ] {
        match session.spclient().get_context(pl).await {
            Ok(ctx) => {
                let total: usize = ctx.pages.iter().map(|p| p.tracks.len()).sum();
                println!("\n{pl}\n  pages={} inline_tracks={total}", ctx.pages.len());
                // Resolve names for the first few via track metadata.
                for t in ctx.pages.iter().flat_map(|p| &p.tracks).take(3) {
                    if let Ok(su) = SpotifyUri::from_uri(t.uri()) {
                        match librespot::metadata::Track::get(&session, &su).await {
                            Ok(tr) => {
                                let artists: Vec<_> =
                                    tr.artists.0.iter().map(|a| a.name.clone()).collect();
                                let cover = tr
                                    .album
                                    .covers
                                    .0
                                    .first()
                                    .and_then(|i| i.id.to_base16().ok());
                                println!(
                                    "    {} — {} [{}] cover={:?}",
                                    tr.name,
                                    artists.join(", "),
                                    tr.album.name,
                                    cover
                                );
                            }
                            Err(e) => println!("    {} metadata err: {e}", t.uri()),
                        }
                    }
                }
            }
            Err(e) => println!("\n{pl}\n  get_context error: {e}"),
        }
    }
    Ok(())
}
