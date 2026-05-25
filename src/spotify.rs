//! Async wrapper around the rspotify web API. Every method returns plain
//! [`crate::model`] domain types so the rest of the app never touches rspotify
//! shapes. All methods are cheap to call from spawned tasks (the client is
//! internally `Arc`-shared and auto-refreshes its token).

use anyhow::{Context, Result};
use rspotify::clients::{BaseClient, OAuthClient};
use rspotify::model::{
    AdditionalType, AlbumId, ArtistId, EpisodeId, Market, PlayableId, PlaylistId, SearchResult,
    SearchType, ShowId, TrackId,
};

use rspotify::prelude::Id;
use rspotify::AuthCodePkceSpotify;

use crate::model::{ConnectDevice, PlaylistRef, RemoteState, Track};

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

    fn as_api(self) -> SearchType {
        match self {
            SearchKind::Tracks => SearchType::Track,
            SearchKind::Albums => SearchType::Album,
            SearchKind::Artists => SearchType::Artist,
            SearchKind::Playlists => SearchType::Playlist,
            SearchKind::Episodes => SearchType::Episode,
            SearchKind::Shows => SearchType::Show,
        }
    }
}

/// Thin façade over the authenticated rspotify client.
#[derive(Clone)]
pub struct Spotify {
    pub client: AuthCodePkceSpotify,
}

impl Spotify {
    pub fn new(client: AuthCodePkceSpotify) -> Self {
        Self { client }
    }

    /// Search the catalog for the chosen result kind.
    pub async fn search(&self, query: &str, kind: SearchKind) -> Result<SearchResults> {
        let res = self
            .client
            .search(query, kind.as_api(), Some(Market::FromToken), None, Some(40), None)
            .await
            .context("search request failed")?;

        Ok(match res {
            SearchResult::Tracks(page) => SearchResults::Tracks(
                page.items.into_iter().filter_map(Track::from_full).collect(),
            ),
            SearchResult::Albums(page) => SearchResults::Albums(
                page.items
                    .into_iter()
                    .filter_map(|a| {
                        let id = a.id.as_ref()?;
                        Some(AlbumRef {
                            id: id.id().to_string(),
                            artists: a
                                .artists
                                .iter()
                                .map(|x| x.name.as_str())
                                .collect::<Vec<_>>()
                                .join(", "),
                            name: a.name,
                        })
                    })
                    .collect(),
            ),
            SearchResult::Artists(page) => SearchResults::Artists(
                page.items
                    .into_iter()
                    .map(|a| ArtistRef {
                        id: a.id.id().to_string(),
                        name: a.name,
                    })
                    .collect(),
            ),
            SearchResult::Playlists(page) => SearchResults::Playlists(
                page.items
                    .into_iter()
                    .map(|p| PlaylistRef {
                        id: p.id.id().to_string(),
                        name: p.name,
                        owner: p.owner.display_name.unwrap_or_default(),
                        total: p.tracks.total,
                    })
                    .collect(),
            ),
            SearchResult::Episodes(page) => SearchResults::Episodes(
                page.items
                    .into_iter()
                    .map(|e| EpisodeRef {
                        uri: e.id.uri(),
                        name: e.name,
                        show: String::new(),
                        duration_ms: e.duration.num_milliseconds().max(0) as u32,
                        album_art_url: crate::model::best_image(&e.images),
                    })
                    .collect(),
            ),
            SearchResult::Shows(page) => SearchResults::Shows(
                page.items
                    .into_iter()
                    .map(|s| ShowRef {
                        id: s.id.id().to_string(),
                        name: s.name,
                        publisher: s.publisher,
                    })
                    .collect(),
            ),
        })
    }

    /// The current user's playlists (first 50).
    pub async fn user_playlists(&self) -> Result<Vec<PlaylistRef>> {
        let page = self
            .client
            .current_user_playlists_manual(Some(50), None)
            .await
            .context("fetching playlists failed")?;
        Ok(page
            .items
            .into_iter()
            .map(|p| PlaylistRef {
                id: p.id.id().to_string(),
                name: p.name,
                owner: p.owner.display_name.unwrap_or_default(),
                total: p.tracks.total,
            })
            .collect())
    }

    /// All tracks of a playlist, following pagination.
    pub async fn playlist_tracks(&self, playlist_id: &str) -> Result<Vec<Track>> {
        let id = PlaylistId::from_id_or_uri(playlist_id).context("bad playlist id")?;
        let mut tracks = Vec::new();
        let mut offset = 0u32;
        loop {
            let page = self
                .client
                .playlist_items_manual(id.clone(), None, Some(Market::FromToken), Some(100), Some(offset))
                .await
                .context("fetching playlist items failed")?;
            let got = page.items.len() as u32;
            tracks.extend(
                page.items
                    .into_iter()
                    .filter_map(|item| item.track)
                    .filter_map(Track::from_playable),
            );
            offset += got;
            if got == 0 || page.next.is_none() {
                break;
            }
        }
        Ok(tracks)
    }

    /// The user's "Liked Songs" (first 50).
    pub async fn saved_tracks(&self) -> Result<Vec<Track>> {
        let page = self
            .client
            .current_user_saved_tracks_manual(Some(Market::FromToken), Some(50), None)
            .await
            .context("fetching liked songs failed")?;
        Ok(page
            .items
            .into_iter()
            .filter_map(|s| Track::from_full(s.track))
            .collect())
    }

    /// All tracks of an album.
    pub async fn album_tracks(&self, album_id: &str) -> Result<Vec<Track>> {
        let id = AlbumId::from_id_or_uri(album_id).context("bad album id")?;
        let album = self
            .client
            .album(id, Some(Market::FromToken))
            .await
            .context("fetching album failed")?;
        let cover = crate::model::best_image(&album.images);
        Ok(album
            .tracks
            .items
            .into_iter()
            .filter_map(|t| {
                let uri = t.id.as_ref()?.uri();
                Some(Track {
                    uri,
                    name: t.name,
                    artists: t
                        .artists
                        .iter()
                        .map(|a| a.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", "),
                    album: album.name.clone(),
                    album_art_url: cover.clone(),
                    duration_ms: t.duration.num_milliseconds().max(0) as u32,
                    kind: crate::model::PlayableKind::Track,
                })
            })
            .collect())
    }

    /// An artist's top tracks for the user's market.
    pub async fn artist_top_tracks(&self, artist_id: &str) -> Result<Vec<Track>> {
        let id = ArtistId::from_id_or_uri(artist_id).context("bad artist id")?;
        let tracks = self
            .client
            .artist_top_tracks(id, Some(Market::FromToken))
            .await
            .context("fetching artist top tracks failed")?;
        Ok(tracks.into_iter().filter_map(Track::from_full).collect())
    }

    /// All episodes of a show (podcast), most-recent first as Spotify returns
    /// them, flattened into playable [`Track`]s.
    pub async fn show_episodes(&self, show_id: &str) -> Result<(String, Vec<Track>)> {
        let id = ShowId::from_id_or_uri(show_id).context("bad show id")?;
        let show = self
            .client
            .get_a_show(id.clone(), Some(Market::FromToken))
            .await
            .context("fetching show failed")?;
        let cover = crate::model::best_image(&show.images);
        let show_name = show.name.clone();
        let publisher = show.publisher.clone();

        let mut tracks = Vec::new();
        let mut offset = 0u32;
        loop {
            let page = self
                .client
                .get_shows_episodes_manual(id.clone(), Some(Market::FromToken), Some(50), Some(offset))
                .await
                .context("fetching show episodes failed")?;
            let got = page.items.len() as u32;
            for e in page.items {
                tracks.push(Track {
                    uri: e.id.uri(),
                    name: e.name,
                    artists: show_name.clone(),
                    album: publisher.clone(),
                    album_art_url: crate::model::best_image(&e.images).or_else(|| cover.clone()),
                    duration_ms: e.duration.num_milliseconds().max(0) as u32,
                    kind: crate::model::PlayableKind::Episode,
                });
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

    /// Append a single playable (track or episode) URI to a playlist.
    pub async fn add_to_playlist(&self, playlist_id: &str, item_uri: &str) -> Result<()> {
        let pid = PlaylistId::from_id_or_uri(playlist_id).context("bad playlist id")?;
        let item = playable_from_uri(item_uri)?;
        self.client
            .playlist_add_items(pid, [item], None)
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

    /// Poll the current remote playback context into a [`RemoteState`].
    pub async fn current_playback(&self) -> Result<Option<RemoteState>> {
        let types = [AdditionalType::Track, AdditionalType::Episode];
        let ctx = self
            .client
            .current_playback(Some(Market::FromToken), Some(&types))
            .await
            .context("fetching current playback failed")?;
        Ok(ctx.map(|c| RemoteState {
            track: c.item.and_then(Track::from_playable),
            is_playing: c.is_playing,
            progress_ms: c
                .progress
                .map(|d| d.num_milliseconds().max(0) as u32)
                .unwrap_or(0),
            device_id: c.device.id,
            volume_percent: c.device.volume_percent.map(|v| v.min(100) as u8),
            shuffle: c.shuffle_state,
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
