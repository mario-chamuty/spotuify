# SpoTUIfy

A terminal Spotify client for Linux, written in Rust. It plays audio **locally**
(it is its own Spotify Connect device via [librespot]) and gives you search
(tracks, albums, artists, playlists, podcasts), your library with playlist
editing, an in-app queue, type-to-filter, podcasts/episodes, audio output
selection, hybrid Spotify Connect remote control, MPRIS media keys, a
configurable theme and keymap, and colored album art (half-blocks or
sixel/kitty pixels) — all from the keyboard.

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
art_mode = "auto"        # auto | halfblocks | sixel | kitty
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

All keybindings are **configurable** (see the `[keys]` table below). The
defaults are:

| Key | Action (`action-name`) |
| --- | --- |
| `1`–`5` | Jump to Search / Library / Tracks / Queue / Output (`tab-search` … `tab-devices`) |
| `Tab` | Cycle tabs (`cycle-tab`) |
| `/` | Filter the current list; in Search, focus the query box (`enter-filter`) |
| `i` | Focus the search box (`focus-search`) |
| `Tab` (in search box) | Cycle result type: Tracks / Albums / Artists / Playlists / Episodes / Podcasts |
| `↑`/`↓` (in search box) | Cycle through search history |
| `↑`/`↓` or `k`/`j` | Move selection (`up` / `down`) |
| `g` / `G` | Jump to top / bottom of the list (`top` / `bottom`) |
| `Enter` | Play the selected item, or open the selected album/artist/playlist/show (`activate`) |
| `e` | Enqueue the selected track (`enqueue`) |
| `L` | Toggle "like" (saved) on the selected/now-playing track (`toggle-like`) |
| `a` | Add the selected track to a playlist (popup picker) (`add-to-playlist`) |
| `c` / `R` / `D` | Create / rename / remove (unfollow) a playlist in Library (`create-playlist` / `rename-playlist` / `delete-playlist`) |
| `Space` | Play / pause (`play-pause`) |
| `n` / `b` | Next / previous track (`next` / `prev`) |
| `[` / `]` | Seek −5s / +5s (`seek-back` / `seek-forward`) |
| `+` / `-` | Volume up / down (`volume-up` / `volume-down`) |
| `s` | Toggle shuffle (`toggle-shuffle`) |
| `r` | Cycle repeat off → all → one (`cycle-repeat`) |
| `?` | Show the key cheat-sheet in the status bar (`help`) |
| `q` / `Ctrl-C` | Quit (`quit`) |

### Filtering

In any list view, press `/` to filter: type to narrow the list
case-insensitively, `Enter` to keep the filter and return to navigation, or
`Esc` to clear it. `Enter`/`e`/`L`/`a` always act on the correct underlying item.

### Media keys & `playerctl` (MPRIS)

SpoTUIfy exposes the standard `org.mpris.MediaPlayer2` D-Bus interface, so your
desktop's media keys and tools like `playerctl` control it:

```sh
playerctl -p spotuify play-pause
playerctl -p spotuify next
playerctl -p spotuify metadata
```

If no session bus is available, MPRIS is silently skipped and everything else
keeps working.

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
- **Spotify Connect (hybrid)** — the *Output* tab also lists your Spotify Connect
  devices (phone, speaker, desktop). Select one to **transfer playback** to it;
  transport controls (`Space`/`n`/`b`/seek/volume) then drive the remote device
  over the Web API and the now-playing/progress display is polled (~1.5s).
  Re-selecting a local output returns control to librespot.
- **Podcasts & audiobooks** — search Episodes and Podcasts, open a show into its
  episode list, and play/queue episodes alongside music tracks.
- **Library writes** — like/unlike tracks (♥ indicator), add a track to a
  playlist, and create / rename / unfollow playlists. (See the re-auth note.)
- **Configurable keybindings & theme** — override any binding and the colour
  scheme from the config (`[keys]` / `[theme]`).
- **Persistent session** — your queue, current track, position, shuffle/repeat
  and search history are saved on quit and restored (paused) on the next launch.
- **Colored album art** rendered as Unicode half-blocks with 24-bit color, or as
  real pixels (sixel / kitty / iTerm2) in capable terminals (`art_mode`).

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
| `art_mode` | Album-art rendering: `auto` (detect sixel/kitty/iTerm2, else half-blocks), `halfblocks`, `sixel`, or `kitty`. |

### Theme (`[theme]`)

Override any of these colours (default is the Spotify-green look). Values may be
`#rrggbb`, `r,g,b`, an indexed `0`–`255`, or a named colour (`green`, `light-blue`…):

```toml
[theme]
accent = "#1ed760"        # selections, active markers, progress gauge
dim = "140,140,140"       # secondary text and borders
highlight_fg = "black"    # selected-row foreground
highlight_bg = "#1ed760"  # selected-row background
like = "#1ed760"          # the ♥ liked indicator
```

### Keybindings (`[keys]`)

Map any action to one key or a list of keys. Key syntax: single characters,
`space`, `enter`, `esc`, `tab`, arrows (`up`/`down`/`left`/`right`),
`home`/`end`/`pageup`/`pagedown`, `f1`–`f12`, the literal `+`/`-`, and
`ctrl+`/`alt+`/`shift+` modifier prefixes. Action names are listed in the
keybinding table above.

```toml
[keys]
play-pause = "p"
quit = ["q", "ctrl+c"]
toggle-like = "f"
seek-forward = "right"
seek-back = "left"
```

Logs are written to `~/.cache/spotuify/spotuify.log`. Set `RUST_LOG` to change
verbosity, e.g. `RUST_LOG=spotuify=debug,librespot=info`. Session state is saved
to `~/.cache/spotuify/state.json`.

### Library writes require one re-authentication

Liking tracks and creating/editing playlists need the
`playlist-modify-public` and `playlist-modify-private` scopes. These were added
to the requested scopes, which **invalidates any cached token**, so the next
launch runs the browser login once more. After that, cached credentials are
reused as before. (To force it manually, delete `~/.cache/spotuify/web-token.json`.)

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
transparently and resumes the current track at its position. Selecting a Spotify
Connect device instead transfers playback over the Web API and switches the app
into a polled "remote" mode. An MPRIS service runs as a background task, sharing
a playback snapshot over a watch channel and feeding media-key actions back into
the same event loop the keyboard uses.

[librespot]: https://github.com/librespot-org/librespot
[rspotify]: https://github.com/ramsayleung/rspotify
[ratatui]: https://github.com/ratatui/ratatui
