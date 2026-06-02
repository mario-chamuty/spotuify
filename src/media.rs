//! Cross-platform media-key + "Now Playing" integration for Windows and macOS,
//! built on the `souvlaki` crate. Linux uses the hand-rolled MPRIS service
//! (`mpris.rs`) instead, so this module is compiled only off Linux.
//!
//! souvlaki wires up the OS controls (Windows SMTC / macOS MPRemoteCommandCenter)
//! but runs no event loop of its own, and its `MediaControls` handle is not
//! `Send`. So we own it on a dedicated OS thread that drives the platform's
//! event loop (a Win32 message pump / a CoreFoundation run loop) and polls a
//! playback [`Snapshot`] from a watch channel to keep the OS metadata in sync.
//!
//! Both channels come straight from `main`, the same pair the MPRIS service
//! uses: an [`Action`] sender (transport requests from the OS back into the app)
//! and a `Snapshot` receiver (current playback, for the OS "Now Playing" UI).

use std::time::Duration;

use souvlaki::{
    MediaControlEvent, MediaControls, MediaMetadata, MediaPlayback, MediaPosition, PlatformConfig,
    SeekDirection,
};
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::watch;

use crate::keys::Action;
use crate::snapshot::Snapshot;

/// How often we re-sync the OS "Now Playing" metadata from the snapshot.
const POLL: Duration = Duration::from_millis(200);

/// Spawn the media-control integration on its own OS thread. Degrades
/// gracefully: if the OS controls can't be created we log and the app keeps
/// working without media-key support.
pub fn spawn(controls: UnboundedSender<Action>, state: watch::Receiver<Snapshot>) {
    let _ = std::thread::Builder::new()
        .name("media-controls".into())
        .spawn(move || run(controls, state));
}

/// Translate a souvlaki event into an app [`Action`] and forward it. Play/Pause/
/// Stop consult the latest snapshot because the app's only transport toggle is
/// `PlayPause`, so a bare "Play" while already playing must be a no-op.
fn forward(
    event: MediaControlEvent,
    controls: &UnboundedSender<Action>,
    state: &watch::Receiver<Snapshot>,
) {
    let playing = state.borrow().playing;
    let action = match event {
        MediaControlEvent::Play => (!playing).then_some(Action::PlayPause),
        MediaControlEvent::Pause | MediaControlEvent::Stop => playing.then_some(Action::PlayPause),
        MediaControlEvent::Toggle => Some(Action::PlayPause),
        MediaControlEvent::Next => Some(Action::Next),
        MediaControlEvent::Previous => Some(Action::Prev),
        MediaControlEvent::Seek(SeekDirection::Forward)
        | MediaControlEvent::SeekBy(SeekDirection::Forward, _) => Some(Action::SeekForward),
        MediaControlEvent::Seek(SeekDirection::Backward)
        | MediaControlEvent::SeekBy(SeekDirection::Backward, _) => Some(Action::SeekBack),
        // Absolute seek / volume / open-uri / raise / quit have no single-Action
        // equivalent; ignore them (matches the MPRIS service).
        _ => None,
    };
    if let Some(a) = action {
        let _ = controls.send(a);
    }
}

/// Push the current snapshot into the OS controls (metadata + playback state).
fn sync(mc: &mut MediaControls, snap: &Snapshot) {
    let _ = mc.set_metadata(MediaMetadata {
        title: non_empty(&snap.title),
        artist: non_empty(&snap.artist),
        album: non_empty(&snap.album),
        cover_url: snap.art_url.as_deref(),
        duration: (snap.length_us > 0).then(|| Duration::from_micros(snap.length_us as u64)),
    });
    let progress = Some(MediaPosition(Duration::from_micros(snap.position_us.max(0) as u64)));
    let playback = if snap.stopped || !snap.has_track {
        MediaPlayback::Stopped
    } else if snap.playing {
        MediaPlayback::Playing { progress }
    } else {
        MediaPlayback::Paused { progress }
    };
    let _ = mc.set_playback(playback);
}

fn non_empty(s: &str) -> Option<&str> {
    (!s.is_empty()).then_some(s)
}

/// Build the souvlaki callback that forwards events into the app. Cloned senders
/// move into the `'static` closure souvlaki requires.
fn make_callback(
    controls: &UnboundedSender<Action>,
    state: &watch::Receiver<Snapshot>,
) -> impl Fn(MediaControlEvent) + Send + 'static {
    let controls = controls.clone();
    let state = state.clone();
    move |event| forward(event, &controls, &state)
}

// ---------------------------------------------------------------------------
// macOS
// ---------------------------------------------------------------------------

#[cfg(target_os = "macos")]
fn run(controls: UnboundedSender<Action>, mut state: watch::Receiver<Snapshot>) {
    let config = PlatformConfig {
        dbus_name: "spotuify",
        display_name: "SpoTUIfy",
        hwnd: None,
    };
    let mut mc = match MediaControls::new(config) {
        Ok(mc) => mc,
        Err(e) => {
            tracing::warn!("media controls unavailable: {e:?}; media keys disabled");
            return;
        }
    };
    if mc.attach(make_callback(&controls, &state)).is_err() {
        tracing::warn!("attaching media controls failed; media keys disabled");
        return;
    }
    sync(&mut mc, &state.borrow().clone());

    loop {
        if state.has_changed().unwrap_or(false) {
            let snap = state.borrow_and_update().clone();
            sync(&mut mc, &snap);
        }
        // Drive the run loop so MPRemoteCommandCenter handlers fire. NOTE: Apple
        // usually delivers these on the *main* run loop; if media keys don't
        // reach us from this secondary thread, the fallback is to run the loop
        // on the main thread (a larger refactor). See the module docs.
        run_loop_tick(POLL);
    }
}

#[cfg(target_os = "macos")]
fn run_loop_tick(dur: Duration) {
    use core_foundation_sys::runloop::{
        kCFRunLoopDefaultMode, kCFRunLoopRunFinished, CFRunLoopRunInMode,
    };
    // SAFETY: plain FFI call into CoreFoundation with the static default-mode
    // constant; runs this thread's run loop for `dur`, then returns.
    let result = unsafe { CFRunLoopRunInMode(kCFRunLoopDefaultMode, dur.as_secs_f64(), 0) };
    // With no input sources registered yet, CFRunLoopRunInMode returns instantly
    // with `Finished` instead of blocking for `dur`; sleep so we don't busy-spin.
    if result == kCFRunLoopRunFinished {
        std::thread::sleep(dur);
    }
}

// ---------------------------------------------------------------------------
// Windows
// ---------------------------------------------------------------------------

#[cfg(target_os = "windows")]
fn run(controls: UnboundedSender<Action>, mut state: watch::Receiver<Snapshot>) {
    use std::ffi::c_void;

    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
    use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, RegisterClassW, SetTimer,
        TranslateMessage, CW_USEDEFAULT, HMENU, MSG, WM_TIMER, WNDCLASSW, WS_EX_TOOLWINDOW,
        WS_OVERLAPPED,
    };

    // The window does nothing itself; everything is driven by souvlaki (events)
    // and the timer poll below. It just needs a valid window proc.
    unsafe extern "system" fn wndproc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        DefWindowProcW(hwnd, msg, wparam, lparam)
    }

    // SAFETY: a self-contained block of Win32/COM FFI. The window we create is a
    // hidden top-level window owned by this thread for the process's lifetime;
    // all handles stay valid for as long as they're used.
    unsafe {
        // WinRT (used by souvlaki's SMTC backend) needs an initialized apartment
        // on this thread; STA pairs with the message pump below. A prior init by
        // another component is harmless (returns a non-fatal HRESULT we ignore).
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);

        let hinstance = HINSTANCE(GetModuleHandleW(None).map(|h| h.0).unwrap_or_default());
        let class_name: Vec<u16> = "SpoTUIfyMediaControls\0".encode_utf16().collect();
        let class_ptr = PCWSTR(class_name.as_ptr());

        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: hinstance,
            lpszClassName: class_ptr,
            ..Default::default()
        };
        RegisterClassW(&wc);

        // A hidden top-level window (never shown): SMTC's GetForWindow wants a
        // real window handle, and a tool window stays off the taskbar/Alt-Tab.
        let hwnd = CreateWindowExW(
            WS_EX_TOOLWINDOW,
            class_ptr,
            class_ptr,
            WS_OVERLAPPED,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            0,
            0,
            HWND(0),
            HMENU(0),
            hinstance,
            None,
        );
        if hwnd.0 == 0 {
            tracing::warn!("could not create media-control window; media keys disabled");
            return;
        }

        let config = PlatformConfig {
            dbus_name: "spotuify",
            display_name: "SpoTUIfy",
            hwnd: Some(hwnd.0 as *mut c_void),
        };
        let mut mc = match MediaControls::new(config) {
            Ok(mc) => mc,
            Err(e) => {
                tracing::warn!("media controls unavailable: {e:?}; media keys disabled");
                return;
            }
        };
        if mc.attach(make_callback(&controls, &state)).is_err() {
            tracing::warn!("attaching media controls failed; media keys disabled");
            return;
        }
        sync(&mut mc, &state.borrow().clone());

        // A timer drives the snapshot poll from inside the message loop, so the
        // SMTC metadata stays current without a second thread.
        SetTimer(hwnd, 1, POLL.as_millis() as u32, None);

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, HWND(0), 0, 0).as_bool() {
            if msg.message == WM_TIMER && state.has_changed().unwrap_or(false) {
                let snap = state.borrow_and_update().clone();
                sync(&mut mc, &snap);
            }
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}
