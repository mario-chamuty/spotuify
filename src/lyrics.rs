//! Lyrics for the now-playing track, from several sources tried in order.
//!
//! 1. **Spotify / Musixmatch** via librespot's lyrics endpoint — best quality,
//!    usually time-synced. Fetched over the playback session.
//! 2. **LRCLIB** (<https://lrclib.net>) — free, keyless; often has synced LRC.
//! 3. **Genius** (<https://genius.com>) — broad catalogue; plain text only.
//! 4. **KaraokeTexty** (<https://www.karaoketexty.cz>) — strong Czech/Slovak
//!    coverage; plain text only.
//!
//! Every web source runs under a short timeout and is strictly best-effort: a
//! failure or timeout just moves to the next source, so a slow network can
//! never leave lyrics stuck "loading". Untrusted search results (Genius,
//! KaraokeTexty) are validated against the requested artist + title — a
//! mismatch is discarded rather than shown, so we never display wrong lyrics.

use std::time::Duration;

use anyhow::{Context, Result};
use librespot::core::{Session, SpotifyId, SpotifyUri};
use librespot::metadata::lyrics::SyncType;
use librespot::metadata::Lyrics as LibrespotLyrics;
use regex::Regex;

use crate::model::{PlayableKind, Track};

/// Browser-like UA for the scrape sources (some reject default clients).
const BROWSER_UA: &str =
    "Mozilla/5.0 (X11; Linux x86_64; rv:120.0) Gecko/20100101 Firefox/120.0";

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

    fn plain(provider: &str, text: &str) -> Option<Self> {
        let lines: Vec<LyricLine> = text
            .lines()
            .map(|l| LyricLine {
                time_ms: 0,
                text: l.trim().to_string(),
            })
            .collect();
        // Drop leading/trailing blank lines.
        let start = lines.iter().position(|l| !l.text.is_empty())?;
        let end = lines.iter().rposition(|l| !l.text.is_empty())? + 1;
        Some(Lyrics {
            synced: false,
            provider: provider.to_string(),
            lines: lines[start..end].to_vec(),
        })
    }
}

/// Fetch lyrics for `track`: Spotify first, then web fallbacks in order.
pub async fn fetch(session: &Session, track: &Track) -> Result<Lyrics> {
    // 1. Spotify / Musixmatch via librespot — best quality, often time-synced.
    match fetch_spotify(session, &track.uri).await {
        Ok(l) if !l.lines.is_empty() => return Ok(l),
        Ok(_) => tracing::debug!("spotify returned empty lyrics, trying fallbacks"),
        Err(e) => tracing::debug!("spotify lyrics unavailable ({e:#}), trying fallbacks"),
    }

    // Podcasts won't be in the lyrics databases.
    if track.kind == PlayableKind::Episode {
        anyhow::bail!("no lyrics for this track");
    }

    let client = http_client();
    let artist = primary_artist(&track.artists);
    let title = clean_title(&track.name);

    // 2..N: best-effort web sources. Errors and "not found" both fall through.
    macro_rules! try_source {
        ($fut:expr, $name:literal) => {
            match $fut.await {
                Ok(Some(l)) => return Ok(l),
                Ok(None) => tracing::debug!("{}: no match", $name),
                Err(e) => tracing::debug!("{} failed: {e:#}", $name),
            }
        };
    }
    try_source!(lrclib(&client, track, &artist, &title), "lrclib");
    try_source!(genius(&client, &artist, &title), "genius");
    try_source!(karaoketexty(&client, &artist, &title), "karaoketexty");

    anyhow::bail!("no lyrics from Spotify, LRCLIB, Genius or KaraokeTexty")
}

/// The Spotify-session lyrics fetch for a `spotify:track:<id>` URI (unchanged).
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

/// Shared HTTP client with timeouts so no source can hang the fetch task.
fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(8))
        .connect_timeout(Duration::from_secs(4))
        .user_agent(BROWSER_UA)
        .build()
        .unwrap_or_default()
}

// ---- LRCLIB ----------------------------------------------------------------

#[derive(serde::Deserialize)]
struct LrcLibItem {
    #[serde(rename = "trackName", default)]
    track_name: String,
    #[serde(rename = "artistName", default)]
    artist_name: String,
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

/// LRCLIB: exact endpoint first (duration-matched), then a validated search.
async fn lrclib(
    client: &reqwest::Client,
    track: &Track,
    artist: &str,
    title: &str,
) -> Result<Option<Lyrics>> {
    let duration = (track.duration_ms / 1000).to_string();
    let resp = client
        .get("https://lrclib.net/api/get")
        .query(&[
            ("artist_name", artist),
            ("track_name", title),
            ("album_name", track.album.as_str()),
            ("duration", duration.as_str()),
        ])
        .header("User-Agent", lrclib_ua())
        .send()
        .await
        .context("lrclib get failed")?;

    // The duration-matched endpoint is trusted without re-validation.
    if resp.status().is_success() {
        let body = resp.text().await.context("reading lrclib get")?;
        if let Ok(item) = serde_json::from_str::<LrcLibItem>(&body) {
            if let Some(l) = lyrics_from_lrclib(item) {
                return Ok(Some(l));
            }
        }
    }

    // Fall back to a fuzzy search; validate the hit's artist + title.
    let body = client
        .get("https://lrclib.net/api/search")
        .query(&[("artist_name", artist), ("track_name", title)])
        .header("User-Agent", lrclib_ua())
        .send()
        .await
        .context("lrclib search failed")?
        .error_for_status()
        .context("lrclib search error")?
        .text()
        .await
        .context("reading lrclib search")?;

    let items: Vec<LrcLibItem> =
        serde_json::from_str(&body).context("parsing lrclib search")?;
    let mut best_plain = None;
    for item in items {
        if !title_matches(title, &item.track_name) || !artist_matches(artist, &item.artist_name) {
            continue;
        }
        if has_text(&item.synced_lyrics) {
            return Ok(lyrics_from_lrclib(item));
        }
        if best_plain.is_none() && has_text(&item.plain_lyrics) {
            best_plain = Some(item);
        }
    }
    Ok(best_plain.and_then(lyrics_from_lrclib))
}

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
    item.plain_lyrics
        .as_deref()
        .and_then(|p| Lyrics::plain("LRCLIB", p))
}

// ---- Genius ----------------------------------------------------------------

/// Genius: public multi-search, validate the top song hit, scrape its page.
async fn genius(client: &reqwest::Client, artist: &str, title: &str) -> Result<Option<Lyrics>> {
    let q = format!("{artist} {title}");
    let body = client
        .get("https://genius.com/api/search/multi")
        .query(&[("q", q.as_str())])
        .send()
        .await
        .context("genius search failed")?
        .error_for_status()
        .context("genius search error")?
        .text()
        .await
        .context("reading genius search")?;

    let json: serde_json::Value =
        serde_json::from_str(&body).context("parsing genius search")?;
    let sections = json
        .get("response")
        .and_then(|r| r.get("sections"))
        .and_then(|s| s.as_array())
        .cloned()
        .unwrap_or_default();

    let mut url = None;
    for section in &sections {
        let Some(hits) = section.get("hits").and_then(|h| h.as_array()) else {
            continue;
        };
        for hit in hits {
            if hit.get("type").and_then(|t| t.as_str()) != Some("song") {
                continue;
            }
            let result = match hit.get("result") {
                Some(r) => r,
                None => continue,
            };
            let cand_title = result
                .get("title")
                .and_then(|t| t.as_str())
                .unwrap_or_default();
            let cand_artist = result
                .get("primary_artist")
                .and_then(|a| a.get("name"))
                .and_then(|n| n.as_str())
                .or_else(|| result.get("artist_names").and_then(|n| n.as_str()))
                .unwrap_or_default();
            if title_matches(title, cand_title) && artist_matches(artist, cand_artist) {
                url = result
                    .get("url")
                    .and_then(|u| u.as_str())
                    .map(str::to_string);
                break;
            }
        }
        if url.is_some() {
            break;
        }
    }

    let Some(url) = url else { return Ok(None) };
    let page = client
        .get(&url)
        .send()
        .await
        .context("genius page failed")?
        .error_for_status()
        .context("genius page error")?
        .text()
        .await
        .context("reading genius page")?;

    Ok(Lyrics::plain("Genius", &extract_genius_lyrics(&page)))
}

/// Extract every top-level `data-lyrics-container` div as clean text. Genius
/// nests `<div>`s inside the container (annotations, ad slots, and a header),
/// so we balance the tags rather than stop at the first `</div>`, and drop the
/// `data-exclude-from-selection` subtrees (the "N Contributors / … Lyrics"
/// header and ads) before converting to text.
fn extract_genius_lyrics(page: &str) -> String {
    let re = Regex::new(r#"data-lyrics-container="true""#).expect("valid regex");
    let mut out = String::new();
    let mut consumed_to = 0usize;
    for m in re.find_iter(page) {
        if m.start() < consumed_to {
            continue; // nested inside a container we already captured
        }
        if let Some((_, inner, end)) = balanced_div(page, m.start()) {
            consumed_to = end;
            let text = html_to_text(&strip_excluded(&inner));
            if !text.trim().is_empty() {
                out.push_str(&text);
                out.push('\n');
            }
        }
    }
    out
}

/// Find the `<div …>…</div>` whose opening tag contains the byte at `attr_pos`,
/// balancing nested `<div>`s. Returns `(open_tag_start, inner_html,
/// index_after_closing_tag)`. All split points are ASCII, so byte slicing is
/// UTF-8 safe.
fn balanced_div(html: &str, attr_pos: usize) -> Option<(usize, String, usize)> {
    let open_start = html[..attr_pos].rfind("<div")?;
    let gt = html[attr_pos..].find('>')? + attr_pos;
    let start = gt + 1;
    let mut i = start;
    let mut depth = 1usize;
    while depth > 0 {
        let next_open = html[i..].find("<div").map(|p| i + p);
        let next_close = html[i..].find("</div>").map(|p| i + p);
        match (next_open, next_close) {
            (_, None) => return None,
            (Some(o), Some(c)) if o < c => {
                depth += 1;
                i = o + 4;
            }
            (_, Some(c)) => {
                depth -= 1;
                if depth == 0 {
                    return Some((open_start, html[start..c].to_string(), c + 6));
                }
                i = c + 6;
            }
        }
    }
    None
}

/// Remove any `data-exclude-from-selection="true"` div subtrees from a fragment.
fn strip_excluded(fragment: &str) -> String {
    let mut s = fragment.to_string();
    while let Some(pos) = s.find(r#"data-exclude-from-selection="true""#) {
        match balanced_div(&s, pos) {
            Some((open_start, _, end)) => s.replace_range(open_start..end, ""),
            None => break,
        }
    }
    s
}

// ---- KaraokeTexty (Czech/Slovak) -------------------------------------------

/// KaraokeTexty: `?q=` search, validate a song hit, scrape its lyrics page.
async fn karaoketexty(
    client: &reqwest::Client,
    artist: &str,
    title: &str,
) -> Result<Option<Lyrics>> {
    let q = format!("{artist} {title}");
    let page = client
        .get("https://www.karaoketexty.cz/search")
        .query(&[("q", q.as_str())])
        .send()
        .await
        .context("karaoketexty search failed")?
        .error_for_status()
        .context("karaoketexty search error")?
        .text()
        .await
        .context("reading karaoketexty search")?;

    // Song results: <a href="/texty-pisni/<artist>/<song>">Artist - Title</a>.
    let re = Regex::new(r#"href="(/texty-pisni/[a-z0-9-]+/[a-z0-9-]+)"[^>]*>([^<]+)</a>"#)
        .expect("valid regex");
    let mut path = None;
    for cap in re.captures_iter(&page) {
        let label = html_to_text(&cap[2]);
        let (cand_artist, cand_title) = match label.split_once('-') {
            Some((a, t)) => (a.trim(), t.trim()),
            None => ("", label.trim()),
        };
        if title_matches(title, cand_title)
            && (cand_artist.is_empty() || artist_matches(artist, cand_artist))
        {
            path = Some(cap[1].to_string());
            break;
        }
    }
    let Some(path) = path else { return Ok(None) };

    let song = client
        .get(format!("https://www.karaoketexty.cz{path}"))
        .send()
        .await
        .context("karaoketexty page failed")?
        .error_for_status()
        .context("karaoketexty page error")?
        .text()
        .await
        .context("reading karaoketexty page")?;

    // Original lyrics sit in `<span class="para_col1">…</span>` (left column;
    // the right column is a translation we skip).
    let col = Regex::new(r#"(?s)<span class="para_col1">(.*?)</span>"#).expect("valid regex");
    let mut text = String::new();
    for cap in col.captures_iter(&song) {
        text.push_str(&html_to_text(&cap[1]));
        text.push('\n');
    }
    Ok(Lyrics::plain("KaraokeTexty", &text))
}

// ---- shared helpers --------------------------------------------------------

fn has_text(s: &Option<String>) -> bool {
    s.as_deref().is_some_and(|s| !s.trim().is_empty())
}

/// LRCLIB/Genius match best on the primary artist; tracks list them comma- or
/// `&`-separated.
fn primary_artist(artists: &str) -> String {
    artists
        .split([',', '&'])
        .next()
        .unwrap_or(artists)
        .trim()
        .to_string()
}

/// Drop `(...)`/`[...]` and ` - …` suffixes (feat./remaster/live) from a title.
fn clean_title(title: &str) -> String {
    let head = title.split(" - ").next().unwrap_or(title);
    let mut out = String::new();
    let mut depth = 0u32;
    for ch in head.chars() {
        match ch {
            '(' | '[' => depth += 1,
            ')' | ']' => depth = depth.saturating_sub(1),
            _ if depth == 0 => out.push(ch),
            _ => {}
        }
    }
    out.trim().to_string()
}

/// Lowercase, strip diacritics, drop punctuation, collapse whitespace — so
/// "Voľnosť" and "volnost" compare equal.
fn normalize(s: &str) -> String {
    let mut out = String::new();
    let mut prev_space = true;
    for ch in s.chars() {
        for lc in ch.to_lowercase() {
            let d = deaccent(lc);
            if d.is_alphanumeric() {
                out.push(d);
                prev_space = false;
            } else if !prev_space {
                out.push(' ');
                prev_space = true;
            }
        }
    }
    out.trim().to_string()
}

fn deaccent(c: char) -> char {
    match c {
        'á' | 'à' | 'â' | 'ä' | 'ã' | 'å' | 'ā' | 'ą' => 'a',
        'č' | 'ç' | 'ć' => 'c',
        'ď' => 'd',
        'é' | 'è' | 'ê' | 'ë' | 'ě' | 'ē' | 'ę' => 'e',
        'í' | 'ì' | 'î' | 'ï' | 'ī' => 'i',
        'ĺ' | 'ľ' | 'ł' => 'l',
        'ñ' | 'ň' => 'n',
        'ó' | 'ò' | 'ô' | 'ö' | 'õ' | 'ō' | 'ø' => 'o',
        'ŕ' | 'ř' => 'r',
        'š' | 'ś' | 'ş' => 's',
        'ť' => 't',
        'ú' | 'ù' | 'û' | 'ü' | 'ū' | 'ů' => 'u',
        'ý' | 'ÿ' => 'y',
        'ž' | 'ź' | 'ż' => 'z',
        other => other,
    }
}

/// True if normalized `a` and `b` are equal or one contains the other.
fn fuzzy(a: &str, b: &str) -> bool {
    let (a, b) = (normalize(a), normalize(b));
    !a.is_empty() && !b.is_empty() && (a == b || a.contains(&b) || b.contains(&a))
}

fn title_matches(want: &str, cand: &str) -> bool {
    fuzzy(want, &clean_title(cand))
}

fn artist_matches(want: &str, cand: &str) -> bool {
    fuzzy(want, &primary_artist(cand)) || fuzzy(want, cand)
}

/// Turn an HTML fragment into plain text: `<br>` to newlines, tags removed,
/// entities decoded.
fn html_to_text(fragment: &str) -> String {
    let br = Regex::new(r"(?i)<br\s*/?>").expect("valid regex");
    let with_nl = br.replace_all(fragment, "\n");
    let tags = Regex::new(r"(?s)<[^>]+>").expect("valid regex");
    let stripped = tags.replace_all(&with_nl, "");
    decode_entities(&stripped)
}

/// Decode the small set of HTML entities that appear in lyrics text.
fn decode_entities(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < s.len() {
        if bytes[i] == b'&' {
            if let Some(semi) = s[i..].find(';').map(|p| i + p) {
                let ent = &s[i + 1..semi];
                let decoded = match ent {
                    "amp" => Some('&'),
                    "lt" => Some('<'),
                    "gt" => Some('>'),
                    "quot" => Some('"'),
                    "apos" | "#39" => Some('\''),
                    "nbsp" => Some(' '),
                    _ => ent
                        .strip_prefix('#')
                        .and_then(|num| {
                            num.strip_prefix(['x', 'X'])
                                .and_then(|h| u32::from_str_radix(h, 16).ok())
                                .or_else(|| num.parse::<u32>().ok())
                        })
                        .and_then(char::from_u32),
                };
                if let Some(c) = decoded {
                    out.push(c);
                    i = semi + 1;
                    continue;
                }
            }
        }
        // Not an entity: copy this char.
        let ch = s[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// Parse LRC text (`[mm:ss.xx] words`) into timestamped lines.
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
    fn normalize_strips_slovak_diacritics() {
        assert_eq!(normalize("Voľnosť"), "volnost");
        assert_eq!(normalize("Pásla Kone"), "pasla kone");
        assert_eq!(normalize("Žltý  kôň!"), "zlty kon");
    }

    #[test]
    fn clean_title_drops_suffixes() {
        assert_eq!(clean_title("Hymna (feat. Rytmus)"), "Hymna");
        assert_eq!(clean_title("Song - Remastered 2011"), "Song");
        assert_eq!(clean_title("Plain"), "Plain");
    }

    #[test]
    fn fuzzy_matches_accents_and_substrings() {
        assert!(title_matches("Voľnosť", "Volnost"));
        assert!(artist_matches("Kontrafakt", "kontrafakt feat. somebody"));
        assert!(!title_matches("Hymna", "Completely Different Song"));
    }

    #[test]
    fn html_to_text_decodes_and_breaks() {
        let frag = "Don&#39;t stop<br/>me &amp; you<br>k&ocirc;&#328;";
        // &ocirc; isn't in our table -> left as-is; numeric ones decode.
        let t = html_to_text(frag);
        assert!(t.contains("Don't stop"));
        assert!(t.contains("me & you"));
        assert!(t.contains('\n'));
        assert!(t.contains('ň')); // &#328; -> ň
    }

    #[test]
    fn parses_synced_lrc_with_fraction_widths() {
        let lrc = "[ar:Artist]\n[00:01.00]first\n[00:12.5]second\n[01:00.250]third";
        let lines = parse_lrc(lrc);
        assert_eq!(lines.len(), 3, "metadata tags must be skipped");
        assert_eq!(lines[0].time_ms, 1_000);
        assert_eq!(lines[1].time_ms, 12_500, "single-digit fraction is tenths");
        assert_eq!(lines[2].time_ms, 60_250);
    }

    #[test]
    fn genius_balances_nested_divs_and_drops_header() {
        let page = concat!(
            r#"<div data-lyrics-container="true">"#,
            r#"<div data-exclude-from-selection="true"><div>5 Contributors</div>Song Lyrics</div>"#,
            r#"First line<br/>Second <b>line</b>"#,
            r#"</div>"#,
            r#"<div data-lyrics-container="true">Chorus line</div>"#,
        );
        let t = extract_genius_lyrics(page);
        assert!(!t.contains("Contributors"), "header leaked:\n{t}");
        assert!(!t.contains("Song Lyrics"), "header leaked:\n{t}");
        assert!(t.contains("First line"), "missing verse:\n{t}");
        assert!(t.contains("Second line"), "tag not stripped:\n{t}");
        assert!(t.contains("Chorus line"), "second container dropped:\n{t}");
    }

    #[test]
    fn plain_trims_blank_edges() {
        let l = Lyrics::plain("X", "\n\n  first\nsecond\n\n").unwrap();
        assert_eq!(l.lines.len(), 2);
        assert_eq!(l.lines[0].text, "first");
        assert!(!l.synced);
    }
}
