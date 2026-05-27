//! Async wrapper around the rspotify web API. Every method returns plain
//! [`crate::model`] domain types so the rest of the app never touches rspotify
//! shapes. All methods are cheap to call from spawned tasks (the client is
//! internally `Arc`-shared and auto-refreshes its token).

use anyhow::{Context, Result};
use rspotify::clients::{BaseClient, OAuthClient};
use rspotify::model::{
    AlbumId, ArtistId, EpisodeId, PlayableId, PlaylistId, ShowId, TrackId,
};

use rspotify::prelude::Id;
use rspotify::AuthCodePkceSpotify;

use crate::model::{ConnectDevice, PlayableKind, PlaylistRef, RemoteState, Track};

/// A non-track search result row that can be "opened" into a track list.
#[derive(Debug, Clone)]
pub struct AlbumRef {
    pub id: String,
    pub name: String,
    pub artists: String,
}

#[derive(Debug, Clone)]
pub struct ArtistRef {
    pub id: String,
    pub name: String,
}

/// A podcast/audiobook episode search row (playable directly).
#[derive(Debug, Clone)]
pub struct EpisodeRef {
    /// `spotify:episode:<id>` URI.
    pub uri: String,
    pub name: String,
    /// Show name when known (search returns simplified episodes without it).
    pub show: String,
    pub duration_ms: u32,
    pub album_art_url: Option<String>,
}

/// A show (podcast) search row that can be opened into its episode list.
#[derive(Debug, Clone)]
pub struct ShowRef {
    pub id: String,
    pub name: String,
    pub publisher: String,
}

/// Results of a search, one variant per [`SearchKind`].
#[derive(Debug, Clone)]
pub enum SearchResults {
    Tracks(Vec<Track>),
    Albums(Vec<AlbumRef>),
    Artists(Vec<ArtistRef>),
    Playlists(Vec<PlaylistRef>),
    Episodes(Vec<EpisodeRef>),
    Shows(Vec<ShowRef>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchKind {
    Tracks,
    Albums,
    Artists,
    Playlists,
    Episodes,
    Shows,
}

impl SearchKind {
    pub const ALL: [SearchKind; 6] = [
        SearchKind::Tracks,
        SearchKind::Albums,
        SearchKind::Artists,
        SearchKind::Playlists,
        SearchKind::Episodes,
        SearchKind::Shows,
    ];

    pub fn label(self) -> &'static str {
        match self {
            SearchKind::Tracks => "Tracks",
            SearchKind::Albums => "Albums",
            SearchKind::Artists => "Artists",
            SearchKind::Playlists => "Playlists",
            SearchKind::Episodes => "Episodes",
            SearchKind::Shows => "Podcasts",
        }
    }

    /// The Spotify `type` query parameter for this search kind. The response
    /// section key is this value plus an `s` (e.g. `track` → `tracks`).
    fn as_param(self) -> &'static str {
        match self {
            SearchKind::Tracks => "track",
            SearchKind::Albums => "album",
            SearchKind::Artists => "artist",
            SearchKind::Playlists => "playlist",
            SearchKind::Episodes => "episode",
            SearchKind::Shows => "show",
        }
    }
}

/// Thin façade over the authenticated rspotify client.
#[derive(Clone)]
pub struct Spotify {
    pub client: AuthCodePkceSpotify,
    /// Raw HTTP client for endpoints whose 2026 API shape rspotify 0.13 can no
    /// longer deserialize (the playlist `tracks` field/endpoint was renamed to
    /// `items`). Reuses the rspotify-managed, auto-refreshing access token.
    http: reqwest::Client,
}

impl Spotify {
    pub fn new(client: AuthCodePkceSpotify) -> Self {
        Self {
            client,
            http: reqwest::Client::new(),
        }
    }

    /// Search the catalog. Spotify's 2026 API caps the search `limit` at 10 per
    /// request and strips fields (e.g. track `popularity`, show `publisher`)
    /// that rspotify 0.13's models require, so results are fetched and parsed via
    /// raw HTTP with lenient structs. A single request (10 results) is used to
    /// stay light on the API; raise `MAX_PAGES` to paginate for more.
    pub async fn search(&self, query: &str, kind: SearchKind) -> Result<SearchResults> {
        const PAGE: u32 = 10; // Spotify development-mode maximum per request
        const MAX_PAGES: u32 = 1;

        let type_param = kind.as_param();
        let key = format!("{type_param}s");
        let mut items: Vec<serde_json::Value> = Vec::new();
        for p in 0..MAX_PAGES {
            let limit = PAGE.to_string();
            let offset = (p * PAGE).to_string();
            let v = self
                .web_get(
                    "search",
                    &[
                        ("q", query),
                        ("type", type_param),
                        ("market", "from_token"),
                        ("limit", &limit),
                        ("offset", &offset),
                    ],
                )
                .await
                .context("search request failed")?;
            let section = v.get(&key);
            let got = section
                .and_then(|s| s.get("items"))
                .and_then(|i| i.as_array())
                .map(|a| {
                    items.extend(a.iter().cloned());
                    a.len()
                })
                .unwrap_or(0);
            let has_next = section
                .and_then(|s| s.get("next"))
                .map(|n| !n.is_null())
                .unwrap_or(false);
            if got < PAGE as usize || !has_next {
                break;
            }
        }
        Ok(build_search_results(kind, items))
    }

    /// Current access token, refreshed via rspotify if it has expired. Lets the
    /// raw-HTTP helpers reuse rspotify's managed token and refresh logic.
    async fn access_token(&self) -> Result<String> {
        self.client
            .auto_reauth()
            .await
            .context("refreshing access token failed")?;
        self.client
            .token
            .lock()
            .await
            .unwrap()
            .as_ref()
            .map(|t| t.access_token.clone())
            .context("no access token available")
    }

    /// Raw authenticated GET against the Web API, returning parsed JSON. Used
    /// for endpoints whose 2026 shape rspotify 0.13 can no longer model.
    async fn web_get(&self, path: &str, query: &[(&str, &str)]) -> Result<serde_json::Value> {
        let token = self.access_token().await?;
        let url = format!("https://api.spotify.com/v1/{path}");
        self.send_with_retry(|| self.http.get(&url).query(query).bearer_auth(&token))
            .await
    }

    /// Raw authenticated POST (JSON body) against the Web API.
    async fn web_post(&self, path: &str, body: &serde_json::Value) -> Result<serde_json::Value> {
        let token = self.access_token().await?;
        let url = format!("https://api.spotify.com/v1/{path}");
        self.send_with_retry(|| self.http.post(&url).bearer_auth(&token).json(body))
            .await
    }

    /// Send a request, retrying on HTTP 429 after the server's `Retry-After`
    /// delay (falling back to exponential backoff). The official client id is
    /// shared across many librespot apps and rate-limited aggressively, so brief
    /// limits self-heal instead of surfacing as errors. These calls run in
    /// background tasks, so a few seconds of backoff doesn't block the UI.
    async fn send_with_retry(
        &self,
        build: impl Fn() -> reqwest::RequestBuilder,
    ) -> Result<serde_json::Value> {
        const MAX_RETRIES: u32 = 4;
        let mut attempt = 0;
        loop {
            let resp = build().send().await.context("HTTP request failed")?;
            if resp.status() == reqwest::StatusCode::TOO_MANY_REQUESTS && attempt < MAX_RETRIES {
                // Honor Retry-After when present; otherwise back off 1,2,4,8s.
                let wait = resp
                    .headers()
                    .get(reqwest::header::RETRY_AFTER)
                    .and_then(|h| h.to_str().ok())
                    .and_then(|s| s.parse::<u64>().ok())
                    .unwrap_or(1u64 << attempt)
                    .clamp(1, 15);
                attempt += 1;
                tracing::warn!("rate limited (429); retry {attempt}/{MAX_RETRIES} after {wait}s");
                tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
                continue;
            }
            return json_or_err(resp).await;
        }
    }

    /// The current user's playlists (first 50). Parsed via raw HTTP: Spotify's
    /// 2026 API renamed the playlist object's `tracks` field to `items`, which
    /// rspotify 0.13 requires and so fails to deserialize.
    pub async fn user_playlists(&self) -> Result<Vec<PlaylistRef>> {
        let v = self
            .web_get("me/playlists", &[("limit", "50")])
            .await
            .context("fetching playlists failed")?;
        let page: PlaylistPage =
            serde_json::from_value(v).context("parsing playlists")?;
        Ok(page.items.into_iter().flatten().map(playlist_ref).collect())
    }

    /// All tracks of a playlist, following pagination. Uses the 2026 `items`
    /// endpoint (`/playlists/{id}/tracks` now returns 403) via raw HTTP.
    pub async fn playlist_tracks(&self, playlist_id: &str) -> Result<Vec<Track>> {
        let id = PlaylistId::from_id_or_uri(playlist_id).context("bad playlist id")?;
        let id = id.id().to_string();
        let mut tracks = Vec::new();
        let mut offset = 0u32;
        loop {
            let off = offset.to_string();
            let v = self
                .web_get(
                    &format!("playlists/{id}/items"),
                    &[("market", "from_token"), ("limit", "100"), ("offset", &off)],
                )
                .await
                .context("fetching playlist items failed")?;
            let page: ItemsPage =
                serde_json::from_value(v).context("parsing playlist items")?;
            let got = page.items.len() as u32;
            tracks.extend(
                page.items
                    .into_iter()
                    .flatten()
                    .filter_map(|item| item.track)
                    .filter_map(raw_to_track),
            );
            offset += got;
            if got == 0 || page.next.is_none() {
                break;
            }
        }
        Ok(tracks)
    }

    /// The user's "Liked Songs" (first 50). Raw HTTP: the 2026 API strips
    /// fields rspotify's track model requires.
    pub async fn saved_tracks(&self) -> Result<Vec<Track>> {
        let v = self
            .web_get("me/tracks", &[("market", "from_token"), ("limit", "50")])
            .await
            .context("fetching liked songs failed")?;
        let page: ItemsPage = serde_json::from_value(v).context("parsing liked songs")?;
        Ok(page
            .items
            .into_iter()
            .flatten()
            .filter_map(|item| item.track)
            .filter_map(raw_to_track)
            .collect())
    }

    /// All tracks of an album, following pagination. Raw HTTP: the 2026 album
    /// object drops fields (popularity/genres/…) that rspotify's `FullAlbum`
    /// requires.
    pub async fn album_tracks(&self, album_id: &str) -> Result<Vec<Track>> {
        let id = AlbumId::from_id_or_uri(album_id).context("bad album id")?;
        let id = id.id().to_string();
        // Album object for name + cover art.
        let v = self
            .web_get(&format!("albums/{id}"), &[("market", "from_token")])
            .await
            .context("fetching album failed")?;
        let album: RawAlbumFull = serde_json::from_value(v).context("parsing album")?;
        let cover = best_image_raw(&album.images);
        let album_name = album.name;

        let mut tracks = Vec::new();
        let mut offset = 0u32;
        loop {
            let off = offset.to_string();
            let v = self
                .web_get(
                    &format!("albums/{id}/tracks"),
                    &[("market", "from_token"), ("limit", "50"), ("offset", &off)],
                )
                .await
                .context("fetching album tracks failed")?;
            let page: AlbumTracksPage =
                serde_json::from_value(v).context("parsing album tracks")?;
            let got = page.items.len() as u32;
            tracks.extend(
                page.items
                    .into_iter()
                    .filter_map(|t| album_track(t, &album_name, &cover)),
            );
            offset += got;
            if got == 0 || page.next.is_none() {
                break;
            }
        }
        Ok(tracks)
    }

    /// An artist's albums and singles (their discography), newest first as
    /// Spotify returns them, de-duplicated by title. `/artists/{id}/top-tracks`
    /// is 403 for non-extended apps in 2026, but `/albums` still works, so the
    /// artist view browses albums (open one to see its tracks).
    pub async fn artist_albums(&self, artist_id: &str) -> Result<Vec<AlbumRef>> {
        let id = ArtistId::from_id_or_uri(artist_id).context("bad artist id")?;
        let v = self
            .web_get(
                &format!("artists/{}/albums", id.id()),
                &[
                    ("market", "from_token"),
                    ("include_groups", "album,single"),
                    ("limit", "50"),
                ],
            )
            .await
            .context("fetching artist albums failed")?;
        #[derive(serde::Deserialize)]
        struct Page {
            #[serde(default)]
            items: Vec<RawAlbumSearch>,
        }
        let page: Page = serde_json::from_value(v).context("parsing artist albums")?;
        let mut seen = std::collections::HashSet::new();
        Ok(page
            .items
            .into_iter()
            .map(album_ref)
            .filter(|a| seen.insert(a.name.to_lowercase()))
            .collect())
    }

    /// All episodes of a show (podcast), most-recent first as Spotify returns
    /// them, flattened into playable [`Track`]s.
    pub async fn show_episodes(&self, show_id: &str) -> Result<(String, Vec<Track>)> {
        let id = ShowId::from_id_or_uri(show_id).context("bad show id")?;
        let id = id.id().to_string();
        let v = self
            .web_get(&format!("shows/{id}"), &[("market", "from_token")])
            .await
            .context("fetching show failed")?;
        let show: RawShowFull = serde_json::from_value(v).context("parsing show")?;
        let cover = best_image_raw(&show.images);
        let show_name = show.name;
        let publisher = show.publisher;

        let mut tracks = Vec::new();
        let mut offset = 0u32;
        loop {
            let off = offset.to_string();
            let v = self
                .web_get(
                    &format!("shows/{id}/episodes"),
                    &[("market", "from_token"), ("limit", "50"), ("offset", &off)],
                )
                .await
                .context("fetching show episodes failed")?;
            let page: EpisodesPage =
                serde_json::from_value(v).context("parsing show episodes")?;
            let got = page.items.len() as u32;
            for e in page.items.into_iter().flatten() {
                if let Some(uri) = e.uri {
                    tracks.push(Track {
                        uri,
                        name: e.name,
                        artists: show_name.clone(),
                        album: publisher.clone(),
                        album_art_url: best_image_raw(&e.images).or_else(|| cover.clone()),
                        duration_ms: e.duration_ms,
                        kind: PlayableKind::Episode,
                    });
                }
            }
            offset += got;
            if got == 0 || page.next.is_none() {
                break;
            }
        }
        Ok((show_name, tracks))
    }

    // ---- Library writes ----------------------------------------------------

    /// Save a track to the user's "Liked Songs".
    pub async fn like_track(&self, track_id: &str) -> Result<()> {
        let id = TrackId::from_id_or_uri(track_id).context("bad track id")?;
        self.client
            .current_user_saved_tracks_add([id])
            .await
            .context("liking track failed")
    }

    /// Remove a track from the user's "Liked Songs".
    pub async fn unlike_track(&self, track_id: &str) -> Result<()> {
        let id = TrackId::from_id_or_uri(track_id).context("bad track id")?;
        self.client
            .current_user_saved_tracks_delete([id])
            .await
            .context("unliking track failed")
    }

    /// For each given track id, whether it is in the user's "Liked Songs".
    pub async fn tracks_saved(&self, track_ids: &[String]) -> Result<Vec<bool>> {
        let ids = track_ids
            .iter()
            .map(|s| TrackId::from_id_or_uri(s).map_err(anyhow::Error::from))
            .collect::<Result<Vec<_>>>()?;
        self.client
            .current_user_saved_tracks_contains(ids)
            .await
            .context("checking saved tracks failed")
    }

    /// Append a single playable (track or episode) URI to a playlist. Posts to
    /// the 2026 `items` endpoint (the old `/tracks` resource now returns 403).
    pub async fn add_to_playlist(&self, playlist_id: &str, item_uri: &str) -> Result<()> {
        let pid = PlaylistId::from_id_or_uri(playlist_id).context("bad playlist id")?;
        let pid = pid.id().to_string();
        // Validate the URI shape before sending it.
        let _ = playable_from_uri(item_uri)?;
        self.web_post(
            &format!("playlists/{pid}/items"),
            &serde_json::json!({ "uris": [item_uri] }),
        )
        .await
        .context("adding to playlist failed")?;
        Ok(())
    }

    /// Create a new private playlist owned by the current user.
    pub async fn create_playlist(&self, name: &str) -> Result<()> {
        let me = self.client.me().await.context("fetching current user failed")?;
        self.client
            .user_playlist_create(me.id, name, Some(false), Some(false), None)
            .await
            .context("creating playlist failed")?;
        Ok(())
    }

    /// Rename an existing playlist.
    pub async fn rename_playlist(&self, playlist_id: &str, name: &str) -> Result<()> {
        let pid = PlaylistId::from_id_or_uri(playlist_id).context("bad playlist id")?;
        self.client
            .playlist_change_detail(pid, Some(name), None, None, None)
            .await
            .context("renaming playlist failed")?;
        Ok(())
    }

    /// Unfollow (remove from library) a playlist.
    pub async fn unfollow_playlist(&self, playlist_id: &str) -> Result<()> {
        let pid = PlaylistId::from_id_or_uri(playlist_id).context("bad playlist id")?;
        self.client
            .playlist_unfollow(pid)
            .await
            .context("unfollowing playlist failed")
    }

    // ---- Spotify Connect (remote control) ----------------------------------

    /// List the user's available Connect devices.
    pub async fn connect_devices(&self) -> Result<Vec<ConnectDevice>> {
        let devices = self.client.device().await.context("listing devices failed")?;
        Ok(devices
            .into_iter()
            .map(|d| ConnectDevice {
                id: d.id,
                name: d.name,
                kind: format!("{:?}", d._type),
                is_active: d.is_active,
                volume_percent: d.volume_percent.map(|v| v.min(100) as u8),
            })
            .collect())
    }

    /// Transfer playback to a Connect device (and optionally start playing).
    pub async fn transfer_playback(&self, device_id: &str, play: bool) -> Result<()> {
        self.client
            .transfer_playback(device_id, Some(play))
            .await
            .context("transferring playback failed")
    }

    /// Poll the current remote playback context into a [`RemoteState`]. Raw
    /// HTTP: the played `item` is a track/episode whose 2026 shape rspotify
    /// can't deserialize. Returns `None` when nothing is playing (HTTP 204).
    pub async fn current_playback(&self) -> Result<Option<RemoteState>> {
        let v = self
            .web_get(
                "me/player",
                &[("market", "from_token"), ("additional_types", "track,episode")],
            )
            .await
            .context("fetching current playback failed")?;
        if v.is_null() {
            return Ok(None);
        }
        let ctx: RawPlayback = serde_json::from_value(v).context("parsing playback")?;
        Ok(Some(RemoteState {
            track: ctx.item.and_then(raw_to_track),
            is_playing: ctx.is_playing,
            progress_ms: ctx.progress_ms.unwrap_or(0),
            device_id: ctx.device.as_ref().and_then(|d| d.id.clone()),
            volume_percent: ctx
                .device
                .as_ref()
                .and_then(|d| d.volume_percent)
                .map(|v| (v as u8).min(100)),
            shuffle: ctx.shuffle_state,
        }))
    }

    /// Resume remote playback on the given device.
    pub async fn remote_resume(&self, device_id: &str) -> Result<()> {
        self.client
            .resume_playback(Some(device_id), None)
            .await
            .context("remote resume failed")
    }

    /// Pause remote playback on the given device.
    pub async fn remote_pause(&self, device_id: &str) -> Result<()> {
        self.client
            .pause_playback(Some(device_id))
            .await
            .context("remote pause failed")
    }

    pub async fn remote_next(&self, device_id: &str) -> Result<()> {
        self.client
            .next_track(Some(device_id))
            .await
            .context("remote next failed")
    }

    pub async fn remote_previous(&self, device_id: &str) -> Result<()> {
        self.client
            .previous_track(Some(device_id))
            .await
            .context("remote previous failed")
    }

    pub async fn remote_seek(&self, position_ms: u32, device_id: &str) -> Result<()> {
        self.client
            .seek_track(chrono::Duration::milliseconds(position_ms as i64), Some(device_id))
            .await
            .context("remote seek failed")
    }

    pub async fn remote_volume(&self, percent: u8, device_id: &str) -> Result<()> {
        self.client
            .volume(percent.min(100), Some(device_id))
            .await
            .context("remote volume failed")
    }
}

/// Build a [`PlayableId`] from a `spotify:track:…` or `spotify:episode:…` URI.
fn playable_from_uri(uri: &str) -> Result<PlayableId<'static>> {
    if uri.starts_with("spotify:episode:") {
        let id = EpisodeId::from_id_or_uri(uri).context("bad episode uri")?;
        Ok(PlayableId::Episode(id.into_static()))
    } else {
        let id = TrackId::from_id_or_uri(uri).context("bad track uri")?;
        Ok(PlayableId::Track(id.into_static()))
    }
}

// ---- Search-result plumbing -------------------------------------------------

/// Parse a page of raw search-result JSON items into our domain
/// [`SearchResults`], skipping anything that fails to deserialize.
fn build_search_results(kind: SearchKind, items: Vec<serde_json::Value>) -> SearchResults {
    fn parse<T: serde::de::DeserializeOwned>(items: Vec<serde_json::Value>) -> Vec<T> {
        items
            .into_iter()
            .filter_map(|v| serde_json::from_value(v).ok())
            .collect()
    }
    match kind {
        SearchKind::Tracks => SearchResults::Tracks(
            parse::<RawPlayable>(items)
                .into_iter()
                .filter_map(raw_to_track)
                .collect(),
        ),
        SearchKind::Albums => {
            SearchResults::Albums(parse::<RawAlbumSearch>(items).into_iter().map(album_ref).collect())
        }
        SearchKind::Artists => SearchResults::Artists(
            parse::<RawArtistObj>(items).into_iter().map(artist_ref).collect(),
        ),
        SearchKind::Playlists => SearchResults::Playlists(
            parse::<RawPlaylist>(items).into_iter().map(playlist_ref).collect(),
        ),
        SearchKind::Episodes => SearchResults::Episodes(
            parse::<RawEpisode>(items).into_iter().map(episode_ref).collect(),
        ),
        SearchKind::Shows => {
            SearchResults::Shows(parse::<RawShowSearch>(items).into_iter().map(show_ref).collect())
        }
    }
}

fn album_ref(a: RawAlbumSearch) -> AlbumRef {
    AlbumRef {
        id: a.id,
        name: a.name,
        artists: a
            .artists
            .iter()
            .map(|x| x.name.as_str())
            .collect::<Vec<_>>()
            .join(", "),
    }
}

fn artist_ref(a: RawArtistObj) -> ArtistRef {
    ArtistRef { id: a.id, name: a.name }
}

fn show_ref(s: RawShowSearch) -> ShowRef {
    ShowRef {
        id: s.id,
        name: s.name,
        publisher: s.publisher,
    }
}

fn episode_ref(e: RawEpisode) -> EpisodeRef {
    EpisodeRef {
        uri: e.uri.unwrap_or_default(),
        name: e.name,
        show: String::new(),
        duration_ms: e.duration_ms,
        album_art_url: best_image_raw(&e.images),
    }
}

// ---- Raw-HTTP plumbing for endpoints rspotify 0.13 can't model --------------
//
// Spotify's 2026 API renamed the playlist `tracks` field/endpoint to `items`
// and shrank the surface available to development-mode apps. These tolerant
// structs read the new (and, where relevant, old) shapes.

/// Convert a non-2xx response into a friendly error; otherwise parse the JSON
/// body. A 403 is reported as a development-mode restriction (Spotify deprecated
/// a range of endpoints for dev apps in 2026).
async fn json_or_err(resp: reqwest::Response) -> Result<serde_json::Value> {
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        anyhow::bail!("Spotify rate limit reached — wait a few seconds and try again");
    }
    if status == reqwest::StatusCode::FORBIDDEN {
        anyhow::bail!("unavailable: Spotify restricts this for development-mode apps (403)");
    }
    if !status.is_success() {
        let snippet: String = body.chars().take(200).collect();
        anyhow::bail!("Spotify API error {status}: {snippet}");
    }
    if body.trim().is_empty() {
        return Ok(serde_json::Value::Null);
    }
    serde_json::from_str(&body).context("parsing Spotify response")
}

/// A page of playlists (`/me/playlists` or a `playlist` search), tolerating
/// `null` entries that Spotify now returns for restricted/editorial playlists.
#[derive(serde::Deserialize, Default)]
struct PlaylistPage {
    #[serde(default)]
    items: Vec<Option<RawPlaylist>>,
}

#[derive(serde::Deserialize)]
struct RawPlaylist {
    id: String,
    name: String,
    #[serde(default)]
    owner: Option<RawOwner>,
    // 2026 shape uses `items`; older clients/responses used `tracks`.
    #[serde(default)]
    items: Option<RawCount>,
    #[serde(default)]
    tracks: Option<RawCount>,
}

#[derive(serde::Deserialize)]
struct RawOwner {
    #[serde(default)]
    display_name: Option<String>,
}

#[derive(serde::Deserialize)]
struct RawCount {
    #[serde(default)]
    total: u32,
}

fn playlist_ref(p: RawPlaylist) -> PlaylistRef {
    PlaylistRef {
        id: p.id,
        name: p.name,
        owner: p.owner.and_then(|o| o.display_name).unwrap_or_default(),
        total: p.items.or(p.tracks).map(|c| c.total).unwrap_or(0),
    }
}

/// A page of playlist items from `/playlists/{id}/items`.
#[derive(serde::Deserialize)]
struct ItemsPage {
    #[serde(default)]
    items: Vec<Option<PlaylistItem>>,
    #[serde(default)]
    next: Option<String>,
}

#[derive(serde::Deserialize)]
struct PlaylistItem {
    #[serde(default)]
    track: Option<RawPlayable>,
}

/// A playable object (track or episode) parsed leniently from raw JSON.
#[derive(serde::Deserialize)]
struct RawPlayable {
    #[serde(rename = "type", default)]
    kind: Option<String>,
    #[serde(default)]
    uri: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    artists: Vec<RawNamed>,
    #[serde(default)]
    album: Option<RawAlbumObj>,
    #[serde(default)]
    duration_ms: Option<u32>,
    #[serde(default)]
    images: Vec<RawImage>,
    #[serde(default)]
    show: Option<RawShowObj>,
}

#[derive(serde::Deserialize)]
struct RawNamed {
    name: String,
}

#[derive(serde::Deserialize)]
struct RawAlbumObj {
    name: String,
    #[serde(default)]
    images: Vec<RawImage>,
}

#[derive(serde::Deserialize)]
struct RawShowObj {
    name: String,
    #[serde(default)]
    publisher: String,
    #[serde(default)]
    images: Vec<RawImage>,
}

#[derive(serde::Deserialize)]
struct RawImage {
    url: String,
    #[serde(default)]
    width: Option<u32>,
}

/// Album image closest to ~300px wide (mirrors [`crate::model::best_image`]).
fn best_image_raw(images: &[RawImage]) -> Option<String> {
    images
        .iter()
        .min_by_key(|i| (i.width.unwrap_or(0) as i64 - 300).abs())
        .map(|i| i.url.clone())
}

/// Convert a raw playable into a domain [`Track`], skipping items without a URI.
fn raw_to_track(p: RawPlayable) -> Option<Track> {
    let uri = p.uri?;
    let name = p.name.unwrap_or_default();
    let duration_ms = p.duration_ms.unwrap_or(0);
    if p.kind.as_deref() == Some("episode") {
        let show_name = p.show.as_ref().map(|s| s.name.clone()).unwrap_or_default();
        let publisher = p.show.as_ref().map(|s| s.publisher.clone()).unwrap_or_default();
        let art = best_image_raw(&p.images)
            .or_else(|| p.show.as_ref().and_then(|s| best_image_raw(&s.images)));
        Some(Track {
            uri,
            name,
            artists: show_name,
            album: publisher,
            album_art_url: art,
            duration_ms,
            kind: PlayableKind::Episode,
        })
    } else {
        let artists = p
            .artists
            .iter()
            .map(|a| a.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        let album = p.album.as_ref().map(|a| a.name.clone()).unwrap_or_default();
        let art = p.album.as_ref().and_then(|a| best_image_raw(&a.images));
        Some(Track {
            uri,
            name,
            artists,
            album,
            album_art_url: art,
            duration_ms,
            kind: PlayableKind::Track,
        })
    }
}

// ---- Search result objects --------------------------------------------------

#[derive(serde::Deserialize)]
struct RawAlbumSearch {
    id: String,
    name: String,
    #[serde(default)]
    artists: Vec<RawNamed>,
}

#[derive(serde::Deserialize)]
struct RawArtistObj {
    id: String,
    name: String,
}

#[derive(serde::Deserialize)]
struct RawShowSearch {
    id: String,
    name: String,
    #[serde(default)]
    publisher: String,
}

/// A simplified episode (from search or a show's episode list): no `show`.
#[derive(serde::Deserialize)]
struct RawEpisode {
    #[serde(default)]
    uri: Option<String>,
    #[serde(default)]
    name: String,
    #[serde(default)]
    duration_ms: u32,
    #[serde(default)]
    images: Vec<RawImage>,
}

// ---- Album / show / playback objects ----------------------------------------

/// Full album object: we need only the name + cover; tracks are paginated
/// separately via `/albums/{id}/tracks`.
#[derive(serde::Deserialize)]
struct RawAlbumFull {
    name: String,
    #[serde(default)]
    images: Vec<RawImage>,
}

#[derive(serde::Deserialize)]
struct AlbumTracksPage {
    #[serde(default)]
    items: Vec<RawAlbumTrack>,
    #[serde(default)]
    next: Option<String>,
}

#[derive(serde::Deserialize)]
struct RawAlbumTrack {
    #[serde(default)]
    uri: Option<String>,
    #[serde(default)]
    name: String,
    #[serde(default)]
    artists: Vec<RawNamed>,
    #[serde(default)]
    duration_ms: u32,
}

fn album_track(t: RawAlbumTrack, album: &str, cover: &Option<String>) -> Option<Track> {
    let uri = t.uri?;
    Some(Track {
        uri,
        name: t.name,
        artists: t
            .artists
            .iter()
            .map(|a| a.name.as_str())
            .collect::<Vec<_>>()
            .join(", "),
        album: album.to_string(),
        album_art_url: cover.clone(),
        duration_ms: t.duration_ms,
        kind: PlayableKind::Track,
    })
}

/// Full show object: name + publisher + cover.
#[derive(serde::Deserialize)]
struct RawShowFull {
    name: String,
    #[serde(default)]
    publisher: String,
    #[serde(default)]
    images: Vec<RawImage>,
}

#[derive(serde::Deserialize)]
struct EpisodesPage {
    #[serde(default)]
    items: Vec<Option<RawEpisode>>,
    #[serde(default)]
    next: Option<String>,
}

/// Current playback context from `/me/player`.
#[derive(serde::Deserialize)]
struct RawPlayback {
    #[serde(default)]
    is_playing: bool,
    #[serde(default)]
    progress_ms: Option<u32>,
    #[serde(default)]
    item: Option<RawPlayable>,
    #[serde(default)]
    device: Option<RawDevice>,
    #[serde(default)]
    shuffle_state: bool,
}

#[derive(serde::Deserialize)]
struct RawDevice {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    volume_percent: Option<u32>,
}
