# SpoTUIfy

A terminal Spotify client for Linux, written in Rust. It plays audio **locally**
(it is its own Spotify Connect device via [librespot]) and gives you search,
your playlists, an in-app queue, audio output selection, and colored album art
rendered with half-block characters — all from the keyboard.

```
┌ 1 Search · 2 Library · 3 Tracks · 4 Queue · 5 Output ──────────────────────┐
│ Search results                          │ Now Playing                       │
│ ▶ Midnight City — M83        (4:03)     │      ▄▄▄▄▄▄▄▄▄▄▄▄▄▄                 │
│   Outro — M83                (2:43)     │      ████ album ███                │
│   Reunion — M83              (3:43)     │      ████  art  ███                │
│                                         │      ▀▀▀▀▀▀▀▀▀▀▀▀▀▀                 │
│                                         │        Midnight City              │
│                                         │              M83                  │
├─────────────────────────────────────────┴───────────────────────────────────┤
│ ▶ Playing   vol  70%   shuffle off   repeat off   ███████▒▒▒▒▒  1:48 / 4:03  │
│ ? help   12 result(s)                                                         │
└───────────────────────────────────────────────────────────────────────────────┘
```

## Requirements

- **Spotify Premium** — librespot can only stream audio for Premium accounts.
- A Linux system with ALSA or PulseAudio (the `rodio` backend uses whatever your
  system exposes through cpal).
- A Rust toolchain (stable) to build it.

## Setup

### 1. Register a Spotify application

SpoTUIfy authenticates with your **own** Spotify app credentials (no secret
needed — it uses the PKCE flow).

1. Go to the [Spotify Developer Dashboard](https://developer.spotify.com/dashboard)
   and create an app (any name/description).
2. In the app settings, add this **exact** Redirect URI:

   ```
   http://127.0.0.1:8888/callback
   ```
3. Copy the app's **Client ID**.

### 2. Build

```sh
cargo build --release
```

The binary lands at `target/release/spotuify`.

### 3. Configure

Run it once to generate a config template, then edit it:

```sh
./target/release/spotuify          # writes the template and exits
$EDITOR ~/.config/spotuify/config.toml
```

Set `client_id` to the Client ID from step 1:

```toml
client_id = "your-client-id-here"
redirect_uri = "http://127.0.0.1:8888/callback"
audio_backend = "rodio"
# audio_device = "..."   # optional; pick from the Output tab instead
volume = 70
cache_size_mb = 1024
normalisation = true
```

### 4. Run

```sh
./target/release/spotuify
```

On first launch it prints a `Browse to: <url>` line. Open that URL, approve
access, and you'll be redirected back to the local listener automatically — no
copy/paste needed. Credentials are cached under `~/.cache/spotuify/`, so
subsequent launches skip the browser.

## Keybindings

| Key | Action |
| --- | --- |
| `1`–`5` | Jump to Search / Library / Tracks / Queue / Output |
| `Tab` | Cycle tabs |
| `/` or `i` | Focus the search box (then type, `Enter` to search, `Esc` to leave) |
| `Tab` (in search box) | Cycle result type: Tracks / Albums / Artists / Playlists |
| `↑`/`↓` or `k`/`j` | Move selection |
| `g` / `G` | Jump to top / bottom of the list |
| `Enter` | Play the selected track, or open the selected album/artist/playlist |
| `e` | Enqueue the selected track |
| `Space` | Play / pause |
| `n` / `b` | Next / previous track |
| `[` / `]` | Seek −5s / +5s |
| `+` / `-` | Volume up / down |
| `s` | Toggle shuffle |
| `r` | Cycle repeat (off → all → one) |
| `?` | Show the key cheat-sheet in the status bar |
| `q` / `Ctrl-C` | Quit |

## Features

- **Local playback** via librespot — the app is its own Connect device, audio
  comes out of your machine.
- **Search** tracks, albums, artists, and playlists; open any of them into a
  track list.
- **Your library** — playlists and Liked Songs.
- **In-app queue** — see what's coming, jump to any entry, enqueue tracks, with
  shuffle and repeat (off/all/one).
- **Audio output selection** — the *Output* tab lists your local output devices;
  selecting one re-routes playback live and remembers the choice.
- **Colored album art** rendered as Unicode half-blocks with 24-bit color
  (best in a truecolor terminal such as kitty, alacritty, foot or wezterm).

## Configuration reference

| Key | Meaning |
| --- | --- |
| `client_id` | Your Spotify app Client ID (required). |
| `redirect_uri` | OAuth redirect; must match the app's registered URI and be a loopback HTTP address with a port. |
| `audio_backend` | librespot backend. `rodio` (default) works via cpal on ALSA/Pulse. |
| `audio_device` | Output device name. Leave unset for the system default; the Output tab edits this. |
| `volume` | Startup volume, 0–100. |
| `cache_size_mb` | librespot audio cache size cap in MB (`null`/omit for unbounded). |
| `normalisation` | Loudness-normalise tracks (replaygain-style). |

Logs are written to `~/.cache/spotuify/spotuify.log`. Set `RUST_LOG` to change
verbosity, e.g. `RUST_LOG=spotuify=debug,librespot=info`.

## Troubleshooting

- **"could not start playback session" / login fails** — confirm the account is
  Premium and that `client_id`/`redirect_uri` exactly match your Spotify app
  (the redirect URI must be byte-for-byte identical, including the port).
- **No sound but it says Playing** — check the *Output* tab and select a device;
  the system default may be a dummy sink.
- **Album art is blocky or colorless** — your terminal isn't in truecolor mode.
  Use a terminal that supports 24-bit color.
- **Re-authenticate from scratch** — delete `~/.cache/spotuify/` and relaunch.

## How it works

One OAuth (PKCE) flow yields a single token that authorizes both halves of the
app: the [rspotify] web API client (search, playlists, library) and the
librespot playback session (`Credentials::with_access_token`). Network calls and
album-art decoding run on background tasks and report back over a channel, so
the UI never blocks. The playback engine keeps its own queue and drives
librespot directly; switching the output device rebuilds the librespot player
transparently and resumes the current track at its position.

[librespot]: https://github.com/librespot-org/librespot
[rspotify]: https://github.com/ramsayleung/rspotify
[ratatui]: https://github.com/ratatui/ratatui
