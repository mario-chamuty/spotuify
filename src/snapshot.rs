//! A small, cheap-to-clone view of playback state shared with the MPRIS task.
//!
//! Lives in its own (platform-independent) module so the app can build and
//! publish snapshots everywhere, while the MPRIS service that consumes them is
//! compiled only on Linux.

#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub playing: bool,
    pub stopped: bool,
    pub has_track: bool,
    pub track_uri: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub art_url: Option<String>,
    /// Track length in microseconds (MPRIS uses µs).
    pub length_us: i64,
    /// Position in microseconds.
    pub position_us: i64,
    /// Volume as a 0.0..=1.0 fraction.
    pub volume: f64,
    pub can_next: bool,
    pub can_prev: bool,
}
