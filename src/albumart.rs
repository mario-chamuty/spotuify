//! Album art rendering: download a cover image and turn it into colored
//! half-block (`▀`) cells. Each character cell encodes two vertical pixels —
//! the upper half via the glyph's foreground color and the lower half via its
//! background color — giving roughly photographic art in any truecolor terminal.

use anyhow::{Context, Result};
use image::imageops::FilterType;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

/// Pre-rendered album art at a specific cell size, cached per track URI.
#[derive(Debug, Clone)]
pub struct AlbumArt {
    pub track_uri: String,
    pub cols: u16,
    pub rows: u16,
    pub lines: Vec<Line<'static>>,
}

/// Download `url` and render it to `cols`×`rows` character cells.
pub async fn fetch_and_render(url: &str, cols: u16, rows: u16) -> Result<Vec<Line<'static>>> {
    let bytes = reqwest::get(url)
        .await
        .context("downloading album art")?
        .error_for_status()
        .context("album art request returned an error")?
        .bytes()
        .await
        .context("reading album art bytes")?;

    // Decoding can be CPU-heavy for large covers; keep it off the event loop.
    let cols = cols.max(1);
    let rows = rows.max(1);
    tokio::task::spawn_blocking(move || render(&bytes, cols, rows))
        .await
        .context("album art render task panicked")?
}

fn render(bytes: &[u8], cols: u16, rows: u16) -> Result<Vec<Line<'static>>> {
    let img = image::load_from_memory(bytes).context("decoding album art")?;
    // Two pixels stacked per cell row, so sample at double the vertical height.
    let resized = img
        .resize_exact(cols as u32, (rows as u32) * 2, FilterType::Triangle)
        .to_rgb8();

    let mut lines = Vec::with_capacity(rows as usize);
    for row in 0..rows as u32 {
        let mut spans = Vec::with_capacity(cols as usize);
        for col in 0..cols as u32 {
            let top = resized.get_pixel(col, row * 2).0;
            let bottom = resized.get_pixel(col, row * 2 + 1).0;
            spans.push(Span::styled(
                "▀",
                Style::default()
                    .fg(Color::Rgb(top[0], top[1], top[2]))
                    .bg(Color::Rgb(bottom[0], bottom[1], bottom[2])),
            ));
        }
        lines.push(Line::from(spans));
    }
    Ok(lines)
}
