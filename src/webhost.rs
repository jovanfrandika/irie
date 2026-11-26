//! `irie player-host`: a tiny helper process that hosts Spotify's Web Playback
//! SDK in an invisible system web view (WebKit on macOS, WebView2 on Windows).
//! It speaks newline-delimited JSON with its parent irie process:
//!
//! - stdout: events from the player page (`boot`, `need_token`, `ready`, ...)
//! - stdin:  `{"token": "..."}` replies and `{"cmd": "...", "arg": ...}` commands
//!
//! The process exits when stdin closes, so it never outlives irie.

use std::io::{BufRead, Write};
use tao::{
    event::Event,
    event_loop::{ControlFlow, EventLoopBuilder},
    window::WindowBuilder,
};
use wry::WebViewBuilder;

enum Ev {
    Js(String),
    Quit,
}

/// Safari's user agent, so Spotify's SDK picks its WebKit (FairPlay) path.
const SAFARI_UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.6 Safari/605.1.15";

fn js_for(line: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(line).ok()?;
    if let Some(t) = v.get("token").and_then(|t| t.as_str()) {
        return Some(format!(
            "window.irie && window.irie.token({})",
            serde_json::to_string(t).ok()?
        ));
    }
    let cmd = v.get("cmd")?.as_str()?;
    let arg = v.get("arg").cloned().unwrap_or(serde_json::Value::Null);
    Some(format!(
        "window.irie && window.irie.cmd({}, {})",
        serde_json::to_string(cmd).ok()?,
        serde_json::to_string(&arg).ok()?
    ))
}

pub fn run(url: &str) -> ! {
    #[allow(unused_mut)]
    let mut event_loop = EventLoopBuilder::<Ev>::with_user_event().build();
    #[cfg(target_os = "macos")]
    {
        use tao::platform::macos::{ActivationPolicy, EventLoopExtMacOS};
        // No Dock icon or menu bar, and never steal focus from the terminal.
        event_loop.set_activation_policy(ActivationPolicy::Prohibited);
        event_loop.set_dock_visibility(false);
        event_loop.set_activate_ignoring_other_apps(false);
    }
    let proxy = event_loop.create_proxy();

    let window = WindowBuilder::new()
        .with_visible(std::env::var_os("IRIE_PLAYER_VISIBLE").is_some())
        .with_title("irie player")
        .build(&event_loop)
        .unwrap_or_else(|e| fail(&format!("could not create player window: {e}")));

    let mut wb = WebViewBuilder::new()
        .with_url(url)
        .with_autoplay(true)
        // The window is never shown, and WebKit throttles then suspends hidden
        // pages, which freezes the FairPlay license request mid-flight (silence).
        .with_background_throttling(wry::BackgroundThrottlingPolicy::Disabled)
        .with_devtools(false)
        .with_ipc_handler(|req| {
            let mut out = std::io::stdout().lock();
            let _ = writeln!(out, "{}", req.body().replace('\n', " "));
            let _ = out.flush();
        });
    if cfg!(target_os = "macos") {
        wb = wb.with_user_agent(SAFARI_UA);
    }
    if std::env::var_os("IRIE_PLAYER_DEBUG").is_some() {
        wb = wb.with_initialization_script_for_main_only(include_str!("webhost_debug.js"), false);
    }
    let webview = wb
        .build(&window)
        .unwrap_or_else(|e| fail(&format!("could not create web view: {e}")));

    // Forward stdin lines into the page; quit when the parent goes away.
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            if let Some(js) = js_for(&line)
                && proxy.send_event(Ev::Js(js)).is_err()
            {
                return;
            }
        }
        let _ = proxy.send_event(Ev::Quit);
    });

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        match event {
            Event::UserEvent(Ev::Js(js)) => {
                let _ = webview.evaluate_script(&js);
            }
            Event::UserEvent(Ev::Quit) => {
                let _ = webview.evaluate_script("window.irie && window.irie.cmd('disconnect')");
                *control_flow = ControlFlow::Exit;
            }
            _ => {}
        }
    })
}

fn fail(msg: &str) -> ! {
    println!(
        "{}",
        serde_json::json!({ "type": "error", "kind": "host", "message": msg })
    );
    std::process::exit(1)
}

#[cfg(test)]
mod tests {
    use super::js_for;

    #[test]
    fn builds_js_from_messages() {
        assert_eq!(
            js_for(r#"{"token":"a\"b"}"#).unwrap(),
            r#"window.irie && window.irie.token("a\"b")"#
        );
        assert_eq!(
            js_for(r#"{"cmd":"seek","arg":1000}"#).unwrap(),
            r#"window.irie && window.irie.cmd("seek", 1000)"#
        );
        assert!(js_for("garbage").is_none());
        assert!(
            js_for(r#"{"cmd":"x');alert(1);//"}"#)
                .unwrap()
                .contains(r#""x');alert(1);//""#)
        );
    }
}
