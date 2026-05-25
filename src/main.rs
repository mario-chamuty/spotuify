//! SpoTUIfy — a terminal Spotify client for Linux with local playback,
//! search, playlists, queues, output selection and colored album art.

mod albumart;
mod app;
mod audio;
mod auth;
mod config;
mod keys;
mod message;
mod model;
mod mpris;
mod persist;
mod player;
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
    let (snapshot_tx, snapshot_rx) = tokio::sync::watch::channel(mpris::Snapshot::default());
    mpris::spawn(control_tx, snapshot_rx);

    // Detect terminal pixel-graphics support *before* entering raw mode, since
    // detection queries the terminal on stdout/stdin.
    let picker = setup_picker(config.art_mode);

    let mut terminal = setup_terminal().context("setting up terminal")?;
    install_panic_hook();

    let mut app = App::new(config, auth.spotify, player);
    app.set_picker(picker);
    app.attach_external_controls(control_rx, snapshot_tx);
    let result = app.run(&mut terminal).await;

    restore_terminal(&mut terminal).ok();
    if let Err(e) = result {
        eprintln!("SpoTUIfy exited with an error: {e:?}");
        std::process::exit(1);
    }
    Ok(())
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
