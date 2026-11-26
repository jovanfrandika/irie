mod api;
mod app;
mod auth;
mod config;
mod engine;
mod jam;
mod oauth;
mod ui;
mod visualizer;

use anyhow::{Result, bail};
use api::{Api, TimeRange, fmt_ms};
use app::{App, Msg};
use auth::Auth;
use clap::{Parser, Subcommand};
use config::{Config, Paths};
use crossterm::event::EventStream;
use futures::StreamExt;
use std::sync::Arc;
use std::time::Duration;

#[derive(Parser)]
#[command(name = "irie", version, about = "A Spotify client for your terminal", long_about = None)]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Log in to Spotify in your browser
    Login,
    /// Forget the saved login
    Logout,
    /// Show what's playing
    Status,
    /// Resume playback, or search for a track and play it
    Play { query: Vec<String> },
    /// Pause playback
    Pause,
    /// Skip to the next track
    Next,
    /// Go back to the previous track
    Prev,
    /// Show the queue, or add a track to it
    Queue {
        #[command(subcommand)]
        action: Option<QueueCmd>,
    },
    /// Show your profile and top artists/tracks
    Me,
    /// List your playlists
    Playlists,
    /// List Spotify Connect devices
    Devices,
    /// Spotify Jam: start, join, leave or show a shared session (experimental)
    Jam {
        #[command(subcommand)]
        action: Option<JamCmd>,
    },
}

#[derive(Subcommand)]
enum QueueCmd {
    /// Search for a track and add the best match to the queue
    Add { query: Vec<String> },
}

#[derive(Subcommand)]
enum JamCmd {
    /// Show the current Jam
    Status,
    /// Start a Jam on the active device and print the invite link
    Start,
    /// Join a Jam from an invite link
    Join { link: String },
    /// Leave the Jam (or end it, if you're the host)
    Leave,
}

#[tokio::main]
async fn main() {
    if let Err(e) = run().await {
        eprintln!("irie: {e:#}");
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    let cli = Cli::parse();
    let paths = Paths::new()?;
    let cfg = Config::load(&paths)?;

    match cli.cmd {
        Some(Cmd::Login) => {
            auth::login(&cfg, &paths).await?;
            return Ok(());
        }
        Some(Cmd::Logout) => {
            auth::logout(&paths)?;
            println!("Logged out.");
            return Ok(());
        }
        _ => {}
    }

    let auth = Arc::new(Auth::init(&cfg, &paths).await?);
    let api = Api::new(auth.clone());

    match cli.cmd {
        None => tui(cfg, paths, api).await,
        Some(cmd) => command(cmd, &cfg, &paths, &api).await,
    }
}

async fn command(cmd: Cmd, cfg: &Config, paths: &Paths, api: &Api) -> Result<()> {
    let print_track = |t: &api::Track| println!("{} — {}", t.name, t.artist_line());
    match cmd {
        Cmd::Status => match api.playback().await? {
            Some(pb) => {
                let state = if pb.is_playing { "▶" } else { "⏸" };
                match &pb.item {
                    Some(t) => println!(
                        "{state} {} — {}  [{} / {}]",
                        t.name,
                        t.artist_line(),
                        fmt_ms(pb.progress_ms.unwrap_or(0)),
                        fmt_ms(t.duration_ms)
                    ),
                    None => println!("{state} (nothing loaded)"),
                }
                if let Some(d) = pb.device {
                    println!(
                        "  on {} ({}), volume {}%",
                        d.name,
                        d.kind.to_lowercase(),
                        d.volume_percent.unwrap_or(0)
                    );
                }
            }
            None => println!("Nothing playing."),
        },
        Cmd::Play { query } if query.is_empty() => api.resume(None).await?,
        Cmd::Play { query } => {
            let q = query.join(" ");
            let tracks = api.search_tracks(&q).await?;
            let Some(t) = tracks.first() else {
                bail!("no tracks found for \"{q}\"")
            };
            match t
                .album
                .as_ref()
                .map(|a| a.uri.as_str())
                .filter(|u| !u.is_empty())
            {
                Some(album) => api.play_context(None, album, Some(&t.uri)).await,
                None => api.play_uris(None, std::slice::from_ref(&t.uri), 0).await,
            }
            .map_err(no_device_hint)?;
            print!("▶ ");
            print_track(t);
        }
        Cmd::Pause => api.pause().await.map_err(no_device_hint)?,
        Cmd::Next => api.next().await.map_err(no_device_hint)?,
        Cmd::Prev => api.previous().await.map_err(no_device_hint)?,
        Cmd::Queue { action: None } => {
            let q = api.queue().await?;
            match &q.currently_playing {
                Some(t) => {
                    print!("Now: ");
                    print_track(t);
                }
                None => println!("Nothing playing."),
            }
            for (i, t) in q.queue.iter().enumerate() {
                print!("{:>3}. ", i + 1);
                print_track(t);
            }
        }
        Cmd::Queue {
            action: Some(QueueCmd::Add { query }),
        } => {
            let q = query.join(" ");
            let tracks = api.search_tracks(&q).await?;
            let Some(t) = tracks.first() else {
                bail!("no tracks found for \"{q}\"")
            };
            api.add_to_queue(&t.uri, None)
                .await
                .map_err(no_device_hint)?;
            print!("Queued: ");
            print_track(t);
        }
        Cmd::Me => {
            let me = api.me().await?;
            println!("{} ({})", me.name(), me.id);
            if let Some(p) = &me.product {
                println!("  plan:      {p}");
            }
            if let Some(c) = &me.country {
                println!("  country:   {c}");
            }
            if let Some(f) = &me.followers {
                println!("  followers: {}", f.total);
            }
            let (artists, tracks) = tokio::join!(
                api.top_artists(TimeRange::Short),
                api.top_tracks(TimeRange::Short)
            );
            println!("\nTop artists (last 4 weeks):");
            for (i, a) in artists?.iter().take(10).enumerate() {
                println!("{:>3}. {}", i + 1, a.name);
            }
            println!("\nTop tracks (last 4 weeks):");
            for (i, t) in tracks?.iter().take(10).enumerate() {
                print!("{:>3}. ", i + 1);
                print_track(t);
            }
        }
        Cmd::Playlists => {
            for p in api.playlists().await? {
                println!("{:<50} {:>5} tracks  {}", p.name, p.total(), p.uri);
            }
        }
        Cmd::Devices => {
            let ds = api.devices().await?;
            if ds.is_empty() {
                println!("No devices. Run `irie` to start the player.");
            }
            for d in ds {
                println!(
                    "{} {} ({})",
                    if d.is_active { "●" } else { " " },
                    d.name,
                    d.kind.to_lowercase()
                );
            }
        }
        Cmd::Jam { action } => {
            let token = api.auth().access_token().await?;
            let session = engine::connect_session(cfg, paths, &token).await?;
            let jam = jam::Jam::new(session);
            let show = |s: &jam::JamSession| {
                println!("{} a Jam", if s.is_host { "Hosting" } else { "In" });
                println!("  invite: {}", s.link());
                for m in &s.members {
                    println!("  • {}{}", m.name, if m.is_host { " (host)" } else { "" });
                }
            };
            match action.unwrap_or(JamCmd::Status) {
                JamCmd::Status => match jam.current().await? {
                    Some(s) => show(&s),
                    None => println!("Not in a Jam."),
                },
                JamCmd::Start => {
                    let dev = api
                        .playback()
                        .await?
                        .and_then(|p| p.device)
                        .and_then(|d| d.id);
                    let s = jam.start(dev.as_deref()).await?;
                    show(&s);
                    if jam::copy_to_clipboard(&s.link()) {
                        println!("  (link copied to clipboard)");
                    }
                }
                JamCmd::Join { link } => show(&jam.join(&link).await?),
                JamCmd::Leave => match jam.current().await? {
                    Some(s) => {
                        jam.leave(&s).await?;
                        println!(
                            "{}",
                            if s.is_host {
                                "Ended the Jam."
                            } else {
                                "Left the Jam."
                            }
                        );
                    }
                    None => println!("Not in a Jam."),
                },
            }
        }
        Cmd::Login | Cmd::Logout => unreachable!(),
    }
    Ok(())
}

fn no_device_hint(e: anyhow::Error) -> anyhow::Error {
    if api::is_status(&e, 404) {
        anyhow::anyhow!(
            "no active device. Run `irie` to start the player, or open Spotify somewhere"
        )
    } else {
        e
    }
}

async fn tui(cfg: Config, paths: Paths, api: Api) -> Result<()> {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Msg>();

    // Start the audio engine before taking over the terminal so errors are readable.
    eprintln!("Connecting to Spotify…");
    let token = api.auth().access_token().await?;
    let mut started = Err(anyhow::anyhow!("not started"));
    for fresh in [false, true] {
        let (creds, cached) = engine::streaming_credentials(&cfg, &paths, &token, fresh).await?;
        started = engine::Engine::start(&cfg, &paths, creds).await;
        if started.is_ok() || !cached {
            break;
        }
        // Cached credentials went stale: forget them and log in again.
        engine::forget_streaming(&paths);
    }
    let engine = match started {
        Ok((engine, task)) => {
            let tx2 = tx.clone();
            tokio::spawn(async move {
                task.await;
                let _ = tx2.send(Msg::EngineDown(
                    "Audio engine disconnected; restart irie to play here".into(),
                ));
            });
            let engine = Arc::new(engine);
            let mut events = engine.events();
            let tx3 = tx.clone();
            tokio::spawn(async move {
                while let Some(ev) = events.recv().await {
                    if tx3.send(Msg::Player(ev)).is_err() {
                        break;
                    }
                }
            });
            Some(engine)
        }
        Err(e) => {
            eprintln!("warning: {e:#}\nContinuing as a remote control only.");
            tokio::time::sleep(Duration::from_secs(2)).await;
            None
        }
    };

    let mut app = App::new(
        api,
        engine.clone(),
        cfg.device_name.clone(),
        cfg.autoconnect,
        tx.clone(),
    );
    if engine.is_none() {
        app.engine_error = Some("engine offline".into());
    }
    app.startup();

    let mut terminal = ratatui::init();
    let result = async {
        let mut input = EventStream::new();
        let mut frame = tokio::time::interval(Duration::from_millis(33));
        let mut poll = tokio::time::interval(Duration::from_secs(3));
        frame.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        while app.running {
            tokio::select! {
                ev = input.next() => match ev {
                    Some(Ok(ev)) => app.on_event(ev),
                    Some(Err(e)) => return Err(e.into()),
                    None => break,
                },
                Some(msg) = rx.recv() => app.on_msg(msg),
                _ = frame.tick() => {
                    let bands = app.viz_bands;
                    app.on_tick(bands);
                    terminal.draw(|f| ui::draw(f, &mut app))?;
                }
                _ = poll.tick() => app.refresh_playback(),
            }
        }
        Ok::<(), anyhow::Error>(())
    }
    .await;
    ratatui::restore();

    if let Some(e) = engine {
        e.shutdown();
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    result
}
