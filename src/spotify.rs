//! Async wrapper around the rspotify web API. Every method returns plain
//! [`crate::model`] domain types so the rest of the app never touches rspotify
//! shapes. All methods are cheap to call from spawned tasks (the client is
//! internally `Arc`-shared and auto-refreshes its token).

use anyhow::{Context, Result};
use rspotify::clients::{BaseClient, OAuthClient};
use rspotify::model::{AlbumId, ArtistId, Market, PlaylistId, SearchResult, SearchType};
use rspotify::prelude::Id;
use rspotify::AuthCodePkceSpotify;

use crate::model::{PlaylistRef, Track};

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

/// Results of a search, one variant per [`SearchKind`].
#[derive(Debug, Clone)]
pub enum SearchResults {
    Tracks(Vec<Track>),
    Albums(Vec<AlbumRef>),
    Artists(Vec<ArtistRef>),
    Playlists(Vec<PlaylistRef>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchKind {
    Tracks,
    Albums,
    Artists,
    Playlists,
}

impl SearchKind {
    pub const ALL: [SearchKind; 4] = [
        SearchKind::Tracks,
        SearchKind::Albums,
        SearchKind::Artists,
        SearchKind::Playlists,
    ];

    pub fn label(self) -> &'static str {
        match self {
            SearchKind::Tracks => "Tracks",
            SearchKind::Albums => "Albums",
            SearchKind::Artists => "Artists",
            SearchKind::Playlists => "Playlists",
        }
    }

    fn as_api(self) -> SearchType {
        match self {
            SearchKind::Tracks => SearchType::Track,
            SearchKind::Albums => SearchType::Album,
            SearchKind::Artists => SearchType::Artist,
            SearchKind::Playlists => SearchType::Playlist,
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
            _ => SearchResults::Tracks(Vec::new()),
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
}
