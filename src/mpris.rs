//! MPRIS (`org.mpris.MediaPlayer2`) D-Bus media controls.
//!
//! Serving these interfaces lets `playerctl` and desktop media keys control
//! SpoTUIfy. The service runs as its own tokio task; it reads a [`Snapshot`] of
//! current playback over a `watch` channel (to answer property reads and emit
//! `PropertiesChanged`) and forwards transport requests to the app as
//! [`Action`]s over an mpsc channel. If the session bus is unavailable we just
//! log and skip — the app keeps working without media-key support.

use std::collections::HashMap;

use tokio::sync::{mpsc, watch};
use zbus::interface;
use zbus::zvariant::{ObjectPath, OwnedValue, Value};

use crate::keys::Action;
use crate::snapshot::Snapshot;

/// The root `org.mpris.MediaPlayer2` interface (app-level controls).
struct MediaPlayer2;

#[interface(name = "org.mpris.MediaPlayer2")]
impl MediaPlayer2 {
    fn raise(&self) {}

    fn quit(&self) {}

    #[zbus(property)]
    fn can_quit(&self) -> bool {
        false
    }

    #[zbus(property)]
    fn can_raise(&self) -> bool {
        false
    }

    #[zbus(property)]
    fn has_track_list(&self) -> bool {
        false
    }

    #[zbus(property)]
    fn identity(&self) -> &str {
        "SpoTUIfy"
    }

    #[zbus(property)]
    fn supported_uri_schemes(&self) -> Vec<String> {
        vec!["spotify".to_string()]
    }

    #[zbus(property)]
    fn supported_mime_types(&self) -> Vec<String> {
        Vec::new()
    }
}

/// The `org.mpris.MediaPlayer2.Player` interface (transport + state).
struct Player {
    controls: mpsc::UnboundedSender<Action>,
    state: watch::Receiver<Snapshot>,
}

impl Player {
    fn send(&self, action: Action) {
        let _ = self.controls.send(action);
    }

    fn snap(&self) -> Snapshot {
        self.state.borrow().clone()
    }
}

#[interface(name = "org.mpris.MediaPlayer2.Player")]
impl Player {
    fn play(&self) {
        // PlayPause toggles; if already playing this is a no-op for the user.
        if !self.snap().playing {
            self.send(Action::PlayPause);
        }
    }

    fn pause(&self) {
        if self.snap().playing {
            self.send(Action::PlayPause);
        }
    }

    fn play_pause(&self) {
        self.send(Action::PlayPause);
    }

    fn stop(&self) {
        // No dedicated stop action; pause if currently playing.
        if self.snap().playing {
            self.send(Action::PlayPause);
        }
    }

    fn next(&self) {
        self.send(Action::Next);
    }

    fn previous(&self) {
        self.send(Action::Prev);
    }

    fn seek(&self, offset_us: i64) {
        if offset_us >= 0 {
            self.send(Action::SeekForward);
        } else {
            self.send(Action::SeekBack);
        }
    }

    fn set_position(&self, _track_id: ObjectPath<'_>, _position_us: i64) {
        // Absolute seek isn't expressible as a single Action; ignore for now.
    }

    fn open_uri(&self, _uri: String) {}

    #[zbus(property)]
    fn playback_status(&self) -> String {
        let s = self.snap();
        if s.stopped || !s.has_track {
            "Stopped".to_string()
        } else if s.playing {
            "Playing".to_string()
        } else {
            "Paused".to_string()
        }
    }

    #[zbus(property)]
    fn metadata(&self) -> HashMap<String, OwnedValue> {
        let s = self.snap();
        let mut m: HashMap<String, OwnedValue> = HashMap::new();
        // A valid object path is required for the track id.
        let trackid = ObjectPath::try_from("/org/mpris/MediaPlayer2/spotuify/track").unwrap();
        if let Ok(v) = OwnedValue::try_from(Value::ObjectPath(trackid)) {
            m.insert("mpris:trackid".to_string(), v);
        }
        if s.length_us > 0 {
            if let Ok(v) = OwnedValue::try_from(Value::I64(s.length_us)) {
                m.insert("mpris:length".to_string(), v);
            }
        }
        if let Some(url) = &s.art_url {
            if let Ok(v) = OwnedValue::try_from(Value::new(url.clone())) {
                m.insert("mpris:artUrl".to_string(), v);
            }
        }
        if !s.title.is_empty() {
            if let Ok(v) = OwnedValue::try_from(Value::new(s.title.clone())) {
                m.insert("xesam:title".to_string(), v);
            }
        }
        if !s.album.is_empty() {
            if let Ok(v) = OwnedValue::try_from(Value::new(s.album.clone())) {
                m.insert("xesam:album".to_string(), v);
            }
        }
        if !s.artist.is_empty() {
            if let Ok(v) = OwnedValue::try_from(Value::new(vec![s.artist.clone()])) {
                m.insert("xesam:artist".to_string(), v);
            }
        }
        if !s.track_uri.is_empty() {
            if let Ok(v) = OwnedValue::try_from(Value::new(s.track_uri.clone())) {
                m.insert("xesam:url".to_string(), v);
            }
        }
        m
    }

    #[zbus(property)]
    fn volume(&self) -> f64 {
        self.snap().volume
    }

    #[zbus(property)]
    fn set_volume(&self, volume: f64) {
        // Coarse: nudge toward the requested level.
        let cur = self.snap().volume;
        if volume > cur + 0.02 {
            self.send(Action::VolumeUp);
        } else if volume + 0.02 < cur {
            self.send(Action::VolumeDown);
        }
    }

    #[zbus(property)]
    fn position(&self) -> i64 {
        self.snap().position_us
    }

    #[zbus(property)]
    fn rate(&self) -> f64 {
        1.0
    }

    #[zbus(property)]
    fn minimum_rate(&self) -> f64 {
        1.0
    }

    #[zbus(property)]
    fn maximum_rate(&self) -> f64 {
        1.0
    }

    #[zbus(property)]
    fn can_go_next(&self) -> bool {
        self.snap().can_next
    }

    #[zbus(property)]
    fn can_go_previous(&self) -> bool {
        self.snap().can_prev
    }

    #[zbus(property)]
    fn can_play(&self) -> bool {
        self.snap().has_track
    }

    #[zbus(property)]
    fn can_pause(&self) -> bool {
        self.snap().has_track
    }

    #[zbus(property)]
    fn can_seek(&self) -> bool {
        self.snap().has_track
    }

    #[zbus(property)]
    fn can_control(&self) -> bool {
        true
    }
}

const PATH: &str = "/org/mpris/MediaPlayer2";
const NAME: &str = "org.mpris.MediaPlayer2.spotuify";

/// Spawn the MPRIS service. Returns immediately; the service runs in the
/// background and degrades gracefully if the session bus is missing.
pub fn spawn(controls: mpsc::UnboundedSender<Action>, state: watch::Receiver<Snapshot>) {
    tokio::spawn(async move {
        if let Err(e) = serve(controls, state).await {
            tracing::warn!("MPRIS unavailable ({e}); media keys/playerctl disabled");
        }
    });
}

async fn serve(
    controls: mpsc::UnboundedSender<Action>,
    mut state: watch::Receiver<Snapshot>,
) -> zbus::Result<()> {
    let player = Player {
        controls,
        state: state.clone(),
    };
    let conn = zbus::connection::Builder::session()?
        .name(NAME)?
        .serve_at(PATH, MediaPlayer2)?
        .serve_at(PATH, player)?
        .build()
        .await?;

    // Emit PropertiesChanged whenever the snapshot changes so clients refresh.
    let iface_ref = conn
        .object_server()
        .interface::<_, Player>(PATH)
        .await?;
    loop {
        if state.changed().await.is_err() {
            break; // app shut down
        }
        let emitter = iface_ref.signal_emitter();
        let iface = iface_ref.get().await;
        // Re-publish the volatile properties clients care about. The values
        // are recomputed from the shared snapshot, so we just notify.
        let _ = iface.playback_status_changed(emitter).await;
        let _ = iface.metadata_changed(emitter).await;
        let _ = iface.volume_changed(emitter).await;
        let _ = iface.can_go_next_changed(emitter).await;
        let _ = iface.can_go_previous_changed(emitter).await;
    }
    Ok(())
}
