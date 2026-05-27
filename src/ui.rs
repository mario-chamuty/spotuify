//! All rendering. `draw` is called once per frame with the full app state. It
//! also records the album-art panel size back into the app so art can be
//! re-rendered at the right resolution when the layout changes.

use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, Gauge, List, ListItem, Paragraph, Tabs, Wrap};
use ratatui::Frame;

use crate::app::{App, Focus, View};
use crate::model::fmt_ms;
use crate::player::Status;
use crate::spotify::{SearchKind, SearchResults};
use crate::theme::Theme;

pub fn draw(f: &mut Frame, app: &mut App) {
    let theme = app.theme;
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

    // Modal overlays on top of everything.
    if app.picker.is_some() {
        render_picker(f, app, f.area());
    }
    if app.prompt.is_some() {
        render_prompt(f, app, f.area());
    }
    if app.eq_open {
        render_equalizer(f, app, f.area());
    }
    if app.help_open {
        render_help(f, app, f.area());
    }
    render_six_seven(f, root[2], app);
    let _ = theme;
}

/// The bouncing six-seven hands: a 2-line overlay just above the volume, with
/// the two 🫴 hands swapping between the upper and lower line (real up/down,
/// since a single line can't raise an emoji).
fn render_six_seven(f: &mut Frame, bar: Rect, app: &App) {
    let at = match app.easter_egg {
        Some((crate::app::Egg::SixSeven, at))
            if at.elapsed() < std::time::Duration::from_secs(2) =>
        {
            at
        }
        _ => return,
    };
    let w = 6u16;
    if bar.y < 2 || bar.width < w + 2 {
        return;
    }
    // Float the box just above the bar, roughly over the "vol" readout.
    let rect = Rect {
        x: bar.x + 13,
        y: bar.y - 2,
        width: w,
        height: 2,
    };
    f.render_widget(Clear, rect);
    // Swap which hand is up ~6×/sec.
    let (top, bottom) = if (at.elapsed().as_millis() / 150) % 2 == 0 {
        ("🫴", "    🫴")
    } else {
        ("    🫴", "🫴")
    };
    let style = Style::default().fg(app.theme.accent);
    let lines = vec![
        Line::from(Span::styled(top, style)),
        Line::from(Span::styled(bottom, style)),
    ];
    f.render_widget(Paragraph::new(Text::from(lines)), rect);
}

/// Modal listing every keybinding (two columns so they all fit). Dismissed by
/// any key.
fn render_help(f: &mut Frame, app: &App, area: Rect) {
    let theme = app.theme;
    let rows = app.keymap.help_rows();
    let half = rows.len().div_ceil(2);
    let (left, right) = rows.split_at(half);

    let height = (half as u16 + 2).min(area.height.saturating_sub(2)).max(3);
    let rect = centered_rect(area, 86, height);
    f.render_widget(Clear, rect);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.accent))
        .title(" Keybindings — press any key to close ");
    let inner = block.inner(rect);
    f.render_widget(block, rect);

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(inner);

    for (slice, col) in [(left, cols[0]), (right, cols[1])] {
        let key_w = slice.iter().map(|(k, _)| k.chars().count()).max().unwrap_or(6);
        let lines: Vec<Line> = slice
            .iter()
            .map(|(keys, desc)| {
                Line::from(vec![
                    Span::styled(
                        format!("{keys:>key_w$}  "),
                        Style::default().fg(theme.accent).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled((*desc).to_string(), Style::default().fg(theme.dim)),
                ])
            })
            .collect();
        f.render_widget(Paragraph::new(Text::from(lines)), col);
    }
}

fn render_tabs(f: &mut Frame, app: &App, area: Rect) {
    let theme = app.theme;
    let titles = [
        "1 Search", "2 Library", "3 Tracks", "4 Queue", "5 Output", "6 Settings", "7 Home",
    ];
    let selected = match app.view {
        View::Search => 0,
        View::Library => 1,
        View::Tracklist => 2,
        View::Queue => 3,
        View::Devices => 4,
        View::Settings => 5,
        View::Home => 6,
    };
    let tabs = Tabs::new(titles.iter().map(|t| Span::raw(*t)).collect::<Vec<_>>())
        .select(selected)
        .style(Style::default().fg(theme.dim))
        .highlight_style(Style::default().fg(theme.accent).add_modifier(Modifier::BOLD))
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
        View::Settings => render_settings(f, app, area),
        View::Home => render_home(f, app, area),
    }
}

fn render_search(f: &mut Frame, app: &mut App, area: Rect) {
    let theme = app.theme;
    let parts = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(1)])
        .split(area);

    let editing = app.focus == Focus::Input;
    let cursor = if editing { "█" } else { "" };
    let input = Paragraph::new(Line::from(vec![
        Span::styled(format!("[{}] ", app.search_kind.label()), Style::default().fg(theme.accent)),
        Span::raw(format!("{}{}", app.search_input, cursor)),
    ]))
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(if editing { theme.accent } else { theme.dim }))
            .title(" Search  (/ or i to edit · Tab type · ↑/↓ history · Enter searches) "),
    );
    f.render_widget(input, parts[0]);

    let items: Vec<ListItem> = match &app.search_results {
        Some(SearchResults::Tracks(tracks)) => tracks
            .iter()
            .map(|t| track_item(theme, &t.name, &t.artists, t.duration_ms, app.liked.contains(&t.uri)))
            .collect(),
        Some(SearchResults::Albums(albums)) => albums
            .iter()
            .map(|a| ListItem::new(two_line(theme, &a.name, &a.artists)))
            .collect(),
        Some(SearchResults::Artists(artists)) => {
            artists.iter().map(|a| ListItem::new(a.name.clone())).collect()
        }
        Some(SearchResults::Playlists(playlists)) => playlists
            .iter()
            .map(|p| ListItem::new(two_line(theme, &p.name, &format!("by {} · {} tracks", p.owner, p.total))))
            .collect(),
        Some(SearchResults::Episodes(eps)) => eps
            .iter()
            .map(|e| ListItem::new(two_line(theme, &e.name, &format!("{} · {}", e.show, fmt_ms(e.duration_ms)))))
            .collect(),
        Some(SearchResults::Shows(shows)) => shows
            .iter()
            .map(|s| ListItem::new(two_line(theme, &s.name, &s.publisher)))
            .collect(),
        None => vec![ListItem::new("Type a query and press Enter.")],
    };

    let kind_hint = match app.search_kind {
        SearchKind::Tracks | SearchKind::Episodes => "Enter plays · e enqueues",
        _ => "Enter opens",
    };
    let list = List::new(items)
        .block(panel(theme, format!(" Results · {kind_hint} ")))
        .highlight_style(highlight(theme))
        .highlight_symbol("▶ ");
    f.render_stateful_widget(list, parts[1], &mut app.search_state);
}

fn render_library(f: &mut Frame, app: &mut App, area: Rect) {
    let theme = app.theme;
    let visible = app.library_visible_indices();
    let mut items: Vec<ListItem> = Vec::with_capacity(visible.len());
    for idx in &visible {
        if *idx == 0 {
            items.push(ListItem::new(Line::from(vec![
                Span::styled("★ ", Style::default().fg(theme.accent)),
                Span::raw("Liked Songs"),
            ])));
        } else if let Some(p) = app.playlists.get(idx - 1) {
            items.push(ListItem::new(two_line(theme, &p.name, &format!("{} tracks", p.total))));
        }
    }
    let title = library_title(app, "Library · Enter opens · c new · R rename · D remove");
    let list = List::new(items)
        .block(panel(theme, title))
        .highlight_style(highlight(theme))
        .highlight_symbol("▶ ");
    f.render_stateful_widget(list, area, &mut app.library_state);
}

fn render_tracklist(f: &mut Frame, app: &mut App, area: Rect) {
    let theme = app.theme;
    let visible = app.tracklist_visible_indices();
    let items: Vec<ListItem> = visible
        .iter()
        .filter_map(|i| app.context_tracks.get(*i))
        .map(|t| track_item(theme, &t.name, &t.artists, t.duration_ms, app.liked.contains(&t.uri)))
        .collect();
    let base = if app.context_title.is_empty() {
        "Tracks · open something from Search or Library".to_string()
    } else {
        format!("{} · Enter plays · e enqueues · L like · a +playlist", app.context_title)
    };
    let title = library_title(app, &base);
    let list = List::new(items)
        .block(panel(theme, title))
        .highlight_style(highlight(theme))
        .highlight_symbol("▶ ");
    f.render_stateful_widget(list, area, &mut app.tracklist_state);
}

fn render_queue(f: &mut Frame, app: &mut App, area: Rect) {
    let theme = app.theme;
    let current = app.player.current;
    let visible = app.queue_visible_indices();
    let items: Vec<ListItem> = visible
        .iter()
        .filter_map(|i| app.player.queue.get(*i).map(|t| (*i, t)))
        .map(|(i, t)| {
            let marker = if Some(i) == current { "♪ " } else { "  " };
            let style = if Some(i) == current {
                Style::default().fg(theme.accent)
            } else {
                Style::default()
            };
            ListItem::new(Line::from(vec![
                Span::styled(marker, style),
                Span::styled(t.name.clone(), style),
                Span::styled(format!("  —  {}", t.artists), Style::default().fg(theme.dim)),
            ]))
        })
        .collect();
    let title = library_title(app, "Queue · Enter jumps to track");
    let list = List::new(items)
        .block(panel(theme, title))
        .highlight_style(highlight(theme))
        .highlight_symbol("▶ ");
    f.render_stateful_widget(list, area, &mut app.queue_state);
}

fn render_devices(f: &mut Frame, app: &mut App, area: Rect) {
    let theme = app.theme;
    let mut items: Vec<ListItem> = Vec::new();
    let active_remote = app.active_remote_device_id();
    let local_active = active_remote.is_none();
    let local_dev = app.player.current_device();

    items.push(section_header(theme, "Local audio output (librespot)"));
    for d in &app.devices {
        let is_active = local_active
            && match local_dev {
                Some(name) => d.name == name,
                None => d.is_default,
            };
        let dot = if is_active { "● " } else { "○ " };
        let suffix = if d.is_default { "  (system default)" } else { "" };
        items.push(ListItem::new(Line::from(vec![
            Span::styled(dot, Style::default().fg(if is_active { theme.accent } else { theme.dim })),
            Span::raw(d.name.clone()),
            Span::styled(suffix, Style::default().fg(theme.dim)),
        ])));
    }

    if !app.connect_devices.is_empty() {
        items.push(section_header(theme, "Spotify Connect devices"));
        for d in &app.connect_devices {
            // Active if we transferred to it, or the server reports it active.
            let is_active = match &active_remote {
                Some(id) => Some(id.as_str()) == d.id.as_deref(),
                None => d.is_active,
            };
            let dot = if is_active { "● " } else { "○ " };
            let vol = d
                .volume_percent
                .map(|v| format!(" · {v}%"))
                .unwrap_or_default();
            items.push(ListItem::new(Line::from(vec![
                Span::styled(dot, Style::default().fg(if is_active { theme.accent } else { theme.dim })),
                Span::raw(d.name.clone()),
                Span::styled(format!("  ({}{})", d.kind, vol), Style::default().fg(theme.dim)),
            ])));
        }
    }

    let list = List::new(items)
        .block(panel(theme, " Output · Enter selects (local or Connect) "))
        .highlight_style(highlight(theme))
        .highlight_symbol("▶ ");
    f.render_stateful_widget(list, area, &mut app.device_state);
}

fn render_now_playing(f: &mut Frame, app: &mut App, area: Rect) {
    let theme = app.theme;
    let title = if app.show_visualizer {
        " Spectrum ".to_string()
    } else if app.show_lyrics {
        match app.lyrics_or_status() {
            Ok(l) if !l.provider.is_empty() => format!(" Lyrics · {} ", l.provider),
            _ => " Lyrics ".to_string(),
        }
    } else {
        " Now Playing ".to_string()
    };
    let block = panel(theme, title);
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

    if app.show_visualizer {
        render_visualizer(f, app, art_area);
    } else if app.show_lyrics {
        render_lyrics(f, app, art_area);
    } else {
        let art_drawn = crate::albumart::render_into(app, f, art_area, cols, rows);
        if !art_drawn {
            let placeholder = Paragraph::new(if app.player.current_track().is_some() {
                "\n  ♪  loading cover…"
            } else {
                "\n  nothing playing"
            })
            .style(Style::default().fg(theme.dim));
            f.render_widget(placeholder, art_area);
        }
    }

    let info = match app.displayed_track() {
        Some(t) => {
            let heart = if app.liked.contains(&t.uri) { "♥ " } else { "" };
            let badge = if t.is_episode() { "🎙 " } else { "" };
            Text::from(vec![
                Line::from(vec![
                    Span::styled(heart, Style::default().fg(theme.like)),
                    Span::raw(badge),
                    Span::styled(t.name.clone(), Style::default().add_modifier(Modifier::BOLD)),
                ]),
                Line::from(Span::styled(t.artists.clone(), Style::default().fg(theme.accent))),
                Line::from(Span::styled(t.album.clone(), Style::default().fg(theme.dim))),
            ])
        }
        None => Text::from(Line::from(Span::styled("—", Style::default().fg(theme.dim)))),
    };
    f.render_widget(Paragraph::new(info).alignment(Alignment::Center).wrap(Wrap { trim: true }), split[1]);
}

/// Render the lyrics panel. For synced lyrics the active line is highlighted and
/// kept roughly centred; unsynced lyrics scroll from the top.
fn render_lyrics(f: &mut Frame, app: &App, area: Rect) {
    let theme = app.theme;
    let lyrics = match app.lyrics_or_status() {
        Ok(l) => l,
        Err(msg) => {
            let p = Paragraph::new(format!("\n  {msg}"))
                .style(Style::default().fg(theme.dim))
                .alignment(Alignment::Center);
            f.render_widget(p, area);
            return;
        }
    };

    let height = area.height as usize;
    let active = lyrics.active_line(app.playback_position());
    // Keep the active line roughly centred; unsynced lyrics start at the top.
    let start = active.map_or(0, |a| a.saturating_sub(height / 2));
    let end = (start + height).min(lyrics.lines.len());

    let rows: Vec<Line> = lyrics.lines[start..end]
        .iter()
        .enumerate()
        .map(|(i, line)| {
            let style = if Some(start + i) == active {
                Style::default().fg(theme.accent).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.dim)
            };
            let text = if line.text.is_empty() { "♪" } else { line.text.as_str() };
            Line::from(Span::styled(text.to_string(), style))
        })
        .collect();

    f.render_widget(
        Paragraph::new(Text::from(rows))
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: true }),
        area,
    );
}

fn render_playback_bar(f: &mut Frame, app: &App, area: Rect) {
    let theme = app.theme;
    let position = app.playback_position();
    let duration = app.displayed_track().map(|t| t.duration_ms).unwrap_or(0);
    let ratio = if duration > 0 {
        (position as f64 / duration as f64).clamp(0.0, 1.0)
    } else {
        0.0
    };

    let state = match app.playback_status() {
        Status::Playing => "▶ Playing",
        Status::Paused => "⏸ Paused",
        Status::Loading => "… Loading",
        Status::Stopped => "■ Stopped",
    };
    let mode = if app.remote_active() { "  REMOTE" } else { "" };
    // Easter egg: a note that flashes next to the volume for ~2s.
    let egg = match app.easter_egg {
        Some((egg, at)) if at.elapsed() < std::time::Duration::from_secs(2) => match egg {
            crate::app::Egg::Nice => " *nice*".to_string(),
            // The bouncing 🫴🫴 hands are drawn as a 2-line overlay (see
            // `render_six_seven`), since a single line can't move them up/down.
            crate::app::Egg::SixSeven => " six seveeeen".to_string(),
        },
        _ => String::new(),
    };
    let title = format!(
        " {state}{mode}   vol {:>3}%{egg}   shuffle {}   repeat {} ",
        app.displayed_volume_percent(),
        if app.displayed_shuffle() { "on" } else { "off" },
        app.player.repeat.label(),
    );

    let label = format!("{} / {}", fmt_ms(position), fmt_ms(duration));
    let gauge = Gauge::default()
        .block(Block::default().borders(Borders::ALL).title(title).border_style(Style::default().fg(theme.dim)))
        .gauge_style(Style::default().fg(theme.accent))
        .ratio(ratio)
        .label(label);
    f.render_widget(gauge, area);
}

fn render_status(f: &mut Frame, app: &App, area: Rect) {
    let theme = app.theme;
    let p = Paragraph::new(Line::from(vec![
        Span::styled(" ? help ", Style::default().fg(Color::Black).bg(theme.accent)),
        Span::raw("  "),
        Span::raw(app.status.clone()),
    ]));
    f.render_widget(p, area);
}

fn render_prompt(f: &mut Frame, app: &App, area: Rect) {
    let theme = app.theme;
    let Some(prompt) = &app.prompt else { return };
    let rect = centered_rect(area, 60, 3);
    f.render_widget(Clear, rect);
    let para = Paragraph::new(Line::from(vec![
        Span::raw(prompt.input.clone()),
        Span::styled("█", Style::default().fg(theme.accent)),
    ]))
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme.accent))
            .title(format!(" {} (Enter confirm · Esc cancel) ", prompt.title)),
    );
    f.render_widget(para, rect);
}

fn render_picker(f: &mut Frame, app: &mut App, area: Rect) {
    let theme = app.theme;
    let Some(picker) = &mut app.picker else { return };
    let h = (picker.items.len() as u16 + 2).min(area.height.saturating_sub(4)).max(3);
    let rect = centered_rect(area, 60, h);
    f.render_widget(Clear, rect);
    let items: Vec<ListItem> = picker
        .items
        .iter()
        .map(|(_, label)| ListItem::new(label.clone()))
        .collect();
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(theme.accent))
                .title(format!(" {} (Enter add · Esc cancel) ", picker.title)),
        )
        .highlight_style(highlight(theme))
        .highlight_symbol("▶ ");
    f.render_stateful_widget(list, rect, &mut picker.state);
}

/// A vertical bar spectrum (one bar per EQ band, low→high left→right).
fn render_visualizer(f: &mut Frame, app: &App, area: Rect) {
    use crate::eq::{BANDS, LABELS};
    const BLOCKS: [char; 9] = [' ', '▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    let theme = app.theme;
    if area.width < BANDS as u16 || area.height < 2 {
        return;
    }
    let bw = (area.width as usize / BANDS).max(1);
    let label_row = bw >= 3;
    let h = if label_row { area.height as usize - 1 } else { area.height as usize };

    let mut lines: Vec<Line> = Vec::with_capacity(area.height as usize);
    for r in 0..h {
        let cell_from_bottom = (h - 1 - r) as f32;
        let mut spans = Vec::with_capacity(BANDS);
        for b in 0..BANDS {
            let eighths = app.viz_levels[b].clamp(0.0, 1.0) * h as f32 * 8.0 - cell_from_bottom * 8.0;
            let ch = BLOCKS[eighths.round().clamp(0.0, 8.0) as usize];
            spans.push(Span::styled(
                ch.to_string().repeat(bw),
                Style::default().fg(theme.accent),
            ));
        }
        lines.push(Line::from(spans));
    }
    if label_row {
        let labels: Vec<Span> = (0..BANDS)
            .map(|b| Span::styled(format!("{:^bw$}", LABELS[b]), Style::default().fg(theme.dim)))
            .collect();
        lines.push(Line::from(labels));
    }
    f.render_widget(Paragraph::new(Text::from(lines)), area);
}

fn render_equalizer(f: &mut Frame, app: &App, area: Rect) {
    use crate::eq::{BANDS, LABELS, MAX_DB};
    let theme = app.theme;
    let eq = app.player.eq();
    let enabled = eq.enabled();

    let rect = centered_rect(area, 64, BANDS as u16 + 2);
    f.render_widget(Clear, rect);

    let state = if enabled { "ON" } else { "OFF" };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.accent))
        .title(format!(
            " Equalizer [{state}] · {} · ↑↓±dB · p preset · a suggest · 0/R reset · space · Esc ",
            app.preset_name()
        ));
    let inner = block.inner(rect);
    f.render_widget(block, rect);

    let lines: Vec<Line> = (0..BANDS)
        .map(|b| {
            let gain = eq.gain(b);
            let selected = b == app.eq_sel;
            // 2·MAX_DB+1 cells with a centre marker; one cell per dB.
            let bar: String = (-MAX_DB..=MAX_DB)
                .map(|cell| {
                    if cell == 0 {
                        '│'
                    } else if (cell > 0 && cell <= gain) || (cell < 0 && cell >= gain) {
                        '█'
                    } else {
                        '·'
                    }
                })
                .collect();
            let label_style = if selected {
                Style::default().fg(theme.accent).add_modifier(Modifier::BOLD)
            } else if enabled {
                Style::default()
            } else {
                Style::default().fg(theme.dim)
            };
            let bar_style = if !enabled {
                Style::default().fg(theme.dim)
            } else if selected {
                Style::default().fg(theme.accent)
            } else {
                Style::default().fg(theme.like)
            };
            // Live energy meter for this band (from the spectrum analyzer).
            let filled = (app.viz_levels[b] * 6.0).round() as usize;
            let meter: String = (0..6).map(|i| if i < filled { '▰' } else { '▱' }).collect();
            Line::from(vec![
                Span::styled(format!("{:>3} {:>+3} ", LABELS[b], gain), label_style),
                Span::styled(bar, bar_style),
                Span::styled(format!("  {meter}"), Style::default().fg(theme.dim)),
            ])
        })
        .collect();

    f.render_widget(Paragraph::new(Text::from(lines)), inner);
}

fn render_home(f: &mut Frame, app: &App, area: Rect) {
    let theme = app.theme;
    let block = panel(theme, " Home · ↑↓ select · Enter play/open ");
    let inner = block.inner(area);
    f.render_widget(block, area);

    let Some(home) = &app.home else {
        let txt = if app.home_loading {
            "\n  Loading your Home…"
        } else {
            "\n  Home will load when you open it."
        };
        f.render_widget(Paragraph::new(txt).style(Style::default().fg(theme.dim)), inner);
        return;
    };

    let accent = Style::default().fg(theme.accent).add_modifier(Modifier::BOLD);
    let mut lines: Vec<Line> = Vec::new();
    let mut i = 0usize; // running selectable index
    let mut sel_line = 0usize;
    let sel = app.home_sel;

    let section = |lines: &mut Vec<Line>, title: &str| {
        if !lines.is_empty() {
            lines.push(Line::from(""));
        }
        lines.push(Line::from(Span::styled(format!(" {title}"), accent)));
    };

    if !home.recently.is_empty() {
        section(&mut lines, "Recently played");
        for t in &home.recently {
            if i == sel {
                sel_line = lines.len();
            }
            lines.push(home_track_line(theme, i == sel, &t.name, &t.artists));
            i += 1;
        }
    }
    if !home.top_tracks.is_empty() {
        section(&mut lines, "Your top tracks");
        for t in &home.top_tracks {
            if i == sel {
                sel_line = lines.len();
            }
            lines.push(home_track_line(theme, i == sel, &t.name, &t.artists));
            i += 1;
        }
    }
    if !home.top_artists.is_empty() {
        section(&mut lines, "Your top artists");
        for a in &home.top_artists {
            if i == sel {
                sel_line = lines.len();
            }
            lines.push(home_simple_line(theme, i == sel, &a.name));
            i += 1;
        }
    }
    if !home.mixes.is_empty() {
        section(&mut lines, "Made for you");
        for m in &home.mixes {
            if i == sel {
                sel_line = lines.len();
            }
            lines.push(home_simple_line(theme, i == sel, &m.label));
            i += 1;
        }
    }
    if i == 0 {
        lines.push(Line::from(Span::styled(
            "  Nothing to show — try playing some music first.",
            Style::default().fg(theme.dim),
        )));
    }

    // Scroll so the selected row stays roughly centred.
    let h = inner.height as usize;
    let max_scroll = lines.len().saturating_sub(h);
    let scroll = sel_line.saturating_sub(h / 2).min(max_scroll) as u16;
    f.render_widget(Paragraph::new(Text::from(lines)).scroll((scroll, 0)), inner);
}

fn home_track_line(theme: Theme, selected: bool, name: &str, artists: &str) -> Line<'static> {
    let (marker, style) = if selected {
        ("▶ ", Style::default().fg(theme.accent).add_modifier(Modifier::BOLD))
    } else {
        ("  ", Style::default())
    };
    Line::from(vec![
        Span::styled(format!("  {marker}{name}"), style),
        Span::styled(format!("  —  {artists}"), Style::default().fg(theme.dim)),
    ])
}

fn home_simple_line(theme: Theme, selected: bool, label: &str) -> Line<'static> {
    let (marker, style) = if selected {
        ("▶ ", Style::default().fg(theme.accent).add_modifier(Modifier::BOLD))
    } else {
        ("  ", Style::default())
    };
    Line::from(Span::styled(format!("  {marker}{label}"), style))
}

fn render_settings(f: &mut Frame, app: &App, area: Rect) {
    use crate::app::SettingRow;
    use crate::eq::{LABELS, MAX_DB};
    let theme = app.theme;
    let eq = app.player.eq();
    let rows = SettingRow::all();

    let header = |label: &str| {
        Line::from(Span::styled(
            format!(" {label}"),
            Style::default().fg(theme.accent).add_modifier(Modifier::BOLD),
        ))
    };

    let mut lines: Vec<Line> = vec![header("Playback")];
    for (i, row) in rows.iter().enumerate() {
        match row {
            SettingRow::EqEnabled => {
                lines.push(Line::from(""));
                lines.push(header("Equalizer"));
            }
            SettingRow::ArtMode => {
                lines.push(Line::from(""));
                lines.push(header("Appearance"));
            }
            SettingRow::ReAuth => {
                lines.push(Line::from(""));
                lines.push(header("Account"));
                lines.push(Line::from(Span::styled(
                    format!("   logged in as {}", app.player.username()),
                    Style::default().fg(theme.dim),
                )));
            }
            _ => {}
        }

        let (label, value) = match *row {
            SettingRow::Normalisation => {
                ("Normalisation".to_string(), on_off(app.config.normalisation).to_string())
            }
            SettingRow::EqEnabled => ("Enabled".to_string(), on_off(eq.enabled()).to_string()),
            SettingRow::EqPreset => ("Preset".to_string(), format!("‹ {} ›", app.preset_name())),
            SettingRow::Volume => ("Volume".to_string(), format!("{}%", app.player.volume_percent())),
            SettingRow::EqBand(b) => {
                let g = eq.gain(b);
                let bar: String = (-MAX_DB..=MAX_DB)
                    .map(|c| {
                        if c == 0 {
                            '│'
                        } else if (c > 0 && c <= g) || (c < 0 && c >= g) {
                            '█'
                        } else {
                            '·'
                        }
                    })
                    .collect();
                (format!("{:>3} Hz", LABELS[b]), format!("{g:+3} dB  {bar}"))
            }
            SettingRow::ArtMode => {
                ("Album art".to_string(), format!("{:?}", app.config.art_mode).to_lowercase())
            }
            SettingRow::ReAuth => ("Re-authenticate".to_string(), "press Enter".to_string()),
        };

        let selected = i == app.settings_sel;
        let marker = if selected { "▶ " } else { "  " };
        let label_style = if selected {
            Style::default().fg(theme.accent).add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        let value_style = if selected {
            Style::default().fg(theme.accent)
        } else {
            Style::default().fg(theme.dim)
        };
        lines.push(Line::from(vec![
            Span::styled(format!("  {marker}{label:<14}"), label_style),
            Span::styled(value, value_style),
        ]));
    }

    let block = panel(theme, " Settings · ↑↓ select · ←→ change · Enter toggle/reset ");
    f.render_widget(Paragraph::new(Text::from(lines)).block(block), area);
}

// ---- small helpers --------------------------------------------------------

/// Append a `· Filter: …` suffix to a panel title when filtering is active.
fn library_title(app: &App, base: &str) -> String {
    if app.filter_active() {
        format!(" {base} · Filter: {}_ ", app.filter_query)
    } else {
        format!(" {base} ")
    }
}

fn section_header(theme: Theme, label: &str) -> ListItem<'static> {
    ListItem::new(Line::from(Span::styled(
        label.to_string(),
        Style::default().fg(theme.dim).add_modifier(Modifier::BOLD),
    )))
}

fn on_off(b: bool) -> &'static str {
    if b {
        "on"
    } else {
        "off"
    }
}

fn panel(theme: Theme, title: impl Into<String>) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.dim))
        .title(title.into())
}

fn highlight(theme: Theme) -> Style {
    Style::default()
        .fg(theme.highlight_fg)
        .bg(theme.highlight_bg)
        .add_modifier(Modifier::BOLD)
}

fn track_item(theme: Theme, name: &str, artists: &str, duration_ms: u32, liked: bool) -> ListItem<'static> {
    let heart = if liked { "♥ " } else { "" };
    ListItem::new(Line::from(vec![
        Span::styled(heart.to_string(), Style::default().fg(theme.like)),
        Span::raw(name.to_string()),
        Span::styled(format!("  —  {artists}"), Style::default().fg(theme.dim)),
        Span::styled(format!("  ({})", fmt_ms(duration_ms)), Style::default().fg(theme.dim).italic()),
    ]))
}

fn two_line(theme: Theme, primary: &str, secondary: &str) -> Text<'static> {
    Text::from(vec![
        Line::from(primary.to_string()),
        Line::from(Span::styled(format!("  {secondary}"), Style::default().fg(theme.dim))),
    ])
}

/// A centered rect of the given width-percent and fixed height.
fn centered_rect(area: Rect, pct_w: u16, height: u16) -> Rect {
    let w = area.width * pct_w / 100;
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    Rect { x, y, width: w.min(area.width), height: height.min(area.height) }
}
