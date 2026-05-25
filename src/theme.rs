//! Configurable colour theme.
//!
//! A [`Theme`] is a small bundle of `ratatui` colours. It is built from a
//! `[theme]` table of colour strings in the config and falls back to the
//! original Spotify-green look for anything unset. Colour strings may be
//! `#rrggbb`, `r,g,b`, an indexed `0`..=`255`, or a named colour (`green`,
//! `light-blue`, …).

use std::str::FromStr;

use ratatui::style::Color;
use serde::{Deserialize, Serialize};

/// Resolved colours threaded into the UI.
#[derive(Debug, Clone, Copy)]
pub struct Theme {
    /// Primary accent (the Spotify green): selections, active markers, gauge.
    pub accent: Color,
    /// Dim/secondary text and borders.
    pub dim: Color,
    /// Foreground of the highlighted (selected) list row.
    pub highlight_fg: Color,
    /// Background of the highlighted (selected) list row.
    pub highlight_bg: Color,
    /// Colour used for the ♥ "liked" indicator.
    pub like: Color,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            accent: Color::Rgb(30, 215, 96),
            dim: Color::Rgb(140, 140, 140),
            highlight_fg: Color::Black,
            highlight_bg: Color::Rgb(30, 215, 96),
            like: Color::Rgb(30, 215, 96),
        }
    }
}

impl Theme {
    /// Build a theme from the config table, keeping defaults for unset fields
    /// and ignoring (with a warning) any unparseable colour string.
    pub fn from_config(cfg: &ThemeConfig) -> Self {
        let mut theme = Theme::default();
        apply(&mut theme.accent, &cfg.accent, "accent");
        apply(&mut theme.dim, &cfg.dim, "dim");
        apply(&mut theme.highlight_fg, &cfg.highlight_fg, "highlight_fg");
        apply(&mut theme.highlight_bg, &cfg.highlight_bg, "highlight_bg");
        apply(&mut theme.like, &cfg.like, "like");
        theme
    }
}

fn apply(slot: &mut Color, value: &Option<String>, field: &str) {
    if let Some(s) = value {
        match parse_color(s) {
            Some(c) => *slot = c,
            None => tracing::warn!("ignoring invalid theme colour `{s}` for `{field}`"),
        }
    }
}

/// Parse a colour string. Adds `r,g,b` support on top of ratatui's own
/// `FromStr` (named, `#rrggbb`, indexed).
pub fn parse_color(s: &str) -> Option<Color> {
    let s = s.trim();
    if let Some((r, g, b)) = parse_rgb_triple(s) {
        return Some(Color::Rgb(r, g, b));
    }
    Color::from_str(s).ok()
}

fn parse_rgb_triple(s: &str) -> Option<(u8, u8, u8)> {
    let parts: Vec<&str> = s.split(',').map(str::trim).collect();
    if parts.len() != 3 {
        return None;
    }
    let r = parts[0].parse().ok()?;
    let g = parts[1].parse().ok()?;
    let b = parts[2].parse().ok()?;
    Some((r, g, b))
}

/// `[theme]` config table: each field is an optional colour string.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ThemeConfig {
    pub accent: Option<String>,
    pub dim: Option<String>,
    pub highlight_fg: Option<String>,
    pub highlight_bg: Option<String>,
    pub like: Option<String>,
}
