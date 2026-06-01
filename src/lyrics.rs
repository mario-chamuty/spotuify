//! Lyrics for the now-playing track.
//!
//! Primary source is librespot's lyrics endpoint (Spotify/Musixmatch), fetched
//! over the playback session (the official client), so it isn't subject to the
//! Web API rate limits and usually carries per-line timestamps.
//!
//! When Spotify has nothing for a track, we fall back to **LRCLIB**
//! (<https://lrclib.net>), a free, keyless community lyrics database. LRCLIB
//! often has time-synced (LRC) lyrics too; if not, we take its plain text and
//! show it unsynced. The fallback is best-effort and matches on artist + title
//! (+ album/duration when the exact endpoint is available).

use anyhow::{Context, Result};
use librespot::core::{Session, SpotifyId, SpotifyUri};
use librespot::metadata::lyrics::SyncType;
use librespot::metadata::Lyrics as LibrespotLyrics;

use crate::model::{PlayableKind, Track};

/// A track's lyrics, flattened to what the UI needs.
#[derive(Debug, Clone)]
pub struct Lyrics {
    /// Whether lines carry per-line timestamps (enables highlight + auto-scroll).
    pub synced: bool,
    pub provider: String,
    pub lines: Vec<LyricLine>,
}

#[derive(Debug, Clone)]
pub struct LyricLine {
    /// Line start time in milliseconds (0 for unsynced lyrics).
    pub time_ms: u32,
    pub text: String,
}

impl Lyrics {
    /// Index of the line that should be highlighted at `position_ms`, for synced
    /// lyrics: the last line whose start time has passed.
    pub fn active_line(&self, position_ms: u32) -> Option<usize> {
        if !self.synced || self.lines.is_empty() {
            return None;
        }
        self.lines
            .iter()
            .rposition(|l| l.time_ms <= position_ms)
            .or(Some(0))
    }
}

/// Fetch lyrics for `track`: try Spotify first, then fall back to LRCLIB.
pub async fn fetch(session: &Session, track: &Track) -> Result<Lyrics> {
    // 1. Spotify / Musixmatch via librespot — best quality, often time-synced.
    match fetch_spotify(session, &track.uri).await {
        Ok(l) if !l.lines.is_empty() => return Ok(l),
        Ok(_) => tracing::debug!("spotify returned empty lyrics, trying lrclib"),
        Err(e) => tracing::debug!("spotify lyrics unavailable ({e:#}), trying lrclib"),
    }

    // 2. LRCLIB fallback. Podcasts won't be there, so don't bother.
    if track.kind == PlayableKind::Episode {
        anyhow::bail!("no lyrics for this track");
    }
    fetch_lrclib(track)
        .await
        .context("no lyrics from Spotify or LRCLIB")
}

/// The original Spotify-session lyrics fetch for a `spotify:track:<id>` URI.
async fn fetch_spotify(session: &Session, track_uri: &str) -> Result<Lyrics> {
    let uri = SpotifyUri::from_uri(track_uri).context("bad track uri")?;
    let id = SpotifyId::try_from(&uri).context("uri has no playable id")?;
    let raw = LibrespotLyrics::get(session, &id)
        .await
        .context("no lyrics for this track")?;

    let synced = matches!(raw.lyrics.sync_type, SyncType::LineSynced);
    let lines = raw
        .lyrics
        .lines
        .iter()
        .map(|l| LyricLine {
            time_ms: l.start_time_ms.parse().unwrap_or(0),
            text: l.words.clone(),
        })
        .collect();

    Ok(Lyrics {
        synced,
        provider: raw.lyrics.provider_display_name.clone(),
        lines,
    })
}

// ---- LRCLIB fallback -------------------------------------------------------

/// One LRCLIB record (subset of fields we use). `syncedLyrics` is LRC text;
/// `plainLyrics` is newline-separated plain text. Either may be absent.
#[derive(serde::Deserialize)]
struct LrcLibItem {
    #[serde(rename = "syncedLyrics", default)]
    synced_lyrics: Option<String>,
    #[serde(rename = "plainLyrics", default)]
    plain_lyrics: Option<String>,
    #[serde(default)]
    instrumental: bool,
}

fn lrclib_ua() -> String {
    format!(
        "spotuify/{} (https://github.com/mario-chamuty/spotuify)",
        env!("CARGO_PKG_VERSION")
    )
}

/// Query LRCLIB for `track`, preferring synced lyrics over plain.
async fn fetch_lrclib(track: &Track) -> Result<Lyrics> {
    let client = reqwest::Client::new();
    let artist = primary_artist(&track.artists);
    let duration = (track.duration_ms / 1000).to_string();

    // Exact-match endpoint first (artist + title + album + duration). LRCLIB
    // allows a small duration tolerance and 404s on no match.
    let resp = client
        .get("https://lrclib.net/api/get")
        .query(&[
            ("artist_name", artist.as_str()),
            ("track_name", track.name.as_str()),
            ("album_name", track.album.as_str()),
            ("duration", duration.as_str()),
        ])
        .header("User-Agent", lrclib_ua())
        .send()
        .await
        .context("lrclib request failed")?;

    let item = if resp.status().is_success() {
        let body = resp.text().await.context("reading lrclib response")?;
        Some(serde_json::from_str::<LrcLibItem>(&body).context("parsing lrclib record")?)
    } else {
        // No exact match — fall back to a fuzzy search and pick the best hit.
        search_lrclib(&client, &artist, &track.name).await?
    };

    let item = item.context("no lyrics on lrclib")?;
    lyrics_from_lrclib(item).context("lrclib record had no usable lyrics")
}

/// Fuzzy LRCLIB search; returns the first hit with synced lyrics, else the
/// first with plain lyrics.
async fn search_lrclib(
    client: &reqwest::Client,
    artist: &str,
    title: &str,
) -> Result<Option<LrcLibItem>> {
    let body = client
        .get("https://lrclib.net/api/search")
        .query(&[("artist_name", artist), ("track_name", title)])
        .header("User-Agent", lrclib_ua())
        .send()
        .await
        .context("lrclib search failed")?
        .error_for_status()
        .context("lrclib search returned an error")?
        .text()
        .await
        .context("reading lrclib search response")?;

    let items: Vec<LrcLibItem> =
        serde_json::from_str(&body).context("parsing lrclib search results")?;

    let mut best_plain = None;
    for item in items {
        if has_text(&item.synced_lyrics) {
            return Ok(Some(item));
        }
        if best_plain.is_none() && has_text(&item.plain_lyrics) {
            best_plain = Some(item);
        }
    }
    Ok(best_plain)
}

/// Turn an LRCLIB record into our `Lyrics`, preferring synced LRC.
fn lyrics_from_lrclib(item: LrcLibItem) -> Option<Lyrics> {
    if item.instrumental {
        return Some(Lyrics {
            synced: false,
            provider: "LRCLIB".to_string(),
            lines: vec![LyricLine {
                time_ms: 0,
                text: "♪ Instrumental".to_string(),
            }],
        });
    }

    if let Some(lrc) = item.synced_lyrics.filter(|s| !s.trim().is_empty()) {
        let lines = parse_lrc(&lrc);
        if !lines.is_empty() {
            return Some(Lyrics {
                synced: true,
                provider: "LRCLIB".to_string(),
                lines,
            });
        }
    }

    if let Some(plain) = item.plain_lyrics.filter(|s| !s.trim().is_empty()) {
        let lines = plain
            .lines()
            .map(|l| LyricLine {
                time_ms: 0,
                text: l.to_string(),
            })
            .collect();
        return Some(Lyrics {
            synced: false,
            provider: "LRCLIB".to_string(),
            lines,
        });
    }

    None
}

fn has_text(s: &Option<String>) -> bool {
    s.as_deref().is_some_and(|s| !s.trim().is_empty())
}

/// LRCLIB matches best on the primary artist; tracks list them comma-separated.
fn primary_artist(artists: &str) -> String {
    artists
        .split(',')
        .next()
        .unwrap_or(artists)
        .trim()
        .to_string()
}

/// Parse LRC text (`[mm:ss.xx] words`) into timestamped lines. Lines may carry
/// several timestamps (repeated lyrics); each becomes its own entry. Metadata
/// tags like `[ar:..]`, `[ti:..]`, `[length:..]` are ignored.
fn parse_lrc(s: &str) -> Vec<LyricLine> {
    let mut out: Vec<LyricLine> = Vec::new();
    for raw in s.lines() {
        let mut rest = raw;
        let mut times: Vec<u32> = Vec::new();
        while rest.starts_with('[') {
            let Some(end) = rest.find(']') else { break };
            if let Some(ms) = parse_lrc_time(&rest[1..end]) {
                times.push(ms);
            }
            rest = &rest[end + 1..];
        }
        let text = rest.trim().to_string();
        for t in times {
            out.push(LyricLine {
                time_ms: t,
                text: text.clone(),
            });
        }
    }
    out.sort_by_key(|l| l.time_ms);
    out
}

/// Parse one LRC timestamp tag (`mm:ss`, `mm:ss.xx`, `mm:ss.xxx`) to ms.
/// Returns `None` for non-timestamp metadata tags.
fn parse_lrc_time(tag: &str) -> Option<u32> {
    let (mm, rest) = tag.split_once(':')?;
    let mm: u32 = mm.trim().parse().ok()?;
    let (ss, frac) = rest.split_once('.').unwrap_or((rest, ""));
    let ss: u32 = ss.trim().parse().ok()?;
    let frac_ms = match frac.trim() {
        "" => 0,
        f => {
            let f3: String = f.chars().take(3).collect();
            let val: u32 = f3.parse().ok()?;
            match f3.len() {
                1 => val * 100,
                2 => val * 10,
                _ => val,
            }
        }
    };
    Some(mm * 60_000 + ss * 1000 + frac_ms)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_synced_lrc_with_fraction_widths() {
        let lrc = "[ar:Artist]\n[ti:Title]\n[length:02:00]\n[00:01.00]first\n[00:12.5]second\n[01:00.250]third";
        let lines = parse_lrc(lrc);
        assert_eq!(lines.len(), 3, "metadata tags must be skipped");
        assert_eq!(lines[0].time_ms, 1_000);
        assert_eq!(lines[0].text, "first");
        assert_eq!(lines[1].time_ms, 12_500, "single-digit fraction is tenths");
        assert_eq!(lines[2].time_ms, 60_250);
    }

    #[test]
    fn repeated_timestamps_expand_to_multiple_lines() {
        let lines = parse_lrc("[00:05.00][01:05.00]chorus");
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].time_ms, 5_000);
        assert_eq!(lines[1].time_ms, 65_000);
        assert!(lines.iter().all(|l| l.text == "chorus"));
    }

    #[test]
    fn primary_artist_takes_first_of_many() {
        assert_eq!(primary_artist("Drake, Future, Metro"), "Drake");
        assert_eq!(primary_artist("Adele"), "Adele");
    }

    #[test]
    fn plain_lyrics_are_unsynced() {
        let item = LrcLibItem {
            synced_lyrics: None,
            plain_lyrics: Some("line one\nline two".to_string()),
            instrumental: false,
        };
        let lyrics = lyrics_from_lrclib(item).expect("plain lyrics");
        assert!(!lyrics.synced);
        assert_eq!(lyrics.lines.len(), 2);
        assert_eq!(lyrics.active_line(999_999), None, "unsynced has no active line");
    }
}
