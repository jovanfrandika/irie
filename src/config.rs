//! User configuration and on-disk locations.

use anyhow::{Context, Result};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Spotify's own desktop client ID, which librespot uses by default. It lets
/// irie talk to Spotify without asking you to register a developer app.
pub const DEFAULT_CLIENT_ID: &str = "65b708073fc0480ea92a077233ca87bd";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Name of the Spotify Connect device irie registers.
    pub device_name: String,
    /// Streaming bitrate: 96, 160 or 320.
    pub bitrate: u16,
    /// Initial volume in percent (0-100).
    pub initial_volume: u8,
    /// OAuth client ID. Override with your own app's ID if you prefer.
    pub client_id: String,
    /// Local port irie listens on for the login callback.
    pub oauth_port: u16,
    /// OAuth redirect URI registered on your app. Defaults to
    /// http://127.0.0.1:<oauth_port>/login; can be a web page such as
    /// https://irie.frandika.com/callback that forwards to irie.
    pub redirect_uri: Option<String>,
    /// Max size of the audio cache in megabytes (0 disables the cache).
    pub audio_cache_mb: u64,
    /// Volume normalisation.
    pub normalisation: bool,
    /// Take over playback on startup when nothing else is playing.
    pub autoconnect: bool,
    /// How irie plays audio: "auto", "web" (Spotify's Web Playback SDK in the
    /// system web view) or "librespot".
    pub backend: Backend,
    /// Page that hosts the web player.
    pub player_url: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Backend {
    Auto,
    Web,
    Librespot,
}

impl Backend {
    /// The backend that will actually be used on this platform.
    pub fn resolve(self) -> Backend {
        match self {
            Backend::Auto if cfg!(any(target_os = "macos", target_os = "windows")) => Backend::Web,
            Backend::Auto => Backend::Librespot,
            b => b,
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            device_name: "irie".into(),
            bitrate: 320,
            initial_volume: 60,
            client_id: DEFAULT_CLIENT_ID.into(),
            oauth_port: 8989,
            redirect_uri: None,
            audio_cache_mb: 1024,
            normalisation: false,
            autoconnect: true,
            backend: Backend::Auto,
            player_url: "https://irie.frandika.com/player".into(),
        }
    }
}

pub struct Paths {
    pub config_dir: PathBuf,
    pub cache_dir: PathBuf,
}

impl Paths {
    pub fn new() -> Result<Self> {
        let dirs =
            ProjectDirs::from("", "", "irie").context("could not determine home directory")?;
        let config_dir = std::env::var_os("IRIE_CONFIG_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| dirs.config_dir().to_path_buf());
        let cache_dir = dirs.cache_dir().to_path_buf();
        std::fs::create_dir_all(&config_dir)?;
        std::fs::create_dir_all(&cache_dir)?;
        Ok(Self {
            config_dir,
            cache_dir,
        })
    }

    pub fn config_file(&self) -> PathBuf {
        self.config_dir.join("config.toml")
    }

    pub fn token_file(&self) -> PathBuf {
        self.config_dir.join("token.json")
    }
}

impl Config {
    pub fn load(paths: &Paths) -> Result<Self> {
        let path = paths.config_file();
        if !path.exists() {
            return Ok(Self::default());
        }
        let raw = std::fs::read_to_string(&path)?;
        toml::from_str(&raw).with_context(|| format!("invalid config at {}", path.display()))
    }

    pub fn redirect_uri(&self) -> String {
        match &self.redirect_uri {
            // Spotify's own client only accepts loopback redirects.
            Some(uri) if self.client_id != DEFAULT_CLIENT_ID => uri.clone(),
            _ => format!("http://127.0.0.1:{}/login", self.oauth_port),
        }
    }
}
