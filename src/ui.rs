//! All rendering. `draw` is called once per frame with the full app state. It
//! also records the album-art panel size back into the app so art can be
//! re-rendered at the right resolution when the layout changes.

use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Gauge, List, ListItem, Paragraph, Tabs, Wrap};
use ratatui::Frame;

use crate::app::{App, Focus, View};
use crate::model::fmt_ms;
use crate::player::Status;
use crate::spotify::{SearchKind, SearchResults};

const GREEN: Color = Color::Rgb(30, 215, 96);
const DIM: Color = Color::Rgb(140, 140, 140);

pub fn draw(f: &mut Frame, app: &mut App) {
    let root = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // tabs
            Constraint::Min(3),    // body
            Constraint::Length(3), // playback bar
            Constraint::Length(1), // status line
        ])
        .split(f.area());

    render_tabs(f, app, root[0]);

    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(62), Constraint::Percentage(38)])
        .split(root[1]);

    render_main(f, app, body[0]);
    render_now_playing(f, app, body[1]);
    render_playback_bar(f, app, root[2]);
    render_status(f, app, root[3]);
}

fn render_tabs(f: &mut Frame, app: &App, area: Rect) {
    let titles = ["1 Search", "2 Library", "3 Tracks", "4 Queue", "5 Output"];
    let selected = match app.view {
        View::Search => 0,
        View::Library => 1,
        View::Tracklist => 2,
        View::Queue => 3,
        View::Devices => 4,
    };
    let tabs = Tabs::new(titles.iter().map(|t| Span::raw(*t)).collect::<Vec<_>>())
        .select(selected)
        .style(Style::default().fg(DIM))
        .highlight_style(Style::default().fg(GREEN).add_modifier(Modifier::BOLD))
        .divider(" · ");
    f.render_widget(tabs, area);
}

fn render_main(f: &mut Frame, app: &mut App, area: Rect) {
    match app.view {
        View::Search => render_search(f, app, area),
        View::Library => render_library(f, app, area),
        View::Tracklist => render_tracklist(f, app, area),
        View::Queue => render_queue(f, app, area),
        View::Devices => render_devices(f, app, area),
    }
}

fn render_search(f: &mut Frame, app: &mut App, area: Rect) {
    let parts = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(1)])
        .split(area);

    let editing = app.focus == Focus::Input;
    let cursor = if editing { "█" } else { "" };
    let input = Paragraph::new(Line::from(vec![
        Span::styled(format!("[{}] ", app.search_kind.label()), Style::default().fg(GREEN)),
        Span::raw(format!("{}{}", app.search_input, cursor)),
    ]))
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(if editing { GREEN } else { DIM }))
            .title(" Search  (/ to edit · Tab switches type · Enter searches) "),
    );
    f.render_widget(input, parts[0]);

    let items: Vec<ListItem> = match &app.search_results {
        Some(SearchResults::Tracks(tracks)) => {
            tracks.iter().map(|t| track_item(&t.name, &t.artists, t.duration_ms)).collect()
        }
        Some(SearchResults::Albums(albums)) => albums
            .iter()
            .map(|a| ListItem::new(two_line(&a.name, &a.artists)))
            .collect(),
        Some(SearchResults::Artists(artists)) => {
            artists.iter().map(|a| ListItem::new(a.name.clone())).collect()
        }
        Some(SearchResults::Playlists(playlists)) => playlists
            .iter()
            .map(|p| ListItem::new(two_line(&p.name, &format!("by {} · {} tracks", p.owner, p.total))))
            .collect(),
        None => vec![ListItem::new("Type a query and press Enter.")],
    };

    let kind_hint = match app.search_kind {
        SearchKind::Tracks => "Enter plays · e enqueues",
        _ => "Enter opens",
    };
    let list = List::new(items)
        .block(panel(format!(" Results · {kind_hint} ")))
        .highlight_style(highlight())
        .highlight_symbol("▶ ");
    f.render_stateful_widget(list, parts[1], &mut app.search_state);
}

fn render_library(f: &mut Frame, app: &mut App, area: Rect) {
    let mut items = vec![ListItem::new(Line::from(vec![
        Span::styled("★ ", Style::default().fg(GREEN)),
        Span::raw("Liked Songs"),
    ]))];
    items.extend(
        app.playlists
            .iter()
            .map(|p| ListItem::new(two_line(&p.name, &format!("{} tracks", p.total)))),
    );
    let list = List::new(items)
        .block(panel(" Library · Enter opens "))
        .highlight_style(highlight())
        .highlight_symbol("▶ ");
    f.render_stateful_widget(list, area, &mut app.library_state);
}

fn render_tracklist(f: &mut Frame, app: &mut App, area: Rect) {
    let items: Vec<ListItem> = app
        .context_tracks
        .iter()
        .map(|t| track_item(&t.name, &t.artists, t.duration_ms))
        .collect();
    let title = if app.context_title.is_empty() {
        " Tracks · open something from Search or Library ".to_string()
    } else {
        format!(" {} · Enter plays · e enqueues ", app.context_title)
    };
    let list = List::new(items)
        .block(panel(title))
        .highlight_style(highlight())
        .highlight_symbol("▶ ");
    f.render_stateful_widget(list, area, &mut app.tracklist_state);
}

fn render_queue(f: &mut Frame, app: &mut App, area: Rect) {
    let current = app.player.current;
    let items: Vec<ListItem> = app
        .player
        .queue
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let marker = if Some(i) == current { "♪ " } else { "  " };
            let style = if Some(i) == current {
                Style::default().fg(GREEN)
            } else {
                Style::default()
            };
            ListItem::new(Line::from(vec![
                Span::styled(marker, style),
                Span::styled(t.name.clone(), style),
                Span::styled(format!("  —  {}", t.artists), Style::default().fg(DIM)),
            ]))
        })
        .collect();
    let list = List::new(items)
        .block(panel(" Queue · Enter jumps to track "))
        .highlight_style(highlight())
        .highlight_symbol("▶ ");
    f.render_stateful_widget(list, area, &mut app.queue_state);
}

fn render_devices(f: &mut Frame, app: &mut App, area: Rect) {
    let active = app.player.current_device();
    let items: Vec<ListItem> = app
        .devices
        .iter()
        .map(|d| {
            let is_active = match active {
                Some(name) => d.name == name,
                None => d.is_default,
            };
            let dot = if is_active { "● " } else { "○ " };
            let suffix = if d.is_default { "  (system default)" } else { "" };
            ListItem::new(Line::from(vec![
                Span::styled(dot, Style::default().fg(if is_active { GREEN } else { DIM })),
                Span::raw(d.name.clone()),
                Span::styled(suffix, Style::default().fg(DIM)),
            ]))
        })
        .collect();
    let list = List::new(items)
        .block(panel(" Audio Output · Enter selects "))
        .highlight_style(highlight())
        .highlight_symbol("▶ ");
    f.render_stateful_widget(list, area, &mut app.device_state);
}

fn render_now_playing(f: &mut Frame, app: &mut App, area: Rect) {
    let block = panel(" Now Playing ");
    let inner = block.inner(area);
    f.render_widget(block, area);

    let split = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(4)])
        .split(inner);
    let art_area = split[0];

    // Largest centered square that fits: cells are ~twice as tall as wide and a
    // half-block packs two pixels per cell, so cols ≈ 2·rows keeps art square.
    let rows = art_area.height.min(art_area.width / 2).max(1);
    let cols = (rows * 2).min(art_area.width);
    app.art_size = (cols, rows);

    match &app.art {
        Some(art) if art.cols == cols && art.rows == rows => {
            let centered = center(art_area, cols, rows);
            f.render_widget(Paragraph::new(Text::from(art.lines.clone())), centered);
        }
        _ => {
            let placeholder = Paragraph::new(if app.player.current_track().is_some() {
                "\n  ♪  loading cover…"
            } else {
                "\n  nothing playing"
            })
            .style(Style::default().fg(DIM));
            f.render_widget(placeholder, art_area);
        }
    }

    let info = match app.player.current_track() {
        Some(t) => Text::from(vec![
            Line::from(Span::styled(
                t.name.clone(),
                Style::default().add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(t.artists.clone(), Style::default().fg(GREEN))),
            Line::from(Span::styled(t.album.clone(), Style::default().fg(DIM))),
        ]),
        None => Text::from(Line::from(Span::styled("—", Style::default().fg(DIM)))),
    };
    f.render_widget(Paragraph::new(info).alignment(Alignment::Center).wrap(Wrap { trim: true }), split[1]);
}

fn render_playback_bar(f: &mut Frame, app: &App, area: Rect) {
    let position = app.player.interpolated_position();
    let duration = app.player.current_track().map(|t| t.duration_ms).unwrap_or(0);
    let ratio = if duration > 0 {
        (position as f64 / duration as f64).clamp(0.0, 1.0)
    } else {
        0.0
    };

    let state = match app.player.status {
        Status::Playing => "▶ Playing",
        Status::Paused => "⏸ Paused",
        Status::Loading => "… Loading",
        Status::Stopped => "■ Stopped",
    };
    let title = format!(
        " {state}   vol {:>3}%   shuffle {}   repeat {} ",
        app.player.volume_percent(),
        if app.player.shuffle { "on" } else { "off" },
        app.player.repeat.label(),
    );

    let label = format!("{} / {}", fmt_ms(position), fmt_ms(duration));
    let gauge = Gauge::default()
        .block(Block::default().borders(Borders::ALL).title(title).border_style(Style::default().fg(DIM)))
        .gauge_style(Style::default().fg(GREEN))
        .ratio(ratio)
        .label(label);
    f.render_widget(gauge, area);
}

fn render_status(f: &mut Frame, app: &App, area: Rect) {
    let p = Paragraph::new(Line::from(vec![
        Span::styled(" ? help ", Style::default().fg(Color::Black).bg(GREEN)),
        Span::raw("  "),
        Span::raw(app.status.clone()),
    ]));
    f.render_widget(p, area);
}

// ---- small helpers --------------------------------------------------------

fn panel(title: impl Into<String>) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(DIM))
        .title(title.into())
}

fn highlight() -> Style {
    Style::default()
        .fg(Color::Black)
        .bg(GREEN)
        .add_modifier(Modifier::BOLD)
}

fn track_item(name: &str, artists: &str, duration_ms: u32) -> ListItem<'static> {
    ListItem::new(Line::from(vec![
        Span::raw(name.to_string()),
        Span::styled(format!("  —  {artists}"), Style::default().fg(DIM)),
        Span::styled(format!("  ({})", fmt_ms(duration_ms)), Style::default().fg(DIM).italic()),
    ]))
}

fn two_line(primary: &str, secondary: &str) -> Text<'static> {
    Text::from(vec![
        Line::from(primary.to_string()),
        Line::from(Span::styled(format!("  {secondary}"), Style::default().fg(DIM))),
    ])
}

/// Center a `cols`×`rows` region inside `area`.
fn center(area: Rect, cols: u16, rows: u16) -> Rect {
    let x = area.x + (area.width.saturating_sub(cols)) / 2;
    let y = area.y + (area.height.saturating_sub(rows)) / 2;
    Rect { x, y, width: cols.min(area.width), height: rows.min(area.height) }
}
