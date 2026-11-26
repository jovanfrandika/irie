//! Parent side of the web player: spawns `irie player-host`, answers its token
//! requests, and turns its events into app messages.

use crate::app::Msg;
use crate::auth::Auth;
use anyhow::{Context, Result};
use serde_json::Value;
use std::process::Stdio;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::Mutex;
use tokio::sync::mpsc::UnboundedSender;

pub struct WebSpeaker {
    stdin: Mutex<ChildStdin>,
    child: Mutex<Child>,
}

/// Events from the player page that the app cares about.
#[derive(Debug, Clone)]
pub enum SpeakerEvent {
    Ready(String),
    NotReady,
    Changed,
    Error(String),
    Exited,
}

impl WebSpeaker {
    pub async fn spawn(
        url: &str,
        name: &str,
        auth: Arc<Auth>,
        tx: UnboundedSender<Msg>,
    ) -> Result<Arc<Self>> {
        let exe = std::env::current_exe().context("could not find the irie executable")?;
        let mut full = url::Url::parse(url).context("invalid player_url")?;
        full.query_pairs_mut().append_pair("name", name);
        let mut child = Command::new(exe)
            .arg("player-host")
            .arg(full.as_str())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .context("could not start the web player")?;
        let stdout = child.stdout.take().context("no stdout")?;
        let stdin = child.stdin.take().context("no stdin")?;
        let me = Arc::new(Self {
            stdin: Mutex::new(stdin),
            child: Mutex::new(child),
        });

        let weak = Arc::downgrade(&me);
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            let mut last_state = String::new();
            while let Ok(Some(line)) = lines.next_line().await {
                let Ok(v) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                let s = |k: &str| {
                    v.get(k)
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string()
                };
                let ev = match s("type").as_str() {
                    "need_token" => {
                        let Some(me) = weak.upgrade() else { break };
                        match auth.access_token().await {
                            Ok(t) => me.send(serde_json::json!({ "token": t })).await,
                            Err(e) => {
                                let _ =
                                    tx.send(Msg::Speaker(SpeakerEvent::Error(format!("{e:#}"))));
                            }
                        }
                        continue;
                    }
                    "boot" => {
                        let drm = v.get("drm").and_then(Value::as_object);
                        let any =
                            drm.is_some_and(|m| m.values().any(|b| b.as_bool() == Some(true)));
                        if any {
                            continue;
                        }
                        SpeakerEvent::Error(
                            "this system's web view can't play protected audio; set backend = \"librespot\"".into(),
                        )
                    }
                    "ready" => SpeakerEvent::Ready(s("device_id")),
                    "not_ready" => SpeakerEvent::NotReady,
                    "connected" if v.get("ok").and_then(Value::as_bool) == Some(false) => {
                        SpeakerEvent::Error("the web player could not connect to Spotify".into())
                    }
                    "state" => {
                        // Only refresh when the track or play/pause changes, not on every tick.
                        let key = format!(
                            "{}|{}",
                            s("uri"),
                            v.get("paused").map(|p| p.to_string()).unwrap_or_default()
                        );
                        if key == last_state {
                            continue;
                        }
                        last_state = key;
                        SpeakerEvent::Changed
                    }
                    "error" => SpeakerEvent::Error(friendly_error(&s("kind"), &s("message"))),
                    _ => continue,
                };
                if tx.send(Msg::Speaker(ev)).is_err() {
                    break;
                }
            }
            let _ = tx.send(Msg::Speaker(SpeakerEvent::Exited));
        });
        Ok(me)
    }

    async fn send(&self, v: Value) {
        let mut stdin = self.stdin.lock().await;
        let _ = stdin.write_all(format!("{v}\n").as_bytes()).await;
        let _ = stdin.flush().await;
    }

    pub async fn command(&self, cmd: &str, arg: Value) {
        self.send(serde_json::json!({ "cmd": cmd, "arg": arg }))
            .await;
    }

    pub async fn shutdown(&self) {
        // Closing stdin tells the host to disconnect and quit.
        let _ = self.stdin.lock().await.shutdown().await;
        let mut child = self.child.lock().await;
        if tokio::time::timeout(std::time::Duration::from_secs(2), child.wait())
            .await
            .is_err()
        {
            let _ = child.kill().await;
        }
    }
}

fn friendly_error(kind: &str, message: &str) -> String {
    match kind {
        "account_error" => {
            "Spotify says this account can't use the web player (Premium is required)".into()
        }
        "authentication_error" => format!(
            "Spotify rejected the player login: {message}. Make sure \"Web Playback SDK\" is enabled for your app, then run `irie login`"
        ),
        "autoplay_failed" => "playback was blocked from starting automatically; press space".into(),
        "sdk_load" => "couldn't load Spotify's player (are you offline?)".into(),
        _ => format!("player: {message}"),
    }
}
