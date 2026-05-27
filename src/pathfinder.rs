//! Spotify's private "pathfinder" GraphQL API.
//!
//! This is the only source of the real Home screen: **Daily Mix 1–6**,
//! **Discover Weekly**, **Release Radar** and the **genre/mood shelves**. It
//! needs a web-player access token (see [`crate::webtoken`]) — not the dev-app
//! token, which is RBAC-denied. Notably it does *not* need a client-token; a
//! valid bearer token plus the `app-platform: WebPlayer` + Origin/Referer
//! headers is enough.
//!
//! The persisted-query hashes rotate, so the current `home` hash is pulled from
//! the community-maintained Meld registry, with the registry's `previous_hash`
//! and a known-good baked-in hash as fallbacks. Mirrors the request shape used
//! by github.com/FrancescoGrazioso/Meld (`Spotify.kt`).

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

use crate::webtoken::WebToken;

const GQL_URL: &str = "https://api-partner.spotify.com/pathfinder/v2/query";
const HASH_URL: &str =
    "https://raw.githubusercontent.com/FrancescoGrazioso/Meld/main/docs/spotify-gql-hashes.json";
/// Used only if the registry can't be reached (verified 2026-05-27).
const FALLBACK_HOME_HASH: &str =
    "40c1423fc26ea0d68cd8f212e79ca47df7968fc40d83d184e756af54fd043143";
const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
    (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";

/// A horizontal shelf on the Home screen (e.g. "Daily Mix", "Pop", "Focus").
#[derive(Debug, Clone)]
pub struct Shelf {
    pub title: String,
    pub items: Vec<ShelfItem>,
}

/// One playable/openable card within a shelf.
#[derive(Debug, Clone)]
pub struct ShelfItem {
    /// `spotify:playlist:…`, `spotify:album:…` or `spotify:artist:…`.
    pub uri: String,
    pub name: String,
    /// Secondary line (playlist description, album artist, …).
    pub subtitle: String,
}

/// Fetch the user's real Home shelves via pathfinder. Returns an error if no
/// token can be minted or the API refuses — the caller should fall back to the
/// public shelves.
pub async fn home_shelves(token: &WebToken) -> Result<Vec<Shelf>> {
    let access = token
        .access_token()
        .await
        .context("no web-player token (sp_dc cookie missing/expired)")?;
    let http = reqwest::Client::new();
    let (hash, prev) = fetch_hashes(&http).await;
    let tz = local_timezone();

    // Try the current hash, then the registry's previous hash if the persisted
    // query was rejected (hash rotated since the registry last updated).
    let mut last_err = None;
    let candidates: Vec<String> = std::iter::once(hash).chain(prev).collect();
    for h in candidates {
        match query_home(&http, &access, &h, &tz).await {
            Ok(v) => return parse_home(&v),
            Err(e) => {
                tracing::warn!("pathfinder home hash {h} failed: {e:#}");
                last_err = Some(e);
            }
        }
    }
    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("no usable home hash")))
}

/// Returns `(primary_hash, previous_hash)` from the Meld registry, falling back
/// to the baked-in hash when the registry is unreachable.
async fn fetch_hashes(http: &reqwest::Client) -> (String, Option<String>) {
    let fetch = async {
        let v: Value = http
            .get(HASH_URL)
            .header("User-Agent", UA)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        anyhow::Ok(v)
    };
    match fetch.await {
        Ok(v) => {
            let op = v.pointer("/operations/home");
            let hash = op
                .and_then(|o| o.get("hash"))
                .and_then(|h| h.as_str())
                .unwrap_or(FALLBACK_HOME_HASH)
                .to_string();
            let prev = op
                .and_then(|o| o.get("previous_hash"))
                .and_then(|h| h.as_str())
                .map(str::to_string);
            (hash, prev)
        }
        Err(e) => {
            tracing::warn!("home hash registry unreachable, using baked-in hash: {e:#}");
            (FALLBACK_HOME_HASH.to_string(), None)
        }
    }
}

async fn query_home(
    http: &reqwest::Client,
    access: &str,
    hash: &str,
    tz: &str,
) -> Result<Value> {
    let body = json!({
        "operationName": "home",
        "variables": {
            "homeEndUserIntegration": "INTEGRATION_WEB_PLAYER",
            "timeZone": tz,
            "sp_t": "",
            "facet": "",
            "sectionItemsLimit": 10,
            "includeEpisodeContentRatingsV2": false
        },
        "extensions": {
            "persistedQuery": { "version": 1, "sha256Hash": hash }
        }
    });

    let resp = http
        .post(GQL_URL)
        .header("Authorization", format!("Bearer {access}"))
        .header("app-platform", "WebPlayer")
        .header("Origin", "https://open.spotify.com")
        .header("Referer", "https://open.spotify.com/")
        .header("Accept", "application/json")
        .header("User-Agent", UA)
        .json(&body)
        .send()
        .await
        .context("pathfinder request failed")?;

    let status = resp.status();
    let v: Value = resp.json().await.context("parsing pathfinder JSON")?;

    // pathfinder returns HTTP 200 with an `errors` array for things like
    // PersistedQueryNotFound (rotated hash) — treat those as failures so the
    // previous-hash fallback kicks in.
    if let Some(msg) = v
        .get("errors")
        .and_then(|e| e.as_array())
        .and_then(|a| a.first())
        .and_then(|e| e.get("message"))
        .and_then(|m| m.as_str())
    {
        bail!("graphql error: {msg}");
    }
    if !status.is_success() {
        bail!("pathfinder HTTP {status}");
    }
    Ok(v)
}

fn parse_home(v: &Value) -> Result<Vec<Shelf>> {
    let sections = v
        .pointer("/data/home/sectionContainer/sections/items")
        .and_then(|x| x.as_array())
        .context("home response has no sections")?;

    let mut out = Vec::new();
    for sec in sections {
        let title = sec
            .pointer("/data/title")
            .and_then(|t| {
                t.get("transformedLabel")
                    .or_else(|| t.get("translatedBaseText"))
                    .or_else(|| t.get("text"))
            })
            .and_then(|s| s.as_str())
            .unwrap_or("")
            .to_string();

        let Some(items_v) = sec.pointer("/sectionItems/items").and_then(|x| x.as_array()) else {
            continue;
        };
        let items: Vec<ShelfItem> = items_v.iter().filter_map(parse_item).collect();
        if items.is_empty() {
            continue;
        }
        out.push(Shelf {
            title: if title.is_empty() {
                "More".to_string()
            } else {
                title
            },
            items,
        });
    }

    if out.is_empty() {
        bail!("home response contained no usable shelves");
    }
    Ok(out)
}

fn parse_item(it: &Value) -> Option<ShelfItem> {
    let content = it.get("content")?;
    let typename = content.get("__typename")?.as_str()?;
    let data = content.get("data")?;
    let uri = data.get("uri")?.as_str()?.to_string();
    if uri.is_empty() {
        return None;
    }

    let (name, subtitle) = match typename {
        "PlaylistResponseWrapper" => (
            str_field(data, "name"),
            str_field(data, "description"),
        ),
        "AlbumResponseWrapper" => (
            str_field(data, "name"),
            data.pointer("/artists/items/0/profile/name")
                .and_then(|s| s.as_str())
                .unwrap_or("")
                .to_string(),
        ),
        "ArtistResponseWrapper" => (
            data.pointer("/profile/name")
                .and_then(|s| s.as_str())
                .unwrap_or("")
                .to_string(),
            "Artist".to_string(),
        ),
        _ => return None,
    };

    let name = clean_text(&name);
    if name.is_empty() {
        return None;
    }
    Some(ShelfItem {
        uri,
        name,
        subtitle: clean_text(&subtitle),
    })
}

/// Editorial playlist descriptions come back as HTML (anchor tags, entities,
/// newlines). Strip tags, decode common entities, and collapse whitespace so a
/// subtitle is plain one-line text.
fn clean_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    let out = out
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ");
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn str_field(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(|s| s.as_str())
        .unwrap_or("")
        .to_string()
}

/// Best-effort IANA timezone name (drives only the time-of-day greeting). Falls
/// back to UTC; the shelves don't depend on it.
fn local_timezone() -> String {
    if let Ok(tz) = std::env::var("TZ") {
        if !tz.trim().is_empty() {
            return tz.trim().to_string();
        }
    }
    if let Ok(tz) = std::fs::read_to_string("/etc/timezone") {
        let tz = tz.trim();
        if !tz.is_empty() {
            return tz.to_string();
        }
    }
    if let Ok(target) = std::fs::read_link("/etc/localtime") {
        if let Some(idx) = target.to_string_lossy().find("zoneinfo/") {
            return target.to_string_lossy()[idx + "zoneinfo/".len()..].to_string();
        }
    }
    "UTC".to_string()
}
