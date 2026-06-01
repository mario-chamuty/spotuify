//! Session persistence: the queue, current selection, playback position and a
//! few preferences are serialized to `~/.cache/spotuify/state.json` on quit and
//! restored on launch (paused — playback is never auto-started).

use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::config;
use crate::model::Track;
use crate::player::Repeat;

/// Everything we carry across runs.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PersistedState {
    pub queue: Vec<Track>,
    pub current_index: Option<usize>,
    pub position_ms: u32,
    pub shuffle: bool,
    pub repeat: PersistedRepeat,
    /// Volume percentage 0..=100.
    pub volume: u8,
    pub search_history: Vec<String>,
    /// The tab the user was on last (so the app reopens where they left off).
    /// `None` for older state files or a transient view (e.g. a track list).
    #[serde(default)]
    pub last_view: Option<PersistedView>,
}

/// Serializable mirror of the stable [`crate::app::View`] tabs. Transient views
/// (a track list opened from search) are not persisted — the data behind them
/// isn't restored, so reopening to them would show an empty pane.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PersistedView {
    Search,
    Library,
    Queue,
    Devices,
    Settings,
    Home,
}

/// Serializable mirror of [`crate::player::Repeat`].
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PersistedRepeat {
    #[default]
    Off,
    All,
    One,
}

impl From<Repeat> for PersistedRepeat {
    fn from(r: Repeat) -> Self {
        match r {
            Repeat::Off => PersistedRepeat::Off,
            Repeat::All => PersistedRepeat::All,
            Repeat::One => PersistedRepeat::One,
        }
    }
}

impl From<PersistedRepeat> for Repeat {
    fn from(r: PersistedRepeat) -> Self {
        match r {
            PersistedRepeat::Off => Repeat::Off,
            PersistedRepeat::All => Repeat::All,
            PersistedRepeat::One => Repeat::One,
        }
    }
}

fn state_path() -> Result<PathBuf> {
    Ok(config::cache_dir()?.join("state.json"))
}

impl PersistedState {
    /// Load the saved state, or `None` if it is missing/unreadable.
    pub fn load() -> Option<Self> {
        let path = state_path().ok()?;
        let text = std::fs::read_to_string(&path).ok()?;
        match serde_json::from_str(&text) {
            Ok(s) => Some(s),
            Err(e) => {
                tracing::warn!("ignoring unreadable state.json: {e}");
                None
            }
        }
    }

    /// Write the state to disk (best-effort; errors are returned for logging).
    pub fn save(&self) -> Result<()> {
        let path = state_path()?;
        let text = serde_json::to_string_pretty(self).context("serializing state")?;
        std::fs::write(&path, text).with_context(|| format!("writing {}", path.display()))?;
        Ok(())
    }
}
