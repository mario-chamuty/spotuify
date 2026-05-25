//! Local playback engine built on librespot.
//!
//! Owns the connected [`Session`], the librespot [`Player`], a software mixer
//! for volume, and an in-app queue. High-level operations (play/pause, next,
//! seek, enqueue, output-device switching) are exposed for the UI; librespot's
//! own player events are forwarded onto a stable channel so that rebuilding the
//! player when the output device changes is invisible to the rest of the app.

use std::time::Instant;

use anyhow::{anyhow, Context, Result};
use librespot::core::authentication::Credentials;
use librespot::core::cache::Cache;
use librespot::core::{Session, SessionConfig, SpotifyId};
use librespot::playback::audio_backend;
use librespot::playback::config::{AudioFormat, PlayerConfig};
use librespot::playback::mixer::softmixer::SoftMixer;
use librespot::playback::mixer::{Mixer, MixerConfig};
use librespot::playback::player::{Player as LibrespotPlayer, PlayerEvent};
use std::sync::Arc;
use tokio::sync::mpsc;

use crate::config::Config;
use crate::model::Track;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Stopped,
    Loading,
    Playing,
    Paused,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Repeat {
    Off,
    All,
    One,
}

impl Repeat {
    pub fn label(self) -> &'static str {
        match self {
            Repeat::Off => "off",
            Repeat::All => "all",
            Repeat::One => "one",
        }
    }
    fn next(self) -> Self {
        match self {
            Repeat::Off => Repeat::All,
            Repeat::All => Repeat::One,
            Repeat::One => Repeat::Off,
        }
    }
}

pub struct Player {
    session: Session,
    inner: Arc<LibrespotPlayer>,
    mixer: Arc<SoftMixer>,

    // Parameters needed to rebuild `inner` when the output device changes.
    player_config: PlayerConfig,
    backend: String,
    audio_format: AudioFormat,
    device: Option<String>,

    // Stable event plumbing (survives player rebuilds).
    events_tx: mpsc::UnboundedSender<PlayerEvent>,
    events_rx: Option<mpsc::UnboundedReceiver<PlayerEvent>>,

    // Queue + playback state.
    pub queue: Vec<Track>,
    pub current: Option<usize>,
    pub status: Status,
    pub repeat: Repeat,
    pub shuffle: bool,
    pub volume: u16,

    position_ms: u32,
    position_anchor: Instant,
    current_id: Option<SpotifyId>,
    /// Whether `current_id` has actually been loaded into librespot. A restored
    /// session sets `current` without loading, so the first play loads it.
    loaded: bool,
    rng_state: u64,
}

impl Player {
    /// Connect a librespot session and build the playback pipeline.
    pub async fn connect(config: &Config, credentials: Credentials, cache: Cache) -> Result<Self> {
        let session_config = SessionConfig {
            client_id: config.client_id.clone(),
            ..Default::default()
        };
        let session = Session::new(session_config, Some(cache));
        session
            .connect(credentials, true)
            .await
            .context("connecting to Spotify (is this a Premium account?)")?;

        let mixer = Arc::new(SoftMixer::open(MixerConfig::default()));
        let volume = config.volume_u16();
        mixer.set_volume(volume);

        let player_config = PlayerConfig {
            normalisation: config.normalisation,
            ..Default::default()
        };

        let (events_tx, events_rx) = mpsc::unbounded_channel();

        let backend = config.audio_backend.clone();
        let audio_format = AudioFormat::default();
        let device = config.audio_device.clone();
        let inner = build_inner(
            &player_config,
            &session,
            &mixer,
            &backend,
            device.clone(),
            audio_format,
            &events_tx,
        )?;

        Ok(Self {
            session,
            inner,
            mixer,
            player_config,
            backend,
            audio_format,
            device,
            events_tx,
            events_rx: Some(events_rx),
            queue: Vec::new(),
            current: None,
            status: Status::Stopped,
            repeat: Repeat::Off,
            shuffle: false,
            volume,
            position_ms: 0,
            position_anchor: Instant::now(),
            current_id: None,
            loaded: false,
            rng_state: seed(),
        })
    }

    /// Rebuild `self.inner` for the current backend/device.
    fn rebuild(&mut self) -> Result<()> {
        self.inner = build_inner(
            &self.player_config,
            &self.session,
            &self.mixer,
            &self.backend,
            self.device.clone(),
            self.audio_format,
            &self.events_tx,
        )?;
        Ok(())
    }

    /// Take the stable event receiver (call once, from the app's run loop).
    pub fn take_events(&mut self) -> mpsc::UnboundedReceiver<PlayerEvent> {
        self.events_rx
            .take()
            .expect("event receiver already taken")
    }

    // ---- Queue control -----------------------------------------------------

    /// Replace the queue and start playing at `start`.
    pub fn play_tracks(&mut self, tracks: Vec<Track>, start: usize) {
        if tracks.is_empty() {
            return;
        }
        self.queue = tracks;
        self.play_index(start.min(self.queue.len() - 1));
    }

    /// Append a track; begin playback if currently idle.
    pub fn enqueue(&mut self, track: Track) {
        self.queue.push(track);
        if self.current.is_none() {
            self.play_index(self.queue.len() - 1);
        }
    }

    fn play_index(&mut self, index: usize) {
        let Some(track) = self.queue.get(index) else {
            return;
        };
        let id = match SpotifyId::from_uri(&track.uri) {
            Ok(id) => id,
            Err(e) => {
                tracing::warn!("skipping unplayable uri {}: {e}", track.uri);
                return;
            }
        };
        self.current = Some(index);
        self.current_id = Some(id);
        self.inner.load(id, true, 0);
        self.loaded = true;
        self.set_position(0);
        self.status = Status::Loading;
    }

    pub fn play_now(&mut self, index: usize) {
        self.play_index(index);
    }

    pub fn toggle_pause(&mut self) {
        match self.status {
            Status::Playing => {
                self.inner.pause();
                self.position_ms = self.interpolated_position();
                self.status = Status::Paused;
            }
            Status::Paused => {
                // A restored session hasn't loaded the track into librespot
                // yet — load it at the saved position on the first play.
                if !self.loaded {
                    if let Some(id) = self.current_id {
                        self.inner.load(id, true, self.position_ms);
                        self.loaded = true;
                        self.status = Status::Loading;
                        return;
                    }
                }
                self.inner.play();
                self.position_anchor = Instant::now();
                self.status = Status::Playing;
            }
            _ => {}
        }
    }

    pub fn next(&mut self) {
        let Some(cur) = self.current else { return };
        if self.queue.is_empty() {
            return;
        }
        let next = match (self.repeat, self.shuffle) {
            (Repeat::One, _) => cur,
            (_, true) => self.random_index(),
            (Repeat::All, false) => (cur + 1) % self.queue.len(),
            (Repeat::Off, false) => {
                if cur + 1 < self.queue.len() {
                    cur + 1
                } else {
                    self.stop();
                    return;
                }
            }
        };
        self.play_index(next);
    }

    pub fn previous(&mut self) {
        if self.interpolated_position() > 3000 {
            self.seek(0);
            return;
        }
        let Some(cur) = self.current else { return };
        let prev = if cur == 0 {
            if self.repeat == Repeat::All {
                self.queue.len().saturating_sub(1)
            } else {
                0
            }
        } else {
            cur - 1
        };
        self.play_index(prev);
    }

    pub fn stop(&mut self) {
        self.inner.stop();
        self.status = Status::Stopped;
        self.current = None;
        self.current_id = None;
        self.loaded = false;
        self.set_position(0);
    }

    // ---- Seeking / volume --------------------------------------------------

    pub fn seek(&mut self, position_ms: u32) {
        self.inner.seek(position_ms);
        self.set_position(position_ms);
    }

    pub fn seek_relative(&mut self, delta_secs: i64) {
        let cur = self.interpolated_position() as i64;
        let dur = self.current_track().map(|t| t.duration_ms).unwrap_or(0) as i64;
        let target = (cur + delta_secs * 1000).clamp(0, dur.max(0));
        self.seek(target as u32);
    }

    pub fn set_volume(&mut self, volume: u16) {
        self.volume = volume;
        self.mixer.set_volume(volume);
    }

    pub fn volume_step(&mut self, delta: i32) {
        let new = (self.volume as i32 + delta).clamp(0, u16::MAX as i32);
        self.set_volume(new as u16);
    }

    /// Volume as a 0..=100 percentage for display.
    pub fn volume_percent(&self) -> u8 {
        ((self.volume as u32 * 100) / u16::MAX as u32) as u8
    }

    pub fn cycle_repeat(&mut self) {
        self.repeat = self.repeat.next();
    }

    pub fn toggle_shuffle(&mut self) {
        self.shuffle = !self.shuffle;
    }

    pub fn set_repeat(&mut self, repeat: Repeat) {
        self.repeat = repeat;
    }

    pub fn set_shuffle(&mut self, shuffle: bool) {
        self.shuffle = shuffle;
    }

    // ---- Session restore ---------------------------------------------------

    /// Restore a saved queue/selection/position *without* starting playback.
    /// The UI shows the track as paused; the user presses play to resume.
    pub fn restore_session(&mut self, queue: Vec<Track>, current: Option<usize>, position_ms: u32) {
        self.queue = queue;
        match current {
            Some(i) if i < self.queue.len() => {
                self.current = Some(i);
                self.current_id = SpotifyId::from_uri(&self.queue[i].uri).ok();
                self.loaded = false;
                self.status = Status::Paused;
                self.position_ms = position_ms;
                self.position_anchor = Instant::now();
            }
            _ => {
                self.current = None;
                self.current_id = None;
                self.loaded = false;
                self.status = Status::Stopped;
            }
        }
    }

    /// Position to persist (the interpolated position while playing).
    pub fn saved_position_ms(&self) -> u32 {
        self.interpolated_position()
    }

    // ---- Output device -----------------------------------------------------

    /// Switch the audio output device, rebuilding the player and resuming the
    /// current track at its current position. Returns the device name now set.
    pub fn set_output_device(&mut self, device: Option<String>) -> Result<()> {
        self.device = device;
        let resume_at = self.interpolated_position();
        let resume = self.current_id;
        let was_playing = matches!(self.status, Status::Playing | Status::Loading);

        self.rebuild()?;

        if let (Some(id), true) = (resume, was_playing) {
            self.inner.load(id, true, resume_at);
            self.set_position(resume_at);
            self.status = Status::Loading;
        }
        Ok(())
    }

    pub fn current_device(&self) -> Option<&str> {
        self.device.as_deref()
    }

    // ---- Event handling ----------------------------------------------------

    /// Apply a librespot event. Returns `true` if the now-playing track changed
    /// (so the app can refresh album art).
    pub fn on_event(&mut self, event: PlayerEvent) -> bool {
        match event {
            PlayerEvent::Playing { position_ms, .. } => {
                self.status = Status::Playing;
                self.set_position(position_ms);
            }
            PlayerEvent::Paused { position_ms, .. } => {
                self.status = Status::Paused;
                self.position_ms = position_ms;
                self.position_anchor = Instant::now();
            }
            PlayerEvent::Seeked { position_ms, .. } => {
                self.set_position(position_ms);
            }
            PlayerEvent::Loading { .. } => {
                self.status = Status::Loading;
            }
            PlayerEvent::EndOfTrack { track_id, .. } if self.current_id == Some(track_id) => {
                self.next();
                return true;
            }
            PlayerEvent::Unavailable { track_id, .. } => {
                tracing::warn!("track unavailable: {track_id}");
                if self.current_id == Some(track_id) {
                    self.next();
                    return true;
                }
            }
            PlayerEvent::Stopped { .. } if self.status != Status::Stopped => {
                self.status = Status::Paused;
            }
            _ => {}
        }
        false
    }

    // ---- Accessors ---------------------------------------------------------

    pub fn current_track(&self) -> Option<&Track> {
        self.current.and_then(|i| self.queue.get(i))
    }

    /// Current playback position, interpolated since the last anchor while
    /// playing.
    pub fn interpolated_position(&self) -> u32 {
        if self.status == Status::Playing {
            let elapsed = self.position_anchor.elapsed().as_millis() as u32;
            let pos = self.position_ms.saturating_add(elapsed);
            match self.current_track() {
                Some(t) => pos.min(t.duration_ms),
                None => pos,
            }
        } else {
            self.position_ms
        }
    }

    fn set_position(&mut self, ms: u32) {
        self.position_ms = ms;
        self.position_anchor = Instant::now();
    }

    fn random_index(&mut self) -> usize {
        // xorshift64 — good enough for shuffle, no extra dependency.
        let mut x = self.rng_state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.rng_state = x;
        (x % self.queue.len() as u64) as usize
    }
}

/// Build a librespot player for the given backend/device and spawn a task that
/// forwards its events onto `events_tx`. The task ends when the returned player
/// is dropped (e.g. on the next rebuild), closing librespot's own channel.
fn build_inner(
    player_config: &PlayerConfig,
    session: &Session,
    mixer: &Arc<SoftMixer>,
    backend: &str,
    device: Option<String>,
    audio_format: AudioFormat,
    events_tx: &mpsc::UnboundedSender<PlayerEvent>,
) -> Result<Arc<LibrespotPlayer>> {
    let backend_fn = audio_backend::find(Some(backend.to_string()))
        .ok_or_else(|| anyhow!("unknown audio backend `{backend}`"))?;
    let sink_builder = move || backend_fn(device, audio_format);

    let inner = LibrespotPlayer::new(
        player_config.clone(),
        session.clone(),
        mixer.get_soft_volume(),
        sink_builder,
    );

    let mut channel = inner.get_player_event_channel();
    let tx = events_tx.clone();
    tokio::spawn(async move {
        while let Some(event) = channel.recv().await {
            if tx.send(event).is_err() {
                break;
            }
        }
    });

    Ok(inner)
}

fn seed() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x9e3779b97f4a7c15);
    nanos | 1
}
