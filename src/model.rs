//! Plain domain types decoupled from rspotify's models, so the UI and player
//! never depend on the web-API crate's shapes directly.

use rspotify::model::{FullTrack, Image, PlayableItem};
use rspotify::prelude::Id;

/// A playable track, flattened to exactly what the UI and player need.
#[derive(Debug, Clone)]
pub struct Track {
    /// Canonical `spotify:track:<id>` URI (also what librespot loads).
    pub uri: String,
    pub name: String,
    pub artists: String,
    pub album: String,
    pub album_art_url: Option<String>,
    pub duration_ms: u32,
}

impl Track {
    pub fn from_full(t: FullTrack) -> Option<Self> {
        let uri = t.id.as_ref()?.uri();
        Some(Self {
            uri,
            name: t.name,
            artists: join_artists(t.artists.iter().map(|a| a.name.as_str())),
            album: t.album.name.clone(),
            album_art_url: best_image(&t.album.images),
            duration_ms: t.duration.num_milliseconds().max(0) as u32,
        })
    }

    pub fn from_playable(item: PlayableItem) -> Option<Self> {
        match item {
            PlayableItem::Track(t) => Track::from_full(t),
            PlayableItem::Episode(_) => None,
        }
    }
}

/// A reference to one of the user's playlists shown in the library.
#[derive(Debug, Clone)]
pub struct PlaylistRef {
    pub id: String,
    pub name: String,
    pub owner: String,
    pub total: u32,
}

/// A local audio output device that playback can be routed to.
#[derive(Debug, Clone)]
pub struct OutputDevice {
    pub name: String,
    pub is_default: bool,
}

fn join_artists<'a>(names: impl Iterator<Item = &'a str>) -> String {
    names.collect::<Vec<_>>().join(", ")
}

/// Pick the album image closest to ~300px wide — large enough for crisp
/// half-block art without downloading the full-resolution cover.
pub fn best_image(images: &[Image]) -> Option<String> {
    images
        .iter()
        .min_by_key(|img| {
            let w = img.width.unwrap_or(0) as i64;
            (w - 300).abs()
        })
        .map(|img| img.url.clone())
}

/// Format milliseconds as `m:ss` (or `h:mm:ss` for long items).
pub fn fmt_ms(ms: u32) -> String {
    let total = ms / 1000;
    let (h, m, s) = (total / 3600, (total % 3600) / 60, total % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}
