//! SpoTUIfy — a terminal Spotify client for Linux with local playback,
//! search, playlists, queues, output selection and colored album art.

mod albumart;
mod app;
mod audio;
mod auth;
mod config;
mod message;
mod model;
mod player;
mod spotify;
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
use crate::config::Config;
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

    let mut terminal = setup_terminal().context("setting up terminal")?;
    install_panic_hook();

    let mut app = App::new(config, auth.spotify, player);
    let result = app.run(&mut terminal).await;

    restore_terminal(&mut terminal).ok();
    if let Err(e) = result {
        eprintln!("SpoTUIfy exited with an error: {e:?}");
        std::process::exit(1);
    }
    Ok(())
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
