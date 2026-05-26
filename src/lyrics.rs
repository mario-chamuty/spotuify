//! Time-synced lyrics via librespot's lyrics endpoint (Spotify/Musixmatch).
//! Fetched over the playback session (the official client), so it isn't subject
//! to the Web API rate limits.

use anyhow::{Context, Result};
use librespot::core::{Session, SpotifyId, SpotifyUri};
use librespot::metadata::lyrics::SyncType;
use librespot::metadata::Lyrics as LibrespotLyrics;

/// A track's lyrics, flattened to what the UI needs.
#[derive(Debug, Clone)]
pub struct Lyrics {
    /// Whether lines carry per-line timestamps (enables highlight + auto-scroll).
    pub synced: bool,
    pub provider: String,
    pub lines: Vec<LyricLine>,
}

#[derive(Debug, Clone)]
pub struct LyricLine {
    /// Line start time in milliseconds (0 for unsynced lyrics).
    pub time_ms: u32,
    pub text: String,
}

impl Lyrics {
    /// Index of the line that should be highlighted at `position_ms`, for synced
    /// lyrics: the last line whose start time has passed.
    pub fn active_line(&self, position_ms: u32) -> Option<usize> {
        if !self.synced || self.lines.is_empty() {
            return None;
        }
        self.lines
            .iter()
            .rposition(|l| l.time_ms <= position_ms)
            .or(Some(0))
    }
}

/// Fetch lyrics for a `spotify:track:<id>` (or episode) URI.
pub async fn fetch(session: &Session, track_uri: &str) -> Result<Lyrics> {
    let uri = SpotifyUri::from_uri(track_uri).context("bad track uri")?;
    let id = SpotifyId::try_from(&uri).context("uri has no playable id")?;
    let raw = LibrespotLyrics::get(session, &id)
        .await
        .context("no lyrics for this track")?;

    let synced = matches!(raw.lyrics.sync_type, SyncType::LineSynced);
    let lines = raw
        .lyrics
        .lines
        .iter()
        .map(|l| LyricLine {
            time_ms: l.start_time_ms.parse().unwrap_or(0),
            text: l.words.clone(),
        })
        .collect();

    Ok(Lyrics {
        synced,
        provider: raw.lyrics.provider_display_name.clone(),
        lines,
    })
}
