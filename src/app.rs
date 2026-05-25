//! Application state and the main event loop. The loop multiplexes terminal
//! input, a repaint tick, librespot player events, and results from background
//! tasks (web API + album art) via `tokio::select!`.

use std::io::Stdout;
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use futures::StreamExt;
use ratatui::backend::CrosstermBackend;
use ratatui::widgets::ListState;
use ratatui::Terminal;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use crate::albumart::AlbumArt;
use crate::audio;
use crate::config::Config;
use crate::message::{OpenMode, Update};
use crate::model::{OutputDevice, Track};
use crate::player::Player;
use crate::spotify::{SearchKind, SearchResults, Spotify};
use crate::{albumart, ui};

pub type Tui = Terminal<CrosstermBackend<Stdout>>;

/// Roughly a 5% volume step (mixer range is 0..=u16::MAX).
const VOLUME_STEP: i32 = (u16::MAX as i32) / 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Search,
    Library,
    Tracklist,
    Queue,
    Devices,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    List,
    Input,
}

pub struct App {
    pub config: Config,
    pub spotify: Spotify,
    pub player: Player,

    updates_tx: UnboundedSender<Update>,
    updates_rx: UnboundedReceiver<Update>,

    pub view: View,
    pub focus: Focus,
    pub should_quit: bool,
    pub status: String,

    // Search
    pub search_input: String,
    pub search_kind: SearchKind,
    pub search_results: Option<SearchResults>,
    pub search_state: ListState,

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

    // Album art
    pub art: Option<AlbumArt>,
    art_pending: Option<(String, u16, u16)>,
    pub art_size: (u16, u16),
}

impl App {
    pub fn new(config: Config, spotify: Spotify, player: Player) -> Self {
        let (updates_tx, updates_rx) = tokio::sync::mpsc::unbounded_channel();
        Self {
            config,
            spotify,
            player,
            updates_tx,
            updates_rx,
            view: View::Search,
            focus: Focus::Input,
            should_quit: false,
            status: "Welcome to SpoTUIfy — press ? for help".to_string(),
            search_input: String::new(),
            search_kind: SearchKind::Tracks,
            search_results: None,
            search_state: ListState::default(),
            playlists: Vec::new(),
            library_state: ListState::default(),
            context_title: String::new(),
            context_tracks: Vec::new(),
            tracklist_state: ListState::default(),
            queue_state: ListState::default(),
            devices: Vec::new(),
            device_state: ListState::default(),
            art: None,
            art_pending: None,
            art_size: (0, 0),
        }
    }

    pub async fn run(&mut self, terminal: &mut Tui) -> Result<()> {
        let mut events = EventStream::new();
        let mut ticker = tokio::time::interval(Duration::from_millis(250));
        let mut player_events = self.player.take_events();

        self.spawn_load_playlists();
        self.refresh_devices();

        while !self.should_quit {
            terminal.draw(|f| ui::draw(f, self))?;
            self.maybe_request_art();

            tokio::select! {
                maybe_event = events.next() => {
                    if let Some(Ok(Event::Key(key))) = maybe_event {
                        if key.kind != KeyEventKind::Release {
                            self.handle_key(key);
                        }
                    }
                }
                _ = ticker.tick() => {}
                Some(event) = player_events.recv() => {
                    if self.player.on_event(event) {
                        self.on_track_changed();
                    }
                }
                Some(update) = self.updates_rx.recv() => self.handle_update(update),
            }
        }

        // Persist any volume/device changes the user made this session.
        self.config.volume = self.player.volume_percent();
        let _ = self.config.save();
        Ok(())
    }

    // ---- Input -------------------------------------------------------------

    fn handle_key(&mut self, key: KeyEvent) {
        if self.focus == Focus::Input {
            self.handle_input_key(key);
            return;
        }

        // Ctrl-C always quits.
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.should_quit = true;
            return;
        }

        match key.code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Char(' ') => self.player.toggle_pause(),
            KeyCode::Char('n') => self.player.next(),
            KeyCode::Char('b') => self.player.previous(),
            KeyCode::Char('+') | KeyCode::Char('=') => self.player.volume_step(VOLUME_STEP),
            KeyCode::Char('-') | KeyCode::Char('_') => self.player.volume_step(-VOLUME_STEP),
            KeyCode::Char('s') => {
                self.player.toggle_shuffle();
                self.status = format!("Shuffle {}", on_off(self.player.shuffle));
            }
            KeyCode::Char('r') => {
                self.player.cycle_repeat();
                self.status = format!("Repeat: {}", self.player.repeat.label());
            }
            KeyCode::Char('[') => self.player.seek_relative(-5),
            KeyCode::Char(']') => self.player.seek_relative(5),
            KeyCode::Char('1') => self.view = View::Search,
            KeyCode::Char('2') => self.goto_library(),
            KeyCode::Char('3') => self.view = View::Tracklist,
            KeyCode::Char('4') => self.view = View::Queue,
            KeyCode::Char('5') => self.goto_devices(),
            KeyCode::Tab => self.cycle_view(),
            KeyCode::Char('?') => {
                self.status =
                    "[1-5] tabs  Tab cycle  Enter play/open  e enqueue  space pause  n/b next/prev  +/- vol  [ ] seek  s shuffle  r repeat  q quit".to_string();
            }
            KeyCode::Char('/') | KeyCode::Char('i') if self.view == View::Search => {
                self.focus = Focus::Input;
            }
            _ => self.handle_view_key(key),
        }
    }

    fn handle_input_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => self.focus = Focus::List,
            KeyCode::Enter => {
                self.focus = Focus::List;
                self.spawn_search();
            }
            KeyCode::Backspace => {
                self.search_input.pop();
            }
            KeyCode::Tab => self.cycle_search_kind(),
            KeyCode::Char(c) => self.search_input.push(c),
            _ => {}
        }
    }

    fn handle_view_key(&mut self, key: KeyEvent) {
        let len = self.current_list_len();
        match key.code {
            KeyCode::Down | KeyCode::Char('j') => move_sel(self.active_state(), len, 1),
            KeyCode::Up | KeyCode::Char('k') => move_sel(self.active_state(), len, -1),
            KeyCode::Char('g') if len > 0 => self.active_state().select(Some(0)),
            KeyCode::Char('G') if len > 0 => self.active_state().select(Some(len - 1)),
            KeyCode::Enter => self.activate_selection(),
            KeyCode::Char('e') => self.enqueue_selection(),
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
            View::Devices => View::Search,
        };
        if self.view == View::Library {
            self.goto_library();
        } else if self.view == View::Devices {
            self.goto_devices();
        }
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
    }

    fn refresh_devices(&mut self) {
        self.devices = audio::output_devices();
        // Highlight the currently active device.
        let active = self.player.current_device();
        let idx = match active {
            Some(name) => self.devices.iter().position(|d| d.name == name),
            None => self.devices.iter().position(|d| d.is_default),
        };
        self.device_state.select(idx.or(Some(0)));
    }

    fn current_list_len(&self) -> usize {
        match self.view {
            View::Search => self.search_results.as_ref().map_or(0, search_len),
            View::Library => self.playlists.len() + 1, // +1 for "Liked Songs"
            View::Tracklist => self.context_tracks.len(),
            View::Queue => self.player.queue.len(),
            View::Devices => self.devices.len(),
        }
    }

    fn active_state(&mut self) -> &mut ListState {
        match self.view {
            View::Search => &mut self.search_state,
            View::Library => &mut self.library_state,
            View::Tracklist => &mut self.tracklist_state,
            View::Queue => &mut self.queue_state,
            View::Devices => &mut self.device_state,
        }
    }

    // ---- Actions -----------------------------------------------------------

    fn activate_selection(&mut self) {
        match self.view {
            View::Search => self.activate_search_selection(),
            View::Library => self.activate_library_selection(),
            View::Tracklist => {
                if let Some(i) = self.tracklist_state.selected() {
                    let tracks = self.context_tracks.clone();
                    self.player.play_tracks(tracks, i);
                    self.on_track_changed();
                }
            }
            View::Queue => {
                if let Some(i) = self.queue_state.selected() {
                    self.player.play_now(i);
                    self.on_track_changed();
                }
            }
            View::Devices => self.activate_device_selection(),
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
        }
    }

    fn activate_library_selection(&mut self) {
        let Some(i) = self.library_state.selected() else {
            return;
        };
        if i == 0 {
            self.spawn_liked();
        } else if let Some(p) = self.playlists.get(i - 1) {
            self.spawn_open_playlist(p.id.clone(), p.name.clone(), OpenMode::Show);
        }
    }

    fn activate_device_selection(&mut self) {
        let Some(i) = self.device_state.selected() else {
            return;
        };
        let Some(dev) = self.devices.get(i).cloned() else {
            return;
        };
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

    fn on_track_changed(&mut self) {
        // Drop stale art so the UI shows a placeholder until the new cover loads.
        self.art = None;
        self.art_pending = None;
        if let Some(i) = self.player.current {
            self.queue_state.select(Some(i));
        }
    }

    // ---- Background tasks --------------------------------------------------

    fn spawn_search(&mut self) {
        let query = self.search_input.trim().to_string();
        if query.is_empty() {
            return;
        }
        self.status = format!("Searching “{query}”…");
        let spotify = self.spotify.clone();
        let tx = self.updates_tx.clone();
        let kind = self.search_kind;
        tokio::spawn(async move {
            let msg = match spotify.search(&query, kind).await {
                Ok(r) => Update::Search(r),
                Err(e) => Update::Error(e.to_string()),
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
                Err(e) => Update::Error(e.to_string()),
            };
            let _ = tx.send(msg);
        });
    }

    fn spawn_liked(&mut self) {
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
                Err(e) => Update::Error(e.to_string()),
            };
            let _ = tx.send(msg);
        });
    }

    fn spawn_open_playlist(&mut self, id: String, name: String, mode: OpenMode) {
        self.status = format!("Loading playlist “{name}”…");
        let spotify = self.spotify.clone();
        let tx = self.updates_tx.clone();
        tokio::spawn(async move {
            let msg = match spotify.playlist_tracks(&id).await {
                Ok(tracks) => Update::Tracks { title: name, tracks, mode },
                Err(e) => Update::Error(e.to_string()),
            };
            let _ = tx.send(msg);
        });
    }

    fn spawn_open_album(&mut self, id: String, name: String, mode: OpenMode) {
        self.status = format!("Loading album “{name}”…");
        let spotify = self.spotify.clone();
        let tx = self.updates_tx.clone();
        tokio::spawn(async move {
            let msg = match spotify.album_tracks(&id).await {
                Ok(tracks) => Update::Tracks { title: name, tracks, mode },
                Err(e) => Update::Error(e.to_string()),
            };
            let _ = tx.send(msg);
        });
    }

    fn spawn_open_artist(&mut self, id: String, name: String) {
        self.status = format!("Loading “{name}” top tracks…");
        let spotify = self.spotify.clone();
        let tx = self.updates_tx.clone();
        tokio::spawn(async move {
            let msg = match spotify.artist_top_tracks(&id).await {
                Ok(tracks) => Update::Tracks {
                    title: format!("{name} — top tracks"),
                    tracks,
                    mode: OpenMode::Show,
                },
                Err(e) => Update::Error(e.to_string()),
            };
            let _ = tx.send(msg);
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
        let Some(track) = self.player.current_track() else {
            return;
        };
        let Some(url) = track.album_art_url.clone() else {
            return;
        };
        let uri = track.uri.clone();

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
                self.status = format!("{} — {count} track(s)", self.context_title);
                if mode == OpenMode::Play && count > 0 {
                    let tracks = self.context_tracks.clone();
                    self.player.play_tracks(tracks, 0);
                    self.on_track_changed();
                }
            }
            Update::AlbumArt { track_uri, cols, rows, lines } => {
                // Ignore art that arrived after the track moved on.
                if self.player.current_track().map(|t| t.uri.as_str()) == Some(track_uri.as_str()) {
                    self.art = Some(AlbumArt { track_uri, cols, rows, lines });
                }
                self.art_pending = None;
            }
            Update::Error(msg) => {
                tracing::error!("{msg}");
                self.status = format!("Error: {msg}");
            }
        }
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
    }
}

fn on_off(v: bool) -> &'static str {
    if v {
        "on"
    } else {
        "off"
    }
}
