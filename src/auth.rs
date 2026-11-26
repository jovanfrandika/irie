//! OAuth login and token refresh. The same token drives both the librespot
//! session (audio) and the Spotify Web API (library, search, queue...).

use crate::config::DEFAULT_CLIENT_ID;
use crate::config::{Config, Paths};
use crate::oauth;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::Mutex;

pub const SCOPES: &[&str] = &[
    "streaming",
    "app-remote-control",
    "user-read-private",
    "user-read-email",
    "user-read-playback-state",
    "user-modify-playback-state",
    "user-read-currently-playing",
    "user-read-recently-played",
    "user-read-playback-position",
    "user-top-read",
    "user-library-read",
    "user-library-modify",
    "user-follow-read",
    "playlist-read-private",
    "playlist-read-collaborative",
    "playlist-modify-private",
    "playlist-modify-public",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredToken {
    pub access_token: String,
    pub refresh_token: String,
    /// Unix timestamp (seconds) when the access token expires.
    pub expires_at: u64,
    /// Client ID that issued the token (refreshes must use the same one).
    #[serde(default = "default_client_id")]
    pub client_id: String,
}

fn default_client_id() -> String {
    DEFAULT_CLIENT_ID.to_string()
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

impl StoredToken {
    fn from_response(
        t: oauth::TokenResponse,
        client_id: &str,
        previous_refresh: Option<&str>,
    ) -> Self {
        let refresh_token = t
            .refresh_token
            .filter(|r| !r.is_empty())
            .or_else(|| previous_refresh.map(String::from))
            .unwrap_or_default();
        Self {
            access_token: t.access_token,
            refresh_token,
            expires_at: now() + t.expires_in,
            client_id: client_id.to_string(),
        }
    }

    fn is_fresh(&self) -> bool {
        self.expires_at > now() + 120
    }
}

/// Run the interactive browser login and persist the token.
pub async fn login(cfg: &Config, paths: &Paths) -> Result<StoredToken> {
    println!("Logging in to Spotify…");
    let token = oauth::Login {
        client_id: &cfg.client_id,
        redirect_uri: &cfg.redirect_uri(),
        scopes: SCOPES,
        port: cfg.oauth_port,
        success_message: "irie is logged in. You can close this tab and go back to your terminal.",
    }
    .run()
    .await
    .context("Spotify login failed")?;
    let stored = StoredToken::from_response(token, &cfg.client_id, None);
    save(&paths.token_file(), &stored)?;
    println!("Logged in.");
    Ok(stored)
}

fn save(path: &PathBuf, t: &StoredToken) -> Result<()> {
    std::fs::write(path, serde_json::to_vec_pretty(t)?)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

/// One-time login with Spotify's own client ID, used only to set up the audio
/// session when the Web API runs on a custom client ID. librespot caches the
/// resulting credentials, so this is needed once per machine.
pub async fn streaming_login(cfg: &Config) -> Result<String> {
    eprintln!(
        "Your own client ID handles the Web API. Audio streaming needs a one-time\n\
         login with Spotify's player client."
    );
    // Spotify's own client only accepts loopback redirects.
    let token = oauth::Login {
        client_id: DEFAULT_CLIENT_ID,
        redirect_uri: &format!("http://127.0.0.1:{}/login", cfg.oauth_port),
        scopes: &[
            "streaming",
            "user-read-private",
            "user-read-playback-state",
            "user-modify-playback-state",
        ],
        port: cfg.oauth_port,
        success_message: "irie can now stream audio. You can close this tab.",
    }
    .run()
    .await
    .context("streaming login failed")?;
    Ok(token.access_token)
}

pub fn load(paths: &Paths) -> Option<StoredToken> {
    let raw = std::fs::read(paths.token_file()).ok()?;
    serde_json::from_slice(&raw).ok()
}

pub fn logout(paths: &Paths) -> Result<()> {
    for p in [
        paths.token_file(),
        paths.cache_dir.join("session").join("credentials.json"),
    ] {
        if p.exists() {
            std::fs::remove_file(p)?;
        }
    }
    Ok(())
}

/// Shared, auto-refreshing token holder.
pub struct Auth {
    path: PathBuf,
    token: Mutex<StoredToken>,
}

impl Auth {
    /// Load a saved token, or run the browser login if there is none.
    pub async fn init(cfg: &Config, paths: &Paths) -> Result<Self> {
        let token = match load(paths) {
            // A token is tied to the client ID that issued it.
            Some(t) if t.client_id == cfg.client_id => t,
            Some(_) => {
                println!("Your client_id changed, so you need to log in again.");
                login(cfg, paths).await?
            }
            None => login(cfg, paths).await?,
        };
        let auth = Self {
            path: paths.token_file(),
            token: Mutex::new(token),
        };
        auth.access_token().await?;
        Ok(auth)
    }

    /// A valid access token, refreshing it first if it is about to expire.
    pub async fn access_token(&self) -> Result<String> {
        let mut t = self.token.lock().await;
        if !t.is_fresh() {
            *t = self.refresh(&t).await?;
        }
        Ok(t.access_token.clone())
    }

    /// Force a refresh (used after a 401 from the API).
    pub async fn invalidate(&self) {
        self.token.lock().await.expires_at = 0;
    }

    async fn refresh(&self, t: &StoredToken) -> Result<StoredToken> {
        if t.refresh_token.is_empty() {
            bail!("session expired: run `irie login`");
        }
        let mut last_err = None;
        for attempt in 0..3 {
            match oauth::refresh(&t.client_id, &t.refresh_token).await {
                Ok(new) => {
                    let stored =
                        StoredToken::from_response(new, &t.client_id, Some(&t.refresh_token));
                    save(&self.path, &stored)?;
                    return Ok(stored);
                }
                Err(e) => {
                    last_err = Some(e);
                    tokio::time::sleep(Duration::from_millis(500 * (attempt + 1))).await;
                }
            }
        }
        bail!(
            "could not refresh login ({}): run `irie login`",
            last_err.map(|e| e.to_string()).unwrap_or_default()
        )
    }
}

#[cfg(test)]
impl Auth {
    pub fn for_test() -> Self {
        Self {
            path: std::env::temp_dir().join("irie-test-token.json"),
            token: Mutex::new(StoredToken {
                access_token: "x".into(),
                refresh_token: "y".into(),
                expires_at: u64::MAX,
                client_id: DEFAULT_CLIENT_ID.into(),
            }),
        }
    }
}
