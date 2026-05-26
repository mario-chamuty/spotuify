# SpoTUIfy

[![CI](https://github.com/mario-chamuty/spotuify/actions/workflows/ci.yml/badge.svg)](https://github.com/mario-chamuty/spotuify/actions/workflows/ci.yml)
[![Release](https://github.com/mario-chamuty/spotuify/actions/workflows/release.yml/badge.svg)](https://github.com/mario-chamuty/spotuify/actions/workflows/release.yml)
[![Latest release](https://img.shields.io/github/v/release/mario-chamuty/spotuify?sort=semver)](https://github.com/mario-chamuty/spotuify/releases/latest)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

A keyboard-driven **terminal Spotify client**, written in Rust. It plays audio
**locally** (it's its own Spotify Connect device via [librespot]) and gives you
search, your library with playlist editing, an in-app queue, **time-synced
lyrics**, audio-output selection, hybrid Spotify Connect remote control, MPRIS
media keys, a configurable theme/keymap, and colored album art (half-blocks or
sixel/kitty pixels).

```
┌──────────────────────────────────────────────────────────────────────────┐
│ 1 Search   2 Library   3 Tracks   4 Queue   5 Output                     │
├─────────────────────────────────────┬────────────────────────────────────┤
│ Search results                      │ Now Playing                        │
│ > Midnight City - M83      4:03     │            ▄▄▄▄▄▄▄▄▄▄▄▄            │
│   Outro - M83              2:43     │            ███ album ██            │
│   Reunion - M83            3:43     │            ▀▀▀▀▀▀▀▀▀▀▀▀            │
│                                     │           Midnight City            │
│                                     │                M83                 │
├─────────────────────────────────────┴────────────────────────────────────┤
│ > Playing   vol 70%   shuffle off   repeat off   #######-----  1:48/4:03 │
│ ? help                                                     12 result(s)  │
└──────────────────────────────────────────────────────────────────────────┘
```

> **Requires Spotify Premium** (librespot only streams for Premium accounts).
> Runs on **Linux, macOS and Windows**; media-key (MPRIS) support is Linux-only.

## Install

### Prebuilt binaries

Grab the archive for your platform from the [latest release][releases], unpack
it, and run the `spotuify` binary. Builds are produced for Linux x86-64, macOS
(Intel + Apple Silicon) and Windows x86-64.

### From source

Needs a stable Rust toolchain. On Linux also install the ALSA headers:

```sh
# Debian/Ubuntu: sudo apt install libasound2-dev pkg-config
cargo build --release
# binary at target/release/spotuify
```

## Setup

SpoTUIfy needs two logins, for two jobs (both cached after the first run):

- **Playback** streams through Spotify's official client — nothing to register.
- **Search & your library** use the Web API. Spotify's 2026 changes rate-limit
  the official client id there, so those features need your **own** free app.

1. Create an app in the [Spotify Developer Dashboard][dashboard] (any name).
2. Add this exact Redirect URI to it: `http://127.0.0.1:8888/callback`
3. Run `spotuify` once to generate `~/.config/spotuify/config.toml`, then set
   your app's **Client ID**:

   ```toml
   client_id = "your-app-client-id"
   ```

Run it again: open the `Browse to: …` URL(s) it prints, approve access, and
you're in. Credentials are cached under `~/.cache/spotuify/`.

## Keybindings

All bindings are configurable via the `[keys]` table (see below). Defaults:

| Key | Action |
| --- | --- |
| `1`–`5` | Switch to Search / Library / Tracks / Queue / Output |
| `Tab` | Cycle tabs · (in search box) cycle result type |
| `/` | Filter the current list; in Search, focus the query box |
| `i` | Focus the search box (`↑`/`↓` recall search history) |
| `↑`/`↓` or `k`/`j` · `g`/`G` | Move selection · jump to top/bottom |
| `Enter` | Play the item, or open the album/artist/playlist/show |
| `e` | Enqueue the selected track |
| `Space` · `n` / `b` | Play/pause · next / previous |
| `[` / `]` · `+` / `-` | Seek ∓5s · volume up/down |
| `s` · `r` | Toggle shuffle · cycle repeat (off→all→one) |
| `L` · `a` | Like/unlike track · add track to a playlist |
| `c` / `R` / `D` | Create / rename / remove a playlist (Library) |
| `y` | Toggle the lyrics panel |
| `?` · `q` / `Ctrl-C` | Help · quit |

## Features

- **Local playback** via librespot — the app is its own Connect device.
- **Search** tracks, albums, artists, playlists and podcasts; open any into a
  track list.
- **Library** — your playlists and Liked Songs, with like/add/create/rename.
- **Queue** with shuffle and repeat (off/all/one).
- **Time-synced lyrics** (`y`) — scrolling, line-highlighted, with a plain-text
  fallback for unsynced lyrics.
- **Output selection** — pick a local audio device, or **transfer playback** to
  a Spotify Connect device (phone/speaker) and control it remotely.
- **Podcasts** — search and play episodes alongside music.
- **MPRIS** (Linux) — `playerctl` and desktop media keys control SpoTUIfy.
- **Persistent session** — queue, position and preferences restore (paused) on
  the next launch.
- **Configurable** keymap and theme; **album art** as half-blocks or real pixels
  (sixel/kitty/iTerm2) where the terminal supports it.

## Configuration

`~/.config/spotuify/config.toml`:

| Key | Meaning |
| --- | --- |
| `client_id` | Your Spotify app Client ID (required for search/library). |
| `redirect_uri` | OAuth redirect; must match your app exactly. Default `http://127.0.0.1:8888/callback`. |
| `audio_backend` | librespot backend; `rodio` (default) works via cpal. |
| `audio_device` | Output device name; unset = system default (the Output tab edits this). |
| `volume` | Startup volume, 0–100. |
| `cache_size_mb` | librespot audio cache cap in MB (`null` = unbounded). |
| `normalisation` | Loudness-normalise tracks. |
| `art_mode` | `auto` · `halfblocks` · `sixel` · `kitty`. |

**Theme** — override colours (`#rrggbb`, `r,g,b`, indexed, or named):

```toml
[theme]
accent = "#1ed760"        # selections, active markers, progress
dim = "140,140,140"       # secondary text and borders
highlight_fg = "black"
highlight_bg = "#1ed760"
like = "#1ed760"
```

**Keys** — map an action to one key or a list. Syntax: chars, `space`, `enter`,
`esc`, `tab`, arrows, `home`/`end`/`pageup`/`pagedown`, `f1`–`f12`, the literal
`+`/`-`, and `ctrl+`/`alt+`/`shift+` prefixes:

```toml
[keys]
play-pause = "p"
quit = ["q", "ctrl+c"]
toggle-lyrics = "y"
```

Logs go to `~/.cache/spotuify/spotuify.log` (set `RUST_LOG` to adjust). Session
state is saved to `~/.cache/spotuify/state.json`.

## Troubleshooting

- **Tracks skip instantly / won't play** — usually a librespot ↔ Spotify
  mismatch. `cargo run --example probe` connects with your cached credentials
  and reports whether tracks resolve audio files and lyrics.
- **Login fails** — confirm the account is Premium, and that `redirect_uri`
  matches your app byte-for-byte (including the port).
- **No sound but it says Playing** — pick a device in the Output tab.
- **Album art looks blocky / colourless** — use a truecolor terminal.
- **Re-authenticate from scratch** — delete `~/.cache/spotuify/`.

## License

MIT — see [LICENSE](LICENSE).

[librespot]: https://github.com/librespot-org/librespot
[dashboard]: https://developer.spotify.com/dashboard
[releases]: https://github.com/mario-chamuty/spotuify/releases/latest
