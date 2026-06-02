# SpoTUIfy – feature plan

## Cross-platform media keys (Windows + macOS)

Linux already has media keys via the hand-rolled MPRIS D-Bus service (`src/mpris.rs`).
Windows and macOS currently drop the control/snapshot channels on the floor
(`main.rs`: `let _ = (control_tx, snapshot_rx)`). Add support via the `souvlaki`
crate, reusing the existing `Action` sender + `Snapshot` watch channel.

Key research findings:
- souvlaki delivers Windows events via WinRT `TypedEventHandler` (not WM_ messages),
  but needs a valid `HWND` from `GetForWindow`. It runs no event loop and
  `MediaControls` is not `Send`, so we own it on a dedicated OS thread.
- macOS `MPRemoteCommandCenter` needs a running CoreFoundation/AppKit run loop;
  souvlaki creates none. Unbundled-CLI key routing is uncertain – user will test.
- Match souvlaki's `windows = "0.44"` to avoid a duplicate windows-crate build.

Tasks:
- [x] Add deps to Cargo.toml: souvlaki (off-Linux, default-features=false), windows 0.44 (Windows), core-foundation (macOS) <!-- NOTE: macOS uses core-foundation-sys (dedupes via souvlaki) -->
- [x] `src/media.rs`: dedicated thread, souvlaki MediaControls, event→Action forward, snapshot→OS sync
- [x] Windows: hidden top-level window + COM init + message pump with WM_TIMER poll <!-- NOTE: needed windows feature Win32_Graphics_Gdi for WNDCLASSW/RegisterClassW; custom wndproc trampoline (DefWindowProcW is generic in 0.44) -->
- [x] macOS: CFRunLoopRunInMode tick loop + snapshot poll (secondary thread; document main-thread fallback)
- [x] Wire `media::spawn` into `main.rs` for `cfg(not(target_os = "linux"))`
- [x] Compile-check on Windows host; rely on CI for macOS compile <!-- NOTE: Windows cargo check + clippy clean; macOS validated via CI PR -->
- [ ] User test: Windows media keys + SMTC panel; macOS media keys + Now Playing widget

## First-run setup wizard (in-app)

Today an empty `client_id` makes the app exit with an error telling the user to
edit config.toml by hand and relaunch. Replace with an in-app guided setup.

Tasks:
- [x] First-run detection (empty client_id) routes into a setup view instead of exit <!-- NOTE: implemented as a pre-TUI console flow in auth::run_first_run_setup, consistent with the existing pre-TUI OAuth prompts -->
- [x] Setup view: open dashboard in browser, show exact redirect URI, paste/type client_id
- [x] Write client_id to config.toml, then continue straight into OAuth (no restart)
- [x] Targeted handling of the OAuth redirect-URI-mismatch error
- [ ] Compile-check; user test the first-run flow

## In-app self-update (opt-in) + disable-checks setting

The startup check is notify-only. Add an in-app, opt-in self-update triggered
from the status-bar badge, plus a setting to stop checking.

- [x] Config: `check_for_updates: bool` (default true); gate the startup check on it
- [x] Settings view: "Updates" section with a "Check for updates" toggle
- [x] Keybinds: `Action::UpdateNow` (alt+u), `Action::DismissUpdate` (alt+d); shown in the badge
- [x] Badge shows the bound keys (only offer update where a prebuilt asset exists)
- [x] `update::download_and_install`: async reqwest download + extract (tar.gz unix / zip windows) + `self_replace` swap
- [x] Trigger flow: Alt+U sets pending update + quits; main installs after the TUI exits, then relaunches
- [x] Deps: self-replace (all), flate2+tar (unix), zip (windows)
- [x] Compile-check on Windows host + CI for macOS/Linux; user test the update flow <!-- NOTE: Windows cargo clippy + 12 tests pass; macOS/Linux via CI PR -->
- [ ] User test: trigger update from the badge (Alt+U), confirm download/swap/relaunch; toggle the setting
