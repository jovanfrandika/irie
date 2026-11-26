//! Spotify Jam (shared listening sessions).
//!
//! Jam has no public Web API, so this talks to the same internal
//! `social-connect` service the official apps use, through the librespot
//! session. It is **experimental**: Spotify can change it at any time.

use anyhow::{Result, anyhow, bail};
use http::Method;
use librespot_core::Session;
use serde_json::Value;

#[derive(Debug, Clone, Default)]
pub struct Member {
    pub name: String,
    pub is_host: bool,
}

#[derive(Debug, Clone, Default)]
pub struct JamSession {
    pub session_id: String,
    pub join_token: String,
    pub is_host: bool,
    pub members: Vec<Member>,
}

impl JamSession {
    pub fn link(&self) -> String {
        format!("https://open.spotify.com/socialsession/{}", self.join_token)
    }

    fn from_json(v: &Value) -> Option<Self> {
        let s = |k: &str| {
            v.get(k)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        };
        let session_id = s("session_id");
        if session_id.is_empty() {
            return None;
        }
        let owner = s("session_owner_id");
        let members = v
            .get("session_members")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .map(|m| {
                        let f = |k: &str| {
                            m.get(k)
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_string()
                        };
                        let username = f("username");
                        let id = f("id");
                        let display = f("display_name");
                        Member {
                            name: if display.is_empty() {
                                username.clone()
                            } else {
                                display
                            },
                            is_host: !owner.is_empty() && (owner == username || owner == id),
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();
        Some(Self {
            session_id,
            join_token: s("join_session_token"),
            is_host: v
                .get("is_session_owner")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            members,
        })
    }
}

/// Extract the join token from a Jam link, URI or bare token.
pub fn parse_token(input: &str) -> Option<String> {
    let input = input.trim();
    let tail = if let Some(i) = input.find("socialsession/") {
        &input[i + "socialsession/".len()..]
    } else if let Some(i) = input.find("socialsession:") {
        &input[i + "socialsession:".len()..]
    } else {
        input
    };
    let token: String = tail
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
        .collect();
    (!token.is_empty() && !token.contains('/')).then_some(token)
}

/// Resolve short `spotify.link/...` URLs to the full Jam link.
pub async fn resolve_link(input: &str) -> Result<String> {
    let input = input.trim();
    if input.contains("spotify.link") || input.contains("spotify.app.link") {
        let resp = reqwest::Client::new().get(input).send().await?;
        let final_url = resp.url().to_string();
        if final_url.contains("socialsession") {
            return Ok(final_url);
        }
        // The landing page sometimes embeds the link instead of redirecting.
        let body = resp.text().await.unwrap_or_default();
        if let Some(i) = body.find("open.spotify.com/socialsession/") {
            let rest = &body[i..];
            let end = rest
                .find(['"', '\'', '?', '&', '<', ' '])
                .unwrap_or(rest.len());
            return Ok(format!("https://{}", &rest[..end]));
        }
        bail!("could not resolve {input} to a Jam link");
    }
    Ok(input.to_string())
}

pub struct Jam {
    session: Session,
}

impl Jam {
    pub fn new(session: Session) -> Self {
        Self { session }
    }

    async fn call(&self, method: Method, endpoint: &str) -> Result<Option<Value>> {
        let res = self
            .session
            .spclient()
            .request_as_json(&method, endpoint, None, None)
            .await;
        match res {
            Ok(bytes) if bytes.is_empty() => Ok(None),
            Ok(bytes) => Ok(serde_json::from_slice(&bytes).ok()),
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("404") || msg.contains("NotFound") || msg.contains("not found") {
                    Ok(None)
                } else {
                    Err(anyhow!("Jam request failed: {msg}"))
                }
            }
        }
    }

    /// The Jam you are currently in, if any.
    pub async fn current(&self) -> Result<Option<JamSession>> {
        Ok(self
            .call(Method::GET, "/social-connect/v2/sessions/current")
            .await?
            .as_ref()
            .and_then(JamSession::from_json))
    }

    /// Start a Jam hosted on `device_id` (or return the one already running).
    pub async fn start(&self, device_id: Option<&str>) -> Result<JamSession> {
        let mut ep = String::from("/social-connect/v2/sessions/current_or_new?activate=true");
        if let Some(d) = device_id {
            ep.push_str(&format!("&local_device_id={d}&type=REMOTE"));
        }
        self.call(Method::GET, &ep)
            .await?
            .as_ref()
            .and_then(JamSession::from_json)
            .ok_or_else(|| anyhow!("Spotify did not start a Jam (is something playing?)"))
    }

    /// Join someone else's Jam from a link or token.
    pub async fn join(&self, link: &str) -> Result<JamSession> {
        let link = resolve_link(link).await?;
        let token =
            parse_token(&link).ok_or_else(|| anyhow!("that doesn't look like a Jam link"))?;
        let ep = format!(
            "/social-connect/v2/sessions/join/{token}?playback_control=listen_and_control&join_type=deeplinking&participant_id="
        );
        let joined = self
            .call(Method::POST, &ep)
            .await?
            .as_ref()
            .and_then(JamSession::from_json);
        match joined {
            Some(s) => Ok(s),
            None => self
                .current()
                .await?
                .ok_or_else(|| anyhow!("could not join that Jam (it may have ended)")),
        }
    }

    /// Leave the Jam, or end it for everyone if you are the host.
    pub async fn leave(&self, jam: &JamSession) -> Result<()> {
        let id = &jam.session_id;
        if jam.is_host {
            self.call(Method::DELETE, &format!("/social-connect/v3/sessions/{id}"))
                .await?;
        } else {
            self.call(
                Method::POST,
                &format!("/social-connect/v3/sessions/{id}/leave"),
            )
            .await?;
        }
        Ok(())
    }
}

/// Copy text to the system clipboard using whatever tool is available.
pub fn copy_to_clipboard(text: &str) -> bool {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let candidates: &[(&str, &[&str])] = &[
        ("pbcopy", &[]),
        ("wl-copy", &[]),
        ("xclip", &["-selection", "clipboard"]),
        ("xsel", &["--clipboard", "--input"]),
        ("clip.exe", &[]),
    ];
    for (cmd, args) in candidates {
        if let Ok(mut child) = Command::new(cmd)
            .args(*args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            if let Some(stdin) = child.stdin.as_mut() {
                let _ = stdin.write_all(text.as_bytes());
            }
            if child.wait().is_ok_and(|s| s.success()) {
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_links() {
        assert_eq!(
            parse_token("https://open.spotify.com/socialsession/AbC123?si=xyz").as_deref(),
            Some("AbC123")
        );
        assert_eq!(
            parse_token("spotify:socialsession:AbC123").as_deref(),
            Some("AbC123")
        );
        assert_eq!(parse_token("  AbC123 ").as_deref(), Some("AbC123"));
        assert_eq!(parse_token(""), None);
    }

    #[test]
    fn parses_session() {
        let v: Value = serde_json::json!({
            "session_id": "s1", "join_session_token": "tok", "is_session_owner": true,
            "session_owner_id": "me",
            "session_members": [{"username": "me", "display_name": "Me"}, {"username": "friend"}]
        });
        let s = JamSession::from_json(&v).unwrap();
        assert!(s.is_host);
        assert_eq!(s.members.len(), 2);
        assert!(s.members[0].is_host);
        assert_eq!(s.members[1].name, "friend");
        assert_eq!(s.link(), "https://open.spotify.com/socialsession/tok");
    }
}
