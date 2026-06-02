# SpoTUIfy

[![Latest release](https://img.shields.io/github/v/release/mario-chamuty/spotuify?sort=semver)](https://github.com/mario-chamuty/spotuify/releases/latest)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

Keyboard-driven terminal Spotify client in Rust. Plays locally via [librespot].
Requires Spotify Premium. Runs on Linux, macOS, Windows (MPRIS media keys are
Linux-only).

```
┌──────────────────────────────────────────────────────────────────────────┐
│ 1 Search   2 Library   3 Tracks   4 Queue   5 Settings   6 Home          │
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

## Install

Prebuilt binaries for Linux, macOS and Windows: [latest release][releases].

From source (Linux needs ALSA headers, e.g. `sudo apt install libasound2-dev
pkg-config`):

```sh
cargo build --release
# binary at target/release/spotuify
```

## Setup

1. Register a free app at the [Spotify Developer Dashboard][dashboard], adding
   the redirect URI `http://127.0.0.1:8888/callback`.
2. Run `spotuify` once to write `~/.config/spotuify/config.toml`, then set:
   ```toml
   client_id = "your-app-client-id"
   ```
3. Run again and follow the `Browse to: ...` OAuth prompt. Credentials cache
   under `~/.cache/spotuify/`.

## Home cookie (optional)

The Home tab can show your real Spotify Home (Daily Mixes, Discover Weekly,
genre/mood shelves) when you're logged into Spotify in a local browser.
SpoTUIfy auto-detects your `sp_dc` session cookie (Firefox, or Chromium-family
on Linux), mints a web-player token, and reads your Home from Spotify's own
endpoint. No setup required; with no cookie found, Home falls back to
public-API shelves (recently played, top tracks/artists, "Made for you" mixes).
To override auto-detection, set `sp_dc` in the config. Verify with
`spotuify --home-probe`.

## Keybindings

Configurable via `[keys]`. Defaults:

| Key | Action |
| --- | --- |
| `1` to `6` | Switch tab (Search, Library, Tracks, Queue, Settings, Home) |
| `Tab` / `Shift+Tab` | Next / previous tab |
| `/` · `i` | Filter the current list · focus search box (`↑↓` recalls history) |
| Arrows or `hjkl` · `g` / `G` | Move · jump top/bottom |
| `Enter` · `e` · `A` · `O` | Play/open · enqueue · open artist · open album |
| `Esc` | Back through opened contexts; in filter, clear |
| `Space` · `n` / `b` | Play/pause · next / previous |
| `[` / `]` · `+` / `-` | Seek 5s · volume |
| `s` · `r` · `L` · `a` | Shuffle · repeat · like · add to playlist |
| `c` / `R` / `D` | Create / rename / remove playlist (Library) |
| `y` · `E` · `v` | Lyrics · equalizer · spectrum visualizer |
| `?` · `q` / `Ctrl-C` | Help modal · quit |

Home tab: `↑↓` between shelves, `←→` between cards. EQ overlay: `←→` selects
band, `↑↓` adjusts gain, `p` / `P` cycles presets, `a` suggests gains from the
live spectrum, `0` resets band, `R` flattens all, `space` toggles EQ, `Esc`
closes.

## Configuration

`~/.config/spotuify/config.toml`:

| Key | Meaning |
| --- | --- |
| `client_id` | Spotify app Client ID. Required for search/library. |
| `redirect_uri` | OAuth redirect. Default `http://127.0.0.1:8888/callback`. |
| `sp_dc` | Manual override for the auto-detected Home cookie. Usually unset. |
| `audio_backend` | librespot backend, `rodio` (default). |
| `audio_device` | Output device name; unset is the system default. |
| `volume` | Startup volume, 0 to 100. |
| `cache_size_mb` | Audio cache cap in MB; `null` is unbounded. |
| `normalisation` | Loudness-normalise tracks. |
| `art_mode` | `auto`, `halfblocks`, `sixel`, `kitty`. |

`[theme]`: `accent`, `dim`, `highlight_fg`, `highlight_bg`, `like` (colors as
`#rrggbb`, `r,g,b`, indexed, or named).

`[equalizer]`: 10-band graphic EQ (31 Hz to 16 kHz, ±12 dB) with `enabled` and
`gains_db = [0, 0, 0, 0, 0, 0, 0, 0, 0, 0]`.

`[keys]`: map an action to one key or a list. Syntax: chars, `space`, `enter`,
`esc`, `tab`, arrows, `home`/`end`/`pageup`/`pagedown`, `f1` to `f12`, literal
`+`/`-`, and `ctrl+` / `alt+` / `shift+` prefixes:

```toml
[keys]
play-pause = "p"
quit = ["q", "ctrl+c"]
```

Logs at `~/.cache/spotuify/spotuify.log` (`RUST_LOG` adjusts level). Session at
`~/.cache/spotuify/state.json`.

## Troubleshooting

- Tracks won't play: `cargo run --example probe` reports whether they resolve.
- Login fails: account must be Premium; `redirect_uri` must match exactly.
- No sound but "Playing": pick a device in Settings, under Output.
- Reauth from scratch: delete `~/.cache/spotuify/`.

## License

MIT. See [LICENSE](LICENSE).

[librespot]: https://github.com/librespot-org/librespot
[dashboard]: https://developer.spotify.com/dashboard
[releases]: https://github.com/mario-chamuty/spotuify/releases/latest
