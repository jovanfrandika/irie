//! OAuth login and token refresh. The same token drives both the librespot
//! session (audio) and the Spotify Web API (library, search, queue...).

use crate::config::{Config, Paths};
use anyhow::{Context, Result, bail};
use librespot_oauth::{OAuthClient, OAuthClientBuilder};
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
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

impl StoredToken {
    fn from_oauth(t: librespot_oauth::OAuthToken, previous_refresh: Option<&str>) -> Self {
        let ttl = t
            .expires_at
            .saturating_duration_since(std::time::Instant::now());
        let refresh_token = if t.refresh_token.is_empty() {
            previous_refresh.unwrap_or_default().to_string()
        } else {
            t.refresh_token
        };
        Self {
            access_token: t.access_token,
            refresh_token,
            expires_at: now() + ttl.as_secs(),
        }
    }

    fn is_fresh(&self) -> bool {
        self.expires_at > now() + 120
    }
}

fn client(cfg: &Config, open_browser: bool) -> Result<OAuthClient> {
    let mut b = OAuthClientBuilder::new(&cfg.client_id, &cfg.redirect_uri(), SCOPES.to_vec())
        .with_custom_message(
            "irie is logged in. You can close this tab and go back to your terminal.",
        );
    if open_browser {
        b = b.open_in_browser();
    }
    Ok(b.build()?)
}

/// Run the interactive browser login and persist the token.
pub async fn login(cfg: &Config, paths: &Paths) -> Result<StoredToken> {
    println!("Opening your browser to log in to Spotify...");
    let token = client(cfg, true)?
        .get_access_token_async()
        .await
        .context("Spotify login failed")?;
    let stored = StoredToken::from_oauth(token, None);
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
         login with Spotify's player client; opening your browser..."
    );
    let token = OAuthClientBuilder::new(
        crate::config::DEFAULT_CLIENT_ID,
        &cfg.redirect_uri(),
        vec![
            "streaming",
            "user-read-private",
            "user-read-playback-state",
            "user-modify-playback-state",
        ],
    )
    .open_in_browser()
    .with_custom_message("irie can now stream audio. You can close this tab.")
    .build()?
    .get_access_token_async()
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
    cfg: Config,
    path: PathBuf,
    token: Mutex<StoredToken>,
}

impl Auth {
    /// Load a saved token, or run the browser login if there is none.
    pub async fn init(cfg: &Config, paths: &Paths) -> Result<Self> {
        let token = match load(paths) {
            Some(t) => t,
            None => login(cfg, paths).await?,
        };
        let auth = Self {
            cfg: cfg.clone(),
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
            match client(&self.cfg, false)?
                .refresh_token_async(&t.refresh_token)
                .await
            {
                Ok(new) => {
                    let stored = StoredToken::from_oauth(new, Some(&t.refresh_token));
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
            cfg: Config::default(),
            path: std::env::temp_dir().join("irie-test-token.json"),
            token: Mutex::new(StoredToken {
                access_token: "x".into(),
                refresh_token: "y".into(),
                expires_at: u64::MAX,
            }),
        }
    }
}
