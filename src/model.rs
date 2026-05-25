//! Plain domain types decoupled from rspotify's models, so the UI and player
//! never depend on the web-API crate's shapes directly.

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
