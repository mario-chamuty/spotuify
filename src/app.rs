//! Application state and the main event loop. The loop multiplexes terminal
//! input, a repaint tick, librespot player events, and results from background
//! tasks (web API + album art) via `tokio::select!`.

use std::io::Stdout;
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyEventKind};
use futures::StreamExt;
use ratatui::backend::CrosstermBackend;
use ratatui::widgets::ListState;
use ratatui::Terminal;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use crate::albumart::AlbumArt;
use crate::audio;
use crate::config::Config;
use crate::keys::{Action, Keymap};
use crate::message::{OpenMode, Update};
use crate::model::{OutputDevice, Track};
use crate::player::Player;
use crate::spotify::{SearchKind, SearchResults, Spotify};
use crate::theme::Theme;
use crate::{albumart, ui};

pub type Tui = Terminal<CrosstermBackend<Stdout>>;

/// Roughly a 5% volume step (mixer range is 0..=u16::MAX).
/// Volume step, in percent.
const VOLUME_STEP: i32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Search,
    Library,
    Tracklist,
    Queue,
    Devices,
    Settings,
    Home,
}

/// A selectable item in the Home view, identifying the underlying data.
#[derive(Debug, Clone, Copy)]
pub enum HomeItem {
    /// A card in pathfinder shelf `shelf`, item `item` (real Spotify Home).
    Shelf(usize, usize),
    Recent(usize),
    TopTrack(usize),
    TopArtist(usize),
    Mix(usize),
}

/// One category row on the Home grid (card-shelf layout).
pub struct HomeShelf {
    pub title: String,
    pub cards: Vec<HomeCard>,
}

/// A single card on a shelf: two display lines plus how to activate it.
pub struct HomeCard {
    pub title: String,
    pub subtitle: String,
    pub item: HomeItem,
}

/// A restorable snapshot of a browse view, for the Esc back-stack.
enum NavSnapshot {
    Tracklist {
        title: String,
        tracks: Vec<Track>,
        sel: Option<usize>,
    },
    Results {
        input: String,
        kind: SearchKind,
        results: SearchResults,
        sel: Option<usize>,
    },
    Tab(View),
}

/// One adjustable row in the Settings view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingRow {
    Normalisation,
    Quality,
    Volume,
    EqEnabled,
    EqPreset,
    EqBand(usize),
    ArtMode,
    ReAuth,
}

impl SettingRow {
    /// All rows in display order.
    pub fn all() -> Vec<SettingRow> {
        let mut v = vec![
            SettingRow::Normalisation,
            SettingRow::Quality,
            SettingRow::Volume,
            SettingRow::EqEnabled,
            SettingRow::EqPreset,
        ];
        v.extend((0..crate::eq::BANDS).map(SettingRow::EqBand));
        v.push(SettingRow::ArtMode);
        v.push(SettingRow::ReAuth);
        v
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    List,
    Input,
    Filter,
}

/// A rendered row in the Devices view. Headers are non-selectable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceRow {
    Header,
    /// Index into `App::devices` (local audio output).
    Local(usize),
    /// Index into `App::connect_devices` (Spotify Connect).
    Connect(usize),
}

pub struct App {
    pub config: Config,
    pub spotify: Spotify,
    pub player: Player,
    pub theme: Theme,
    pub keymap: Keymap,

    updates_tx: UnboundedSender<Update>,
    updates_rx: UnboundedReceiver<Update>,

    // External controls (MPRIS): incoming actions + outgoing playback snapshot.
    control_rx: Option<UnboundedReceiver<crate::keys::Action>>,
    snapshot_tx: Option<tokio::sync::watch::Sender<crate::snapshot::Snapshot>>,

    pub view: View,
    /// Browse history, so Esc walks back through opened contexts
    /// (e.g. playlist → album → artist → Esc → album → Esc → playlist).
    nav_stack: Vec<NavSnapshot>,
    pub focus: Focus,
    pub should_quit: bool,
    pub status: String,

    // Search
    pub search_input: String,
    pub search_kind: SearchKind,
    pub search_results: Option<SearchResults>,
    pub search_state: ListState,

    // Search history (most-recent last). `history_pos` is the index currently
    // recalled while editing, or `None` when typing fresh text.
    pub search_history: Vec<String>,
    history_pos: Option<usize>,

    // Type-to-filter. When active, `filter_query` filters the focused list and
    // `filter_map` maps visible row indices back to underlying item indices.
    pub filter_query: String,
    pub filter_map: Vec<usize>,

    // Library (user playlists)
    pub playlists: Vec<crate::model::PlaylistRef>,
    pub library_state: ListState,

    // Opened track list (playlist/album/artist/liked/search tracks)
    pub context_title: String,
    pub context_tracks: Vec<Track>,
    pub tracklist_state: ListState,

    // Queue + devices
    pub queue_state: ListState,
    pub devices: Vec<OutputDevice>,
    pub device_state: ListState,

    // Spotify Connect (Web API) remote devices and remote-control state.
    pub connect_devices: Vec<crate::model::ConnectDevice>,
    /// When `Some`, transport routes to this Connect device id (remote mode);
    /// `None` means local librespot playback.
    pub remote_device_id: Option<String>,
    /// Latest polled remote playback snapshot (in remote mode).
    pub remote_state: Option<crate::model::RemoteState>,

    // Set of liked-track URIs (for the ♥ indicator), kept best-effort.
    pub liked: std::collections::HashSet<String>,

    // A modal text prompt (playlist create/rename) or playlist picker overlay.
    pub prompt: Option<Prompt>,
    pub picker: Option<Picker>,

    // Album art (half-block path)
    pub art: Option<AlbumArt>,
    art_pending: Option<(String, u16, u16)>,
    pub art_size: (u16, u16),

    // Pixel-graphics art (sixel/kitty/iTerm via ratatui-image). `image_picker`
    // is set when a pixel protocol is active; `pixel_art` holds the current
    // track's resizable protocol.
    pub image_picker: Option<ratatui_image::picker::Picker>,
    pub pixel_art: Option<PixelArt>,
    pixel_pending: Option<String>,

    // Lyrics. Shown in the Now Playing panel (in place of art) when toggled on.
    pub show_lyrics: bool,
    pub lyrics: Option<crate::lyrics::Lyrics>,
    /// Track URI the loaded `lyrics` belong to.
    lyrics_for: Option<String>,
    lyrics_pending: Option<String>,

    // Equalizer overlay.
    pub eq_open: bool,
    pub eq_sel: usize,

    /// Help modal (`?`): lists every keybinding.
    pub help_open: bool,

    /// Selected row in the Settings view (index into `SettingRow::all()`).
    pub settings_sel: usize,

    // Spectrum analyzer.
    spectrum: crate::analyzer::SharedSpectrum,
    /// Smoothed per-band display levels (0..1) for the visualizer.
    pub viz_levels: [f32; crate::eq::BANDS],
    /// Slow per-band dB average, for the experimental EQ suggestion.
    viz_avg_db: [f32; crate::eq::BANDS],
    /// Show the spectrum visualizer in the Now Playing panel.
    pub show_visualizer: bool,

    /// A transient note flashed next to the volume (easter egg).
    pub easter_egg: Option<(Egg, std::time::Instant)>,

    // Home tab.
    pub home: Option<crate::spotify::Home>,
    /// Grid selection: `(shelf index, card index within that shelf)`.
    pub home_sel: (usize, usize),
    pub home_loading: bool,
}

/// Volume easter eggs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Egg {
    Nice,
    SixSeven,
}

/// A decoded album cover bound to a terminal pixel-graphics protocol.
pub struct PixelArt {
    pub track_uri: String,
    pub protocol: ratatui_image::protocol::StatefulProtocol,
}

/// A modal single-line text prompt. `on_submit` records what to do with the
/// entered text.
#[derive(Debug, Clone)]
pub struct Prompt {
    pub title: String,
    pub input: String,
    pub kind: PromptKind,
}

#[derive(Debug, Clone)]
pub enum PromptKind {
    CreatePlaylist,
    RenamePlaylist { id: String },
}

/// A modal list picker (currently only used to choose a target playlist for
/// "add to playlist").
#[derive(Debug, Clone)]
pub struct Picker {
    pub title: String,
    pub state: ListState,
    /// (playlist id, label) rows.
    pub items: Vec<(String, String)>,
    /// The track URI to add to the chosen playlist.
    pub track_uri: String,
}

impl App {
    pub fn new(config: Config, spotify: Spotify, mut player: Player) -> Self {
        let (updates_tx, updates_rx) = tokio::sync::mpsc::unbounded_channel();
        let theme = Theme::from_config(&config.theme);
        let keymap = Keymap::build(&config.keys);

        // Restore the previous session (queue/position/prefs) — paused.
        let mut search_history = Vec::new();
        if let Some(state) = crate::persist::PersistedState::load() {
            player.restore_session(state.queue, state.current_index, state.position_ms);
            player.set_shuffle(state.shuffle);
            player.set_repeat(state.repeat.into());
            search_history = state.search_history;
        }

        let spectrum = player.spectrum();
        let mut app = Self {
            config,
            spotify,
            player,
            theme,
            keymap,
            updates_tx,
            updates_rx,
            control_rx: None,
            snapshot_tx: None,
            view: View::Search,
            nav_stack: Vec::new(),
            focus: Focus::Input,
            should_quit: false,
            status: "Welcome to SpoTUIfy — press ? for help".to_string(),
            search_input: String::new(),
            search_kind: SearchKind::Tracks,
            search_results: None,
            search_state: ListState::default(),
            search_history,
            history_pos: None,
            filter_query: String::new(),
            filter_map: Vec::new(),
            playlists: Vec::new(),
            library_state: ListState::default(),
            context_title: String::new(),
            context_tracks: Vec::new(),
            tracklist_state: ListState::default(),
            queue_state: ListState::default(),
            devices: Vec::new(),
            device_state: ListState::default(),
            connect_devices: Vec::new(),
            remote_device_id: None,
            remote_state: None,
            liked: std::collections::HashSet::new(),
            prompt: None,
            picker: None,
            art: None,
            art_pending: None,
            art_size: (0, 0),
            image_picker: None,
            pixel_art: None,
            pixel_pending: None,
            show_lyrics: false,
            lyrics: None,
            lyrics_for: None,
            lyrics_pending: None,
            eq_open: false,
            eq_sel: 0,
            help_open: false,
            settings_sel: 0,
            spectrum,
            viz_levels: [0.0; crate::eq::BANDS],
            viz_avg_db: [-30.0; crate::eq::BANDS],
            show_visualizer: false,
            easter_egg: None,
            home: None,
            home_sel: (0, 0),
            home_loading: false,
        };
        // Reflect a restored now-playing track in the queue selection.
        if let Some(i) = app.player.current {
            app.queue_state.select(Some(i));
        }
        app
    }

    /// Set the terminal pixel-graphics picker (enables sixel/kitty art). When
    /// `None`, album art uses the coloured half-block renderer.
    pub fn set_picker(&mut self, picker: Option<ratatui_image::picker::Picker>) {
        self.image_picker = picker;
    }

    /// Attach the MPRIS external-control channel and snapshot publisher.
    pub fn attach_external_controls(
        &mut self,
        control_rx: UnboundedReceiver<crate::keys::Action>,
        snapshot_tx: tokio::sync::watch::Sender<crate::snapshot::Snapshot>,
    ) {
        self.control_rx = Some(control_rx);
        self.snapshot_tx = Some(snapshot_tx);
    }

    pub async fn run(&mut self, terminal: &mut Tui) -> Result<()> {
        let mut events = EventStream::new();
        let mut ticker = tokio::time::interval(Duration::from_millis(250));
        let mut remote_poll = tokio::time::interval(Duration::from_millis(1500));
        let mut viz_poll = tokio::time::interval(Duration::from_millis(60));
        let mut player_events = self.player.take_events();
        // Take the external-control receiver out so we can borrow it in select!.
        let mut control_rx = self.control_rx.take();

        self.spawn_load_playlists();
        self.refresh_devices();

        while !self.should_quit {
            terminal.draw(|f| ui::draw(f, self))?;
            self.maybe_request_art();
            self.maybe_request_lyrics();
            self.publish_snapshot();

            tokio::select! {
                maybe_event = events.next() => {
                    if let Some(Ok(Event::Key(key))) = maybe_event {
                        if key.kind != KeyEventKind::Release {
                            self.handle_key(key);
                        }
                    }
                }
                _ = ticker.tick() => {}
                _ = remote_poll.tick() => self.spawn_poll_remote(),
                _ = viz_poll.tick() => self.update_spectrum(),
                Some(event) = player_events.recv() => {
                    if self.player.on_event(event) {
                        self.on_track_changed();
                    }
                }
                Some(action) = recv_opt(&mut control_rx) => self.do_action(action),
                Some(update) = self.updates_rx.recv() => self.handle_update(update),
            }
        }

        // Persist volume/device config and the full session state on quit.
        self.config.volume = self.player.volume_percent();
        let _ = self.config.save();
        self.save_state();
        Ok(())
    }

    /// Save the queue/position/preferences for the next launch.
    fn save_state(&self) {
        let state = crate::persist::PersistedState {
            queue: self.player.queue.clone(),
            current_index: self.player.current,
            position_ms: self.player.saved_position_ms(),
            shuffle: self.player.shuffle,
            repeat: self.player.repeat.into(),
            volume: self.player.volume_percent(),
            search_history: self.search_history.clone(),
        };
        if let Err(e) = state.save() {
            tracing::warn!("saving session state failed: {e}");
        }
    }

    /// Publish a playback snapshot for the MPRIS task (no-op if not attached).
    fn publish_snapshot(&self) {
        let Some(tx) = &self.snapshot_tx else { return };
        let status = self.playback_status();
        let track = self.player.current_track();
        let snap = crate::snapshot::Snapshot {
            playing: status == crate::player::Status::Playing,
            stopped: status == crate::player::Status::Stopped,
            has_track: track.is_some(),
            track_uri: track.map(|t| t.uri.clone()).unwrap_or_default(),
            title: track.map(|t| t.name.clone()).unwrap_or_default(),
            artist: track.map(|t| t.artists.clone()).unwrap_or_default(),
            album: track.map(|t| t.album.clone()).unwrap_or_default(),
            art_url: track.and_then(|t| t.album_art_url.clone()),
            length_us: track.map(|t| t.duration_ms as i64 * 1000).unwrap_or(0),
            position_us: self.playback_position() as i64 * 1000,
            volume: self.player.volume_percent() as f64 / 100.0,
            can_next: !self.player.queue.is_empty(),
            can_prev: !self.player.queue.is_empty(),
        };
        // Only send when something changed to avoid signal spam.
        if tx.borrow().track_uri != snap.track_uri
            || tx.borrow().playing != snap.playing
            || (tx.borrow().position_us - snap.position_us).abs() > 900_000
            || tx.borrow().volume != snap.volume
        {
            let _ = tx.send(snap);
        }
    }

    // ---- Input -------------------------------------------------------------

    fn handle_key(&mut self, key: KeyEvent) {
        // Free-text input modes consume keys directly (typing, not bindings).
        match self.focus {
            Focus::Input => return self.handle_input_key(key),
            Focus::Filter => return self.handle_filter_key(key),
            Focus::List => {}
        }

        // The help modal is dismissed by any key.
        if self.help_open {
            self.help_open = false;
            return;
        }
        // Modal overlays capture keys before any keybinding resolution.
        if self.eq_open {
            return self.handle_eq_key(key);
        }
        if self.picker.is_some() {
            self.handle_picker_key(key);
            return;
        }
        if self.prompt.is_some() {
            return self.handle_prompt_key(key);
        }

        // Esc walks back through opened contexts.
        if key.code == KeyCode::Esc
            && matches!(self.view, View::Tracklist | View::Search)
            && !self.nav_stack.is_empty()
        {
            self.nav_back();
            return;
        }

        // The Settings view edits values with arrows/Enter; consume those here,
        // letting everything else (global controls, tab switches) fall through.
        if self.view == View::Settings && self.handle_settings_key(key) {
            return;
        }
        // The Home view navigates its shelves with arrows/Enter.
        if self.view == View::Home && self.handle_home_key(key) {
            return;
        }

        // Resolve the chord to an action via the (configurable) keymap.
        let Some(action) = self.keymap.action(key.code, key.modifiers) else {
            return;
        };
        self.do_action(action);
    }

    /// Dispatch a resolved [`Action`]. This is also the entry point used by
    /// external controllers (MPRIS) so behaviour stays in one place.
    pub fn do_action(&mut self, action: Action) {
        match action {
            Action::Quit => self.should_quit = true,
            Action::PlayPause => self.transport_toggle_pause(),
            Action::Next => self.transport_next(),
            Action::Prev => self.transport_prev(),
            Action::VolumeUp => self.transport_volume_step(VOLUME_STEP),
            Action::VolumeDown => self.transport_volume_step(-VOLUME_STEP),
            Action::SeekForward => self.transport_seek_relative(5),
            Action::SeekBack => self.transport_seek_relative(-5),
            Action::ToggleShuffle => {
                self.player.toggle_shuffle();
                self.status = format!("Shuffle {}", on_off(self.player.shuffle));
            }
            Action::CycleRepeat => {
                self.player.cycle_repeat();
                self.status = format!("Repeat: {}", self.player.repeat.label());
            }
            Action::TabSearch => self.view = View::Search,
            Action::TabLibrary => self.goto_library(),
            Action::TabTracks => self.view = View::Tracklist,
            Action::TabQueue => self.view = View::Queue,
            Action::TabDevices => self.goto_devices(),
            Action::TabSettings => self.view = View::Settings,
            Action::TabHome => self.goto_home(),
            Action::CycleTab => self.cycle_view(),
            Action::Help => self.show_help(),
            Action::FocusSearch => {
                if self.view == View::Search {
                    self.focus = Focus::Input;
                }
            }
            Action::Up => {
                let len = self.current_list_len();
                move_sel(self.active_state(), len, -1);
            }
            Action::Down => {
                let len = self.current_list_len();
                move_sel(self.active_state(), len, 1);
            }
            Action::Top => {
                if self.current_list_len() > 0 {
                    self.active_state().select(Some(0));
                }
            }
            Action::Bottom => {
                let len = self.current_list_len();
                if len > 0 {
                    self.active_state().select(Some(len - 1));
                }
            }
            Action::Activate => self.activate_selection(),
            Action::Enqueue => self.enqueue_selection(),
            Action::OpenAlbum => self.open_album_from_selection(),
            Action::ToggleLike => self.toggle_like_selection(),
            Action::AddToPlaylist => self.open_add_to_playlist(),
            Action::ToggleLyrics => {
                self.show_lyrics = !self.show_lyrics;
                self.status = if self.show_lyrics {
                    "Lyrics on".to_string()
                } else {
                    "Lyrics off".to_string()
                };
            }
            Action::ToggleEqualizer => self.eq_open = !self.eq_open,
            Action::ToggleVisualizer => {
                self.show_visualizer = !self.show_visualizer;
                self.status = format!("Visualizer {}", on_off(self.show_visualizer));
            }
            Action::EnterFilter => self.enter_filter(),
            Action::CreatePlaylist => self.prompt_create_playlist(),
            Action::RenamePlaylist => self.prompt_rename_playlist(),
            Action::DeletePlaylist => self.delete_selected_playlist(),
            Action::CycleTabBack => self.cycle_view_back(),
            Action::OpenArtist => self.open_artist_from_selection(),
        }
    }

    fn show_help(&mut self) {
        self.help_open = true;
    }

    /// Open the artist of the selected (or now-playing) track, if it has a
    /// known artist id.
    fn open_artist_from_selection(&mut self) {
        let artist = self
            .selected_track()
            .or_else(|| self.player.current_track())
            .and_then(|t| t.artist.clone());
        match artist {
            Some((id, name)) => self.spawn_open_artist(id, name),
            None => self.status = "No artist info for this item.".to_string(),
        }
    }

    fn open_album_from_selection(&mut self) {
        let album = self
            .selected_track()
            .or_else(|| self.player.current_track())
            .and_then(|t| t.album_id.clone().map(|id| (id, t.album.clone())));
        match album {
            Some((id, name)) => self.spawn_open_album(id, name, OpenMode::Show),
            None => self.status = "No album info for this item.".to_string(),
        }
    }

    fn handle_input_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => self.focus = Focus::List,
            KeyCode::Enter => {
                self.focus = Focus::List;
                self.history_reset();
                self.spawn_search();
            }
            KeyCode::Backspace => {
                self.search_input.pop();
                self.history_pos = None;
            }
            KeyCode::Tab => self.cycle_search_kind(),
            KeyCode::Up => self.history_prev(),
            KeyCode::Down => self.history_next(),
            KeyCode::Char(c) => {
                self.search_input.push(c);
                self.history_pos = None;
            }
            _ => {}
        }
    }

    // ---- Navigation helpers ------------------------------------------------

    fn cycle_view(&mut self) {
        self.view = match self.view {
            View::Search => View::Library,
            View::Library => View::Tracklist,
            View::Tracklist => View::Queue,
            View::Queue => View::Devices,
            View::Devices => View::Settings,
            View::Settings => View::Home,
            View::Home => View::Search,
        };
        self.on_view_entered();
    }

    fn cycle_view_back(&mut self) {
        self.view = match self.view {
            View::Search => View::Home,
            View::Home => View::Settings,
            View::Settings => View::Devices,
            View::Devices => View::Queue,
            View::Queue => View::Tracklist,
            View::Tracklist => View::Library,
            View::Library => View::Search,
        };
        self.on_view_entered();
    }

    /// Side effects when a view becomes active (lazy data loads).
    fn on_view_entered(&mut self) {
        if self.view == View::Library {
            self.goto_library();
        } else if self.view == View::Devices {
            self.goto_devices();
        } else if self.view == View::Home {
            self.goto_home();
        }
    }

    fn goto_home(&mut self) {
        self.view = View::Home;
        if self.home.is_none() && !self.home_loading {
            self.spawn_load_home();
        }
    }

    /// Selectable Home items in display order (the renderer adds the headers).
    /// When pathfinder shelves are present they are the Home; otherwise the
    /// stable fallback shelves are shown.
    pub fn home_items(&self) -> Vec<HomeItem> {
        let mut v = Vec::new();
        if let Some(h) = &self.home {
            if !h.shelves.is_empty() {
                for (si, shelf) in h.shelves.iter().enumerate() {
                    v.extend((0..shelf.items.len()).map(|ii| HomeItem::Shelf(si, ii)));
                }
            } else {
                v.extend((0..h.recently.len()).map(HomeItem::Recent));
                v.extend((0..h.top_tracks.len()).map(HomeItem::TopTrack));
                v.extend((0..h.top_artists.len()).map(HomeItem::TopArtist));
                v.extend((0..h.mixes.len()).map(HomeItem::Mix));
            }
        }
        v
    }

    /// The Home laid out as card shelves: the real pathfinder shelves (Daily
    /// Mixes, Discover Weekly, genres) when present, otherwise the four stable
    /// categories. Drives both navigation and rendering.
    pub fn home_shelves(&self) -> Vec<HomeShelf> {
        let mut shelves = Vec::new();
        let Some(h) = &self.home else {
            return shelves;
        };

        if !h.shelves.is_empty() {
            for (si, shelf) in h.shelves.iter().enumerate() {
                let cards: Vec<HomeCard> = shelf
                    .items
                    .iter()
                    .enumerate()
                    .map(|(ii, it)| HomeCard {
                        title: it.name.clone(),
                        subtitle: it.subtitle.clone(),
                        item: HomeItem::Shelf(si, ii),
                    })
                    .collect();
                if !cards.is_empty() {
                    shelves.push(HomeShelf {
                        title: shelf.title.clone(),
                        cards,
                    });
                }
            }
            return shelves;
        }

        // Stable fallback: the four public-API categories.
        if !h.recently.is_empty() {
            shelves.push(HomeShelf {
                title: "Recently played".to_string(),
                cards: h
                    .recently
                    .iter()
                    .enumerate()
                    .map(|(i, t)| HomeCard {
                        title: t.name.clone(),
                        subtitle: t.artists.clone(),
                        item: HomeItem::Recent(i),
                    })
                    .collect(),
            });
        }
        if !h.top_tracks.is_empty() {
            shelves.push(HomeShelf {
                title: "Your Top Tracks".to_string(),
                cards: h
                    .top_tracks
                    .iter()
                    .enumerate()
                    .map(|(i, t)| HomeCard {
                        title: t.name.clone(),
                        subtitle: t.artists.clone(),
                        item: HomeItem::TopTrack(i),
                    })
                    .collect(),
            });
        }
        if !h.top_artists.is_empty() {
            shelves.push(HomeShelf {
                title: "Your Top Artists".to_string(),
                cards: h
                    .top_artists
                    .iter()
                    .enumerate()
                    .map(|(i, a)| HomeCard {
                        title: a.name.clone(),
                        subtitle: String::new(),
                        item: HomeItem::TopArtist(i),
                    })
                    .collect(),
            });
        }
        if !h.mixes.is_empty() {
            shelves.push(HomeShelf {
                title: "Made for you".to_string(),
                cards: h
                    .mixes
                    .iter()
                    .enumerate()
                    .map(|(i, m)| HomeCard {
                        title: m.label.clone(),
                        subtitle: String::new(),
                        item: HomeItem::Mix(i),
                    })
                    .collect(),
            });
        }
        shelves
    }

    fn handle_home_key(&mut self, key: KeyEvent) -> bool {
        let shelves = self.home_shelves();
        if shelves.is_empty() {
            return false;
        }
        let (mut shelf, mut col) = self.home_sel;
        shelf = shelf.min(shelves.len() - 1);
        let cards_in = |s: usize| shelves[s].cards.len().saturating_sub(1);
        col = col.min(cards_in(shelf));
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                shelf = shelf.saturating_sub(1);
                col = col.min(cards_in(shelf));
            }
            KeyCode::Down | KeyCode::Char('j') => {
                shelf = (shelf + 1).min(shelves.len() - 1);
                col = col.min(cards_in(shelf));
            }
            KeyCode::Left | KeyCode::Char('h') => {
                col = col.saturating_sub(1);
            }
            KeyCode::Right | KeyCode::Char('l') => {
                col = (col + 1).min(cards_in(shelf));
            }
            KeyCode::Home => {
                shelf = 0;
                col = 0;
            }
            KeyCode::End => {
                shelf = shelves.len() - 1;
                col = cards_in(shelf);
            }
            KeyCode::Enter => {
                self.home_sel = (shelf, col);
                self.activate_home();
                return true;
            }
            _ => return false,
        }
        self.home_sel = (shelf, col);
        true
    }

    fn activate_home(&mut self) {
        let (shelf, col) = self.home_sel;
        let item = match self
            .home_shelves()
            .get(shelf)
            .and_then(|s| s.cards.get(col))
        {
            Some(card) => card.item,
            None => return,
        };
        enum A {
            Play(Vec<Track>, usize),
            Artist(String, String),
            Playlist(String, String),
            Album(String, String),
        }
        // Resolve to owned data so the `&self.home` borrow ends before the
        // `&mut self` calls below.
        let act = {
            let Some(home) = self.home.as_ref() else { return };
            match item {
                HomeItem::Shelf(si, ii) => {
                    let Some(it) = home.shelves.get(si).and_then(|s| s.items.get(ii)) else {
                        return;
                    };
                    let id = it.uri.rsplit(':').next().unwrap_or("").to_string();
                    let name = it.name.clone();
                    if it.uri.contains(":playlist:") {
                        A::Playlist(id, name)
                    } else if it.uri.contains(":album:") {
                        A::Album(id, name)
                    } else if it.uri.contains(":artist:") {
                        A::Artist(id, name)
                    } else {
                        return;
                    }
                }
                HomeItem::Recent(i) => A::Play(home.recently.clone(), i),
                HomeItem::TopTrack(i) => A::Play(home.top_tracks.clone(), i),
                HomeItem::TopArtist(i) => match home.top_artists.get(i) {
                    Some(a) => A::Artist(a.id.clone(), a.name.clone()),
                    None => return,
                },
                HomeItem::Mix(i) => match home.mixes.get(i) {
                    Some(m) => A::Playlist(m.playlist_id.clone(), m.label.clone()),
                    None => return,
                },
            }
        };
        match act {
            A::Play(tracks, i) => {
                self.player.play_tracks(tracks, i);
                self.on_track_changed();
            }
            A::Artist(id, name) => self.spawn_open_artist(id, name),
            A::Playlist(id, label) => self.spawn_open_playlist(id, label, OpenMode::Show),
            A::Album(id, name) => self.spawn_open_album(id, name, OpenMode::Show),
        }
    }

    fn spawn_load_home(&mut self) {
        self.home_loading = true;
        self.status = "Loading Home…".to_string();
        let spotify = self.spotify.clone();
        let session = self.player.session();
        let sp_dc_cfg = self.config.sp_dc.clone();
        let tx = self.updates_tx.clone();
        tokio::spawn(async move {
            let mut home = crate::spotify::Home::default();

            // The real Spotify Home (Daily Mixes, Discover Weekly, genre/mood
            // shelves) via the private pathfinder API. Uses the configured
            // sp_dc cookie, or auto-detects one from a local browser profile.
            let sp_dc =
                tokio::task::spawn_blocking(move || crate::cookie::resolve(&sp_dc_cfg))
                    .await
                    .unwrap_or_default();
            if !sp_dc.trim().is_empty() {
                let token = crate::webtoken::WebToken::new(sp_dc);
                match crate::pathfinder::home_shelves(&token).await {
                    Ok(shelves) => home.shelves = shelves,
                    Err(e) => tracing::warn!("pathfinder Home unavailable, using fallback: {e:#}"),
                }
            }

            // Stable fallback shelves when pathfinder yielded nothing (no cookie,
            // expired cookie, or API refusal).
            if home.shelves.is_empty() {
                home.recently = spotify.recently_played().await.unwrap_or_default();
                home.top_tracks = spotify.top_tracks().await.unwrap_or_default();
                home.top_artists = spotify.top_artists().await.unwrap_or_default();
                let seeds: Vec<(String, String)> = home
                    .top_tracks
                    .iter()
                    .take(6)
                    .map(|t| (t.uri.clone(), t.name.clone()))
                    .collect();
                home.mixes = crate::browse::inspired_mixes(&session, &seeds).await;
            }

            let _ = tx.send(Update::Home(Box::new(home)));
        });
    }

    fn goto_library(&mut self) {
        self.view = View::Library;
        if self.playlists.is_empty() {
            self.spawn_load_playlists();
        }
    }

    fn goto_devices(&mut self) {
        self.view = View::Devices;
        self.refresh_devices();
        self.spawn_refresh_connect_devices();
    }

    fn refresh_devices(&mut self) {
        self.devices = audio::output_devices();
        // Select the first selectable row (skip the leading header).
        let rows = self.device_rows();
        let idx = rows.iter().position(|r| !matches!(r, DeviceRow::Header));
        self.device_state.select(idx.or(Some(0)));
    }

    /// Fetch the user's Spotify Connect devices in the background.
    fn spawn_refresh_connect_devices(&self) {
        let spotify = self.spotify.clone();
        let tx = self.updates_tx.clone();
        tokio::spawn(async move {
            match spotify.connect_devices().await {
                Ok(devs) => {
                    let _ = tx.send(Update::ConnectDevices(devs));
                }
                Err(e) => tracing::warn!("listing Connect devices failed: {e}"),
            }
        });
    }

    /// Poll remote playback state (only meaningful in remote mode).
    fn spawn_poll_remote(&self) {
        if !self.remote_active() {
            return;
        }
        let spotify = self.spotify.clone();
        let tx = self.updates_tx.clone();
        tokio::spawn(async move {
            match spotify.current_playback().await {
                Ok(state) => {
                    let _ = tx.send(Update::RemoteState(state));
                }
                Err(e) => tracing::warn!("polling remote playback failed: {e}"),
            }
        });
    }

    fn current_list_len(&self) -> usize {
        match self.view {
            View::Search => self.search_results.as_ref().map_or(0, search_len),
            View::Library => {
                if self.filter_active() {
                    self.filter_map.len()
                } else {
                    self.playlists.len() + 1 // +1 for "Liked Songs"
                }
            }
            View::Tracklist => {
                if self.filter_active() {
                    self.filter_map.len()
                } else {
                    self.context_tracks.len()
                }
            }
            View::Queue => {
                if self.filter_active() {
                    self.filter_map.len()
                } else {
                    self.player.queue.len()
                }
            }
            View::Devices => self.device_rows().len(),
            View::Settings => SettingRow::all().len(),
            View::Home => self.home_items().len(),
        }
    }

    fn active_state(&mut self) -> &mut ListState {
        match self.view {
            View::Search => &mut self.search_state,
            View::Library => &mut self.library_state,
            View::Tracklist => &mut self.tracklist_state,
            View::Queue => &mut self.queue_state,
            View::Devices => &mut self.device_state,
            // Settings/Home have their own (arrow-driven) navigation.
            View::Settings | View::Home => &mut self.library_state,
        }
    }

    // ---- Visible (filter-aware) index helpers used by the UI ---------------

    /// Underlying tracklist indices currently visible (after filtering).
    pub fn tracklist_visible_indices(&self) -> Vec<usize> {
        if self.filter_active() {
            self.filter_map.clone()
        } else {
            (0..self.context_tracks.len()).collect()
        }
    }

    /// Underlying queue indices currently visible (after filtering).
    pub fn queue_visible_indices(&self) -> Vec<usize> {
        if self.filter_active() {
            self.filter_map.clone()
        } else {
            (0..self.player.queue.len()).collect()
        }
    }

    /// Library row indices (0 = Liked Songs, then playlist index+1).
    pub fn library_visible_indices(&self) -> Vec<usize> {
        if self.filter_active() {
            self.filter_map.clone()
        } else {
            (0..self.playlists.len() + 1).collect()
        }
    }

    /// All rendered rows of the Devices view, including non-selectable headers.
    pub fn device_rows(&self) -> Vec<DeviceRow> {
        let mut rows = vec![DeviceRow::Header];
        for i in 0..self.devices.len() {
            rows.push(DeviceRow::Local(i));
        }
        if !self.connect_devices.is_empty() {
            rows.push(DeviceRow::Header);
            for i in 0..self.connect_devices.len() {
                rows.push(DeviceRow::Connect(i));
            }
        }
        rows
    }

    // ---- Playback state accessors (local or remote) ------------------------

    /// Whether transport is currently routed to a Connect device.
    pub fn remote_active(&self) -> bool {
        self.remote_device_id.is_some()
    }

    /// The active Connect device id, if in remote mode.
    pub fn active_remote_device_id(&self) -> Option<String> {
        self.remote_device_id.clone()
    }

    /// The track to display: the remote snapshot's track in remote mode (when
    /// known), otherwise the local player's current track.
    pub fn displayed_track(&self) -> Option<&Track> {
        if self.remote_active() {
            if let Some(s) = &self.remote_state {
                if let Some(t) = &s.track {
                    return Some(t);
                }
            }
        }
        self.player.current_track()
    }

    /// Lyrics to render, or a status message ("Loading…" / "No lyrics…") for the
    /// lyrics panel.
    pub fn lyrics_or_status(&self) -> Result<&crate::lyrics::Lyrics, &'static str> {
        match &self.lyrics {
            Some(l) if !l.lines.is_empty() => Ok(l),
            Some(_) => Err("No lyrics for this track."),
            None if self.displayed_track().is_none() => Err("Nothing playing."),
            None => {
                let uri = self.displayed_track().map(|t| t.uri.as_str());
                if self.lyrics_for.as_deref() == uri {
                    Err("No lyrics for this track.")
                } else {
                    Err("Loading lyrics…")
                }
            }
        }
    }

    /// Volume percentage to display (remote device volume in remote mode).
    pub fn displayed_volume_percent(&self) -> u8 {
        if self.remote_active() {
            if let Some(v) = self.remote_state.as_ref().and_then(|s| s.volume_percent) {
                return v;
            }
        }
        self.player.volume_percent()
    }

    /// Shuffle state to display (remote shuffle in remote mode).
    pub fn displayed_shuffle(&self) -> bool {
        if self.remote_active() {
            if let Some(s) = &self.remote_state {
                return s.shuffle;
            }
        }
        self.player.shuffle
    }

    /// Current playback position (remote snapshot if remote, else local).
    pub fn playback_position(&self) -> u32 {
        if self.remote_active() {
            self.remote_state.as_ref().map(|s| s.progress_ms).unwrap_or(0)
        } else {
            self.player.interpolated_position()
        }
    }

    /// Current playback status (derived from remote snapshot if remote).
    pub fn playback_status(&self) -> crate::player::Status {
        if self.remote_active() {
            match &self.remote_state {
                Some(s) if s.is_playing => crate::player::Status::Playing,
                Some(_) => crate::player::Status::Paused,
                None => crate::player::Status::Stopped,
            }
        } else {
            self.player.status
        }
    }

    // ---- Actions -----------------------------------------------------------

    /// Snapshot the current browse view onto the back-stack before opening a
    /// new context, so Esc can return to it.
    fn push_nav(&mut self) {
        let snap = match self.view {
            View::Tracklist if !self.context_tracks.is_empty() => NavSnapshot::Tracklist {
                title: self.context_title.clone(),
                tracks: self.context_tracks.clone(),
                sel: self.tracklist_state.selected(),
            },
            View::Search => match &self.search_results {
                Some(results) => NavSnapshot::Results {
                    input: self.search_input.clone(),
                    kind: self.search_kind,
                    results: results.clone(),
                    sel: self.search_state.selected(),
                },
                None => NavSnapshot::Tab(View::Search),
            },
            other => NavSnapshot::Tab(other),
        };
        self.nav_stack.push(snap);
        if self.nav_stack.len() > 32 {
            self.nav_stack.remove(0);
        }
    }

    /// Pop the back-stack and restore that browse view.
    fn nav_back(&mut self) {
        self.filter_query.clear();
        match self.nav_stack.pop() {
            Some(NavSnapshot::Tracklist { title, tracks, sel }) => {
                self.context_title = title;
                self.context_tracks = tracks;
                self.tracklist_state.select(sel);
                self.view = View::Tracklist;
            }
            Some(NavSnapshot::Results { input, kind, results, sel }) => {
                self.search_input = input;
                self.search_kind = kind;
                self.search_results = Some(results);
                self.search_state.select(sel);
                self.view = View::Search;
            }
            Some(NavSnapshot::Tab(v)) => self.view = v,
            None => self.view = View::Library,
        }
    }

    fn activate_selection(&mut self) {
        match self.view {
            View::Search => self.activate_search_selection(),
            View::Library => self.activate_library_selection(),
            View::Tracklist => {
                if let Some(i) = self
                    .tracklist_state
                    .selected()
                    .and_then(|v| self.resolve_index(v))
                {
                    let tracks = self.context_tracks.clone();
                    self.player.play_tracks(tracks, i);
                    self.on_track_changed();
                }
            }
            View::Queue => {
                if let Some(i) = self
                    .queue_state
                    .selected()
                    .and_then(|v| self.resolve_index(v))
                {
                    self.player.play_now(i);
                    self.on_track_changed();
                }
            }
            View::Devices => self.activate_device_selection(),
            View::Settings | View::Home => {} // handled by their own key handlers
        }
    }

    fn activate_search_selection(&mut self) {
        let Some(results) = &self.search_results else {
            return;
        };
        let Some(i) = self.search_state.selected() else {
            return;
        };
        match results {
            SearchResults::Tracks(tracks) => {
                let tracks = tracks.clone();
                self.player.play_tracks(tracks, i);
                self.on_track_changed();
            }
            SearchResults::Albums(albums) => {
                if let Some(a) = albums.get(i) {
                    self.spawn_open_album(a.id.clone(), a.name.clone(), OpenMode::Show);
                }
            }
            SearchResults::Artists(artists) => {
                if let Some(a) = artists.get(i) {
                    self.spawn_open_artist(a.id.clone(), a.name.clone());
                }
            }
            SearchResults::Playlists(playlists) => {
                if let Some(p) = playlists.get(i) {
                    self.spawn_open_playlist(p.id.clone(), p.name.clone(), OpenMode::Show);
                }
            }
            SearchResults::Episodes(eps) => {
                // Play the whole episode result list from the selection.
                let tracks: Vec<Track> = eps
                    .iter()
                    .map(|e| Track {
                        uri: e.uri.clone(),
                        name: e.name.clone(),
                        artists: e.show.clone(),
                        album: String::new(),
                        album_art_url: e.album_art_url.clone(),
                        duration_ms: e.duration_ms,
                        kind: crate::model::PlayableKind::Episode,
                        artist: None,
                        album_id: None,
                    })
                    .collect();
                self.player.play_tracks(tracks, i);
                self.on_track_changed();
            }
            SearchResults::Shows(shows) => {
                if let Some(s) = shows.get(i) {
                    self.spawn_open_show(s.id.clone(), s.name.clone());
                }
            }
        }
    }

    fn activate_library_selection(&mut self) {
        let Some(i) = self
            .library_state
            .selected()
            .and_then(|v| self.resolve_index(v))
        else {
            return;
        };
        if i == 0 {
            self.spawn_liked();
        } else if let Some(p) = self.playlists.get(i - 1) {
            self.spawn_open_playlist(p.id.clone(), p.name.clone(), OpenMode::Show);
        }
    }

    fn activate_device_selection(&mut self) {
        let Some(sel) = self.device_state.selected() else {
            return;
        };
        match self.device_rows().get(sel).copied() {
            Some(DeviceRow::Local(i)) => self.select_local_device(i),
            Some(DeviceRow::Connect(i)) => self.select_connect_device(i),
            _ => {} // header or out of range
        }
    }

    fn select_local_device(&mut self, i: usize) {
        let Some(dev) = self.devices.get(i).cloned() else {
            return;
        };
        // Leaving remote mode: stop polling and return control to librespot.
        let was_remote = self.remote_device_id.take();
        self.remote_state = None;
        if was_remote.is_some() {
            self.status = "Back to local playback".to_string();
        }
        let device = if dev.is_default { None } else { Some(dev.name.clone()) };
        match self.player.set_output_device(device.clone()) {
            Ok(()) => {
                self.config.audio_device = device;
                let _ = self.config.save();
                self.status = format!("Output → {}", dev.name);
            }
            Err(e) => self.status = format!("Output change failed: {e}"),
        }
    }

    fn select_connect_device(&mut self, i: usize) {
        let Some(dev) = self.connect_devices.get(i).cloned() else {
            return;
        };
        let Some(id) = dev.id.clone() else {
            self.status = format!("“{}” cannot be controlled remotely.", dev.name);
            return;
        };
        // Enter remote mode: transfer playback to the Connect device and start
        // polling its state.
        self.remote_device_id = Some(id.clone());
        self.remote_state = None;
        self.status = format!("Transferring playback → {}", dev.name);
        let spotify = self.spotify.clone();
        let tx = self.updates_tx.clone();
        tokio::spawn(async move {
            if let Err(e) = spotify.transfer_playback(&id, true).await {
                let _ = tx.send(Update::Error(format!("{e:#}")));
            }
        });
    }

    fn enqueue_selection(&mut self) {
        let track = match self.view {
            View::Search => match self.search_results.as_ref() {
                Some(SearchResults::Tracks(tracks)) => {
                    self.search_state.selected().and_then(|i| tracks.get(i)).cloned()
                }
                _ => None,
            },
            View::Tracklist => self
                .tracklist_state
                .selected()
                .and_then(|i| self.context_tracks.get(i))
                .cloned(),
            _ => None,
        };
        if let Some(track) = track {
            self.status = format!("Queued: {}", track.name);
            self.player.enqueue(track);
        }
    }

    // ---- Transport: routes to the Connect device in remote mode, else local

    fn transport_toggle_pause(&mut self) {
        if let Some(id) = self.remote_device_id.clone() {
            let want_play = !self
                .remote_state
                .as_ref()
                .map(|s| s.is_playing)
                .unwrap_or(false);
            // Optimistic local flip for snappy UI; the next poll corrects it.
            if let Some(s) = self.remote_state.as_mut() {
                s.is_playing = want_play;
            }
            let spotify = self.spotify.clone();
            let tx = self.updates_tx.clone();
            tokio::spawn(async move {
                let res = if want_play {
                    spotify.remote_resume(&id).await
                } else {
                    spotify.remote_pause(&id).await
                };
                if let Err(e) = res {
                    let _ = tx.send(Update::Error(format!("{e:#}")));
                }
            });
        } else {
            self.player.toggle_pause();
        }
    }

    fn transport_next(&mut self) {
        if let Some(id) = self.remote_device_id.clone() {
            self.remote_call(move |s| async move { s.remote_next(&id).await });
        } else {
            self.player.next();
            self.on_track_changed();
        }
    }

    fn transport_prev(&mut self) {
        if let Some(id) = self.remote_device_id.clone() {
            self.remote_call(move |s| async move { s.remote_previous(&id).await });
        } else {
            self.player.previous();
            self.on_track_changed();
        }
    }

    fn transport_seek_relative(&mut self, secs: i64) {
        if let Some(id) = self.remote_device_id.clone() {
            let cur = self.playback_position() as i64;
            let target = (cur + secs * 1000).max(0) as u32;
            self.remote_call(move |s| async move { s.remote_seek(target, &id).await });
        } else {
            self.player.seek_relative(secs);
        }
    }

    fn transport_volume_step(&mut self, delta: i32) {
        let pct = if let Some(id) = self.remote_device_id.clone() {
            let cur = self
                .remote_state
                .as_ref()
                .and_then(|s| s.volume_percent)
                .unwrap_or(50) as i32;
            let new = (cur + delta).clamp(0, 100) as u8;
            if let Some(s) = self.remote_state.as_mut() {
                s.volume_percent = Some(new);
            }
            self.remote_call(move |s| async move { s.remote_volume(new, &id).await });
            new
        } else {
            self.player.volume_step(delta);
            self.config.volume = self.player.volume_percent();
            self.player.volume_percent()
        };
        self.trigger_volume_egg(pct);
    }

    /// A tiny easter egg: flash a note next to the volume at certain values.
    fn trigger_volume_egg(&mut self, pct: u8) {
        let egg = match pct {
            69 => Some(Egg::Nice),
            67 => Some(Egg::SixSeven),
            _ => None,
        };
        if let Some(egg) = egg {
            self.easter_egg = Some((egg, std::time::Instant::now()));
        }
    }

    /// Spawn a remote control call, forwarding any error to the status line.
    fn remote_call<F, Fut>(&self, f: F)
    where
        F: FnOnce(Spotify) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<()>> + Send,
    {
        let spotify = self.spotify.clone();
        let tx = self.updates_tx.clone();
        tokio::spawn(async move {
            if let Err(e) = f(spotify).await {
                let _ = tx.send(Update::Error(format!("{e:#}")));
            }
        });
    }

    // ---- Search history ----------------------------------------------------

    fn history_reset(&mut self) {
        self.history_pos = None;
    }

    /// Recall an older query (Up in the search box).
    fn history_prev(&mut self) {
        if self.search_history.is_empty() {
            return;
        }
        let next = match self.history_pos {
            None => self.search_history.len() - 1,
            Some(0) => 0,
            Some(i) => i - 1,
        };
        self.history_pos = Some(next);
        self.search_input = self.search_history[next].clone();
    }

    /// Recall a newer query, or clear back to an empty box past the newest.
    fn history_next(&mut self) {
        let Some(i) = self.history_pos else { return };
        if i + 1 < self.search_history.len() {
            self.history_pos = Some(i + 1);
            self.search_input = self.search_history[i + 1].clone();
        } else {
            self.history_pos = None;
            self.search_input.clear();
        }
    }

    /// Record a query into the rolling history (deduped, newest last, max 50).
    fn record_history(&mut self, query: &str) {
        let q = query.trim();
        if q.is_empty() {
            return;
        }
        self.search_history.retain(|h| h != q);
        self.search_history.push(q.to_string());
        let len = self.search_history.len();
        if len > 50 {
            self.search_history.drain(0..len - 50);
        }
    }

    // ---- Type-to-filter ----------------------------------------------------

    fn enter_filter(&mut self) {
        if self.view == View::Search {
            // In search view, `/` focuses the query box instead.
            self.focus = Focus::Input;
            return;
        }
        self.filter_query.clear();
        self.rebuild_filter();
        self.focus = Focus::Filter;
        self.status = "Filter: (type to filter · Enter keep · Esc clear)".to_string();
    }

    fn handle_filter_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.filter_query.clear();
                self.filter_map.clear();
                self.focus = Focus::List;
                self.status.clear();
            }
            KeyCode::Enter => {
                // Keep the filter applied but return to list navigation.
                self.focus = Focus::List;
            }
            KeyCode::Backspace => {
                self.filter_query.pop();
                self.rebuild_filter();
            }
            KeyCode::Char(c) => {
                self.filter_query.push(c);
                self.rebuild_filter();
            }
            _ => {}
        }
    }

    /// Recompute `filter_map` from the active list for the current query and
    /// reset the selection to the first match.
    fn rebuild_filter(&mut self) {
        let q = self.filter_query.to_lowercase();
        let labels = self.filterable_labels();
        self.filter_map = labels
            .into_iter()
            .enumerate()
            .filter(|(_, label)| q.is_empty() || label.to_lowercase().contains(&q))
            .map(|(i, _)| i)
            .collect();
        let sel = (!self.filter_map.is_empty()).then_some(0);
        self.active_state().select(sel);
    }

    /// Whether a filter is currently constraining the active view.
    pub fn filter_active(&self) -> bool {
        self.focus == Focus::Filter || !self.filter_query.is_empty()
    }

    /// Searchable text for each row of the currently focused list.
    fn filterable_labels(&self) -> Vec<String> {
        match self.view {
            View::Tracklist => self
                .context_tracks
                .iter()
                .map(|t| format!("{} {}", t.name, t.artists))
                .collect(),
            View::Queue => self
                .player
                .queue
                .iter()
                .map(|t| format!("{} {}", t.name, t.artists))
                .collect(),
            View::Library => {
                let mut v = vec!["Liked Songs".to_string()];
                v.extend(self.playlists.iter().map(|p| p.name.clone()));
                v
            }
            View::Devices => self.devices.iter().map(|d| d.name.clone()).collect(),
            View::Search | View::Settings | View::Home => Vec::new(),
        }
    }

    /// Map a visible (possibly filtered) row index to the underlying item
    /// index for the active view.
    fn resolve_index(&self, visible: usize) -> Option<usize> {
        if self.filter_active() && !matches!(self.view, View::Search) {
            self.filter_map.get(visible).copied()
        } else {
            Some(visible)
        }
    }

    /// The track highlighted in the current view, if the view holds tracks.
    fn selected_track(&self) -> Option<&Track> {
        match self.view {
            View::Search => match self.search_results.as_ref()? {
                SearchResults::Tracks(tracks) => {
                    tracks.get(self.resolve_index(self.search_state.selected()?)?)
                }
                _ => None,
            },
            View::Tracklist => self
                .context_tracks
                .get(self.resolve_index(self.tracklist_state.selected()?)?),
            View::Queue => self
                .player
                .queue
                .get(self.resolve_index(self.queue_state.selected()?)?),
            _ => None,
        }
    }

    // ---- Library writes (filled in by the library-writes feature) ----------

    fn toggle_like_selection(&mut self) {
        let Some(uri) = self.selected_or_current_track_uri() else {
            self.status = "Nothing to like.".to_string();
            return;
        };
        let Some(id) = uri.strip_prefix("spotify:track:").map(str::to_string) else {
            self.status = "Only tracks can be liked.".to_string();
            return;
        };
        let currently_liked = self.liked.contains(&uri);
        // Optimistically flip local state; the task corrects on error.
        if currently_liked {
            self.liked.remove(&uri);
        } else {
            self.liked.insert(uri.clone());
        }
        self.status = if currently_liked {
            "Removed from Liked Songs".to_string()
        } else {
            "Added to Liked Songs".to_string()
        };
        let spotify = self.spotify.clone();
        let tx = self.updates_tx.clone();
        tokio::spawn(async move {
            let res = if currently_liked {
                spotify.unlike_track(&id).await
            } else {
                spotify.like_track(&id).await
            };
            if let Err(e) = res {
                let _ = tx.send(Update::Error(format!("{e:#}")));
            }
        });
    }

    fn open_add_to_playlist(&mut self) {
        let Some(uri) = self.selected_or_current_track_uri() else {
            self.status = "Select a track to add first.".to_string();
            return;
        };
        if self.playlists.is_empty() {
            self.status = "No playlists loaded yet — open Library first.".to_string();
            return;
        }
        let items: Vec<(String, String)> = self
            .playlists
            .iter()
            .map(|p| (p.id.clone(), p.name.clone()))
            .collect();
        let mut state = ListState::default();
        state.select(Some(0));
        self.picker = Some(Picker {
            title: "Add to playlist".to_string(),
            state,
            items,
            track_uri: uri,
        });
    }

    fn prompt_create_playlist(&mut self) {
        self.prompt = Some(Prompt {
            title: "New playlist name".to_string(),
            input: String::new(),
            kind: PromptKind::CreatePlaylist,
        });
    }

    fn prompt_rename_playlist(&mut self) {
        let Some(p) = self.selected_playlist() else {
            self.status = "Select one of your playlists to rename.".to_string();
            return;
        };
        self.prompt = Some(Prompt {
            title: format!("Rename “{}”", p.name),
            input: p.name.clone(),
            kind: PromptKind::RenamePlaylist { id: p.id.clone() },
        });
    }

    fn delete_selected_playlist(&mut self) {
        let Some(p) = self.selected_playlist().cloned() else {
            self.status = "Select one of your playlists to remove.".to_string();
            return;
        };
        self.status = format!("Unfollowing “{}”…", p.name);
        let spotify = self.spotify.clone();
        let tx = self.updates_tx.clone();
        tokio::spawn(async move {
            match spotify.unfollow_playlist(&p.id).await {
                Ok(()) => {
                    if let Ok(pl) = spotify.user_playlists().await {
                        let _ = tx.send(Update::Playlists(pl));
                    }
                }
                Err(e) => {
                    let _ = tx.send(Update::Error(format!("{e:#}")));
                }
            }
        });
    }

    /// The currently selected library playlist (skips the Liked Songs row).
    fn selected_playlist(&self) -> Option<&crate::model::PlaylistRef> {
        if self.view != View::Library {
            return None;
        }
        let visible = self.library_state.selected()?;
        let idx = self.resolve_index(visible)?;
        if idx == 0 {
            None // Liked Songs row
        } else {
            self.playlists.get(idx - 1)
        }
    }

    /// URI of the selected track in a track list/queue/search, else the
    /// now-playing track. Used by like / add-to-playlist.
    fn selected_or_current_track_uri(&self) -> Option<String> {
        let from_list = match self.view {
            View::Tracklist => self
                .tracklist_state
                .selected()
                .and_then(|v| self.resolve_index(v))
                .and_then(|i| self.context_tracks.get(i))
                .map(|t| t.uri.clone()),
            View::Queue => self
                .queue_state
                .selected()
                .and_then(|v| self.resolve_index(v))
                .and_then(|i| self.player.queue.get(i))
                .map(|t| t.uri.clone()),
            View::Search => match self.search_results.as_ref() {
                Some(SearchResults::Tracks(tracks)) => self
                    .search_state
                    .selected()
                    .and_then(|i| tracks.get(i))
                    .map(|t| t.uri.clone()),
                _ => None,
            },
            _ => None,
        };
        from_list.or_else(|| self.player.current_track().map(|t| t.uri.clone()))
    }

    // ---- Equalizer overlay -------------------------------------------------

    fn handle_eq_key(&mut self, key: KeyEvent) {
        let eq = self.player.eq();
        // Whether this key changed the curve (so we can auto-enable the EQ —
        // otherwise "changing the equalizer does nothing" because it's off).
        let mut changed = false;
        match key.code {
            KeyCode::Esc | KeyCode::Char('E') | KeyCode::Char('q') => self.eq_open = false,
            KeyCode::Left | KeyCode::Char('h') => self.eq_sel = self.eq_sel.saturating_sub(1),
            KeyCode::Right | KeyCode::Char('l') => {
                self.eq_sel = (self.eq_sel + 1).min(crate::eq::BANDS - 1);
            }
            KeyCode::Up | KeyCode::Char('k') | KeyCode::Char('+') | KeyCode::Char('=') => {
                eq.adjust(self.eq_sel, 1);
                changed = true;
            }
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Char('-') | KeyCode::Char('_') => {
                eq.adjust(self.eq_sel, -1);
                changed = true;
            }
            KeyCode::Char('0') => {
                eq.adjust(self.eq_sel, -eq.gain(self.eq_sel));
                changed = true;
            }
            KeyCode::Char('R') => {
                eq.reset();
                changed = true;
            }
            KeyCode::Char(' ') | KeyCode::Char('t') => eq.toggle(),
            KeyCode::Char('p') => self.apply_preset(1),
            KeyCode::Char('P') => self.apply_preset(-1),
            KeyCode::Char('a') => self.apply_suggestion(),
            _ => {}
        }
        // Touching a band turns the EQ on, so the change is audible immediately.
        if changed && !eq.enabled() {
            eq.toggle();
        }
        // Mirror into config so the change survives a restart (also saved on quit).
        self.config.equalizer.enabled = eq.enabled();
        self.config.equalizer.gains_db = eq.gains();
        if !self.eq_open {
            let _ = self.config.save();
        }
    }

    // ---- Settings view -----------------------------------------------------

    /// Handle a key in the Settings view. Returns `true` if it was consumed
    /// (arrows/Enter edit settings); other keys fall through to the keymap.
    fn handle_settings_key(&mut self, key: KeyEvent) -> bool {
        let rows = SettingRow::all();
        let len = rows.len();
        self.settings_sel = self.settings_sel.min(len - 1);
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.settings_sel = self.settings_sel.saturating_sub(1);
                true
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.settings_sel = (self.settings_sel + 1).min(len - 1);
                true
            }
            KeyCode::Home => {
                self.settings_sel = 0;
                true
            }
            KeyCode::End => {
                self.settings_sel = len - 1;
                true
            }
            KeyCode::Left => {
                self.adjust_setting(rows[self.settings_sel], -1);
                true
            }
            KeyCode::Right => {
                self.adjust_setting(rows[self.settings_sel], 1);
                true
            }
            KeyCode::Enter => {
                self.activate_setting(rows[self.settings_sel]);
                true
            }
            _ => false,
        }
    }

    fn adjust_setting(&mut self, row: SettingRow, dir: i32) {
        match row {
            SettingRow::Normalisation => self.set_normalisation(!self.config.normalisation),
            SettingRow::Quality => self.set_quality(dir),
            SettingRow::EqEnabled => {
                let eq = self.player.eq();
                eq.toggle();
                self.config.equalizer.enabled = eq.enabled();
                let _ = self.config.save();
            }
            SettingRow::Volume => {
                self.player.volume_step(dir * VOLUME_STEP);
                self.config.volume = self.player.volume_percent();
                self.trigger_volume_egg(self.config.volume);
                let _ = self.config.save();
            }
            SettingRow::EqPreset => self.apply_preset(dir),
            SettingRow::EqBand(i) => {
                let eq = self.player.eq();
                eq.adjust(i, dir);
                if !eq.enabled() {
                    eq.toggle(); // adjusting a band turns the EQ on
                }
                self.config.equalizer.enabled = eq.enabled();
                self.config.equalizer.gains_db = eq.gains();
                let _ = self.config.save();
            }
            SettingRow::ArtMode => self.cycle_art_mode(dir),
            SettingRow::ReAuth => {}
        }
    }

    fn activate_setting(&mut self, row: SettingRow) {
        match row {
            SettingRow::Normalisation | SettingRow::EqEnabled | SettingRow::ArtMode
            | SettingRow::EqPreset | SettingRow::Quality => self.adjust_setting(row, 1),
            SettingRow::EqBand(i) => {
                let eq = self.player.eq();
                eq.adjust(i, -eq.gain(i)); // reset band to 0 dB
                self.config.equalizer.gains_db = eq.gains();
                let _ = self.config.save();
            }
            SettingRow::Volume => {}
            SettingRow::ReAuth => self.reauthenticate(),
        }
    }

    fn set_normalisation(&mut self, on: bool) {
        match self.player.set_normalisation(on) {
            Ok(()) => {
                self.config.normalisation = on;
                let _ = self.config.save();
                self.status = format!("Normalisation {}", on_off(on));
            }
            Err(e) => self.status = format!("Normalisation change failed: {e}"),
        }
    }

    fn set_quality(&mut self, dir: i32) {
        let quality = self.config.audio_quality.cycle(dir);
        match self.player.set_quality(quality) {
            Ok(()) => {
                self.config.audio_quality = quality;
                let _ = self.config.save();
                self.status = format!("Quality: {} ({} kbps)", quality.label(), quality.kbps());
            }
            Err(e) => self.status = format!("Quality change failed: {e}"),
        }
    }

    /// Apply the next/previous EQ preset (wrapping). From a custom curve, steps
    /// to the first/last preset.
    fn apply_preset(&mut self, dir: i32) {
        let eq = self.player.eq();
        let len = crate::eq::PRESETS.len() as i32;
        let next = match eq.matched_preset() {
            Some(i) => (i as i32 + dir).rem_euclid(len),
            None if dir >= 0 => 0,
            None => len - 1,
        } as usize;
        let (name, gains) = &crate::eq::PRESETS[next];
        eq.set_gains(gains);
        if !eq.enabled() {
            eq.toggle();
        }
        self.config.equalizer.enabled = eq.enabled();
        self.config.equalizer.gains_db = eq.gains();
        let _ = self.config.save();
        self.status = format!("EQ preset: {name}");
    }

    /// Name of the active EQ preset, or "Custom".
    pub fn preset_name(&self) -> &'static str {
        self.player
            .eq()
            .matched_preset()
            .map_or("Custom", |i| crate::eq::PRESETS[i].0)
    }

    fn cycle_art_mode(&mut self, dir: i32) {
        use crate::config::ArtMode::*;
        let order = [Auto, Halfblocks, Sixel, Kitty];
        let cur = order.iter().position(|m| *m == self.config.art_mode).unwrap_or(0) as i32;
        let next = (cur + dir).rem_euclid(order.len() as i32) as usize;
        self.config.art_mode = order[next];
        let _ = self.config.save();
        self.status = "Album-art mode saved — restart to apply.".to_string();
    }

    fn reauthenticate(&mut self) {
        match crate::config::clear_credentials() {
            Ok(()) => self.status = "Signed out — restart SpoTUIfy to log in again.".to_string(),
            Err(e) => self.status = format!("Sign out failed: {e}"),
        }
    }

    // ---- Spectrum analyzer -------------------------------------------------

    /// Refresh the visualizer's per-band levels (smoothed) and the slow dB
    /// average used by the EQ suggestion. Only runs while something is shown.
    fn update_spectrum(&mut self) {
        if !(self.show_visualizer || self.eq_open) {
            return;
        }
        let playing = self.player.status == crate::player::Status::Playing;
        for i in 0..crate::eq::BANDS {
            let rms = if playing { self.spectrum.band(i) } else { 0.0 };
            let db = 20.0 * (rms + 1e-6).log10();
            let target = ((db + 60.0) / 60.0).clamp(0.0, 1.0);
            let cur = self.viz_levels[i];
            // Fast attack, slow decay reads well for a bar meter.
            let rate = if target > cur { 0.5 } else { 0.15 };
            self.viz_levels[i] = cur + (target - cur) * rate;
            if playing && rms > 1e-4 {
                self.viz_avg_db[i] += (db - self.viz_avg_db[i]) * 0.03;
            }
        }
    }

    /// Experimental: nudge the EQ toward a flatter balance based on the measured
    /// long-term spectrum (loud bands cut a little, quiet bands lifted), capped
    /// at ±6 dB. Needs a few seconds of playback first.
    fn apply_suggestion(&mut self) {
        let max = self.viz_avg_db.iter().copied().fold(f32::MIN, f32::max);
        let min = self.viz_avg_db.iter().copied().fold(f32::MAX, f32::min);
        if max - min < 1.0 {
            self.status = "Play a track for a few seconds, then press a.".to_string();
            return;
        }
        let mean: f32 = self.viz_avg_db.iter().sum::<f32>() / crate::eq::BANDS as f32;
        let eq = self.player.eq();
        for i in 0..crate::eq::BANDS {
            let delta = -((self.viz_avg_db[i] - mean) * 0.4);
            let target = (delta.round() as i32).clamp(-6, 6);
            eq.adjust(i, target - eq.gain(i)); // set band to `target`
        }
        if !eq.enabled() {
            eq.toggle();
        }
        self.config.equalizer.enabled = eq.enabled();
        self.config.equalizer.gains_db = eq.gains();
        let _ = self.config.save();
        self.status = "Suggested EQ from the spectrum (experimental).".to_string();
    }

    // ---- Modal prompt + picker handling ------------------------------------

    fn handle_prompt_key(&mut self, key: KeyEvent) {
        let Some(prompt) = self.prompt.as_mut() else {
            return;
        };
        match key.code {
            KeyCode::Esc => self.prompt = None,
            KeyCode::Backspace => {
                prompt.input.pop();
            }
            KeyCode::Char(c) => prompt.input.push(c),
            KeyCode::Enter => {
                let prompt = self.prompt.take().unwrap();
                let name = prompt.input.trim().to_string();
                if name.is_empty() {
                    return;
                }
                match prompt.kind {
                    PromptKind::CreatePlaylist => self.spawn_create_playlist(name),
                    PromptKind::RenamePlaylist { id } => self.spawn_rename_playlist(id, name),
                }
            }
            _ => {}
        }
    }

    /// Handle a key while the playlist picker overlay is open. Returns `true`
    /// if the key was consumed.
    fn handle_picker_key(&mut self, key: KeyEvent) -> bool {
        let Some(picker) = self.picker.as_mut() else {
            return false;
        };
        let len = picker.items.len();
        match key.code {
            KeyCode::Esc => self.picker = None,
            KeyCode::Up | KeyCode::Char('k') => move_sel(&mut picker.state, len, -1),
            KeyCode::Down | KeyCode::Char('j') => move_sel(&mut picker.state, len, 1),
            KeyCode::Enter => {
                let picker = self.picker.take().unwrap();
                if let Some(i) = picker.state.selected() {
                    if let Some((id, label)) = picker.items.get(i) {
                        self.spawn_add_to_playlist(
                            id.clone(),
                            picker.track_uri.clone(),
                            label.clone(),
                        );
                    }
                }
            }
            _ => {}
        }
        true
    }

    fn on_track_changed(&mut self) {
        // Drop stale art so the UI shows a placeholder until the new cover loads.
        self.art = None;
        self.art_pending = None;
        self.pixel_art = None;
        self.pixel_pending = None;
        self.lyrics = None;
        self.lyrics_for = None;
        self.lyrics_pending = None;
        if let Some(i) = self.player.current {
            self.queue_state.select(Some(i));
        }
        // Refresh the ♥ indicator for the now-playing track.
        if let Some(uri) = self.player.current_track().map(|t| t.uri.clone()) {
            self.spawn_refresh_liked(vec![uri]);
        }
    }

    // ---- Background tasks --------------------------------------------------

    fn spawn_search(&mut self) {
        let query = self.search_input.trim().to_string();
        if query.is_empty() {
            return;
        }
        self.record_history(&query);
        self.status = format!("Searching “{query}”…");
        let spotify = self.spotify.clone();
        let tx = self.updates_tx.clone();
        let kind = self.search_kind;
        tokio::spawn(async move {
            let msg = match spotify.search(&query, kind).await {
                Ok(r) => Update::Search(r),
                Err(e) => Update::Error(format!("{e:#}")),
            };
            let _ = tx.send(msg);
        });
    }

    fn spawn_load_playlists(&self) {
        let spotify = self.spotify.clone();
        let tx = self.updates_tx.clone();
        tokio::spawn(async move {
            let msg = match spotify.user_playlists().await {
                Ok(p) => Update::Playlists(p),
                Err(e) => Update::Error(format!("{e:#}")),
            };
            let _ = tx.send(msg);
        });
    }

    fn spawn_liked(&mut self) {
        self.push_nav();
        self.status = "Loading Liked Songs…".to_string();
        let spotify = self.spotify.clone();
        let tx = self.updates_tx.clone();
        tokio::spawn(async move {
            let msg = match spotify.saved_tracks().await {
                Ok(tracks) => Update::Tracks {
                    title: "Liked Songs".to_string(),
                    tracks,
                    mode: OpenMode::Show,
                },
                Err(e) => Update::Error(format!("{e:#}")),
            };
            let _ = tx.send(msg);
        });
    }

    fn spawn_open_playlist(&mut self, id: String, name: String, mode: OpenMode) {
        self.push_nav();
        self.status = format!("Loading playlist “{name}”…");
        // Resolved over the playback session: the Web API playlist endpoints are
        // 403 for development-mode apps since 2026.
        let session = self.player.session();
        let tx = self.updates_tx.clone();
        tokio::spawn(async move {
            let msg = match crate::browse::playlist_tracks(&session, &id).await {
                Ok(tracks) => Update::Tracks { title: name, tracks, mode },
                Err(e) => Update::Error(format!("{e:#}")),
            };
            let _ = tx.send(msg);
        });
    }

    fn spawn_open_album(&mut self, id: String, name: String, mode: OpenMode) {
        self.push_nav();
        self.status = format!("Loading album “{name}”…");
        let spotify = self.spotify.clone();
        let tx = self.updates_tx.clone();
        tokio::spawn(async move {
            let msg = match spotify.album_tracks(&id).await {
                Ok(tracks) => Update::Tracks { title: name, tracks, mode },
                Err(e) => Update::Error(format!("{e:#}")),
            };
            let _ = tx.send(msg);
        });
    }

    fn spawn_open_artist(&mut self, id: String, name: String) {
        self.push_nav();
        self.status = format!("Loading “{name}” albums…");
        let spotify = self.spotify.clone();
        let tx = self.updates_tx.clone();
        tokio::spawn(async move {
            let msg = match spotify.artist_albums(&id).await {
                Ok(albums) => Update::Search(SearchResults::Albums(albums)),
                Err(e) => Update::Error(format!("{e:#}")),
            };
            let _ = tx.send(msg);
        });
    }

    fn spawn_open_show(&mut self, id: String, name: String) {
        self.push_nav();
        self.status = format!("Loading podcast “{name}”…");
        let spotify = self.spotify.clone();
        let tx = self.updates_tx.clone();
        tokio::spawn(async move {
            let msg = match spotify.show_episodes(&id).await {
                Ok((title, tracks)) => Update::Tracks {
                    title,
                    tracks,
                    mode: OpenMode::Show,
                },
                Err(e) => Update::Error(format!("{e:#}")),
            };
            let _ = tx.send(msg);
        });
    }

    fn spawn_create_playlist(&mut self, name: String) {
        self.status = format!("Creating playlist “{name}”…");
        let spotify = self.spotify.clone();
        let tx = self.updates_tx.clone();
        tokio::spawn(async move {
            match spotify.create_playlist(&name).await {
                Ok(()) => {
                    if let Ok(pl) = spotify.user_playlists().await {
                        let _ = tx.send(Update::Playlists(pl));
                    }
                }
                Err(e) => {
                    let _ = tx.send(Update::Error(format!("{e:#}")));
                }
            }
        });
    }

    fn spawn_rename_playlist(&mut self, id: String, name: String) {
        self.status = format!("Renaming to “{name}”…");
        let spotify = self.spotify.clone();
        let tx = self.updates_tx.clone();
        tokio::spawn(async move {
            match spotify.rename_playlist(&id, &name).await {
                Ok(()) => {
                    if let Ok(pl) = spotify.user_playlists().await {
                        let _ = tx.send(Update::Playlists(pl));
                    }
                }
                Err(e) => {
                    let _ = tx.send(Update::Error(format!("{e:#}")));
                }
            }
        });
    }

    fn spawn_add_to_playlist(&mut self, playlist_id: String, track_uri: String, label: String) {
        self.status = format!("Adding to “{label}”…");
        let spotify = self.spotify.clone();
        let tx = self.updates_tx.clone();
        tokio::spawn(async move {
            if let Err(e) = spotify.add_to_playlist(&playlist_id, &track_uri).await {
                let _ = tx.send(Update::Error(format!("{e:#}")));
            }
        });
    }

    /// Refresh which of the given tracks the user has saved, updating `liked`.
    fn spawn_refresh_liked(&self, uris: Vec<String>) {
        let ids: Vec<String> = uris
            .iter()
            .filter_map(|u| u.strip_prefix("spotify:track:").map(str::to_string))
            .collect();
        if ids.is_empty() {
            return;
        }
        let spotify = self.spotify.clone();
        let tx = self.updates_tx.clone();
        tokio::spawn(async move {
            match spotify.tracks_saved(&ids).await {
                Ok(flags) => {
                    let liked: Vec<String> = ids
                        .into_iter()
                        .zip(flags)
                        .filter(|(_, saved)| *saved)
                        .map(|(id, _)| format!("spotify:track:{id}"))
                        .collect();
                    let _ = tx.send(Update::Liked(liked));
                }
                Err(e) => tracing::warn!("checking saved tracks failed: {e}"),
            }
        });
    }

    fn cycle_search_kind(&mut self) {
        let idx = SearchKind::ALL.iter().position(|k| *k == self.search_kind).unwrap_or(0);
        self.search_kind = SearchKind::ALL[(idx + 1) % SearchKind::ALL.len()];
    }

    fn maybe_request_art(&mut self) {
        let (cols, rows) = self.art_size;
        if cols == 0 || rows == 0 {
            return;
        }
        let Some(track) = self.displayed_track() else {
            return;
        };
        let Some(url) = track.album_art_url.clone() else {
            return;
        };
        let uri = track.uri.clone();

        // Pixel-graphics path: fetch and decode once per track (the protocol
        // resizes itself when the panel changes).
        if self.image_picker.is_some() {
            let have = self
                .pixel_art
                .as_ref()
                .is_some_and(|p| p.track_uri == uri);
            let pending = self.pixel_pending.as_deref() == Some(uri.as_str());
            if have || pending {
                return;
            }
            self.pixel_pending = Some(uri.clone());
            let tx = self.updates_tx.clone();
            tokio::spawn(async move {
                match albumart::fetch_image(&url).await {
                    Ok(image) => {
                        let _ = tx.send(Update::ArtImage { track_uri: uri, image });
                    }
                    Err(e) => tracing::warn!("album art (image) failed: {e}"),
                }
            });
            return;
        }

        // Half-block path.
        let have = self
            .art
            .as_ref()
            .is_some_and(|a| a.track_uri == uri && a.cols == cols && a.rows == rows);
        let pending = self.art_pending.as_ref() == Some(&(uri.clone(), cols, rows));
        if have || pending {
            return;
        }

        self.art_pending = Some((uri.clone(), cols, rows));
        let tx = self.updates_tx.clone();
        tokio::spawn(async move {
            match albumart::fetch_and_render(&url, cols, rows).await {
                Ok(lines) => {
                    let _ = tx.send(Update::AlbumArt { track_uri: uri, cols, rows, lines });
                }
                Err(e) => tracing::warn!("album art failed: {e}"),
            }
        });
    }

    /// Fetch lyrics for the now-playing track when the lyrics panel is shown
    /// and they aren't already loaded/loading for it.
    fn maybe_request_lyrics(&mut self) {
        if !self.show_lyrics {
            return;
        }
        let Some(track) = self.displayed_track().cloned() else {
            return;
        };
        let uri = track.uri.clone();
        let loaded = self.lyrics_for.as_deref() == Some(uri.as_str());
        let pending = self.lyrics_pending.as_deref() == Some(uri.as_str());
        if loaded || pending {
            return;
        }
        self.lyrics_pending = Some(uri.clone());
        let session = self.player.session();
        let tx = self.updates_tx.clone();
        tokio::spawn(async move {
            let lyrics = crate::lyrics::fetch(&session, &track).await.ok();
            let _ = tx.send(Update::Lyrics { track_uri: uri, lyrics });
        });
    }

    // ---- Updates -----------------------------------------------------------

    fn handle_update(&mut self, update: Update) {
        match update {
            Update::Search(results) => {
                let len = search_len(&results);
                self.search_results = Some(results);
                self.search_state.select((len > 0).then_some(0));
                self.view = View::Search;
                self.status = format!("{len} result(s)");
            }
            Update::Playlists(playlists) => {
                self.playlists = playlists;
                if self.library_state.selected().is_none() {
                    self.library_state.select(Some(0));
                }
            }
            Update::Tracks { title, tracks, mode } => {
                let count = tracks.len();
                self.context_title = title;
                self.context_tracks = tracks;
                self.tracklist_state.select((count > 0).then_some(0));
                self.view = View::Tracklist;
                self.filter_query.clear();
                self.status = format!("{} — {count} track(s)", self.context_title);
                // Best-effort: learn which of these are already liked.
                let uris: Vec<String> =
                    self.context_tracks.iter().map(|t| t.uri.clone()).collect();
                self.spawn_refresh_liked(uris);
                if mode == OpenMode::Play && count > 0 {
                    let tracks = self.context_tracks.clone();
                    self.player.play_tracks(tracks, 0);
                    self.on_track_changed();
                }
            }
            Update::AlbumArt { track_uri, cols, rows, lines } => {
                // Ignore art that arrived after the track moved on.
                if self.displayed_track().map(|t| t.uri.as_str()) == Some(track_uri.as_str()) {
                    self.art = Some(AlbumArt { track_uri, cols, rows, lines });
                }
                self.art_pending = None;
            }
            Update::ArtImage { track_uri, image } => {
                self.pixel_pending = None;
                let still_current =
                    self.displayed_track().map(|t| t.uri.as_str()) == Some(track_uri.as_str());
                if let (true, Some(picker)) = (still_current, self.image_picker.as_ref()) {
                    let protocol = picker.new_resize_protocol(image);
                    self.pixel_art = Some(PixelArt { track_uri, protocol });
                }
            }
            Update::Liked(uris) => {
                self.liked.extend(uris);
            }
            Update::ConnectDevices(devs) => {
                self.connect_devices = devs;
                if self.view == View::Devices {
                    self.refresh_devices();
                }
            }
            Update::RemoteState(state) => {
                if self.remote_active() {
                    // If the server reports playback moved to a different
                    // device than the one we selected, follow it.
                    if let Some(s) = &state {
                        if let Some(did) = &s.device_id {
                            self.remote_device_id = Some(did.clone());
                        }
                    }
                    self.remote_state = state;
                }
            }
            Update::Lyrics { track_uri, lyrics } => {
                self.lyrics_pending = None;
                // Ignore lyrics that arrived after the track moved on.
                if self.displayed_track().map(|t| t.uri.as_str()) == Some(track_uri.as_str()) {
                    self.lyrics = lyrics;
                    self.lyrics_for = Some(track_uri);
                }
            }
            Update::Home(home) => {
                self.home = Some(*home);
                self.home_loading = false;
                self.home_sel = (0, 0);
                self.status = "Home".to_string();
            }
            Update::Error(msg) => {
                tracing::error!("{msg}");
                self.status = format!("Error: {msg}");
            }
        }
    }
}

/// Await an optional receiver. When the receiver is `None`, this future never
/// resolves so the corresponding `select!` arm is effectively disabled.
async fn recv_opt(
    rx: &mut Option<UnboundedReceiver<crate::keys::Action>>,
) -> Option<crate::keys::Action> {
    match rx {
        Some(rx) => rx.recv().await,
        None => std::future::pending().await,
    }
}

fn move_sel(state: &mut ListState, len: usize, delta: isize) {
    if len == 0 {
        state.select(None);
        return;
    }
    let cur = state.selected().unwrap_or(0) as isize;
    let new = (cur + delta).rem_euclid(len as isize) as usize;
    state.select(Some(new));
}

fn search_len(results: &SearchResults) -> usize {
    match results {
        SearchResults::Tracks(v) => v.len(),
        SearchResults::Albums(v) => v.len(),
        SearchResults::Artists(v) => v.len(),
        SearchResults::Playlists(v) => v.len(),
        SearchResults::Episodes(v) => v.len(),
        SearchResults::Shows(v) => v.len(),
    }
}

fn on_off(v: bool) -> &'static str {
    if v {
        "on"
    } else {
        "off"
    }
}
