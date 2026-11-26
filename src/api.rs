//! A small Spotify Web API client with only the endpoints irie needs.
//!
//! Models are deliberately lenient (`#[serde(default)]` everywhere) because
//! Spotify regularly drops or renames fields.

use crate::auth::Auth;
use anyhow::{Result, anyhow, bail};
use reqwest::{Method, StatusCode};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Duration;

const BASE: &str = "https://api.spotify.com/v1";

#[derive(Clone)]
pub struct Api {
    http: reqwest::Client,
    auth: Arc<Auth>,
}

// ---------- models ----------

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Followers {
    pub total: u64,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct User {
    pub id: String,
    pub display_name: Option<String>,
    pub email: Option<String>,
    pub country: Option<String>,
    pub product: Option<String>,
    pub followers: Option<Followers>,
    pub uri: String,
}

impl User {
    pub fn name(&self) -> &str {
        self.display_name
            .as_deref()
            .filter(|s| !s.is_empty())
            .unwrap_or(&self.id)
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Named {
    pub name: String,
    pub uri: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Artist {
    pub name: String,
    pub uri: String,
    pub genres: Vec<String>,
    pub followers: Option<Followers>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Album {
    pub name: String,
    pub uri: String,
    pub artists: Vec<Named>,
    pub release_date: Option<String>,
    pub total_tracks: Option<u32>,
}

/// A track or a podcast episode.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Track {
    pub name: String,
    pub uri: String,
    pub duration_ms: u64,
    pub explicit: bool,
    pub artists: Vec<Named>,
    pub album: Option<Album>,
    /// Set for podcast episodes.
    pub show: Option<Named>,
    pub is_local: bool,
}

impl Track {
    pub fn artist_line(&self) -> String {
        if !self.artists.is_empty() {
            self.artists
                .iter()
                .map(|a| a.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        } else if let Some(show) = &self.show {
            show.name.clone()
        } else {
            String::new()
        }
    }

    pub fn album_name(&self) -> &str {
        self.album
            .as_ref()
            .map(|a| a.name.as_str())
            .unwrap_or_default()
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Total {
    pub total: u64,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Playlist {
    pub id: String,
    pub name: String,
    pub uri: String,
    pub description: Option<String>,
    pub owner: Option<User>,
    pub tracks: Option<Total>,
    pub items: Option<Total>,
}

impl Playlist {
    pub fn total(&self) -> u64 {
        self.items
            .as_ref()
            .or(self.tracks.as_ref())
            .map(|t| t.total)
            .unwrap_or(0)
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Paging<T> {
    #[serde(default = "Vec::new")]
    pub items: Vec<Option<T>>,
    #[serde(default)]
    pub next: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Device {
    pub id: Option<String>,
    pub name: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub is_active: bool,
    pub is_restricted: bool,
    pub volume_percent: Option<u32>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Context {
    pub uri: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct PlaybackState {
    pub device: Option<Device>,
    pub shuffle_state: bool,
    pub repeat_state: String,
    pub is_playing: bool,
    pub progress_ms: Option<u64>,
    pub item: Option<Track>,
    pub context: Option<Context>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Queue {
    pub currently_playing: Option<Track>,
    pub queue: Vec<Track>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct SavedItem {
    track: Option<Track>,
    item: Option<Track>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct SearchResults {
    pub tracks: Option<Paging<Track>>,
    pub artists: Option<Paging<Artist>>,
    pub albums: Option<Paging<Album>>,
    pub playlists: Option<Paging<Playlist>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeRange {
    Short,
    Medium,
    Long,
}

impl TimeRange {
    pub fn param(self) -> &'static str {
        match self {
            Self::Short => "short_term",
            Self::Medium => "medium_term",
            Self::Long => "long_term",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Short => "last 4 weeks",
            Self::Medium => "last 6 months",
            Self::Long => "all time",
        }
    }
    pub fn next(self) -> Self {
        match self {
            Self::Short => Self::Medium,
            Self::Medium => Self::Long,
            Self::Long => Self::Short,
        }
    }
}

pub fn flatten<T>(p: Option<Paging<T>>) -> Vec<T> {
    p.map(|p| p.items.into_iter().flatten().collect())
        .unwrap_or_default()
}

fn enc(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

// ---------- client ----------

impl Api {
    pub fn new(auth: Arc<Auth>) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .build()
            .expect("http client");
        Self { http, auth }
    }

    pub fn auth(&self) -> &Arc<Auth> {
        &self.auth
    }

    /// Send a request; returns `None` for empty (204) responses.
    async fn send(&self, method: Method, path: &str, body: Option<Value>) -> Result<Option<Value>> {
        let url = if path.starts_with("http") {
            path.to_string()
        } else {
            format!("{BASE}{path}")
        };
        let mut retried_auth = false;
        for _ in 0..4 {
            let token = self.auth.access_token().await?;
            let mut req = self.http.request(method.clone(), &url).bearer_auth(token);
            req = match &body {
                Some(b) => req.json(b),
                None if method != Method::GET => req.header("Content-Length", "0"),
                None => req,
            };
            let resp = req.send().await?;
            let status = resp.status();
            match status {
                StatusCode::TOO_MANY_REQUESTS => {
                    let wait = resp
                        .headers()
                        .get("retry-after")
                        .and_then(|v| v.to_str().ok())
                        .and_then(|v| v.parse::<u64>().ok())
                        .unwrap_or(1)
                        .min(10);
                    tokio::time::sleep(Duration::from_secs(wait)).await;
                    continue;
                }
                StatusCode::UNAUTHORIZED if !retried_auth => {
                    retried_auth = true;
                    self.auth.invalidate().await;
                    continue;
                }
                _ => {}
            }
            let bytes = resp.bytes().await?;
            if !status.is_success() {
                let msg = serde_json::from_slice::<Value>(&bytes)
                    .ok()
                    .and_then(|v| v["error"]["message"].as_str().map(String::from))
                    .unwrap_or_else(|| String::from_utf8_lossy(&bytes).chars().take(200).collect());
                bail!(ApiError {
                    status: status.as_u16(),
                    message: msg
                });
            }
            if bytes.is_empty() {
                return Ok(None);
            }
            // Some player endpoints return a non-JSON snapshot id; ignore those.
            return Ok(serde_json::from_slice(&bytes).ok());
        }
        Err(anyhow!(
            "Spotify is rate limiting requests, try again shortly"
        ))
    }

    async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        let v = self
            .send(Method::GET, path, None)
            .await?
            .unwrap_or(Value::Null);
        Ok(serde_json::from_value(v)?)
    }

    async fn get_opt<T: DeserializeOwned>(&self, path: &str) -> Result<Option<T>> {
        match self.send(Method::GET, path, None).await? {
            None | Some(Value::Null) => Ok(None),
            Some(v) => Ok(Some(serde_json::from_value(v)?)),
        }
    }

    /// Follow `next` links up to `cap` items.
    async fn all_pages<T: DeserializeOwned>(&self, first: &str, cap: usize) -> Result<Vec<T>> {
        let mut out = Vec::new();
        let mut next = Some(first.to_string());
        while let Some(url) = next {
            let page: Paging<T> = self.get(&url).await?;
            out.extend(page.items.into_iter().flatten());
            next = page.next;
            if out.len() >= cap {
                break;
            }
        }
        Ok(out)
    }

    // ----- profile -----

    pub async fn me(&self) -> Result<User> {
        self.get("/me").await
    }

    pub async fn top_artists(&self, range: TimeRange) -> Result<Vec<Artist>> {
        let p: Paging<Artist> = self
            .get(&format!(
                "/me/top/artists?limit=20&time_range={}",
                range.param()
            ))
            .await?;
        Ok(flatten(Some(p)))
    }

    pub async fn top_tracks(&self, range: TimeRange) -> Result<Vec<Track>> {
        let p: Paging<Track> = self
            .get(&format!(
                "/me/top/tracks?limit=20&time_range={}",
                range.param()
            ))
            .await?;
        Ok(flatten(Some(p)))
    }

    pub async fn recently_played(&self) -> Result<Vec<Track>> {
        let p: Paging<SavedItem> = self.get("/me/player/recently-played?limit=20").await?;
        Ok(p.items
            .into_iter()
            .flatten()
            .filter_map(|i| i.track.or(i.item))
            .collect())
    }

    // ----- library -----

    pub async fn playlists(&self) -> Result<Vec<Playlist>> {
        self.all_pages("/me/playlists?limit=50", 1000).await
    }

    pub async fn liked_tracks(&self) -> Result<Vec<Track>> {
        let items: Vec<SavedItem> = self.all_pages("/me/tracks?limit=50", 500).await?;
        Ok(items
            .into_iter()
            .filter_map(|i| i.track.or(i.item))
            .collect())
    }

    pub async fn playlist_tracks(&self, id: &str) -> Result<Vec<Track>> {
        // Spotify renamed `/tracks` to `/items`; try the new one first.
        let items: Vec<SavedItem> = match self
            .all_pages(&format!("/playlists/{id}/items?limit=50"), 1000)
            .await
        {
            Ok(v) => v,
            Err(e) if is_status(&e, 404) || is_status(&e, 403) => {
                self.all_pages(&format!("/playlists/{id}/tracks?limit=50"), 1000)
                    .await?
            }
            Err(e) => return Err(e),
        };
        Ok(items
            .into_iter()
            .filter_map(|i| i.item.or(i.track))
            .filter(|t| !t.uri.is_empty())
            .collect())
    }

    pub async fn search(&self, q: &str) -> Result<SearchResults> {
        self.get(&format!(
            "/search?q={}&type=track,artist,album,playlist&limit=10",
            enc(q)
        ))
        .await
    }

    pub async fn search_tracks(&self, q: &str) -> Result<Vec<Track>> {
        let r: SearchResults = self
            .get(&format!("/search?q={}&type=track&limit=10", enc(q)))
            .await?;
        Ok(flatten(r.tracks))
    }

    // ----- player -----

    pub async fn playback(&self) -> Result<Option<PlaybackState>> {
        self.get_opt("/me/player?additional_types=track,episode")
            .await
    }

    pub async fn devices(&self) -> Result<Vec<Device>> {
        #[derive(Deserialize)]
        struct D {
            devices: Vec<Device>,
        }
        Ok(self.get::<D>("/me/player/devices").await?.devices)
    }

    pub async fn queue(&self) -> Result<Queue> {
        Ok(self.get_opt("/me/player/queue").await?.unwrap_or_default())
    }

    pub async fn add_to_queue(&self, uri: &str, device: Option<&str>) -> Result<()> {
        let mut path = format!("/me/player/queue?uri={}", enc(uri));
        if let Some(d) = device {
            path.push_str(&format!("&device_id={d}"));
        }
        self.send(Method::POST, &path, None).await.map(|_| ())
    }

    pub async fn transfer(&self, device_id: &str, play: bool) -> Result<()> {
        self.send(
            Method::PUT,
            "/me/player",
            Some(json!({ "device_ids": [device_id], "play": play })),
        )
        .await
        .map(|_| ())
    }

    fn with_device(path: &str, device: Option<&str>) -> String {
        match device {
            Some(d) => format!(
                "{path}{}device_id={d}",
                if path.contains('?') { "&" } else { "?" }
            ),
            None => path.to_string(),
        }
    }

    /// Play a context (album / playlist / artist), optionally starting at a track.
    pub async fn play_context(
        &self,
        device: Option<&str>,
        context_uri: &str,
        start_uri: Option<&str>,
    ) -> Result<()> {
        let mut body = json!({ "context_uri": context_uri });
        if let Some(u) = start_uri {
            body["offset"] = json!({ "uri": u });
        }
        self.send(
            Method::PUT,
            &Self::with_device("/me/player/play", device),
            Some(body),
        )
        .await
        .map(|_| ())
    }

    /// Play an explicit list of tracks, starting at `offset`.
    pub async fn play_uris(
        &self,
        device: Option<&str>,
        uris: &[String],
        offset: usize,
    ) -> Result<()> {
        // The API caps the list; keep a window around the chosen track.
        let start = offset.saturating_sub(50);
        let window: Vec<&String> = uris.iter().skip(start).take(100).collect();
        let body = json!({ "uris": window, "offset": { "position": offset - start } });
        self.send(
            Method::PUT,
            &Self::with_device("/me/player/play", device),
            Some(body),
        )
        .await
        .map(|_| ())
    }

    pub async fn resume(&self, device: Option<&str>) -> Result<()> {
        self.send(
            Method::PUT,
            &Self::with_device("/me/player/play", device),
            None,
        )
        .await
        .map(|_| ())
    }

    pub async fn pause(&self) -> Result<()> {
        self.send(Method::PUT, "/me/player/pause", None)
            .await
            .map(|_| ())
    }

    pub async fn next(&self) -> Result<()> {
        self.send(Method::POST, "/me/player/next", None)
            .await
            .map(|_| ())
    }

    pub async fn previous(&self) -> Result<()> {
        self.send(Method::POST, "/me/player/previous", None)
            .await
            .map(|_| ())
    }

    pub async fn seek(&self, ms: u64) -> Result<()> {
        self.send(
            Method::PUT,
            &format!("/me/player/seek?position_ms={ms}"),
            None,
        )
        .await
        .map(|_| ())
    }

    pub async fn volume(&self, pct: u8) -> Result<()> {
        self.send(
            Method::PUT,
            &format!("/me/player/volume?volume_percent={pct}"),
            None,
        )
        .await
        .map(|_| ())
    }

    pub async fn shuffle(&self, on: bool) -> Result<()> {
        self.send(Method::PUT, &format!("/me/player/shuffle?state={on}"), None)
            .await
            .map(|_| ())
    }

    pub async fn repeat(&self, state: &str) -> Result<()> {
        self.send(
            Method::PUT,
            &format!("/me/player/repeat?state={state}"),
            None,
        )
        .await
        .map(|_| ())
    }
}

#[derive(Debug)]
pub struct ApiError {
    pub status: u16,
    pub message: String,
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Spotify API {}: {}", self.status, self.message)
    }
}

impl std::error::Error for ApiError {}

pub fn is_status(e: &anyhow::Error, status: u16) -> bool {
    e.downcast_ref::<ApiError>()
        .is_some_and(|a| a.status == status)
}

pub fn fmt_ms(ms: u64) -> String {
    let s = ms / 1000;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}
