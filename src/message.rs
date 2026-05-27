//! Messages produced by background tasks (web API calls, album-art rendering)
//! and consumed by the app's event loop. Keeping these on a channel keeps the
//! UI responsive: network and image work never blocks rendering or input.

use ratatui::text::Line;

use crate::model::{ConnectDevice, PlaylistRef, RemoteState, Track};
use crate::spotify::SearchResults;

/// How an opened track list should behave once it arrives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenMode {
    /// Just display the tracks in the track-list view.
    Show,
    /// Display and immediately start playing from the top.
    Play,
}

pub enum Update {
    Search(SearchResults),
    Playlists(Vec<PlaylistRef>),
    /// A resolved track list (playlist/album/artist/liked songs/search).
    Tracks {
        title: String,
        tracks: Vec<Track>,
        mode: OpenMode,
    },
    AlbumArt {
        track_uri: String,
        cols: u16,
        rows: u16,
        lines: Vec<Line<'static>>,
    },
    /// A decoded cover image for the pixel-graphics (sixel/kitty) art path.
    ArtImage {
        track_uri: String,
        image: image::DynamicImage,
    },
    /// The set of track URIs (from a recently shown list) the user has saved.
    Liked(Vec<String>),
    /// The user's Spotify Connect devices.
    ConnectDevices(Vec<ConnectDevice>),
    /// A polled snapshot of remote (Connect) playback.
    RemoteState(Option<RemoteState>),
    /// Fetched lyrics for a track (`None` = none available).
    Lyrics {
        track_uri: String,
        lyrics: Option<crate::lyrics::Lyrics>,
    },
    /// Assembled Home content.
    Home(Box<crate::spotify::Home>),
    Error(String),
}
