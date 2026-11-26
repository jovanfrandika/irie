//! TUI application state, input handling and background work.

use crate::api::{
    Album, Api, Artist, Device, PlaybackState, Playlist, Queue, TimeRange, Track, User,
};
use crate::engine::Engine;
use crate::jam::{self, Jam, JamSession};
use crate::visualizer::{self, Analyzer, FFT_SIZE};
use crate::webplayer::{SpeakerEvent, WebSpeaker};
use anyhow::Result;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use librespot_playback::player::PlayerEvent;
use ratatui::widgets::TableState;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc::UnboundedSender;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Now,
    Library,
    Search,
    Queue,
    Profile,
    Jam,
}

impl Tab {
    pub const ALL: [Tab; 6] = [
        Tab::Now,
        Tab::Library,
        Tab::Search,
        Tab::Queue,
        Tab::Profile,
        Tab::Jam,
    ];
    pub fn title(self) -> &'static str {
        match self {
            Tab::Now => "Now Playing",
            Tab::Library => "Library",
            Tab::Search => "Search",
            Tab::Queue => "Queue",
            Tab::Profile => "Profile",
            Tab::Jam => "Jam",
        }
    }
    fn index(self) -> usize {
        Self::ALL.iter().position(|t| *t == self).unwrap_or(0)
    }
}

#[derive(Debug, Clone)]
pub enum Item {
    Liked,
    Playlist(Playlist),
    Track(Track),
    Album(Album),
    Artist(Artist),
}

/// How to start playback from a track list.
#[derive(Debug, Clone)]
pub enum PlayCtx {
    /// A playlist or album URI; tracks play in that context.
    Context(String),
    /// A plain list of tracks.
    Uris,
}

pub struct TrackView {
    pub title: String,
    pub ctx: PlayCtx,
    pub tracks: Vec<Track>,
    pub state: TableState,
    pub loading: bool,
}

pub enum Msg {
    Playback(Option<PlaybackState>),
    Me(User),
    Playlists(Vec<Playlist>),
    Tracks {
        title: String,
        tracks: Vec<Track>,
    },
    Search(Vec<Item>),
    Queue(Queue),
    Top {
        artists: Vec<Artist>,
        tracks: Vec<Track>,
        recent: Vec<Track>,
    },
    Devices(Vec<Device>),
    Jam(Option<JamSession>),
    Player(PlayerEvent),
    RefreshSoon,
    Queued,
    Info(String),
    Error(String),
    EngineDown(String),
    Speaker(SpeakerEvent),
    JamReady(Arc<Jam>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Prompt {
    JamLink,
}

pub struct App {
    pub api: Api,
    pub engine: Option<Arc<Engine>>,
    pub speaker: Option<Arc<WebSpeaker>>,
    pub jam_client: Option<Arc<Jam>>,
    pub device_id: Option<String>,
    pub device_name: String,
    tx: UnboundedSender<Msg>,
    pub running: bool,
    pub tab: Tab,
    pub help: bool,

    pub playback: Option<PlaybackState>,
    playback_at: Instant,
    pending_seek: Option<(u64, Instant)>,

    pub me: Option<User>,
    pub top_range: TimeRange,
    pub top_artists: Vec<Artist>,
    pub top_tracks: Vec<Track>,
    pub recent: Vec<Track>,
    pub profile_state: TableState,

    pub library: Vec<Item>,
    pub library_state: TableState,
    pub tracks_view: Option<TrackView>,

    pub search_input: String,
    pub search_editing: bool,
    pub search_results: Vec<Item>,
    pub search_state: TableState,
    pub searching: bool,

    pub queue: Option<Queue>,
    pub queue_state: TableState,

    pub jam: Option<JamSession>,
    pub jam_busy: bool,
    pub prompt: Option<(Prompt, String)>,

    pub devices: Option<(Vec<Device>, TableState)>,

    pub status: Option<(String, bool, Instant)>,
    pub analyzer: Analyzer,
    pub viz_mode: visualizer::Mode,
    pub engine_error: Option<String>,
    pub viz_bands: usize,
    autoconnected: bool,
    autoconnect: bool,
    playback_loaded: bool,
    /// Why the web player's audio can't be visualized, if it can't.
    pub capture_problem: Option<String>,
}

fn sel(state: &mut TableState, len: usize, delta: isize) {
    if len == 0 {
        state.select(None);
        return;
    }
    let cur = state.selected().unwrap_or(0) as isize;
    state.select(Some((cur + delta).clamp(0, len as isize - 1) as usize));
}

impl App {
    pub fn new(
        api: Api,
        engine: Option<Arc<Engine>>,
        speaker: Option<Arc<WebSpeaker>>,
        device_name: String,
        autoconnect: bool,
        tx: UnboundedSender<Msg>,
    ) -> Self {
        let device_id = engine.as_ref().map(|e| e.device_id.clone());
        let jam_client = engine
            .as_ref()
            .map(|e| Arc::new(Jam::new(e.session.clone())));
        Self {
            api,
            engine,
            speaker,
            jam_client,
            device_id,
            device_name,
            tx,
            running: true,
            tab: Tab::Now,
            help: false,
            playback: None,
            playback_at: Instant::now(),
            pending_seek: None,
            me: None,
            top_range: TimeRange::Short,
            top_artists: vec![],
            top_tracks: vec![],
            recent: vec![],
            profile_state: TableState::default(),
            library: vec![Item::Liked],
            library_state: TableState::default().with_selected(Some(0)),
            tracks_view: None,
            search_input: String::new(),
            search_editing: false,
            search_results: vec![],
            search_state: TableState::default(),
            searching: false,
            queue: None,
            queue_state: TableState::default(),
            jam: None,
            jam_busy: false,
            prompt: None,
            devices: None,
            status: None,
            analyzer: Analyzer::default(),
            viz_mode: visualizer::Mode::Bars,
            engine_error: None,
            viz_bands: 48,
            autoconnected: false,
            autoconnect,
            playback_loaded: false,
            capture_problem: None,
        }
    }

    // ---------- background work ----------

    fn spawn<F>(&self, fut: F)
    where
        F: Future<Output = Result<Msg>> + Send + 'static,
    {
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let msg = match fut.await {
                Ok(m) => m,
                Err(e) => Msg::Error(format!("{e:#}")),
            };
            let _ = tx.send(msg);
        });
    }

    /// Run a player command, then refresh the playback state.
    fn command<F>(&self, label: &'static str, fut: F)
    where
        F: Future<Output = Result<()>> + Send + 'static,
    {
        self.spawn(async move {
            fut.await.map_err(|e| anyhow::anyhow!("{label}: {e:#}"))?;
            tokio::time::sleep(Duration::from_millis(350)).await;
            Ok(Msg::RefreshSoon)
        });
    }

    pub fn startup(&mut self) {
        let api = self.api.clone();
        self.spawn(async move { Ok(Msg::Me(api.me().await?)) });
        self.load_playlists();
        self.refresh_playback();
        self.load_top();
        self.refresh_jam();
    }

    pub fn refresh_playback(&self) {
        let api = self.api.clone();
        self.spawn(async move { Ok(Msg::Playback(api.playback().await?)) });
    }

    fn load_playlists(&self) {
        let api = self.api.clone();
        self.spawn(async move { Ok(Msg::Playlists(api.playlists().await?)) });
    }

    fn load_top(&self) {
        let api = self.api.clone();
        let range = self.top_range;
        self.spawn(async move {
            let (artists, tracks, recent) = tokio::join!(
                api.top_artists(range),
                api.top_tracks(range),
                api.recently_played()
            );
            Ok(Msg::Top {
                artists: artists?,
                tracks: tracks?,
                recent: recent.unwrap_or_default(),
            })
        });
    }

    fn load_queue(&self) {
        let api = self.api.clone();
        self.spawn(async move { Ok(Msg::Queue(api.queue().await?)) });
    }

    fn refresh_jam(&self) {
        if let Some(j) = self.jam_client.clone() {
            self.spawn(async move { Ok(Msg::Jam(j.current().await?)) });
        }
    }

    fn info(&mut self, s: impl Into<String>) {
        self.status = Some((s.into(), false, Instant::now()));
    }

    fn error(&mut self, s: impl Into<String>) {
        self.status = Some((s.into(), true, Instant::now()));
    }

    // ---------- playback helpers ----------

    pub fn is_local(&self) -> bool {
        match (&self.playback, &self.device_id) {
            (Some(pb), Some(id)) => {
                pb.device.as_ref().and_then(|d| d.id.as_deref()) == Some(id.as_str())
            }
            _ => false,
        }
    }

    /// Device to target when starting new playback.
    fn target_device(&self) -> Option<String> {
        let active = self
            .playback
            .as_ref()
            .and_then(|p| p.device.as_ref())
            .filter(|d| d.is_active);
        match active {
            Some(d) => d.id.clone(),
            None => self.device_id.clone(),
        }
    }

    pub fn progress_ms(&self) -> u64 {
        if let Some((ms, at)) = self.pending_seek
            && at.elapsed() < Duration::from_secs(2)
        {
            return ms;
        }
        let Some(pb) = &self.playback else { return 0 };
        let base = pb.progress_ms.unwrap_or(0);
        let dur = pb.item.as_ref().map(|t| t.duration_ms).unwrap_or(u64::MAX);
        if pb.is_playing {
            (base + self.playback_at.elapsed().as_millis() as u64).min(dur)
        } else {
            base
        }
    }

    pub fn volume_pct(&self) -> Option<u8> {
        if self.is_local()
            && let Some(e) = &self.engine
        {
            return Some(e.volume_pct());
        }
        self.playback
            .as_ref()?
            .device
            .as_ref()?
            .volume_percent
            .map(|v| v as u8)
    }

    fn toggle_play(&mut self) {
        let playing = self.playback.as_ref().is_some_and(|p| p.is_playing);
        let progress = self.progress_ms();
        if let Some(pb) = &mut self.playback {
            pb.progress_ms = Some(progress);
            pb.is_playing = !playing;
            self.playback_at = Instant::now();
        }
        if self.is_local()
            && let Some(e) = &self.engine
        {
            let _ = e.spirc.play_pause();
            return;
        }
        if self.speaker_cmd("toggle", serde_json::Value::Null) {
            return;
        }
        let api = self.api.clone();
        if self.playback.is_none() {
            // Nothing active anywhere: wake up our own device.
            if let Some(id) = self.device_id.clone() {
                self.command("play", async move { api.transfer(&id, true).await });
            }
        } else if playing {
            self.command("pause", async move { api.pause().await });
        } else {
            let dev = self.target_device();
            self.command("play", async move { api.resume(dev.as_deref()).await });
        }
    }

    fn skip(&mut self, forward: bool) {
        if self.is_local()
            && let Some(e) = &self.engine
        {
            let _ = if forward {
                e.spirc.next()
            } else {
                e.spirc.prev()
            };
            return;
        }
        if self.speaker_cmd(
            if forward { "next" } else { "prev" },
            serde_json::Value::Null,
        ) {
            return;
        }
        let api = self.api.clone();
        if forward {
            self.command("next", async move { api.next().await });
        } else {
            self.command("previous", async move { api.previous().await });
        }
    }

    fn seek_by(&mut self, delta_ms: i64) {
        let Some(pb) = &self.playback else { return };
        let dur = pb.item.as_ref().map(|t| t.duration_ms).unwrap_or(0);
        let pos = (self.progress_ms() as i64 + delta_ms).clamp(0, dur as i64) as u64;
        self.pending_seek = Some((pos, Instant::now()));
        if self.is_local()
            && let Some(e) = &self.engine
        {
            let _ = e.spirc.set_position_ms(pos as u32);
            return;
        }
        if self.speaker_cmd("seek", serde_json::json!(pos)) {
            return;
        }
        let api = self.api.clone();
        self.command("seek", async move { api.seek(pos).await });
    }

    fn change_volume(&mut self, delta: i16) {
        let cur = self.volume_pct().unwrap_or(50) as i16;
        let v = (cur + delta).clamp(0, 100) as u8;
        if self.is_local()
            && let Some(e) = &self.engine
        {
            e.set_volume_pct(v);
            return;
        }
        if let Some(d) = self.playback.as_mut().and_then(|p| p.device.as_mut()) {
            d.volume_percent = Some(v as u32);
        }
        let api = self.api.clone();
        self.command("volume", async move { api.volume(v).await });
    }

    fn toggle_shuffle(&mut self) {
        let Some(pb) = &mut self.playback else { return };
        pb.shuffle_state = !pb.shuffle_state;
        let on = pb.shuffle_state;
        self.info(if on { "Shuffle on" } else { "Shuffle off" });
        if self.is_local()
            && let Some(e) = &self.engine
        {
            let _ = e.spirc.shuffle(on);
            return;
        }
        let api = self.api.clone();
        self.command("shuffle", async move { api.shuffle(on).await });
    }

    fn cycle_repeat(&mut self) {
        let Some(pb) = &mut self.playback else { return };
        let next = match pb.repeat_state.as_str() {
            "off" => "context",
            "context" => "track",
            _ => "off",
        };
        pb.repeat_state = next.to_string();
        self.info(format!("Repeat: {next}"));
        if self.is_local()
            && let Some(e) = &self.engine
        {
            let _ = e.spirc.repeat(next != "off");
            let _ = e.spirc.repeat_track(next == "track");
            return;
        }
        let api = self.api.clone();
        self.command("repeat", async move { api.repeat(next).await });
    }

    fn play_tracks(&mut self, ctx: PlayCtx, tracks: &[Track], idx: usize) {
        let Some(track) = tracks.get(idx) else { return };
        let dev = self.target_device();
        let api = self.api.clone();
        self.info(format!("Playing {}", track.name));
        match ctx {
            PlayCtx::Context(uri) => {
                let start = track.uri.clone();
                self.command("play", async move {
                    api.play_context(dev.as_deref(), &uri, Some(&start)).await
                });
            }
            PlayCtx::Uris => {
                let uris: Vec<String> = tracks.iter().map(|t| t.uri.clone()).collect();
                self.command("play", async move {
                    api.play_uris(dev.as_deref(), &uris, idx).await
                });
            }
        }
    }

    fn play_track_in_album(&mut self, t: &Track) {
        let dev = self.target_device();
        let api = self.api.clone();
        let uri = t.uri.clone();
        let album = t
            .album
            .as_ref()
            .map(|a| a.uri.clone())
            .filter(|u| !u.is_empty());
        self.info(format!("Playing {}", t.name));
        self.command("play", async move {
            match album {
                Some(a) => api.play_context(dev.as_deref(), &a, Some(&uri)).await,
                None => api.play_uris(dev.as_deref(), &[uri], 0).await,
            }
        });
    }

    fn play_context(&mut self, uri: String, name: &str) {
        let dev = self.target_device();
        let api = self.api.clone();
        self.info(format!("Playing {name}"));
        self.command("play", async move {
            api.play_context(dev.as_deref(), &uri, None).await
        });
    }

    fn enqueue(&mut self, t: &Track) {
        let api = self.api.clone();
        let uri = t.uri.clone();
        let tx = self.tx.clone();
        self.info(format!("Queued {}", t.name));
        self.spawn(async move {
            api.add_to_queue(&uri, None).await?;
            let _ = tx.send(Msg::RefreshSoon);
            Ok(Msg::Queued)
        });
    }

    fn open_devices(&mut self) {
        let api = self.api.clone();
        self.spawn(async move { Ok(Msg::Devices(api.devices().await?)) });
    }

    fn open_item(&mut self, item: Item) {
        match item {
            Item::Liked => {
                self.tracks_view = Some(TrackView {
                    title: "Liked Songs".into(),
                    ctx: PlayCtx::Uris,
                    tracks: vec![],
                    state: TableState::default(),
                    loading: true,
                });
                let api = self.api.clone();
                self.spawn(async move {
                    Ok(Msg::Tracks {
                        title: "Liked Songs".into(),
                        tracks: api.liked_tracks().await?,
                    })
                });
            }
            Item::Playlist(p) => {
                self.tracks_view = Some(TrackView {
                    title: p.name.clone(),
                    ctx: PlayCtx::Context(p.uri.clone()),
                    tracks: vec![],
                    state: TableState::default(),
                    loading: true,
                });
                let api = self.api.clone();
                self.spawn(async move {
                    Ok(Msg::Tracks {
                        title: p.name.clone(),
                        tracks: api.playlist_tracks(&p.id).await?,
                    })
                });
            }
            Item::Track(t) => self.play_track_in_album(&t),
            Item::Album(a) => self.play_context(a.uri.clone(), &a.name),
            Item::Artist(a) => self.play_context(a.uri.clone(), &a.name),
        }
    }

    fn run_search(&mut self) {
        let q = self.search_input.trim().to_string();
        if q.is_empty() {
            return;
        }
        self.searching = true;
        let api = self.api.clone();
        self.spawn(async move {
            let r = api.search(&q).await?;
            use crate::api::flatten as take;
            let mut items: Vec<Item> = take(r.tracks).into_iter().map(Item::Track).collect();
            items.extend(take(r.artists).into_iter().take(5).map(Item::Artist));
            items.extend(take(r.albums).into_iter().take(5).map(Item::Album));
            items.extend(take(r.playlists).into_iter().take(5).map(Item::Playlist));
            Ok(Msg::Search(items))
        });
    }

    // ---------- jam ----------

    fn jam_start(&mut self) {
        let Some(j) = self.jam_client.clone() else {
            return self.error("Still connecting to Spotify for Jam, try again in a moment");
        };
        self.jam_busy = true;
        // Hosting a Jam needs something playing; make sure it's on irie.
        let dev = self.target_device();
        self.spawn(async move { Ok(Msg::Jam(Some(j.start(dev.as_deref()).await?))) });
    }

    fn jam_join(&mut self, link: String) {
        let Some(j) = self.jam_client.clone() else {
            return self.error("Still connecting to Spotify for Jam, try again in a moment");
        };
        self.jam_busy = true;
        self.spawn(async move { Ok(Msg::Jam(Some(j.join(&link).await?))) });
    }

    fn jam_leave(&mut self) {
        let (Some(j), Some(s)) = (self.jam_client.clone(), self.jam.clone()) else {
            return;
        };
        self.jam_busy = true;
        self.spawn(async move {
            j.leave(&s).await?;
            Ok(Msg::Jam(None))
        });
    }

    /// Once we know both the playback state and our device ID, take over
    /// playback if nothing else is playing.
    fn maybe_autoconnect(&mut self) {
        if !self.autoconnect || self.autoconnected || !self.playback_loaded {
            return;
        }
        let Some(id) = self.device_id.clone() else {
            return;
        };
        self.autoconnected = true;
        let idle = self.playback.as_ref().is_none_or(|p| !p.is_playing);
        if !idle {
            return;
        }
        let api = self.api.clone();
        // The device needs a moment to show up in Spotify's device list.
        self.spawn(async move {
            for _ in 0..6 {
                tokio::time::sleep(Duration::from_millis(800)).await;
                if api.transfer(&id, false).await.is_ok() {
                    tokio::time::sleep(Duration::from_millis(400)).await;
                    return Ok(Msg::RefreshSoon);
                }
            }
            Ok(Msg::Info("Press d to pick a device".into()))
        });
    }

    /// Send a command straight to the local web player when it's the active
    /// device (instant, no round trip through Spotify). Returns false otherwise.
    fn speaker_cmd(&self, cmd: &'static str, arg: serde_json::Value) -> bool {
        let Some(sp) = self.speaker.clone().filter(|_| self.is_local()) else {
            return false;
        };
        let tx = self.tx.clone();
        tokio::spawn(async move {
            sp.command(cmd, arg).await;
            tokio::time::sleep(Duration::from_millis(500)).await;
            let _ = tx.send(Msg::RefreshSoon);
        });
        true
    }

    /// True when audio comes from irie but there's no PCM to analyse.
    /// Returns a reason when the web player plays locally but its audio
    /// can't be captured for the visualizer.
    pub fn visualizer_unavailable(&self) -> Option<&str> {
        if self.engine.is_some() || !self.is_local() {
            return None;
        }
        self.speaker.as_ref()?;
        self.capture_problem.as_deref()
    }

    // ---------- events ----------

    pub fn on_msg(&mut self, msg: Msg) {
        match msg {
            Msg::Playback(pb) => {
                self.playback = pb;
                self.playback_at = Instant::now();
                self.pending_seek = None;
                self.playback_loaded = true;
                self.maybe_autoconnect();
            }
            Msg::Me(u) => self.me = Some(u),
            Msg::Playlists(ps) => {
                self.library = std::iter::once(Item::Liked)
                    .chain(ps.into_iter().map(Item::Playlist))
                    .collect();
                sel(&mut self.library_state, self.library.len(), 0);
            }
            Msg::Tracks { title, tracks } => {
                if let Some(v) = &mut self.tracks_view
                    && v.title == title
                {
                    v.tracks = tracks;
                    v.loading = false;
                    sel(&mut v.state, v.tracks.len(), 0);
                }
            }
            Msg::Search(items) => {
                self.searching = false;
                self.search_results = items;
                self.search_state = TableState::default();
                sel(&mut self.search_state, self.search_results.len(), 0);
            }
            Msg::Queue(q) => {
                let len = q.queue.len();
                self.queue = Some(q);
                sel(&mut self.queue_state, len, 0);
            }
            Msg::Top {
                artists,
                tracks,
                recent,
            } => {
                self.top_artists = artists;
                self.top_tracks = tracks;
                self.recent = recent;
                sel(&mut self.profile_state, self.top_tracks.len(), 0);
            }
            Msg::Devices(ds) => {
                let mut st = TableState::default();
                st.select(ds.iter().position(|d| d.is_active).or(Some(0)));
                if ds.is_empty() {
                    self.error("No devices found");
                } else {
                    self.devices = Some((ds, st));
                }
            }
            Msg::Jam(s) => {
                self.jam_busy = false;
                if s.is_some() && self.jam.is_none() {
                    self.info("Jam is live, press y to copy the invite link");
                }
                self.jam = s;
            }
            Msg::Player(ev) => match ev {
                PlayerEvent::Playing { .. }
                | PlayerEvent::Paused { .. }
                | PlayerEvent::TrackChanged { .. }
                | PlayerEvent::Seeked { .. }
                | PlayerEvent::Stopped { .. }
                | PlayerEvent::VolumeChanged { .. } => {
                    let tx = self.tx.clone();
                    tokio::spawn(async move {
                        tokio::time::sleep(Duration::from_millis(400)).await;
                        let _ = tx.send(Msg::RefreshSoon);
                    });
                }
                _ => {}
            },
            Msg::RefreshSoon => {
                self.refresh_playback();
                if self.tab == Tab::Queue {
                    self.load_queue();
                }
            }
            Msg::Queued => {}
            Msg::Info(s) => self.info(s),
            Msg::Error(s) => {
                self.jam_busy = false;
                self.searching = false;
                if let Some(v) = &mut self.tracks_view {
                    v.loading = false;
                }
                self.error(s);
            }
            Msg::Speaker(ev) => match ev {
                SpeakerEvent::Ready(id) => {
                    self.device_id = Some(id);
                    self.engine_error = None;
                    self.info(format!("{} is ready to play", self.device_name));
                    self.maybe_autoconnect();
                    let tx = self.tx.clone();
                    tokio::spawn(async move {
                        tokio::time::sleep(Duration::from_millis(600)).await;
                        let _ = tx.send(Msg::RefreshSoon);
                    });
                }
                SpeakerEvent::NotReady => self.error("Web player went offline; reconnecting…"),
                SpeakerEvent::Changed => {
                    let tx = self.tx.clone();
                    tokio::spawn(async move {
                        tokio::time::sleep(Duration::from_millis(300)).await;
                        let _ = tx.send(Msg::RefreshSoon);
                    });
                }
                SpeakerEvent::Error(e) => self.error(e),
                SpeakerEvent::Capture(problem) => self.capture_problem = problem,
                SpeakerEvent::Exited => {
                    self.engine_error = Some("web player stopped".into());
                    self.device_id = None;
                    self.error("The web player stopped; restart irie to play here");
                }
            },
            Msg::JamReady(j) => {
                self.jam_client = Some(j);
                if self.tab == Tab::Jam {
                    self.refresh_jam();
                }
            }
            Msg::EngineDown(s) => {
                self.engine_error = Some(s.clone());
                self.error(s);
            }
        }
    }

    pub fn on_tick(&mut self, bands: usize) {
        let tap = match (&self.engine, &self.speaker) {
            (Some(e), _) => Some(e.tap.clone()),
            (None, Some(sp)) => Some(sp.tap.clone()),
            _ => None,
        };
        let has_audio = match tap {
            Some(t) => {
                self.analyzer.sample_rate = t.rate();
                t.snapshot(&mut self.analyzer.samples[..FFT_SIZE])
            }
            None => false,
        };
        self.analyzer.update(bands, has_audio);
        if let Some((_, _, at)) = &self.status
            && at.elapsed() > Duration::from_secs(5)
        {
            self.status = None;
        }
        // Track finished but no event yet (remote device): refresh.
        if let Some(pb) = &self.playback
            && pb.is_playing
            && let Some(t) = &pb.item
            && self.progress_ms() >= t.duration_ms
            && self.playback_at.elapsed() > Duration::from_secs(1)
        {
            self.playback_at = Instant::now() - Duration::from_millis(500);
            self.refresh_playback();
        }
    }

    pub fn on_event(&mut self, ev: Event) {
        let Event::Key(key) = ev else { return };
        if key.kind != KeyEventKind::Press {
            return;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.running = false;
            return;
        }
        if self.prompt.is_some() {
            return self.on_prompt_key(key);
        }
        if self.search_editing {
            return self.on_search_key(key);
        }
        if self.help {
            self.help = false;
            return;
        }
        if self.devices.is_some() {
            return self.on_devices_key(key);
        }

        // Tab-specific keys first.
        if self.on_tab_key(key) {
            return;
        }

        match key.code {
            KeyCode::Char('q') => self.running = false,
            KeyCode::Char('?') => self.help = true,
            KeyCode::Tab => self.set_tab(Tab::ALL[(self.tab.index() + 1) % Tab::ALL.len()]),
            KeyCode::BackTab => {
                self.set_tab(Tab::ALL[(self.tab.index() + Tab::ALL.len() - 1) % Tab::ALL.len()])
            }
            KeyCode::Char(c @ '1'..='6') => self.set_tab(Tab::ALL[c as usize - '1' as usize]),
            KeyCode::Char(' ') => self.toggle_play(),
            KeyCode::Char('n') => self.skip(true),
            KeyCode::Char('p') => self.skip(false),
            KeyCode::Char('.') | KeyCode::Char('>') | KeyCode::Right => self.seek_by(10_000),
            KeyCode::Char(',') | KeyCode::Char('<') | KeyCode::Left => self.seek_by(-10_000),
            KeyCode::Char('+') | KeyCode::Char('=') => self.change_volume(5),
            KeyCode::Char('-') | KeyCode::Char('_') => self.change_volume(-5),
            KeyCode::Char('s') => self.toggle_shuffle(),
            KeyCode::Char('r') => self.cycle_repeat(),
            KeyCode::Char('d') => self.open_devices(),
            KeyCode::Char('v') => self.viz_mode = self.viz_mode.next(),
            KeyCode::Char('/') => {
                self.set_tab(Tab::Search);
                self.search_editing = true;
            }
            _ => {}
        }
    }

    fn set_tab(&mut self, tab: Tab) {
        self.tab = tab;
        match tab {
            Tab::Queue => self.load_queue(),
            Tab::Jam => self.refresh_jam(),
            _ => {}
        }
    }

    /// Returns true if the key was handled.
    fn on_tab_key(&mut self, key: KeyEvent) -> bool {
        let code = key.code;
        let (up, down) = (
            matches!(code, KeyCode::Up | KeyCode::Char('k')),
            matches!(code, KeyCode::Down | KeyCode::Char('j')),
        );
        let delta: isize = match code {
            _ if up => -1,
            _ if down => 1,
            KeyCode::PageUp => -10,
            KeyCode::PageDown => 10,
            KeyCode::Char('g') | KeyCode::Home => -100_000,
            KeyCode::Char('G') | KeyCode::End => 100_000,
            _ => 0,
        };

        match self.tab {
            Tab::Now => false,
            Tab::Library => {
                if let Some(v) = &mut self.tracks_view {
                    if delta != 0 {
                        sel(&mut v.state, v.tracks.len(), delta);
                        return true;
                    }
                    match code {
                        KeyCode::Esc | KeyCode::Backspace | KeyCode::Char('h') => {
                            self.tracks_view = None;
                            true
                        }
                        KeyCode::Enter => {
                            let (ctx, tracks, i) = (
                                v.ctx.clone(),
                                v.tracks.clone(),
                                v.state.selected().unwrap_or(0),
                            );
                            self.play_tracks(ctx, &tracks, i);
                            true
                        }
                        KeyCode::Char('a') => {
                            if let Some(t) =
                                v.state.selected().and_then(|i| v.tracks.get(i)).cloned()
                            {
                                self.enqueue(&t);
                            }
                            true
                        }
                        _ => false,
                    }
                } else {
                    if delta != 0 {
                        sel(&mut self.library_state, self.library.len(), delta);
                        return true;
                    }
                    match code {
                        KeyCode::Enter | KeyCode::Char('l') => {
                            if let Some(item) = self
                                .library_state
                                .selected()
                                .and_then(|i| self.library.get(i))
                                .cloned()
                            {
                                self.open_item(item);
                            }
                            true
                        }
                        KeyCode::Char('P') => {
                            if let Some(Item::Playlist(p)) = self
                                .library_state
                                .selected()
                                .and_then(|i| self.library.get(i))
                                .cloned()
                            {
                                self.play_context(p.uri.clone(), &p.name);
                            }
                            true
                        }
                        KeyCode::Char('R') => {
                            self.load_playlists();
                            self.info("Refreshing playlists");
                            true
                        }
                        _ => false,
                    }
                }
            }
            Tab::Search => {
                if delta != 0 {
                    sel(&mut self.search_state, self.search_results.len(), delta);
                    return true;
                }
                match code {
                    KeyCode::Char('i') => {
                        self.search_editing = true;
                        true
                    }
                    KeyCode::Enter => {
                        if let Some(item) = self
                            .search_state
                            .selected()
                            .and_then(|i| self.search_results.get(i))
                            .cloned()
                        {
                            if let Item::Playlist(_) = item {
                                self.tab = Tab::Library;
                            }
                            self.open_item(item);
                        }
                        true
                    }
                    KeyCode::Char('a') => {
                        if let Some(Item::Track(t)) = self
                            .search_state
                            .selected()
                            .and_then(|i| self.search_results.get(i))
                            .cloned()
                        {
                            self.enqueue(&t);
                        }
                        true
                    }
                    _ => false,
                }
            }
            Tab::Queue => {
                let len = self.queue.as_ref().map(|q| q.queue.len()).unwrap_or(0);
                if delta != 0 {
                    sel(&mut self.queue_state, len, delta);
                    return true;
                }
                match code {
                    KeyCode::Char('R') => {
                        self.load_queue();
                        true
                    }
                    KeyCode::Enter => {
                        // Spotify can't jump inside the queue; skip forward instead.
                        let i = self.queue_state.selected().unwrap_or(0);
                        for _ in 0..=i.min(20) {
                            self.skip(true);
                        }
                        true
                    }
                    _ => false,
                }
            }
            Tab::Profile => {
                if delta != 0 {
                    sel(&mut self.profile_state, self.top_tracks.len(), delta);
                    return true;
                }
                match code {
                    KeyCode::Char('t') => {
                        self.top_range = self.top_range.next();
                        self.load_top();
                        true
                    }
                    KeyCode::Enter => {
                        let tracks = self.top_tracks.clone();
                        self.play_tracks(
                            PlayCtx::Uris,
                            &tracks,
                            self.profile_state.selected().unwrap_or(0),
                        );
                        true
                    }
                    KeyCode::Char('a') => {
                        if let Some(t) = self
                            .profile_state
                            .selected()
                            .and_then(|i| self.top_tracks.get(i))
                            .cloned()
                        {
                            self.enqueue(&t);
                        }
                        true
                    }
                    _ => false,
                }
            }
            Tab::Jam => {
                if self.jam_busy {
                    return false;
                }
                match code {
                    KeyCode::Char('c') if self.jam.is_none() => {
                        self.jam_start();
                        true
                    }
                    KeyCode::Char('i') if self.jam.is_none() => {
                        self.prompt = Some((Prompt::JamLink, String::new()));
                        true
                    }
                    KeyCode::Char('e') if self.jam.is_some() => {
                        self.jam_leave();
                        true
                    }
                    KeyCode::Char('y') => {
                        if let Some(link) = self.jam.as_ref().map(|j| j.link()) {
                            if jam::copy_to_clipboard(&link) {
                                self.info("Invite link copied");
                            } else {
                                self.info(link);
                            }
                        }
                        true
                    }
                    KeyCode::Char('R') => {
                        self.refresh_jam();
                        true
                    }
                    _ => false,
                }
            }
        }
    }

    fn on_search_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => self.search_editing = false,
            KeyCode::Enter => {
                self.search_editing = false;
                self.run_search();
            }
            KeyCode::Backspace => {
                self.search_input.pop();
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.search_input.clear()
            }
            KeyCode::Char(c) => self.search_input.push(c),
            _ => {}
        }
    }

    fn on_prompt_key(&mut self, key: KeyEvent) {
        let Some((kind, buf)) = &mut self.prompt else {
            return;
        };
        match key.code {
            KeyCode::Esc => self.prompt = None,
            KeyCode::Enter => {
                let (kind, text) = (*kind, buf.trim().to_string());
                self.prompt = None;
                if !text.is_empty() {
                    match kind {
                        Prompt::JamLink => self.jam_join(text),
                    }
                }
            }
            KeyCode::Backspace => {
                buf.pop();
            }
            KeyCode::Char(c) => buf.push(c),
            _ => {}
        }
    }

    fn on_devices_key(&mut self, key: KeyEvent) {
        let Some((devs, st)) = &mut self.devices else {
            return;
        };
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('d') => self.devices = None,
            KeyCode::Up | KeyCode::Char('k') => sel(st, devs.len(), -1),
            KeyCode::Down | KeyCode::Char('j') => sel(st, devs.len(), 1),
            KeyCode::Enter => {
                if let Some(d) = st.selected().and_then(|i| devs.get(i)).cloned() {
                    self.devices = None;
                    if let Some(id) = d.id {
                        let api = self.api.clone();
                        self.info(format!("Switching to {}", d.name));
                        let play = self.playback.as_ref().is_some_and(|p| p.is_playing);
                        self.command("transfer", async move { api.transfer(&id, play).await });
                    }
                }
            }
            _ => {}
        }
    }
}
