//! The audio engine: a librespot session + player registered as a Spotify
//! Connect device. Every decoded PCM packet is also copied into a [`Tap`] so
//! the visualizer can analyse what is actually playing.

use crate::config::{Config, Paths};
use anyhow::{Context, Result};
use librespot_connect::{ConnectConfig, Spirc};
use librespot_core::{
    Session, SessionConfig, authentication::Credentials, cache::Cache, config::DeviceType,
};
use librespot_playback::{
    audio_backend::{self, Sink, SinkResult},
    config::{AudioFormat, Bitrate, PlayerConfig},
    convert::Converter,
    decoder::AudioPacket,
    mixer::{self, Mixer, MixerConfig},
    player::{Player, PlayerEvent},
};
use sha1::{Digest, Sha1};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const SAMPLE_RATE: f32 = 44_100.0;
/// librespot's rodio sink keeps up to this many packets queued, so what we
/// see in `write` is this far ahead of what you hear.
const SINK_QUEUE_PACKETS: f32 = 26.0;

struct Chunk {
    at: Instant,
    mono: Vec<f32>,
}

/// A short, timestamped history of the mono PCM signal.
#[derive(Clone, Default)]
pub struct Tap {
    inner: Arc<Mutex<VecDeque<Chunk>>>,
}

impl Tap {
    fn push(&self, interleaved: &[f64]) {
        let mono: Vec<f32> = interleaved
            .chunks_exact(2)
            .map(|f| ((f[0] + f[1]) * 0.5) as f32)
            .collect();
        let mut q = self.inner.lock().unwrap();
        q.push_back(Chunk {
            at: Instant::now(),
            mono,
        });
        while q.len() > 400 {
            q.pop_front();
        }
    }

    fn clear(&self) {
        self.inner.lock().unwrap().clear();
    }

    /// Fill `out` with the most recent `out.len()` samples that are audible
    /// right now (compensating for the sink's buffer). Returns false if there
    /// is no recent audio (paused / stopped).
    pub fn snapshot(&self, out: &mut [f32]) -> bool {
        let q = self.inner.lock().unwrap();
        let Some(last) = q.back() else { return false };
        let chunk_secs = last.mono.len() as f32 / SAMPLE_RATE;
        let delay = Duration::from_secs_f32((chunk_secs * SINK_QUEUE_PACKETS).min(1.5));
        let now = Instant::now();
        if now.duration_since(last.at) > delay + Duration::from_millis(400) {
            return false; // nothing written for a while: silence
        }
        let target = now.checked_sub(delay).unwrap_or(now);
        // Last chunk written at or before `target` is the one playing now.
        let end = q.iter().rposition(|c| c.at <= target).unwrap_or(0);
        let mut filled = out.len();
        for c in q.iter().take(end + 1).rev() {
            for &s in c.mono.iter().rev() {
                if filled == 0 {
                    return true;
                }
                filled -= 1;
                out[filled] = s;
            }
        }
        out[..filled].fill(0.0);
        true
    }
}

struct TapSink {
    inner: Box<dyn Sink>,
    tap: Tap,
}

impl Sink for TapSink {
    fn start(&mut self) -> SinkResult<()> {
        self.inner.start()
    }
    fn stop(&mut self) -> SinkResult<()> {
        self.tap.clear();
        self.inner.stop()
    }
    fn write(&mut self, packet: AudioPacket, converter: &mut Converter) -> SinkResult<()> {
        if let AudioPacket::Samples(s) = &packet {
            self.tap.push(s);
        }
        self.inner.write(packet, converter)
    }
}

pub fn device_id(name: &str) -> String {
    Sha1::digest(format!("irie:{name}").as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

pub struct Engine {
    pub session: Session,
    pub spirc: Spirc,
    pub player: Arc<Player>,
    pub mixer: Arc<dyn Mixer>,
    pub device_id: String,
    pub tap: Tap,
}

impl Engine {
    /// Connect to Spotify and register the Connect device. Returns the engine
    /// and the Spirc event loop future, which the caller must drive.
    pub async fn start(
        cfg: &Config,
        paths: &Paths,
        access_token: String,
    ) -> Result<(Self, impl Future<Output = ()> + Send + 'static)> {
        let device_id = device_id(&cfg.device_name);
        let session_config = SessionConfig {
            device_id: device_id.clone(),
            client_id: cfg.client_id.clone(),
            ..SessionConfig::default()
        };

        let audio_dir = (cfg.audio_cache_mb > 0).then(|| paths.cache_dir.join("audio"));
        let cache = Cache::new(
            Some(paths.cache_dir.join("session")),
            Some(paths.cache_dir.join("session")),
            audio_dir,
            (cfg.audio_cache_mb > 0).then_some(cfg.audio_cache_mb * 1024 * 1024),
        )
        .ok();

        let session = Session::new(session_config, cache);

        let mixer_fn = mixer::find(None).context("no software mixer available")?;
        let mixer = mixer_fn(MixerConfig::default())?;

        let player_config = PlayerConfig {
            bitrate: match cfg.bitrate {
                96 => Bitrate::Bitrate96,
                160 => Bitrate::Bitrate160,
                _ => Bitrate::Bitrate320,
            },
            normalisation: cfg.normalisation,
            ..PlayerConfig::default()
        };

        let backend = audio_backend::find(None).context("no audio backend available")?;
        let tap = Tap::default();
        let sink_tap = tap.clone();
        let player = Player::new(
            player_config,
            session.clone(),
            mixer.get_soft_volume(),
            move || {
                Box::new(TapSink {
                    inner: backend(None, AudioFormat::default()),
                    tap: sink_tap,
                }) as Box<dyn Sink>
            },
        );

        let connect_config = ConnectConfig {
            name: cfg.device_name.clone(),
            device_type: DeviceType::Computer,
            initial_volume: (cfg.initial_volume.min(100) as u32 * u16::MAX as u32 / 100) as u16,
            ..ConnectConfig::default()
        };

        let (spirc, task) = Spirc::new(
            connect_config,
            session.clone(),
            Credentials::with_access_token(access_token),
            player.clone(),
            mixer.clone(),
        )
        .await
        .context("could not start the Spotify Connect device (a Premium account is required)")?;

        Ok((
            Self {
                session,
                spirc,
                player,
                mixer,
                device_id,
                tap,
            },
            task,
        ))
    }

    pub fn events(&self) -> tokio::sync::mpsc::UnboundedReceiver<PlayerEvent> {
        self.player.get_player_event_channel()
    }

    /// Volume of the local device in percent.
    pub fn volume_pct(&self) -> u8 {
        ((self.mixer.volume() as u32 * 100 + u16::MAX as u32 / 2) / u16::MAX as u32) as u8
    }

    pub fn set_volume_pct(&self, pct: u8) {
        let v = (pct.min(100) as u32 * u16::MAX as u32 / 100) as u16;
        let _ = self.spirc.set_volume(v);
    }

    pub fn shutdown(&self) {
        let _ = self.spirc.shutdown();
    }
}

/// A bare session (no playback) for one-shot CLI commands that need
/// Spotify's internal API, such as Jam.
pub async fn connect_session(cfg: &Config, paths: &Paths, access_token: String) -> Result<Session> {
    let session = Session::new(
        SessionConfig {
            device_id: device_id(&format!("{}-cli", cfg.device_name)),
            client_id: cfg.client_id.clone(),
            ..SessionConfig::default()
        },
        Cache::new(Some(paths.cache_dir.join("session")), None, None, None).ok(),
    );
    session
        .connect(Credentials::with_access_token(access_token), true)
        .await?;
    Ok(session)
}
