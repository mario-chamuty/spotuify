//! Persistent configuration and on-disk paths.
//!
//! Config lives at `~/.config/spotuify/config.toml`. Caches (librespot
//! credentials + audio cache, and the rspotify web-API token) live under
//! `~/.cache/spotuify/`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::keys::KeyBinding;
use crate::theme::ThemeConfig;

const APP_DIR: &str = "spotuify";

/// User-editable settings, serialized as TOML.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// librespot audio backend. "rodio" works everywhere via cpal.
    pub audio_backend: String,

    /// Selected output device name (as reported by the backend). `None` means
    /// the system default device.
    pub audio_device: Option<String>,

    /// Startup volume as a percentage, 0..=100.
    pub volume: u8,

    /// librespot audio cache size limit, in megabytes. `None` disables the
    /// limit (cache grows unbounded).
    pub cache_size_mb: Option<u64>,

    /// Normalise loudness across tracks (replaygain-style).
    pub normalisation: bool,

    /// Album-art rendering mode: `auto`, `halfblocks`, `sixel`, or `kitty`.
    /// `auto` lets the terminal-graphics detector pick the best protocol and
    /// falls back to coloured half-blocks otherwise.
    pub art_mode: ArtMode,

    /// Colour theme overrides (`[theme]` table). Unset fields use the default
    /// Spotify-green look.
    #[serde(default)]
    pub theme: ThemeConfig,

    /// Keybinding overrides (`[keys]` table): `action-name = "key"` or a list.
    #[serde(default)]
    pub keys: HashMap<String, KeyBinding>,
}

/// How album art is drawn.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ArtMode {
    /// Detect the best terminal-graphics protocol; fall back to half-blocks.
    #[default]
    Auto,
    /// Always render coloured half-blocks (works in any truecolor terminal).
    Halfblocks,
    /// Force the sixel protocol.
    Sixel,
    /// Force the kitty graphics protocol.
    Kitty,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            audio_backend: "rodio".to_string(),
            audio_device: None,
            volume: 70,
            cache_size_mb: Some(1024),
            normalisation: true,
            art_mode: ArtMode::Auto,
            theme: ThemeConfig::default(),
            keys: HashMap::new(),
        }
    }
}

impl Config {
    /// Load the config from disk. On first run a commented template is written
    /// for reference and the defaults are used — no configuration is required to
    /// run (authentication uses Spotify's official client id).
    pub fn load() -> Result<Self> {
        let path = config_path()?;
        if !path.exists() {
            let cfg = Config::default();
            cfg.write_template(&path)?;
            return Ok(cfg);
        }
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading config at {}", path.display()))?;
        let cfg: Config = toml::from_str(&text)
            .with_context(|| format!("parsing config at {}", path.display()))?;
        Ok(cfg)
    }

    /// Persist the current config back to disk (used when the user changes
    /// volume or output device from the UI).
    pub fn save(&self) -> Result<()> {
        let path = config_path()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = toml::to_string_pretty(self)?;
        std::fs::write(&path, text)
            .with_context(|| format!("writing config to {}", path.display()))?;
        Ok(())
    }

    fn write_template(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let header = "\
# SpoTUIfy configuration
#
# Everything here is optional — SpoTUIfy runs with no configuration and logs in
# through Spotify's official client (a developer app is NOT required). A Spotify
# Premium account is required for playback. Edit the values below to taste; see
# the README for the [theme] and [keys] tables.

";
        let body = toml::to_string_pretty(self)?;
        std::fs::write(path, format!("{header}{body}"))?;
        Ok(())
    }

    /// Volume as the 0..=65535 range librespot's mixer expects.
    pub fn volume_u16(&self) -> u16 {
        ((self.volume.min(100) as u32 * u16::MAX as u32) / 100) as u16
    }
}

fn config_path() -> Result<PathBuf> {
    let dir = dirs::config_dir().context("could not determine config directory")?;
    Ok(dir.join(APP_DIR).join("config.toml"))
}

/// Root cache directory, created if missing.
pub fn cache_dir() -> Result<PathBuf> {
    let dir = dirs::cache_dir()
        .context("could not determine cache directory")?
        .join(APP_DIR);
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Directory where librespot stores reusable credentials + the audio cache.
pub fn librespot_cache_dir() -> Result<PathBuf> {
    let dir = cache_dir()?.join("librespot");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// File where the rspotify web-API OAuth token is cached.
pub fn web_token_path() -> Result<PathBuf> {
    Ok(cache_dir()?.join("web-token.json"))
}

/// File the TUI logs to (the terminal is owned by the UI).
pub fn log_path() -> Result<PathBuf> {
    Ok(cache_dir()?.join("spotuify.log"))
}
