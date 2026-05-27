//! Playlist track listing via librespot's internal context API.
//!
//! Spotify's 2026 changes return 403 on the Web API playlist `tracks`/`items`
//! endpoints for development-mode apps (even for editorial playlists), so we
//! resolve the playlist *context* over the playback session (the official
//! client, like lyrics) and fetch each track's metadata. This also works for
//! Spotify-owned editorial playlists the Web API refuses.

use anyhow::{Context, Result};
use futures::stream::{self, StreamExt};
use librespot::core::{Session, SpotifyId, SpotifyUri};
use librespot::metadata::{Metadata, Track as LibrespotTrack};

use crate::model::{PlayableKind, Track};

/// Safety cap so a giant playlist can't fan out into thousands of requests.
const MAX_TRACKS: usize = 500;
/// How many track-metadata fetches to run at once.
const CONCURRENCY: usize = 8;

/// Resolve a playlist (bare id or `spotify:playlist:<id>` uri) to its tracks,
/// in playlist order.
pub async fn playlist_tracks(session: &Session, playlist: &str) -> Result<Vec<Track>> {
    let uri = if playlist.starts_with("spotify:") {
        playlist.to_string()
    } else {
        format!("spotify:playlist:{playlist}")
    };

    let ctx = session
        .spclient()
        .get_context(&uri)
        .await
        .context("resolving playlist")?;

    let mut uris: Vec<String> = Vec::new();
    for page in &ctx.pages {
        for t in &page.tracks {
            let u = t.uri();
            if uris.len() >= MAX_TRACKS {
                break;
            }
            if u.starts_with("spotify:track:") || u.starts_with("spotify:episode:") {
                uris.push(u.to_string());
            }
        }
    }

    // Fetch metadata concurrently, then restore playlist order by index.
    let mut indexed: Vec<(usize, Option<Track>)> = stream::iter(uris.into_iter().enumerate())
        .map(|(i, u)| async move { (i, fetch_track(session, &u).await) })
        .buffer_unordered(CONCURRENCY)
        .collect()
        .await;
    indexed.sort_by_key(|(i, _)| *i);
    Ok(indexed.into_iter().filter_map(|(_, t)| t).collect())
}

async fn fetch_track(session: &Session, uri: &str) -> Option<Track> {
    // Episodes can appear in playlists but have no track metadata; skip them.
    if uri.contains(":episode:") {
        return None;
    }
    let su = SpotifyUri::from_uri(uri).ok()?;
    let t = LibrespotTrack::get(session, &su).await.ok()?;

    let artists = t
        .artists
        .0
        .iter()
        .map(|a| a.name.clone())
        .collect::<Vec<_>>()
        .join(", ");
    let cover = t
        .album
        .covers
        .0
        .first()
        .and_then(|img| img.id.to_base16().ok())
        .map(|hex| format!("https://i.scdn.co/image/{}", hex.to_lowercase()));
    let artist = t.artists.0.first().and_then(|a| {
        SpotifyId::try_from(&a.id)
            .ok()
            .and_then(|s| s.to_base62().ok())
            .map(|id| (id, a.name.clone()))
    });
    let album_id = SpotifyId::try_from(&t.album.id).ok().and_then(|s| s.to_base62().ok());

    Some(Track {
        uri: uri.to_string(),
        name: t.name,
        artists,
        album: t.album.name,
        album_art_url: cover,
        duration_ms: t.duration.max(0) as u32,
        kind: PlayableKind::Track,
        artist,
        album_id,
    })
}
