//! Configurable keybindings.
//!
//! Every user-triggerable behaviour is an [`Action`]. A [`Keymap`] maps a
//! `(KeyCode, KeyModifiers)` chord to an `Action`. The map is built from
//! compiled-in defaults and then overlaid with any `[keys]` table from the
//! config (`action-name = "space"` or `action-name = ["q", "ctrl+c"]`).

use std::collections::HashMap;

use crossterm::event::{KeyCode, KeyModifiers};
use serde::{Deserialize, Serialize};

/// Everything the user can trigger via a key. List-navigation, playback
/// transport, view switching, and the richer library/filter actions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Action {
    // Playback transport
    PlayPause,
    Next,
    Prev,
    VolumeUp,
    VolumeDown,
    SeekForward,
    SeekBack,
    ToggleShuffle,
    CycleRepeat,
    // Global
    Quit,
    Help,
    CycleTab,
    FocusSearch,
    // Tab switches
    TabSearch,
    TabLibrary,
    TabTracks,
    TabQueue,
    TabDevices,
    // List navigation
    Up,
    Down,
    Top,
    Bottom,
    Activate,
    Enqueue,
    // Library writes / filtering
    ToggleLike,
    AddToPlaylist,
    EnterFilter,
    CreatePlaylist,
    RenamePlaylist,
    DeletePlaylist,
}

impl Action {
    /// Stable kebab-case name used as the config key for this action.
    pub fn name(self) -> &'static str {
        match self {
            Action::PlayPause => "play-pause",
            Action::Next => "next",
            Action::Prev => "prev",
            Action::VolumeUp => "volume-up",
            Action::VolumeDown => "volume-down",
            Action::SeekForward => "seek-forward",
            Action::SeekBack => "seek-back",
            Action::ToggleShuffle => "toggle-shuffle",
            Action::CycleRepeat => "cycle-repeat",
            Action::Quit => "quit",
            Action::Help => "help",
            Action::CycleTab => "cycle-tab",
            Action::FocusSearch => "focus-search",
            Action::TabSearch => "tab-search",
            Action::TabLibrary => "tab-library",
            Action::TabTracks => "tab-tracks",
            Action::TabQueue => "tab-queue",
            Action::TabDevices => "tab-devices",
            Action::Up => "up",
            Action::Down => "down",
            Action::Top => "top",
            Action::Bottom => "bottom",
            Action::Activate => "activate",
            Action::Enqueue => "enqueue",
            Action::ToggleLike => "toggle-like",
            Action::AddToPlaylist => "add-to-playlist",
            Action::EnterFilter => "enter-filter",
            Action::CreatePlaylist => "create-playlist",
            Action::RenamePlaylist => "rename-playlist",
            Action::DeletePlaylist => "delete-playlist",
        }
    }

    /// Look up an action by its config name.
    fn from_name(name: &str) -> Option<Action> {
        ALL_ACTIONS.iter().copied().find(|a| a.name() == name)
    }
}

/// All actions, used both for config parsing and to document defaults.
const ALL_ACTIONS: &[Action] = &[
    Action::PlayPause,
    Action::Next,
    Action::Prev,
    Action::VolumeUp,
    Action::VolumeDown,
    Action::SeekForward,
    Action::SeekBack,
    Action::ToggleShuffle,
    Action::CycleRepeat,
    Action::Quit,
    Action::Help,
    Action::CycleTab,
    Action::FocusSearch,
    Action::TabSearch,
    Action::TabLibrary,
    Action::TabTracks,
    Action::TabQueue,
    Action::TabDevices,
    Action::Up,
    Action::Down,
    Action::Top,
    Action::Bottom,
    Action::Activate,
    Action::Enqueue,
    Action::ToggleLike,
    Action::AddToPlaylist,
    Action::EnterFilter,
    Action::CreatePlaylist,
    Action::RenamePlaylist,
    Action::DeletePlaylist,
];

/// Compiled-in default chords for each action, as parseable strings.
fn default_bindings() -> Vec<(Action, &'static [&'static str])> {
    vec![
        (Action::PlayPause, &["space"]),
        (Action::Next, &["n"]),
        (Action::Prev, &["b"]),
        (Action::VolumeUp, &["+", "="]),
        (Action::VolumeDown, &["-", "_"]),
        (Action::SeekForward, &["]"]),
        (Action::SeekBack, &["["]),
        (Action::ToggleShuffle, &["s"]),
        (Action::CycleRepeat, &["r"]),
        (Action::Quit, &["q", "ctrl+c"]),
        (Action::Help, &["?"]),
        (Action::CycleTab, &["tab"]),
        (Action::FocusSearch, &["i"]),
        (Action::TabSearch, &["1"]),
        (Action::TabLibrary, &["2"]),
        (Action::TabTracks, &["3"]),
        (Action::TabQueue, &["4"]),
        (Action::TabDevices, &["5"]),
        (Action::Up, &["up", "k"]),
        (Action::Down, &["down", "j"]),
        (Action::Top, &["g", "home"]),
        (Action::Bottom, &["G", "end"]),
        (Action::Activate, &["enter"]),
        (Action::Enqueue, &["e"]),
        (Action::ToggleLike, &["L"]),
        (Action::AddToPlaylist, &["a"]),
        (Action::EnterFilter, &["/"]),
        (Action::CreatePlaylist, &["c"]),
        (Action::RenamePlaylist, &["R"]),
        (Action::DeletePlaylist, &["D"]),
    ]
}

/// A resolved chord: a key plus the modifiers that must be held.
type Chord = (KeyCode, KeyModifiers);

/// Maps key chords to actions.
#[derive(Debug, Clone)]
pub struct Keymap {
    map: HashMap<Chord, Action>,
}

impl Keymap {
    /// Build the keymap from compiled-in defaults, then overlay user overrides.
    /// A user override for an action replaces *all* default chords for that
    /// action (so the action's previous default chords are removed first).
    pub fn build(overrides: &HashMap<String, KeyBinding>) -> Self {
        let mut map: HashMap<Chord, Action> = HashMap::new();
        for (action, chords) in default_bindings() {
            for c in chords {
                if let Some(chord) = parse_key(c) {
                    map.insert(chord, action);
                }
            }
        }

        for (name, binding) in overrides {
            let Some(action) = Action::from_name(name) else {
                tracing::warn!("ignoring unknown keybinding action `{name}`");
                continue;
            };
            // Drop existing chords bound to this action so the override fully
            // replaces the defaults.
            map.retain(|_, a| *a != action);
            for key in binding.keys() {
                match parse_key(key) {
                    Some(chord) => {
                        map.insert(chord, action);
                    }
                    None => tracing::warn!("ignoring unparseable key `{key}` for `{name}`"),
                }
            }
        }

        Self { map }
    }

    /// Resolve a pressed key to an action, if any is bound.
    pub fn action(&self, code: KeyCode, modifiers: KeyModifiers) -> Option<Action> {
        self.map.get(&normalize(code, modifiers)).copied()
    }
}

impl Default for Keymap {
    fn default() -> Self {
        Self::build(&HashMap::new())
    }
}

/// A config value for one action: either a single key string or a list of them.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum KeyBinding {
    One(String),
    Many(Vec<String>),
}

impl KeyBinding {
    fn keys(&self) -> Vec<&str> {
        match self {
            KeyBinding::One(s) => vec![s.as_str()],
            KeyBinding::Many(v) => v.iter().map(|s| s.as_str()).collect(),
        }
    }
}

/// Normalize a chord so lookups are consistent. For printable characters we
/// fold the SHIFT modifier into the character casing crossterm already reports,
/// so e.g. `G` matches even though SHIFT is set on the event.
fn normalize(code: KeyCode, modifiers: KeyModifiers) -> Chord {
    let mut m = modifiers;
    if let KeyCode::Char(_) = code {
        m.remove(KeyModifiers::SHIFT);
    }
    // Keep only the modifiers we care about for bindings.
    m &= KeyModifiers::CONTROL | KeyModifiers::ALT;
    (code, m)
}

/// Parse a key description into a chord.
///
/// Supports single characters, `space`, `enter`, `esc`, `tab`, the arrows,
/// `home`/`end`/`pageup`/`pagedown`, `f1`..`f12`, and `ctrl+`/`alt+`/`shift+`
/// modifier prefixes, plus literal `+` and `-`.
pub fn parse_key(spec: &str) -> Option<Chord> {
    let spec = spec.trim();
    if spec.is_empty() {
        return None;
    }

    // The bare `+` / `-` keys are literal and must not be parsed as separators.
    if spec == "+" || spec == "-" {
        return Some((KeyCode::Char(spec.chars().next().unwrap()), KeyModifiers::NONE));
    }

    let mut modifiers = KeyModifiers::NONE;
    // Split on `+`, treating a trailing empty token (from a literal `+`) as the
    // key itself.
    let parts: Vec<&str> = spec.split('+').collect();
    let (key_part, mod_parts) = match parts.last() {
        // "ctrl+" -> the key is a literal '+'
        Some(&"") if parts.len() >= 2 => ("+", &parts[..parts.len() - 1]),
        Some(&last) => (last, &parts[..parts.len() - 1]),
        None => return None,
    };

    for m in mod_parts {
        match m.trim().to_ascii_lowercase().as_str() {
            "ctrl" | "control" => modifiers |= KeyModifiers::CONTROL,
            "alt" | "meta" => modifiers |= KeyModifiers::ALT,
            "shift" => modifiers |= KeyModifiers::SHIFT,
            other => {
                tracing::warn!("unknown key modifier `{other}`");
                return None;
            }
        }
    }

    let code = parse_code(key_part)?;

    // For a printable char with SHIFT requested, crossterm reports the char with
    // SHIFT folded into casing; mirror normalize() so the chord matches.
    let chord = normalize(code, modifiers);
    Some(chord)
}

fn parse_code(s: &str) -> Option<KeyCode> {
    let lower = s.to_ascii_lowercase();
    let code = match lower.as_str() {
        "space" => KeyCode::Char(' '),
        "enter" | "return" | "cr" => KeyCode::Enter,
        "esc" | "escape" => KeyCode::Esc,
        "tab" => KeyCode::Tab,
        "backtab" => KeyCode::BackTab,
        "backspace" | "bs" => KeyCode::Backspace,
        "delete" | "del" => KeyCode::Delete,
        "insert" | "ins" => KeyCode::Insert,
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        "pageup" | "pgup" => KeyCode::PageUp,
        "pagedown" | "pgdn" | "pgdown" => KeyCode::PageDown,
        _ => {
            // Function keys f1..f12.
            if let Some(num) = lower.strip_prefix('f') {
                if let Ok(n) = num.parse::<u8>() {
                    if (1..=12).contains(&n) {
                        return Some(KeyCode::F(n));
                    }
                }
            }
            // Otherwise a single character (case-sensitive: `G` != `g`).
            let mut chars = s.chars();
            let c = chars.next()?;
            if chars.next().is_some() {
                return None; // multi-char, unknown token
            }
            return Some(KeyCode::Char(c));
        }
    };
    Some(code)
}
