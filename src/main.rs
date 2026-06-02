//! SpoTUIfy — a terminal Spotify client for Linux with local playback,
//! search, playlists, queues, output selection and colored album art.

mod albumart;
mod analyzer;
mod app;
mod audio;
mod auth;
mod browse;
mod config;
mod cookie;
mod eq;
mod keys;
mod lyrics;
// Cross-platform media keys (Windows SMTC / macOS Now Playing). Linux uses the
// MPRIS service below instead, so this module is built everywhere but Linux.
#[cfg(not(target_os = "linux"))]
mod media;
mod message;
mod model;
// MPRIS is a freedesktop/Linux interface; only built there.
#[cfg(target_os = "linux")]
mod mpris;
mod pathfinder;
mod persist;
mod player;
mod snapshot;
mod spotify;
mod theme;
mod ui;
mod update;
mod webtoken;

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
use crate::config::Config;
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
    let mut config = match Config::load() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("\n{e}\n");
            std::process::exit(1);
        }
    };

    // First run: walk the user through the one-time Web API app setup (open the
    // dashboard, register the redirect URI, paste the client id) and save it,
    // instead of erroring out and making them edit the config by hand. Runs on
    // the normal screen, like the OAuth prompts below. Skipping leaves the id
    // empty, and `authenticate` then prints the manual instructions.
    if config.client_id.trim().is_empty() {
        if let Err(e) = auth::run_first_run_setup(&mut config) {
            eprintln!("\nSetup couldn't complete: {e:#}\n");
            std::process::exit(1);
        }
    }

    // Diagnostic: `spotuify --home-probe` mints a web-player token from the
    // configured `sp_dc` cookie and prints the real Home shelves (Daily Mixes,
    // genre/mood, …) without starting playback or the TUI. Used to verify the
    // pathfinder Home end-to-end.
    if std::env::args().any(|a| a == "--home-probe") {
        return probe_home(&config).await;
    }

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
    // Windows/macOS get the same media-key support via souvlaki (SMTC / Now
    // Playing); Linux's MPRIS service above covers it there.
    #[cfg(not(target_os = "linux"))]
    media::spawn(control_tx, snapshot_rx);

    // Detect the terminal's graphics capabilities *before* entering raw mode,
    // since detection queries the terminal on stdout/stdin. The app derives the
    // active picker from this for the configured art mode (and can re-derive it
    // live when the mode changes).
    let base_picker = detect_base_picker(&config);

    // ALSA/JACK and other C audio libraries write diagnostics straight to fd 2,
    // which would scribble all over the alternate-screen TUI. Redirect stderr to
    // a log file for the lifetime of the UI, then restore it (Unix only).
    let stderr_guard = config::cache_dir()
        .ok()
        .map(|dir| stderr_log::redirect(&dir.join("stderr.log")));

    let mut terminal = setup_terminal().context("setting up terminal")?;
    install_panic_hook();

    let mut app = App::new(config, auth.spotify, player);
    app.set_base_picker(base_picker);
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

    // The user triggered an in-app update: now that the TUI is gone, download
    // and swap the binary on the normal screen, then relaunch into it.
    if let Some(info) = app.pending_update.clone() {
        println!(
            "\n  Updating SpoTUIfy v{} → v{} …",
            env!("CARGO_PKG_VERSION"),
            info.latest
        );
        match update::download_and_install(&info).await {
            Ok(v) => {
                println!("  Updated to v{v}. Restarting…\n");
                relaunch();
            }
            Err(e) => {
                eprintln!("\n  Update failed: {e:#}");
                eprintln!("  You can download it manually: {}\n", info.url);
                std::process::exit(1);
            }
        }
    }
    Ok(())
}

/// Re-exec the (freshly updated) binary in place, passing through the original
/// arguments. Diverges: on success the process is replaced (Unix) or a new one
/// is spawned and this one exits (Windows).
fn relaunch() -> ! {
    let exe = std::env::current_exe().unwrap_or_else(|e| {
        eprintln!("  Couldn't locate the updated binary to restart: {e}");
        std::process::exit(0);
    });
    let args: Vec<String> = std::env::args().skip(1).collect();

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let err = std::process::Command::new(&exe).args(&args).exec();
        eprintln!("  Restart failed ({err}); please start SpoTUIfy again.");
        std::process::exit(1);
    }
    #[cfg(not(unix))]
    {
        let _ = std::process::Command::new(&exe).args(&args).spawn();
        std::process::exit(0);
    }
}

/// Mint a web-player token from the `sp_dc` cookie and print the real Home
/// shelves. No auth, playback or TUI — purely a verification path.
async fn probe_home(config: &Config) -> Result<()> {
    let sp_dc = cookie::resolve(&config.sp_dc);
    if sp_dc.trim().is_empty() {
        eprintln!(
            "No `sp_dc` cookie found — couldn't auto-detect one from a browser, \
             and none is set in config. Log into open.spotify.com in Firefox/Chrome, \
             or set `sp_dc` manually. See the README."
        );
        std::process::exit(1);
    }
    let source = if config.sp_dc.trim().is_empty() {
        "auto-detected from browser"
    } else {
        "from config"
    };
    println!("Using sp_dc ({source}). Minting web-player token and fetching Home…\n");
    let token = webtoken::WebToken::new(sp_dc);
    let shelves = match pathfinder::home_shelves(&token).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Home probe failed: {e:#}");
            std::process::exit(1);
        }
    };

    // Render exactly what the TUI's Home tab draws: card shelves. This proves
    // the layout rather than describing it.
    let grid: Vec<app::HomeShelf> = shelves
        .iter()
        .enumerate()
        .map(|(si, s)| app::HomeShelf {
            title: s.title.clone(),
            cards: s
                .items
                .iter()
                .enumerate()
                .map(|(ii, it)| app::HomeCard {
                    title: it.name.clone(),
                    subtitle: it.subtitle.clone(),
                    item: app::HomeItem::Shelf(si, ii),
                })
                .collect(),
        })
        .collect();

    println!("Got {} shelves — rendered as the Home tab draws them:\n", grid.len());
    let (lines, _) = ui::home_grid_lines(&grid, (0, 0), theme::Theme::default(), 100);
    for line in &lines {
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        println!("{text}");
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


/// Detect the terminal's graphics capabilities + cell pixel size.
///
/// The cell pixel size matters for sixel/kitty: the image is sized as
/// cells × cell-pixels, so a wrong cell size renders the cover too small (or
/// overflowing). Many terminals don't answer ratatui-image's CSI font-size
/// query, so we prefer the cell size derived from the terminal's reported
/// window pixel dimensions (TIOCGWINSZ via crossterm), keeping the protocol
/// type from the CSI query when it succeeded.
fn detect_base_picker(config: &Config) -> Option<ratatui_image::picker::Picker> {
    use ratatui_image::picker::Picker;

    let queried = Picker::from_query_stdio().ok();

    // Explicit user override wins.
    if let Some((w, h)) = config.cell_pixel_size {
        if w > 0 && h > 0 {
            tracing::info!("cell size from config override: {:?}", (w, h));
            let mut picker = Picker::from_fontsize((w, h));
            if let Some(q) = queried {
                picker.set_protocol_type(q.protocol_type());
            }
            return Some(picker);
        }
    }

    if let Ok(ws) = crossterm::terminal::window_size() {
        if ws.width > 0 && ws.height > 0 && ws.columns > 0 && ws.rows > 0 {
            let cell = (ws.width / ws.columns, ws.height / ws.rows);
            if cell.0 > 0 && cell.1 > 0 {
                tracing::info!("cell size from window pixels: {cell:?}");
                let mut picker = Picker::from_fontsize(cell);
                if let Some(q) = queried {
                    picker.set_protocol_type(q.protocol_type());
                }
                return Some(picker);
            }
        }
    }

    if let Some(q) = &queried {
        tracing::info!("cell size from CSI query: {:?}", q.font_size());
    } else {
        tracing::info!("no terminal graphics detection; using half-block fallback");
    }
    queried
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
