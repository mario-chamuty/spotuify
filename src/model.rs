//! Plain domain types decoupled from rspotify's models, so the UI and player
//! never depend on the web-API crate's shapes directly.

use rspotify::model::{FullEpisode, FullTrack, Image, PlayableItem};
use rspotify::prelude::Id;
use serde::{Deserialize, Serialize};

/// Whether a playable item is a music track or a podcast/audiobook episode.
/// Both are loaded by librespot via their `spotify:…` URI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlayableKind {
    Track,
    Episode,
}

/// A playable item (track or episode), flattened to exactly what the UI and
/// player need. The `uri` is the canonical `spotify:track:<id>` /
/// `spotify:episode:<id>` form that librespot loads.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Track {
    pub uri: String,
    pub name: String,
    /// Artists (tracks) or the show name (episodes).
    pub artists: String,
    /// Album (tracks) or publisher/show (episodes).
    pub album: String,
    pub album_art_url: Option<String>,
    pub duration_ms: u32,
    #[serde(default = "default_kind")]
    pub kind: PlayableKind,
}

fn default_kind() -> PlayableKind {
    PlayableKind::Track
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
            kind: PlayableKind::Track,
        })
    }

    /// Build a playable from a podcast/audiobook episode.
    pub fn from_episode(e: FullEpisode) -> Self {
        Self {
            uri: e.id.uri(),
            name: e.name,
            artists: e.show.name.clone(),
            album: e.show.publisher.clone(),
            album_art_url: best_image(&e.images).or_else(|| best_image(&e.show.images)),
            duration_ms: e.duration.num_milliseconds().max(0) as u32,
            kind: PlayableKind::Episode,
        }
    }

    pub fn from_playable(item: PlayableItem) -> Option<Self> {
        match item {
            PlayableItem::Track(t) => Track::from_full(t),
            PlayableItem::Episode(e) => Some(Track::from_episode(e)),
        }
    }

    pub fn is_episode(&self) -> bool {
        self.kind == PlayableKind::Episode
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

/// A Spotify Connect device reported by the Web API (phone, speaker, desktop…).
#[derive(Debug, Clone)]
pub struct ConnectDevice {
    /// Spotify device id, used as the transfer/transport target. `None` for
    /// restricted devices that cannot be controlled directly.
    pub id: Option<String>,
    pub name: String,
    /// Human-readable device category (`Smartphone`, `Speaker`, …).
    pub kind: String,
    pub is_active: bool,
    pub volume_percent: Option<u8>,
}

/// A snapshot of remote (Connect) playback polled from the Web API.
#[derive(Debug, Clone)]
pub struct RemoteState {
    pub track: Option<Track>,
    pub is_playing: bool,
    pub progress_ms: u32,
    pub device_id: Option<String>,
    pub volume_percent: Option<u8>,
    pub shuffle: bool,
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
