//! SpoTUIfy — a terminal Spotify client for Linux with local playback,
//! search, playlists, queues, output selection and colored album art.

mod albumart;
mod app;
mod audio;
mod auth;
mod config;
mod keys;
mod lyrics;
mod message;
mod model;
// MPRIS is a freedesktop/Linux interface; only built there.
#[cfg(target_os = "linux")]
mod mpris;
mod persist;
mod player;
mod snapshot;
mod spotify;
mod theme;
mod ui;

use std::io::{self, Stdout};

use anyhow::{Context, Result};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use tracing_subscriber::EnvFilter;

use crate::app::App;
use crate::config::{ArtMode, Config};
use crate::player::Player;

#[tokio::main]
async fn main() -> Result<()> {
    // Both `ring` and `aws-lc-rs` end up in the dependency graph, so rustls 0.23
    // can't auto-select a crypto provider and panics on first TLS use. Install
    // one explicitly before any TLS (OAuth, playback session, web API) happens.
    rustls::crypto::ring::default_provider()
        .install_default()
        .ok();

    let _guard = init_logging();

    // Config errors (missing client id, first-run template) are friendly and
    // must print before we ever touch the terminal.
    let config = match Config::load() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("\n{e}\n");
            std::process::exit(1);
        }
    };

    // Authentication and the playback session connect before the TUI starts so
    // the OAuth "Browse to: …" prompt is visible on the normal screen.
    let auth = auth::authenticate(&config)
        .await
        .context("authentication failed")?;
    println!("  Connecting playback session…");
    let player = Player::connect(&config, auth.librespot_credentials, auth.cache)
        .await
        .context("could not start playback session")?;

    // MPRIS media controls: a control channel feeds Actions back into the app,
    // and a watch channel publishes a playback snapshot for property reads.
    let (control_tx, control_rx) = tokio::sync::mpsc::unbounded_channel();
    let (snapshot_tx, snapshot_rx) = tokio::sync::watch::channel(snapshot::Snapshot::default());
    #[cfg(target_os = "linux")]
    mpris::spawn(control_tx, snapshot_rx);
    #[cfg(not(target_os = "linux"))]
    let _ = (control_tx, snapshot_rx); // MPRIS (media keys) is Linux-only

    // Detect terminal pixel-graphics support *before* entering raw mode, since
    // detection queries the terminal on stdout/stdin.
    let picker = setup_picker(config.art_mode);

    // ALSA/JACK and other C audio libraries write diagnostics straight to fd 2,
    // which would scribble all over the alternate-screen TUI. Redirect stderr to
    // a log file for the lifetime of the UI, then restore it (Unix only).
    let stderr_guard = config::cache_dir()
        .ok()
        .map(|dir| stderr_log::redirect(&dir.join("stderr.log")));

    let mut terminal = setup_terminal().context("setting up terminal")?;
    install_panic_hook();

    let mut app = App::new(config, auth.spotify, player);
    app.set_picker(picker);
    app.attach_external_controls(control_rx, snapshot_tx);
    let result = app.run(&mut terminal).await;

    restore_terminal(&mut terminal).ok();
    if let Some(guard) = stderr_guard {
        guard.restore();
    }
    if let Err(e) = result {
        eprintln!("SpoTUIfy exited with an error: {e:?}");
        std::process::exit(1);
    }
    Ok(())
}

/// Keeps the C audio libraries' stderr chatter off the alternate-screen TUI by
/// pointing fd 2 at a log file for the UI's lifetime. Unix-only; a no-op
/// elsewhere (Windows audio backends don't spew to stderr like ALSA does).
#[cfg(unix)]
mod stderr_log {
    use std::os::fd::AsRawFd;
    use std::path::Path;

    pub struct Guard(Option<std::os::fd::RawFd>);

    pub fn redirect(path: &Path) -> Guard {
        let Ok(file) = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(path)
        else {
            return Guard(None);
        };
        // SAFETY: plain dup/dup2/close on valid fds; `file`'s fd stays alive
        // until after dup2, after which fd 2 references the same open file.
        unsafe {
            let saved = libc::dup(libc::STDERR_FILENO);
            if saved < 0 {
                return Guard(None);
            }
            if libc::dup2(file.as_raw_fd(), libc::STDERR_FILENO) < 0 {
                libc::close(saved);
                return Guard(None);
            }
            Guard(Some(saved))
        }
    }

    impl Guard {
        /// Restore the original stderr.
        pub fn restore(self) {
            if let Some(fd) = self.0 {
                // SAFETY: `fd` is a valid dup of the original stderr.
                unsafe {
                    libc::dup2(fd, libc::STDERR_FILENO);
                    libc::close(fd);
                }
            }
        }
    }
}

#[cfg(not(unix))]
mod stderr_log {
    use std::path::Path;

    pub struct Guard;

    pub fn redirect(_path: &Path) -> Guard {
        Guard
    }

    impl Guard {
        pub fn restore(self) {}
    }
}

/// Build a pixel-graphics `Picker` per the configured `art_mode`. Returns
/// `None` for `halfblocks` (or when detection picks half-blocks / fails), in
/// which case the app uses the coloured half-block renderer.
fn setup_picker(mode: ArtMode) -> Option<ratatui_image::picker::Picker> {
    use ratatui_image::picker::{Picker, ProtocolType};

    match mode {
        ArtMode::Halfblocks => None,
        ArtMode::Auto => match Picker::from_query_stdio() {
            Ok(picker) if picker.protocol_type() != ProtocolType::Halfblocks => Some(picker),
            Ok(_) => None, // detected half-blocks: use our richer renderer
            Err(e) => {
                tracing::info!("terminal graphics detection failed ({e}); using half-blocks");
                None
            }
        },
        ArtMode::Sixel | ArtMode::Kitty => {
            // Forced protocol: query for the font size, then override the type.
            let mut picker = match Picker::from_query_stdio() {
                Ok(p) => p,
                Err(_) => Picker::from_fontsize((8, 16)),
            };
            picker.set_protocol_type(match mode {
                ArtMode::Sixel => ProtocolType::Sixel,
                _ => ProtocolType::Kitty,
            });
            Some(picker)
        }
    }
}

fn init_logging() -> Option<tracing_appender::non_blocking::WorkerGuard> {
    let path = config::log_path().ok()?;
    let file = std::fs::File::create(&path).ok()?;
    let (writer, guard) = tracing_appender::non_blocking(file);
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("spotuify=info,librespot=warn,rspotify=warn"));
    tracing_subscriber::fmt()
        .with_writer(writer)
        .with_ansi(false)
        .with_env_filter(filter)
        .init();
    Some(guard)
}

fn setup_terminal() -> Result<Terminal<CrosstermBackend<Stdout>>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let terminal = Terminal::new(CrosstermBackend::new(stdout))?;
    Ok(terminal)
}

fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> Result<()> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}

/// Make sure a panic doesn't leave the user's terminal in raw/alternate mode.
fn install_panic_hook() {
    let original = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
        original(info);
    }));
}
