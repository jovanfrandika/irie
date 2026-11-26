//! Spotify OAuth (authorization code + PKCE).
//!
//! The redirect URI can be the local listener (`http://127.0.0.1:<port>/login`)
//! or a web page such as `https://irie.frandika.com/callback`, which forwards
//! the code to the local listener. If that isn't possible (e.g. over SSH), the
//! user can paste the final URL into the terminal instead.

use anyhow::{Context, Result, anyhow, bail};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const AUTHORIZE: &str = "https://accounts.spotify.com/authorize";
const TOKEN: &str = "https://accounts.spotify.com/api/token";

#[derive(Debug, Deserialize)]
pub struct TokenResponse {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default = "default_expiry")]
    pub expires_in: u64,
}

fn default_expiry() -> u64 {
    3600
}

fn random_string(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    getrandom::fill(&mut buf).expect("system RNG");
    URL_SAFE_NO_PAD.encode(buf)
}

fn enc(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

pub struct Login<'a> {
    pub client_id: &'a str,
    pub redirect_uri: &'a str,
    pub scopes: &'a [&'a str],
    /// Local port the callback is delivered to.
    pub port: u16,
    pub success_message: &'a str,
}

impl Login<'_> {
    fn is_local(&self) -> bool {
        self.redirect_uri.starts_with("http://127.0.0.1")
            || self.redirect_uri.starts_with("http://localhost")
    }

    /// Run the browser flow and exchange the code for tokens.
    pub async fn run(&self) -> Result<TokenResponse> {
        let verifier = random_string(48);
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        // The port rides along in `state` so a web callback page knows where to forward.
        let state = format!("{}.{}", self.port, random_string(12));
        let url = format!(
            "{AUTHORIZE}?client_id={}&response_type=code&redirect_uri={}&code_challenge_method=S256&code_challenge={}&state={}&scope={}",
            enc(self.client_id),
            enc(self.redirect_uri),
            challenge,
            enc(&state),
            enc(&self.scopes.join(" ")),
        );

        let listener = TcpListener::bind(("127.0.0.1", self.port))
            .await
            .with_context(|| {
                format!(
                    "could not listen on 127.0.0.1:{} (is another irie logging in?)",
                    self.port
                )
            })?;

        open_browser(&url);
        println!("If your browser didn't open, visit:\n\n  {url}\n");
        let paste = !self.is_local() && cfg!(unix);
        if paste {
            println!(
                "Waiting for the login… (or paste the URL from the irie page here and press enter)"
            );
        } else {
            println!("Waiting for the login…");
        }

        let cancel = Arc::new(AtomicBool::new(false));
        let stdin_task = {
            let cancel = cancel.clone();
            tokio::task::spawn_blocking(move || {
                if paste {
                    read_line_cancellable(&cancel)
                } else {
                    None
                }
            })
        };

        let result = tokio::select! {
            r = accept_code(&listener, &state, self.success_message) => r,
            Ok(Some(line)) = stdin_task => parse_callback(&line, &state),
        };
        cancel.store(true, Ordering::Relaxed);
        let code = result?;
        self.exchange(&code, &verifier).await
    }

    async fn exchange(&self, code: &str, verifier: &str) -> Result<TokenResponse> {
        let resp = reqwest::Client::new()
            .post(TOKEN)
            .form(&[
                ("grant_type", "authorization_code"),
                ("code", code),
                ("redirect_uri", self.redirect_uri),
                ("client_id", self.client_id),
                ("code_verifier", verifier),
            ])
            .send()
            .await?;
        token_result(resp).await
    }
}

pub async fn refresh(client_id: &str, refresh_token: &str) -> Result<TokenResponse> {
    let resp = reqwest::Client::new()
        .post(TOKEN)
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", client_id),
        ])
        .send()
        .await?;
    token_result(resp).await
}

async fn token_result(resp: reqwest::Response) -> Result<TokenResponse> {
    let status = resp.status();
    let body = resp.text().await?;
    if !status.is_success() {
        let v: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
        let msg = v["error_description"]
            .as_str()
            .or(v["error"].as_str())
            .unwrap_or(&body);
        bail!("Spotify rejected the login ({status}): {msg}");
    }
    Ok(serde_json::from_str(&body)?)
}

/// Pull `code` out of a callback URL or query string, checking `state`.
fn parse_callback(input: &str, state: &str) -> Result<String> {
    let input = input.trim();
    let query = input.split_once('?').map(|(_, q)| q).unwrap_or(input);
    let query = query.split('#').next().unwrap_or_default();
    let mut code = None;
    let mut got_state = None;
    let mut error = None;
    for (k, v) in url::form_urlencoded::parse(query.as_bytes()) {
        match k.as_ref() {
            "code" => code = Some(v.into_owned()),
            "state" => got_state = Some(v.into_owned()),
            "error" => error = Some(v.into_owned()),
            _ => {}
        }
    }
    if let Some(e) = error {
        bail!("login was cancelled or denied ({e})");
    }
    if got_state.as_deref() != Some(state) {
        bail!("login response didn't match this request; try again");
    }
    code.ok_or_else(|| anyhow!("no authorization code in that URL"))
}

async fn accept_code(listener: &TcpListener, state: &str, success: &str) -> Result<String> {
    loop {
        let (mut sock, _) = listener.accept().await?;
        let mut buf = vec![0u8; 8192];
        let n = sock.read(&mut buf).await.unwrap_or(0);
        let req = String::from_utf8_lossy(&buf[..n]);
        let target = req
            .lines()
            .next()
            .and_then(|l| l.split_whitespace().nth(1))
            .unwrap_or("/");
        if !target.contains("code=") && !target.contains("error=") {
            let _ = sock
                .write_all(
                    b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .await;
            continue;
        }
        let result = parse_callback(target, state);
        let (title, msg) = match &result {
            Ok(_) => ("You're in", success.to_string()),
            Err(e) => ("Login failed", e.to_string()),
        };
        let page = callback_page(title, &msg);
        let head = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            page.len()
        );
        let _ = sock.write_all(head.as_bytes()).await;
        let _ = sock.write_all(page.as_bytes()).await;
        let _ = sock.shutdown().await;
        return result;
    }
}

fn callback_page(title: &str, msg: &str) -> String {
    let esc = |s: &str| {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    };
    format!(
        r#"<!doctype html><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>irie</title>
<style>body{{margin:0;min-height:100vh;display:grid;place-items:center;background:#0e0f0e;color:#e6e6e8;font:16px/1.5 ui-sans-serif,system-ui,sans-serif}}
main{{text-align:center;padding:24px}}h1{{font-size:28px;margin:0 0 8px}}h1 span:nth-child(1){{color:#1ed760}}h1 span:nth-child(2){{color:#fac828}}h1 span:nth-child(3){{color:#e63c32}}p{{color:#9a9aa2}}</style>
<main><h1><span>i</span><span>r</span><span>i</span><span style="color:#1ed760">e</span></h1><h2>{}</h2><p>{}</p></main>"#,
        esc(title),
        esc(msg)
    )
}

fn open_browser(url: &str) {
    use std::process::{Command, Stdio};
    let cmd: &[&str] = if cfg!(target_os = "macos") {
        &["open"]
    } else if cfg!(windows) {
        &["cmd", "/C", "start", ""]
    } else {
        &["xdg-open"]
    };
    let _ = Command::new(cmd[0])
        .args(&cmd[1..])
        .arg(url)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}

/// Read one line from stdin, giving up when `cancel` is set (unix only).
fn read_line_cancellable(cancel: &AtomicBool) -> Option<String> {
    #[cfg(unix)]
    {
        use std::io::BufRead;
        loop {
            if cancel.load(Ordering::Relaxed) {
                return None;
            }
            let mut fds = libc::pollfd {
                fd: 0,
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: polling a single valid pollfd for stdin.
            let ready = unsafe { libc::poll(&mut fds, 1, 200) };
            if ready > 0 {
                let mut line = String::new();
                std::io::stdin().lock().read_line(&mut line).ok()?;
                if line.trim().is_empty() {
                    continue;
                }
                return Some(line);
            }
            if ready < 0 {
                return None;
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = cancel;
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_callback_urls() {
        let st = "8989.abc";
        assert_eq!(
            parse_callback(
                "https://irie.frandika.com/callback?code=XYZ&state=8989.abc",
                st
            )
            .unwrap(),
            "XYZ"
        );
        assert_eq!(
            parse_callback("/login?code=XYZ&state=8989.abc", st).unwrap(),
            "XYZ"
        );
        assert!(parse_callback("/login?code=XYZ&state=other", st).is_err());
        assert!(parse_callback("/login?error=access_denied&state=8989.abc", st).is_err());
    }

    #[test]
    fn pkce_challenge_is_base64url_sha256() {
        // RFC 7636 appendix B test vector.
        let v = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        assert_eq!(
            URL_SAFE_NO_PAD.encode(Sha256::digest(v.as_bytes())),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[tokio::test]
    async fn listener_receives_forwarded_code() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let state = format!("{port}.xyz");
        let url = format!("http://127.0.0.1:{port}/login?code=THECODE&state={state}");
        let client = tokio::spawn(async move {
            // A stray request first (browsers ask for favicons).
            let _ = reqwest::get(format!("http://127.0.0.1:{port}/favicon.ico")).await;
            reqwest::get(url).await.unwrap().text().await.unwrap()
        });
        let code = accept_code(&listener, &state, "ok").await.unwrap();
        assert_eq!(code, "THECODE");
        assert!(client.await.unwrap().contains("You're in"));
    }
}
