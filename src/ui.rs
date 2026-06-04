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
        .title(" Keybindings – press any key to close ");
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
        "1 Search", "2 Library", "3 Tracks", "4 Queue", "5 Home", "6 Settings",
    ];
    let selected = match app.view {
        View::Search => 0,
        View::Library => 1,
        View::Tracklist => 2,
        View::Queue => 3,
        View::Home => 4,
        View::Settings => 5,
        // Artist is a transient detail view, not a tab – highlight none.
        View::Artist => titles.len(),
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
        View::Artist => render_artist(f, app, area),
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

    let playing_uri = app.player.current_track().map(|t| t.uri.clone());
    let items: Vec<ListItem> = match &app.search_results {
        Some(SearchResults::Tracks(tracks)) => tracks
            .iter()
            .map(|t| {
                let playing = playing_uri.as_deref() == Some(t.uri.as_str());
                track_item(theme, &t.name, &t.artists, t.duration_ms, app.liked.contains(&t.uri), playing)
            })
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
    let playing_uri = app.player.current_track().map(|t| t.uri.clone());
    let visible = app.tracklist_visible_indices();
    let items: Vec<ListItem> = visible
        .iter()
        .filter_map(|i| app.context_tracks.get(*i))
        .map(|t| {
            let playing = playing_uri.as_deref() == Some(t.uri.as_str());
            track_item(theme, &t.name, &t.artists, t.duration_ms, app.liked.contains(&t.uri), playing)
        })
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
                Span::styled(format!("  –  {}", t.artists), Style::default().fg(theme.dim)),
            ]))
        })
        .collect();
    let title = library_title(app, "Queue · Enter jumps · x removes");
    let list = List::new(items)
        .block(panel(theme, title))
        .highlight_style(highlight(theme))
        .highlight_symbol("▶ ");
    f.render_stateful_widget(list, area, &mut app.queue_state);
}

/// Artist detail: a "Popular" section of top tracks, then "Albums" and
/// "Singles & EPs" sections of openable releases. Row order matches
/// `App::artist_rows` so the selection highlight lands correctly.
fn render_artist(f: &mut Frame, app: &mut App, area: Rect) {
    let theme = app.theme;
    let playing_uri = app.player.current_track().map(|t| t.uri.clone());
    let accent = Style::default().fg(theme.accent).add_modifier(Modifier::BOLD);
    let header = |label: &str| ListItem::new(Line::from(Span::styled(label.to_string(), accent)));

    let mut items: Vec<ListItem> = Vec::new();
    if !app.artist_top_tracks.is_empty() {
        items.push(header("Popular"));
        for t in &app.artist_top_tracks {
            let playing = playing_uri.as_deref() == Some(t.uri.as_str());
            items.push(track_item(theme, &t.name, &t.artists, t.duration_ms, app.liked.contains(&t.uri), playing));
        }
    }
    let mut album_row = |a: &crate::spotify::AlbumRef| {
        ListItem::new(Line::from(vec![
            Span::raw("  "),
            Span::raw(a.name.clone()),
        ]))
    };
    if !app.artist_albums.is_empty() {
        items.push(header("Albums"));
        items.extend(app.artist_albums.iter().map(&mut album_row));
    }
    if !app.artist_singles.is_empty() {
        items.push(header("Singles & EPs"));
        items.extend(app.artist_singles.iter().map(&mut album_row));
    }

    if items.is_empty() {
        items.push(ListItem::new(Line::from(Span::styled(
            "  Nothing to show for this artist.",
            Style::default().fg(theme.dim),
        ))));
    }

    let title = format!(" {} · Enter plays/opens · Esc back ", app.artist_title);
    let list = List::new(items)
        .block(panel(theme, title))
        .highlight_style(highlight(theme))
        .highlight_symbol("▶ ");
    f.render_stateful_widget(list, area, &mut app.artist_state);
}

fn render_now_playing(f: &mut Frame, app: &mut App, area: Rect) {
    let theme = app.theme;
    let block = panel(theme, " Now Playing ");
    let inner = block.inner(area);
    f.render_widget(block, area);

    // Album art and the track info are always shown; lyrics and the spectrum
    // are independent toggles that stack below, each scaled to its share so
    // several can be visible at once.
    let mut constraints = vec![Constraint::Fill(2), Constraint::Length(4)];
    if app.show_lyrics {
        constraints.push(Constraint::Fill(3));
    }
    if app.show_visualizer {
        constraints.push(Constraint::Fill(2));
    }
    let rects = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(inner);

    render_cover(f, app, rects[0]);
    render_track_info(f, app, rects[1]);

    let mut idx = 2;
    if app.show_lyrics {
        let title = match app.lyrics_or_status() {
            Ok(l) if !l.provider.is_empty() => format!("Lyrics · {}", l.provider),
            _ => "Lyrics".to_string(),
        };
        let body = section_body(f, theme, rects[idx], &title);
        app.lyrics_view_h = body.height as usize;
        render_lyrics(f, app, body);
        idx += 1;
    }
    if app.show_visualizer {
        let body = section_body(f, theme, rects[idx], "Spectrum");
        render_visualizer(f, app, body);
    }
}

/// Draw a small dim section header on the first row and return the body rect
/// below it (used to label the stacked lyrics/spectrum sections).
fn section_body(f: &mut Frame, theme: Theme, area: Rect, title: &str) -> Rect {
    if area.height == 0 {
        return area;
    }
    let header = Rect { x: area.x, y: area.y, width: area.width, height: 1 };
    f.render_widget(
        Paragraph::new(Span::styled(
            format!(" {title}"),
            Style::default().fg(theme.accent).add_modifier(Modifier::BOLD),
        )),
        header,
    );
    Rect {
        x: area.x,
        y: area.y + 1,
        width: area.width,
        height: area.height - 1,
    }
}

/// Render the album cover (or a placeholder) into `area`, sizing the art to the
/// largest centred square that fits and recording it in `app.art_size` so the
/// fetcher requests the cover at the current scale.
fn render_cover(f: &mut Frame, app: &mut App, area: Rect) {
    let theme = app.theme;
    // Cells are ~twice as tall as wide and a half-block packs two pixels per
    // cell, so cols ≈ 2·rows keeps the art square. Cap the height to the
    // user's configured size so the cover doesn't fill the panel, and centre
    // it in the area.
    let max_rows = app
        .config
        .art_size
        .clamp(crate::app::ART_SIZE_MIN, crate::app::ART_SIZE_MAX);
    let rows = area.height.min(area.width / 2).clamp(1, max_rows);
    let cols = (rows * 2).min(area.width);
    app.art_size = (cols, rows);
    let cover = Rect {
        x: area.x + (area.width.saturating_sub(cols)) / 2,
        y: area.y + (area.height.saturating_sub(rows)) / 2,
        width: cols,
        height: rows,
    };

    let art_drawn = crate::albumart::render_into(app, f, cover, cols, rows);
    if !art_drawn {
        // Only shown when no art is drawn – never over a working cover. In a
        // pixel mode (sixel/kitty), add a hint since those can silently render
        // nothing in terminals that don't support the chosen protocol.
        let msg = if app.player.current_track().is_none() {
            "\n  nothing playing".to_string()
        } else if app.image_picker.is_some() {
            "\n  ♪  loading cover…\n\n  no image? try another\n  Album-art mode in Settings".to_string()
        } else {
            "\n  ♪  loading cover…".to_string()
        };
        f.render_widget(
            Paragraph::new(msg)
                .style(Style::default().fg(theme.dim))
                .alignment(Alignment::Center),
            area,
        );
    }
}

/// Render the now-playing track's name/artist/album, centred.
fn render_track_info(f: &mut Frame, app: &App, area: Rect) {
    let theme = app.theme;
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
        None => Text::from(Line::from(Span::styled("–", Style::default().fg(theme.dim)))),
    };
    f.render_widget(
        Paragraph::new(info).alignment(Alignment::Center).wrap(Wrap { trim: true }),
        area,
    );
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
    let active = lyrics.active_line(app.lyrics_position());
    // Synced: keep the active line roughly centred. Unsynced: use the manual
    // scroll offset (PageUp/PageDown), clamped to the content.
    let start = match active {
        Some(a) => a.saturating_sub(height / 2),
        None => {
            let max_start = lyrics.lines.len().saturating_sub(height);
            app.lyrics_scroll.min(max_start)
        }
    };
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

    // Right-aligned "update available" badge, drawn over the status row. Shows
    // the bound keys so the update/dismiss actions are discoverable.
    if let Some(u) = &app.update_available {
        let dismiss = app.keymap.display(crate::keys::Action::DismissUpdate);
        let label = if crate::update::can_self_update() {
            let update = app.keymap.display(crate::keys::Action::UpdateNow);
            format!(" ⬆ v{} · {update} update · {dismiss} dismiss ", u.latest)
        } else {
            format!(" ⬆ v{} available · {dismiss} dismiss ", u.latest)
        };
        let badge = Paragraph::new(Line::from(Span::styled(
            label,
            Style::default()
                .fg(Color::Black)
                .bg(theme.accent)
                .add_modifier(Modifier::BOLD),
        )))
        .alignment(Alignment::Right);
        f.render_widget(badge, area);
    }
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
    let block = panel(theme, " Home · ↑↓ shelves · ←→ cards · Enter play/open ");
    let inner = block.inner(area);
    f.render_widget(block, area);

    if app.home.is_none() {
        let txt = if app.home_loading {
            "\n  Loading your Home…"
        } else {
            "\n  Home will load when you open it."
        };
        f.render_widget(Paragraph::new(txt).style(Style::default().fg(theme.dim)), inner);
        return;
    }

    let shelves = app.home_shelves();
    if shelves.is_empty() {
        f.render_widget(
            Paragraph::new("\n  Nothing to show – try playing some music first.")
                .style(Style::default().fg(theme.dim)),
            inner,
        );
        return;
    }

    let (lines, sel_top) = home_grid_lines(&shelves, app.home_sel, theme, inner.width as usize);

    // Vertical scroll: keep the whole selected shelf block (title + 2 card
    // lines) in view.
    let h = inner.height as usize;
    let max_scroll = lines.len().saturating_sub(h);
    let scroll = (sel_top + 3).saturating_sub(h).min(max_scroll) as u16;
    f.render_widget(Paragraph::new(Text::from(lines)).scroll((scroll, 0)), inner);
}

/// Build the Home grid as styled lines: each shelf is a section title followed
/// by a row of borderless cards (title + subtitle), the selected card marked
/// with a slim accent bar. Returns the lines and the first line of the selected
/// shelf's block (for vertical scrolling). Pure, so it can be unit-tested and
/// reused by the `--home-probe` diagnostic.
pub(crate) fn home_grid_lines(
    shelves: &[crate::app::HomeShelf],
    sel: (usize, usize),
    theme: Theme,
    avail_width: usize,
) -> (Vec<Line<'static>>, usize) {
    let accent = Style::default().fg(theme.accent).add_modifier(Modifier::BOLD);
    let accent_plain = Style::default().fg(theme.accent);
    let dim = Style::default().fg(theme.dim);
    let normal = Style::default();
    let (sel_shelf, sel_col) = sel;

    const CONTENT_W: usize = 22; // visible chars of title/subtitle per card
    const PREFIX: usize = 2; // accent bar + space ("▌ " / "  ")
    const GAP: usize = 3; // blank columns between cards
    const INDENT: usize = 2; // left margin of a card row
    let col_w = PREFIX + CONTENT_W;
    let avail = avail_width.saturating_sub(INDENT);
    let per_row = ((avail + GAP) / (col_w + GAP)).max(1);

    let mut lines: Vec<Line> = Vec::new();
    let mut sel_top = 0usize;

    for (si, shelf) in shelves.iter().enumerate() {
        let n = shelf.cards.len();
        // Horizontal window: keep the selected card visible on its own shelf.
        let start = if si == sel_shelf && sel_col >= per_row {
            (sel_col + 1 - per_row).min(n.saturating_sub(per_row))
        } else {
            0
        };
        let end = (start + per_row).min(n);

        // Blank line between shelves for breathing room.
        if si > 0 {
            lines.push(Line::from(""));
        }
        if si == sel_shelf {
            sel_top = lines.len();
        }

        // Section title (+ a position hint when the shelf scrolls horizontally).
        let mut title_spans = vec![Span::styled(format!(" {}", shelf.title), accent)];
        if start > 0 || end < n {
            title_spans.push(Span::styled(format!("   {}–{}/{}", start + 1, end, n), dim));
        }
        lines.push(Line::from(title_spans));

        // Two content lines per card row: title, then subtitle. A left accent
        // bar on both lines marks the selected card – no per-card borders.
        let mut l_title: Vec<Span> = vec![Span::raw(" ".repeat(INDENT))];
        let mut l_sub: Vec<Span> = vec![Span::raw(" ".repeat(INDENT))];
        for col in start..end {
            let card = &shelf.cards[col];
            let selected = si == sel_shelf && col == sel_col;
            let bar = if selected { "▌ " } else { "  " };
            let tstyle = if selected { accent } else { normal };
            let sstyle = if selected { accent_plain } else { dim };

            l_title.push(Span::styled(bar, accent_plain));
            l_title.push(Span::styled(fit(&card.title, CONTENT_W), tstyle));
            l_sub.push(Span::styled(bar, accent_plain));
            l_sub.push(Span::styled(fit(&card.subtitle, CONTENT_W), sstyle));

            if col + 1 < end {
                let g = Span::raw(" ".repeat(GAP));
                l_title.push(g.clone());
                l_sub.push(g);
            }
        }
        lines.push(Line::from(l_title));
        lines.push(Line::from(l_sub));
    }

    (lines, sel_top)
}

/// Truncate (with an ellipsis) or right-pad `s` to exactly `w` columns.
fn fit(s: &str, w: usize) -> String {
    if w == 0 {
        return String::new();
    }
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= w {
        let mut out: String = chars.iter().collect();
        out.push_str(&" ".repeat(w - chars.len()));
        out
    } else {
        let mut out: String = chars[..w - 1].iter().collect();
        out.push('…');
        out
    }
}

fn render_settings(f: &mut Frame, app: &App, area: Rect) {
    use crate::app::SettingRow;
    use crate::eq::{LABELS, MAX_DB};
    let theme = app.theme;
    let eq = app.player.eq();
    let rows = app.setting_rows();
    let dim = Style::default().fg(theme.dim);

    let header = |label: &str| {
        Line::from(Span::styled(
            format!(" {label}"),
            Style::default().fg(theme.accent).add_modifier(Modifier::BOLD),
        ))
    };

    // Active-output state for the Output section's ●/○ indicators.
    let active_remote = app.active_remote_device_id();
    let local_active = active_remote.is_none();
    let local_dev = app.player.current_device();
    let mut output_header_done = false;

    let mut lines: Vec<Line> = vec![header("Playback")];
    for (i, row) in rows.iter().enumerate() {
        // Section headers.
        match row {
            SettingRow::EqEnabled => {
                lines.push(Line::from(""));
                lines.push(header("Equalizer"));
            }
            SettingRow::ArtMode => {
                lines.push(Line::from(""));
                lines.push(header("Appearance"));
            }
            SettingRow::OutputLocal(_) if !output_header_done => {
                lines.push(Line::from(""));
                lines.push(header("Output · Enter selects"));
                output_header_done = true;
            }
            SettingRow::OutputConnect(j) => {
                if !output_header_done {
                    lines.push(Line::from(""));
                    lines.push(header("Output · Enter selects"));
                    output_header_done = true;
                }
                // Sub-header before the first Connect device.
                if *j == 0 {
                    lines.push(Line::from(Span::styled("   Spotify Connect", dim)));
                }
            }
            SettingRow::CheckUpdates => {
                lines.push(Line::from(""));
                lines.push(header("Updates"));
            }
            SettingRow::ReAuth => {
                lines.push(Line::from(""));
                lines.push(header("Account"));
                lines.push(Line::from(Span::styled(
                    format!("   logged in as {}", app.player.username()),
                    dim,
                )));
            }
            _ => {}
        }

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
            dim
        };

        // Output device rows render with an active-state dot rather than a value.
        match *row {
            SettingRow::OutputLocal(di) => {
                if let Some(d) = app.devices.get(di) {
                    let is_active = local_active
                        && match local_dev {
                            Some(name) => d.name == name,
                            None => d.is_default,
                        };
                    let dot = if is_active { "● " } else { "○ " };
                    let suffix = if d.is_default { "  (system default)" } else { "" };
                    lines.push(Line::from(vec![
                        Span::styled(format!("  {marker}"), label_style),
                        Span::styled(dot, Style::default().fg(if is_active { theme.accent } else { theme.dim })),
                        Span::styled(d.name.clone(), label_style),
                        Span::styled(suffix, dim),
                    ]));
                }
                continue;
            }
            SettingRow::OutputConnect(di) => {
                if let Some(d) = app.connect_devices.get(di) {
                    let is_active = match &active_remote {
                        Some(id) => Some(id.as_str()) == d.id.as_deref(),
                        None => d.is_active,
                    };
                    let dot = if is_active { "● " } else { "○ " };
                    let vol = d.volume_percent.map(|v| format!(" · {v}%")).unwrap_or_default();
                    lines.push(Line::from(vec![
                        Span::styled(format!("  {marker}"), label_style),
                        Span::styled(dot, Style::default().fg(if is_active { theme.accent } else { theme.dim })),
                        Span::styled(d.name.clone(), label_style),
                        Span::styled(format!("  ({}{})", d.kind, vol), dim),
                    ]));
                }
                continue;
            }
            _ => {}
        }

        let (label, value) = match *row {
            SettingRow::Normalisation => {
                ("Normalisation".to_string(), on_off(app.config.normalisation).to_string())
            }
            SettingRow::Quality => {
                let q = app.config.audio_quality;
                ("Quality".to_string(), format!("‹ {} ({} kbps) ›", q.label(), q.kbps()))
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
            SettingRow::ArtSize => {
                ("Art size".to_string(), format!("‹ {} rows ›", app.config.art_size))
            }
            SettingRow::CheckUpdates => (
                "Check for updates".to_string(),
                on_off(app.config.check_for_updates).to_string(),
            ),
            SettingRow::ReAuth => ("Re-authenticate".to_string(), "press Enter".to_string()),
            // Output rows handled above (with `continue`).
            SettingRow::OutputLocal(_) | SettingRow::OutputConnect(_) => unreachable!(),
        };

        lines.push(Line::from(vec![
            Span::styled(format!("  {marker}{label:<14}"), label_style),
            Span::styled(value, value_style),
        ]));
    }

    let block = panel(theme, " Settings · ↑↓ select · ←→ change · Enter toggle/select ");
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

fn track_item(
    theme: Theme,
    name: &str,
    artists: &str,
    duration_ms: u32,
    liked: bool,
    playing: bool,
) -> ListItem<'static> {
    // A reserved-width marker so the now-playing row stands out while names
    // stay aligned with the rest of the list.
    let marker = if playing { "♪ " } else { "  " };
    let name_style = if playing {
        Style::default().fg(theme.accent).add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    };
    let heart = if liked { "♥ " } else { "" };
    ListItem::new(Line::from(vec![
        Span::styled(marker, Style::default().fg(theme.accent)),
        Span::styled(heart.to_string(), Style::default().fg(theme.like)),
        Span::styled(name.to_string(), name_style),
        Span::styled(format!("  –  {artists}"), Style::default().fg(theme.dim)),
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

#[cfg(test)]
mod tests {
    use super::home_grid_lines;
    use crate::app::{HomeCard, HomeItem, HomeShelf};
    use crate::theme::Theme;

    fn flatten(lines: &[ratatui::text::Line]) -> String {
        lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn home_renders_borderless_card_shelves() {
        let shelves = vec![
            HomeShelf {
                title: "Daily Mix".to_string(),
                cards: vec![
                    HomeCard {
                        title: "Daily Mix 1".to_string(),
                        subtitle: "Drake, Future".to_string(),
                        item: HomeItem::Mix(0),
                    },
                    HomeCard {
                        title: "Daily Mix 2".to_string(),
                        subtitle: "Tame Impala".to_string(),
                        item: HomeItem::Mix(1),
                    },
                ],
            },
            HomeShelf {
                title: "Made For You".to_string(),
                cards: vec![HomeCard {
                    title: "Discover Weekly".to_string(),
                    subtitle: "Your weekly mixtape".to_string(),
                    item: HomeItem::Mix(2),
                }],
            },
        ];

        let (lines, sel_top) = home_grid_lines(&shelves, (0, 0), Theme::default(), 80);
        let text = flatten(&lines);

        // The redesign drops the heavy per-card box borders.
        assert!(!text.contains('┌'), "borders should be gone:\n{text}");
        assert!(!text.contains('│'), "borders should be gone:\n{text}");
        // Section titles, card titles and subtitles all show up.
        assert!(text.contains("Daily Mix"));
        assert!(text.contains("Daily Mix 1"));
        assert!(text.contains("Drake, Future"));
        assert!(text.contains("Discover Weekly"));
        // The selected card is marked with the accent bar.
        assert!(text.contains('▌'), "selected card not marked:\n{text}");
        // First shelf's title is the first line (no leading blank before it).
        assert_eq!(sel_top, 0, "selected shelf should anchor at the top");
        // Each shelf is title + 2 card lines; the second adds a leading blank.
        assert!(lines.len() >= 6, "unexpected line count: {}", lines.len());
    }
}
