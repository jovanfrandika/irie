//! Rendering.

use crate::api::{Track, fmt_ms};
use crate::app::{App, Item, Prompt, Tab};
use crate::visualizer::Mode;
use ratatui::{
    Frame,
    buffer::Buffer,
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Modifier, Style, Stylize},
    symbols::Marker,
    text::{Line, Span},
    widgets::{
        Block, BorderType, Borders, Cell, Clear, LineGauge, Paragraph, Row, Table, TableState,
        Tabs, Widget, Wrap,
        canvas::{Canvas, Line as CLine},
    },
};

const GREEN: Color = Color::Rgb(30, 215, 96);
const GOLD: Color = Color::Rgb(250, 200, 40);
const RED: Color = Color::Rgb(230, 60, 50);
const DIM: Color = Color::Rgb(120, 120, 130);
const FG: Color = Color::Rgb(225, 225, 230);
const SEL_BG: Color = Color::Rgb(45, 52, 48);

fn block(title: impl std::fmt::Display) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(Color::Rgb(70, 75, 80)))
        .title(Span::styled(
            format!(" {title} "),
            Style::new().fg(GREEN).bold(),
        ))
}

fn highlight() -> Style {
    Style::new()
        .bg(SEL_BG)
        .fg(Color::White)
        .add_modifier(Modifier::BOLD)
}

/// Green → gold → red, by height fraction 0..1.
fn rasta(t: f32) -> Color {
    let lerp = |a: Color, b: Color, t: f32| {
        let (Color::Rgb(r1, g1, b1), Color::Rgb(r2, g2, b2)) = (a, b) else {
            return a;
        };
        let m = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t) as u8;
        Color::Rgb(m(r1, r2), m(g1, g2), m(b1, b2))
    };
    if t < 0.55 {
        lerp(GREEN, GOLD, t / 0.55)
    } else {
        lerp(GOLD, RED, (t - 0.55) / 0.45)
    }
}

fn popup(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width.saturating_sub(4));
    let h = h.min(area.height.saturating_sub(2));
    Rect {
        x: area.x + (area.width - w) / 2,
        y: area.y + (area.height - h) / 2,
        width: w,
        height: h,
    }
}

pub fn draw(f: &mut Frame, app: &mut App) {
    let [header, body, player, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(5),
        Constraint::Length(5),
        Constraint::Length(1),
    ])
    .areas(f.area());

    draw_header(f, app, header);
    match app.tab {
        Tab::Now => draw_now(f, app, body),
        Tab::Library => draw_library(f, app, body),
        Tab::Search => draw_search(f, app, body),
        Tab::Queue => draw_queue(f, app, body),
        Tab::Profile => draw_profile(f, app, body),
        Tab::Jam => draw_jam(f, app, body),
    }
    draw_player(f, app, player);
    draw_footer(f, app, footer);

    if let Some((devs, st)) = &mut app.devices {
        let area = popup(f.area(), 60, devs.len() as u16 + 4);
        f.render_widget(Clear, area);
        let rows = devs.iter().map(|d| {
            Row::new(vec![
                Cell::from(if d.is_active { "●" } else { " " }).style(Style::new().fg(GREEN)),
                Cell::from(d.name.clone()),
                Cell::from(d.kind.to_lowercase()).style(Style::new().fg(DIM)),
                Cell::from(
                    d.volume_percent
                        .map(|v| format!("{v}%"))
                        .unwrap_or_default(),
                )
                .style(Style::new().fg(DIM)),
            ])
        });
        let t = Table::new(
            rows,
            [
                Constraint::Length(2),
                Constraint::Min(10),
                Constraint::Length(12),
                Constraint::Length(5),
            ],
        )
        .block(block("Devices · enter to switch"))
        .row_highlight_style(highlight());
        f.render_stateful_widget(t, area, st);
    }

    if let Some((kind, buf)) = &app.prompt {
        let area = popup(f.area(), 70, 5);
        f.render_widget(Clear, area);
        let title = match kind {
            Prompt::JamLink => "Join a Jam · paste the invite link",
        };
        let p = Paragraph::new(vec![
            Line::from(""),
            Line::from(vec![
                Span::raw(" "),
                Span::raw(buf.as_str()),
                Span::styled("▏", Style::new().fg(GREEN)),
            ]),
        ])
        .block(block(title));
        f.render_widget(p, area);
    }

    if app.help {
        draw_help(f);
    }
}

fn draw_header(f: &mut Frame, app: &App, area: Rect) {
    let [logo, tabs, dev] = Layout::horizontal([
        Constraint::Length(8),
        Constraint::Min(10),
        Constraint::Length(28),
    ])
    .areas(area);
    f.render_widget(
        Line::from(vec![
            Span::styled(" i", Style::new().fg(GREEN).bold()),
            Span::styled("r", Style::new().fg(GOLD).bold()),
            Span::styled("i", Style::new().fg(RED).bold()),
            Span::styled("e ", Style::new().fg(GREEN).bold()),
        ]),
        logo,
    );
    let titles = Tab::ALL
        .iter()
        .enumerate()
        .map(|(i, t)| format!("{} {}", i + 1, t.title()));
    let idx = Tab::ALL.iter().position(|t| *t == app.tab).unwrap_or(0);
    f.render_widget(
        Tabs::new(titles)
            .select(idx)
            .style(Style::new().fg(DIM))
            .highlight_style(Style::new().fg(GREEN).bold().underlined())
            .divider(Span::styled("│", Style::new().fg(Color::Rgb(60, 60, 60)))),
        tabs,
    );
    let device = app.playback.as_ref().and_then(|p| p.device.as_ref());
    let (txt, color) = match (device, &app.engine_error) {
        (_, Some(_)) => ("audio engine offline".to_string(), RED),
        (Some(d), _) if app.is_local() => (format!("♪ {}", d.name), GREEN),
        (Some(d), _) => (format!("⇢ {}", d.name), GOLD),
        (None, _) if app.device_id.is_some() => (format!("♪ {} ready", app.device_name), DIM),
        (None, _) if app.speaker.is_some() => (format!("♪ starting {}…", app.device_name), DIM),
        (None, _) => ("no active device".to_string(), DIM),
    };
    let jam = if app.jam.is_some() { "◉ jam  " } else { "" };
    f.render_widget(
        Line::from(vec![
            Span::styled(jam, Style::new().fg(RED).bold()),
            Span::styled(txt, Style::new().fg(color)),
        ])
        .alignment(Alignment::Right),
        dev,
    );
}

fn draw_now(f: &mut Frame, app: &mut App, area: Rect) {
    let outer = block(format!("Now Playing · {}", app.viz_mode.label()));
    let inner = outer.inner(area);
    f.render_widget(outer, area);
    let [info, viz] = Layout::vertical([Constraint::Length(4), Constraint::Min(3)]).areas(inner);

    let lines = match app.playback.as_ref().and_then(|p| p.item.as_ref()) {
        Some(t) => vec![
            Line::from(Span::styled(
                t.name.clone(),
                Style::new().fg(Color::White).bold(),
            )),
            Line::from(Span::styled(t.artist_line(), Style::new().fg(GREEN))),
            Line::from(Span::styled(
                {
                    let album = t.album_name();
                    let year = t
                        .album
                        .as_ref()
                        .and_then(|a| a.release_date.as_deref())
                        .map(|d| &d[..d.len().min(4)]);
                    match year {
                        Some(y) if !y.is_empty() => format!("{album} · {y}"),
                        _ => album.to_string(),
                    }
                },
                Style::new().fg(DIM),
            )),
        ],
        None => vec![
            Line::from(Span::styled(
                "Nothing playing",
                Style::new().fg(Color::White).bold(),
            )),
            Line::from(Span::styled(
                "Pick something from Library (2) or Search (/).",
                Style::new().fg(DIM),
            )),
        ],
    };
    f.render_widget(Paragraph::new(lines).alignment(Alignment::Center), info);

    let remote = app.playback.as_ref().is_some_and(|p| p.is_playing) && !app.is_local();
    if remote {
        let msg = Paragraph::new(vec![
            Line::from(""),
            Line::from(Span::styled(
                "Playing on another device",
                Style::new().fg(GOLD),
            )),
            Line::from(Span::styled(
                "The visualizer reacts to audio played by irie. Press d to listen here.",
                Style::new().fg(DIM),
            )),
        ])
        .alignment(Alignment::Center)
        .wrap(Wrap { trim: true });
        f.render_widget(msg, viz);
        return;
    }
    if let Some(reason) = app.visualizer_unavailable() {
        let msg = Paragraph::new(vec![
            Line::from(""),
            Line::from(Span::styled(
                "♪ playing through irie's web player",
                Style::new().fg(GREEN),
            )),
            Line::from(Span::styled(reason.to_string(), Style::new().fg(DIM))),
        ])
        .alignment(Alignment::Center)
        .wrap(Wrap { trim: true });
        f.render_widget(msg, viz);
        return;
    }

    match app.viz_mode {
        Mode::Bars | Mode::Mirror => {
            let bands = ((viz.width as usize + 1) / 3).max(4);
            app.viz_bands = bands;
            f.render_widget(
                Spectrum {
                    bands: &app.analyzer.bands,
                    peaks: &app.analyzer.peaks,
                    mirror: app.viz_mode == Mode::Mirror,
                },
                viz,
            );
        }
        Mode::Wave => {
            let s = &app.analyzer.samples;
            let n = (viz.width as usize * 2).max(2);
            let span = s.len().min(2048);
            let start = s.len() - span;
            let pts: Vec<(f64, f64)> = (0..n)
                .map(|i| {
                    let idx = start + i * span / n;
                    (i as f64, (s[idx] as f64 * 2.5).clamp(-1.0, 1.0))
                })
                .collect();
            let level = app.analyzer.level;
            let canvas = Canvas::default()
                .marker(Marker::Braille)
                .x_bounds([0.0, n as f64])
                .y_bounds([-1.0, 1.0])
                .paint(move |ctx| {
                    for w in pts.windows(2) {
                        ctx.draw(&CLine {
                            x1: w[0].0,
                            y1: w[0].1,
                            x2: w[1].0,
                            y2: w[1].1,
                            color: rasta(level),
                        });
                    }
                });
            f.render_widget(canvas, viz);
        }
    }
}

struct Spectrum<'a> {
    bands: &'a [f32],
    peaks: &'a [f32],
    mirror: bool,
}

impl Widget for Spectrum<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        const EIGHTHS: [&str; 9] = [" ", "▁", "▂", "▃", "▄", "▅", "▆", "▇", "█"];
        let h = area.height as usize;
        if h == 0 || self.bands.is_empty() {
            return;
        }
        let used = self.bands.len() * 3 - 1;
        let x0 = area.x + (area.width.saturating_sub(used as u16)) / 2;
        for (i, (&b, &p)) in self.bands.iter().zip(self.peaks).enumerate() {
            let x = x0 + i as u16 * 3;
            if x + 1 >= area.right() {
                break;
            }
            if self.mirror {
                let half = h / 2;
                let cells = (b * half as f32).round() as usize;
                let mid = area.y as usize + half;
                for r in 0..cells.min(half) {
                    let color = rasta(r as f32 / half.max(1) as f32);
                    for dx in 0..2 {
                        if let Some(c) = buf.cell_mut((x + dx, (mid - 1 - r) as u16)) {
                            c.set_symbol("█").set_fg(color);
                        }
                        if mid + r < area.bottom() as usize
                            && let Some(c) = buf.cell_mut((x + dx, (mid + r) as u16))
                        {
                            c.set_symbol("▀").set_fg(color).set_bg(Color::Reset);
                            c.modifier = Modifier::DIM;
                        }
                    }
                }
                continue;
            }
            let units = (b * (h * 8) as f32).round() as usize;
            for r in 0..h {
                let filled = units.saturating_sub(r * 8).min(8);
                if filled == 0 {
                    break;
                }
                let y = area.bottom() - 1 - r as u16;
                let color = rasta(r as f32 / h as f32);
                for dx in 0..2 {
                    if let Some(c) = buf.cell_mut((x + dx, y)) {
                        c.set_symbol(EIGHTHS[filled]).set_fg(color);
                    }
                }
            }
            // Peak marker.
            let pr = (p * h as f32) as usize;
            if p > 0.02 && pr < h && pr * 8 >= units {
                let y = area.bottom() - 1 - pr as u16;
                for dx in 0..2 {
                    if let Some(c) = buf.cell_mut((x + dx, y)) {
                        c.set_symbol("▔").set_fg(Color::Rgb(200, 200, 200));
                    }
                }
            }
        }
    }
}

fn track_rows<'a>(tracks: &'a [Track], current: Option<&'a str>) -> impl Iterator<Item = Row<'a>> {
    tracks.iter().enumerate().map(move |(i, t)| {
        let playing = current == Some(t.uri.as_str());
        let num = if playing {
            Span::styled("♪", Style::new().fg(GREEN))
        } else {
            Span::styled(format!("{}", i + 1), Style::new().fg(DIM))
        };
        let name_style = if playing {
            Style::new().fg(GREEN).bold()
        } else {
            Style::new().fg(FG)
        };
        let mut name = vec![Span::styled(t.name.clone(), name_style)];
        if t.explicit {
            name.push(Span::styled(" 🅴", Style::new().fg(DIM)));
        }
        Row::new(vec![
            Cell::from(num),
            Cell::from(Line::from(name)),
            Cell::from(t.artist_line()).style(Style::new().fg(Color::Rgb(180, 180, 185))),
            Cell::from(t.album_name().to_string()).style(Style::new().fg(DIM)),
            Cell::from(Line::from(fmt_ms(t.duration_ms)).alignment(Alignment::Right))
                .style(Style::new().fg(DIM)),
        ])
    })
}

fn track_table<'a>(tracks: &'a [Track], current: Option<&'a str>, title: String) -> Table<'a> {
    Table::new(
        track_rows(tracks, current),
        [
            Constraint::Length(4),
            Constraint::Percentage(40),
            Constraint::Percentage(28),
            Constraint::Percentage(32),
            Constraint::Length(7),
        ],
    )
    .header(
        Row::new(vec!["#", "Title", "Artist", "Album", "Time"])
            .style(Style::new().fg(DIM).add_modifier(Modifier::ITALIC)),
    )
    .block(block(title))
    .row_highlight_style(highlight())
    .column_spacing(1)
}

fn current_uri(app: &App) -> Option<String> {
    app.playback
        .as_ref()
        .and_then(|p| p.item.as_ref())
        .map(|t| t.uri.clone())
}

fn draw_library(f: &mut Frame, app: &mut App, area: Rect) {
    let [left, right] =
        Layout::horizontal([Constraint::Percentage(32), Constraint::Min(20)]).areas(area);
    let focus_left = app.tracks_view.is_none();
    let rows = app.library.iter().map(|it| match it {
        Item::Liked => Row::new(vec![Line::from(vec![
            Span::styled("♥ ", Style::new().fg(RED)),
            Span::styled("Liked Songs", Style::new().fg(FG).bold()),
        ])]),
        Item::Playlist(p) => Row::new(vec![Line::from(vec![
            Span::styled(p.name.clone(), Style::new().fg(FG)),
            Span::styled(format!("  {}", p.total()), Style::new().fg(DIM)),
        ])]),
        _ => Row::new(vec![""]),
    });
    let mut lb = block(format!(
        "Playlists ({})",
        app.library.len().saturating_sub(1)
    ));
    if focus_left {
        lb = lb.border_style(Style::new().fg(GREEN));
    }
    let t = Table::new(rows, [Constraint::Min(5)])
        .block(lb)
        .row_highlight_style(highlight());
    f.render_stateful_widget(t, left, &mut app.library_state);

    let cur = current_uri(app);
    match &mut app.tracks_view {
        Some(v) if v.loading => {
            f.render_widget(
                Paragraph::new("Loading…")
                    .fg(DIM)
                    .block(block(v.title.clone())),
                right,
            );
        }
        Some(v) => {
            let title = format!("{} · {} tracks", v.title, v.tracks.len());
            let t = track_table(&v.tracks, cur.as_deref(), title).block(
                block(format!("{} · {} tracks", v.title, v.tracks.len()))
                    .border_style(Style::new().fg(GREEN)),
            );
            f.render_stateful_widget(t, right, &mut v.state);
        }
        None => {
            let p = Paragraph::new(vec![
                Line::from(""),
                Line::from(Span::styled("Enter  open playlist", Style::new().fg(DIM))),
                Line::from(Span::styled("P      play playlist", Style::new().fg(DIM))),
                Line::from(Span::styled("R      refresh", Style::new().fg(DIM))),
            ])
            .alignment(Alignment::Center)
            .block(block("Tracks"));
            f.render_widget(p, right);
        }
    }
}

fn draw_search(f: &mut Frame, app: &mut App, area: Rect) {
    let [input, results] =
        Layout::vertical([Constraint::Length(3), Constraint::Min(3)]).areas(area);
    let mut ib = block("Search");
    if app.search_editing {
        ib = ib.border_style(Style::new().fg(GREEN));
    }
    let mut spans = vec![
        Span::styled(" ⌕ ", Style::new().fg(GREEN)),
        Span::raw(app.search_input.clone()),
    ];
    if app.search_editing {
        spans.push(Span::styled("▏", Style::new().fg(GREEN)));
    } else if app.search_input.is_empty() {
        spans.push(Span::styled("press / or i to type", Style::new().fg(DIM)));
    }
    f.render_widget(Paragraph::new(Line::from(spans)).block(ib), input);

    if app.searching {
        f.render_widget(
            Paragraph::new("Searching…").fg(DIM).block(block("Results")),
            results,
        );
        return;
    }
    let cur = current_uri(app);
    let rows = app.search_results.iter().map(|it| {
        let (kind, color, name, sub, extra) = match it {
            Item::Track(t) => (
                "track",
                GREEN,
                t.name.clone(),
                t.artist_line(),
                fmt_ms(t.duration_ms),
            ),
            Item::Artist(a) => (
                "artist",
                GOLD,
                a.name.clone(),
                a.genres
                    .iter()
                    .take(3)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", "),
                String::new(),
            ),
            Item::Album(a) => (
                "album",
                Color::Rgb(120, 170, 255),
                a.name.clone(),
                a.artists
                    .iter()
                    .map(|x| x.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
                a.release_date
                    .clone()
                    .map(|d| d.chars().take(4).collect())
                    .unwrap_or_default(),
            ),
            Item::Playlist(p) => (
                "playlist",
                RED,
                p.name.clone(),
                p.owner
                    .as_ref()
                    .map(|o| o.name().to_string())
                    .unwrap_or_default(),
                format!("{} tracks", p.total()),
            ),
            Item::Liked => ("", DIM, String::new(), String::new(), String::new()),
        };
        let playing = matches!(it, Item::Track(t) if Some(&t.uri) == cur.as_ref());
        Row::new(vec![
            Cell::from(kind).style(Style::new().fg(color)),
            Cell::from(name).style(if playing {
                Style::new().fg(GREEN).bold()
            } else {
                Style::new().fg(FG)
            }),
            Cell::from(sub).style(Style::new().fg(Color::Rgb(180, 180, 185))),
            Cell::from(Line::from(extra).alignment(Alignment::Right)).style(Style::new().fg(DIM)),
        ])
    });
    let t = Table::new(
        rows,
        [
            Constraint::Length(9),
            Constraint::Percentage(45),
            Constraint::Percentage(40),
            Constraint::Length(10),
        ],
    )
    .block(block(format!("Results ({})", app.search_results.len())))
    .row_highlight_style(highlight());
    f.render_stateful_widget(t, results, &mut app.search_state);
}

fn draw_queue(f: &mut Frame, app: &mut App, area: Rect) {
    let [now, rest] = Layout::vertical([Constraint::Length(3), Constraint::Min(3)]).areas(area);
    let (cur, list) = match &app.queue {
        Some(q) => (q.currently_playing.clone(), q.queue.clone()),
        None => (None, vec![]),
    };
    let now_line = match &cur {
        Some(t) => Line::from(vec![
            Span::styled(" ♪ ", Style::new().fg(GREEN)),
            Span::styled(t.name.clone(), Style::new().fg(Color::White).bold()),
            Span::styled(format!("  {}", t.artist_line()), Style::new().fg(DIM)),
        ]),
        None => Line::from(Span::styled(" Nothing playing", Style::new().fg(DIM))),
    };
    f.render_widget(Paragraph::new(now_line).block(block("Now")), now);
    let t = track_table(&list, None, format!("Up next ({})", list.len()));
    let mut st: TableState = app.queue_state;
    f.render_stateful_widget(t, rest, &mut st);
    app.queue_state = st;
}

fn draw_profile(f: &mut Frame, app: &mut App, area: Rect) {
    let [left, right] =
        Layout::horizontal([Constraint::Percentage(38), Constraint::Min(30)]).areas(area);
    let [card, artists] = Layout::vertical([Constraint::Length(9), Constraint::Min(3)]).areas(left);

    let kv = |k: &str, v: String| {
        Line::from(vec![
            Span::styled(format!("{k:<10}"), Style::new().fg(DIM)),
            Span::styled(v, Style::new().fg(FG)),
        ])
    };
    let lines = match &app.me {
        Some(u) => vec![
            Line::from(Span::styled(
                u.name().to_string(),
                Style::new().fg(Color::White).bold(),
            )),
            Line::from(""),
            kv("username", u.id.clone()),
            kv("plan", u.product.clone().unwrap_or_else(|| "—".into())),
            kv("country", u.country.clone().unwrap_or_else(|| "—".into())),
            kv(
                "followers",
                u.followers
                    .as_ref()
                    .map(|f| f.total.to_string())
                    .unwrap_or_else(|| "—".into()),
            ),
            kv("email", u.email.clone().unwrap_or_else(|| "—".into())),
        ],
        None => vec![Line::from(Span::styled("Loading…", Style::new().fg(DIM)))],
    };
    f.render_widget(Paragraph::new(lines).block(block("Profile")), card);

    let rows = app.top_artists.iter().enumerate().map(|(i, a)| {
        Row::new(vec![
            Cell::from(format!("{}", i + 1)).style(Style::new().fg(DIM)),
            Cell::from(a.name.clone()).style(Style::new().fg(FG)),
            Cell::from(a.genres.first().cloned().unwrap_or_default()).style(Style::new().fg(DIM)),
        ])
    });
    f.render_widget(
        Table::new(
            rows,
            [
                Constraint::Length(3),
                Constraint::Percentage(55),
                Constraint::Percentage(45),
            ],
        )
        .block(block(format!("Top artists · {}", app.top_range.label()))),
        artists,
    );

    let [top, recent] =
        Layout::vertical([Constraint::Percentage(60), Constraint::Min(4)]).areas(right);
    let cur = current_uri(app);
    let t = track_table(
        &app.top_tracks,
        cur.as_deref(),
        format!("Top tracks · {} · t to change", app.top_range.label()),
    );
    f.render_stateful_widget(t, top, &mut app.profile_state);
    let rt = track_table(&app.recent, cur.as_deref(), "Recently played".into());
    f.render_widget(rt, recent);
}

fn draw_jam(f: &mut Frame, app: &mut App, area: Rect) {
    let b = block("Jam · experimental");
    let inner = b.inner(area);
    f.render_widget(b, area);

    let key = |k: &str, d: &str| {
        Line::from(vec![
            Span::styled(format!("{k:>3}  "), Style::new().fg(GREEN).bold()),
            Span::styled(d.to_string(), Style::new().fg(FG)),
        ])
    };
    let mut lines = vec![Line::from("")];
    if app.jam_client.is_none() {
        lines.push(Line::from(Span::styled(
            "Connecting to Spotify…",
            Style::new().fg(DIM),
        )));
    } else if app.jam_busy {
        lines.push(Line::from(Span::styled(
            "Talking to Spotify…",
            Style::new().fg(DIM),
        )));
    } else if let Some(j) = &app.jam {
        lines.push(Line::from(vec![
            Span::styled("◉ ", Style::new().fg(RED)),
            Span::styled(
                if j.is_host {
                    "You're hosting a Jam"
                } else {
                    "You're in a Jam"
                },
                Style::new().fg(Color::White).bold(),
            ),
        ]));
        lines.push(Line::from(""));
        lines.push(Line::from(vec![
            Span::styled("invite  ", Style::new().fg(DIM)),
            Span::styled(j.link(), Style::new().fg(GOLD).underlined()),
        ]));
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            format!("listening ({})", j.members.len()),
            Style::new().fg(DIM),
        )));
        for m in &j.members {
            let mut s = vec![
                Span::raw("  • "),
                Span::styled(m.name.clone(), Style::new().fg(FG)),
            ];
            if m.is_host {
                s.push(Span::styled("  host", Style::new().fg(GOLD)));
            }
            lines.push(Line::from(s));
        }
        lines.push(Line::from(""));
        lines.push(key("y", "copy invite link"));
        lines.push(key(
            "e",
            if j.is_host {
                "end the Jam for everyone"
            } else {
                "leave the Jam"
            },
        ));
        lines.push(key("R", "refresh members"));
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "Friends can add to the queue from their own Spotify app; it shows up in Queue (4).",
            Style::new().fg(DIM),
        )));
    } else {
        lines.push(Line::from(Span::styled(
            "Listen together in real time.",
            Style::new().fg(Color::White).bold(),
        )));
        lines.push(Line::from(Span::styled(
            "Start a Jam from what's playing and share the link, or join a friend's.",
            Style::new().fg(DIM),
        )));
        lines.push(Line::from(""));
        lines.push(key("c", "start a Jam"));
        lines.push(key("i", "join with an invite link"));
        lines.push(key("R", "check for an active Jam"));
    }
    f.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }),
        inner.inner(ratatui::layout::Margin {
            horizontal: 2,
            vertical: 0,
        }),
    );
}

fn draw_player(f: &mut Frame, app: &App, area: Rect) {
    let b = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(Color::Rgb(70, 75, 80)));
    let inner = b.inner(area);
    f.render_widget(b, area);
    let [l1, l2, l3] = Layout::vertical([Constraint::Length(1); 3]).areas(inner);

    let pb = app.playback.as_ref();
    let playing = pb.is_some_and(|p| p.is_playing);
    let track = pb.and_then(|p| p.item.as_ref());
    let icon = if playing { "▶" } else { "⏸" };

    let mut left = vec![Span::styled(
        format!(" {icon} "),
        Style::new().fg(GREEN).bold(),
    )];
    match track {
        Some(t) => {
            left.push(Span::styled(
                t.name.clone(),
                Style::new().fg(Color::White).bold(),
            ));
            left.push(Span::styled(
                format!("  {}", t.artist_line()),
                Style::new().fg(Color::Rgb(180, 180, 185)),
            ));
        }
        None => left.push(Span::styled("Not playing", Style::new().fg(DIM))),
    }
    let shuffle = pb.is_some_and(|p| p.shuffle_state);
    let repeat = pb.map(|p| p.repeat_state.as_str()).unwrap_or("off");
    let on = |b: bool| {
        if b {
            Style::new().fg(GREEN).bold()
        } else {
            Style::new().fg(DIM)
        }
    };
    let vol = app
        .volume_pct()
        .map(|v| format!("vol {v:>3}% "))
        .unwrap_or_default();
    let right = Line::from(vec![
        Span::styled("⇄ shuffle  ", on(shuffle)),
        Span::styled(
            if repeat == "track" {
                "↻ track  "
            } else {
                "↻ repeat  "
            },
            on(repeat != "off"),
        ),
        Span::styled(vol, Style::new().fg(FG)),
    ])
    .alignment(Alignment::Right);
    let [ll, lr] = Layout::horizontal([Constraint::Min(10), Constraint::Length(34)]).areas(l1);
    f.render_widget(Line::from(left), ll);
    f.render_widget(right, lr);

    let dur = track.map(|t| t.duration_ms).unwrap_or(0);
    let pos = app.progress_ms();
    let ratio = if dur > 0 {
        (pos as f64 / dur as f64).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let [t0, gauge, t1] = Layout::horizontal([
        Constraint::Length(9),
        Constraint::Min(5),
        Constraint::Length(9),
    ])
    .areas(l2);
    f.render_widget(Line::from(format!("{:>8}", fmt_ms(pos))).fg(DIM), t0);
    f.render_widget(
        LineGauge::default()
            .ratio(ratio)
            .label("")
            .filled_style(Style::new().fg(GREEN))
            .unfilled_style(Style::new().fg(Color::Rgb(60, 60, 65)))
            .filled_symbol("━")
            .unfilled_symbol("─"),
        gauge,
    );
    f.render_widget(Line::from(format!(" {}", fmt_ms(dur))).fg(DIM), t1);

    // Tiny level meter + context.
    let level = (app.analyzer.level * 12.0).round() as usize;
    let meter: Vec<Span> = (0..12)
        .map(|i| {
            Span::styled(
                if i < level { "▮" } else { "▯" },
                Style::new().fg(if i < level {
                    rasta(i as f32 / 12.0)
                } else {
                    Color::Rgb(55, 55, 60)
                }),
            )
        })
        .collect();
    let ctx = pb
        .and_then(|p| p.context.as_ref())
        .map(|c| c.uri.split(':').nth(1).unwrap_or("").to_string())
        .unwrap_or_default();
    let mut l3s = vec![Span::raw(" ")];
    l3s.extend(meter);
    if !ctx.is_empty() {
        l3s.push(Span::styled(format!("   from {ctx}"), Style::new().fg(DIM)));
    }
    f.render_widget(Line::from(l3s), l3);
}

fn draw_footer(f: &mut Frame, app: &App, area: Rect) {
    if let Some((msg, err, _)) = &app.status {
        let style = if *err {
            Style::new().fg(RED)
        } else {
            Style::new().fg(GOLD)
        };
        f.render_widget(Line::from(Span::styled(format!(" {msg}"), style)), area);
        return;
    }
    let hints: &[(&str, &str)] = match app.tab {
        Tab::Now => &[
            ("space", "play/pause"),
            ("n/p", "next/prev"),
            ("←/→", "seek"),
            ("+/-", "vol"),
            ("v", "visual"),
            ("d", "devices"),
        ],
        Tab::Library if app.tracks_view.is_some() => {
            &[("enter", "play"), ("a", "queue"), ("esc", "back")]
        }
        Tab::Library => &[("enter", "open"), ("P", "play playlist"), ("R", "refresh")],
        Tab::Search => &[("/", "type"), ("enter", "play"), ("a", "queue")],
        Tab::Queue => &[("enter", "skip to"), ("R", "refresh")],
        Tab::Profile => &[("enter", "play"), ("a", "queue"), ("t", "time range")],
        Tab::Jam => &[
            ("c", "start"),
            ("i", "join"),
            ("y", "copy link"),
            ("e", "end/leave"),
        ],
    };
    let mut spans = vec![Span::raw(" ")];
    for (k, d) in hints.iter().chain(&[("?", "help"), ("q", "quit")]) {
        spans.push(Span::styled(*k, Style::new().fg(GREEN)));
        spans.push(Span::styled(format!(" {d}  "), Style::new().fg(DIM)));
    }
    f.render_widget(Line::from(spans), area);
}

fn draw_help(f: &mut Frame) {
    let rows = [
        ("Global", ""),
        ("1-6 / tab", "switch view"),
        ("space", "play / pause"),
        ("n / p", "next / previous track"),
        ("← → , .", "seek 10s"),
        ("+ / -", "volume"),
        ("s / r", "shuffle / repeat"),
        ("d", "pick a playback device"),
        ("v", "cycle visualizer style"),
        ("/", "search"),
        ("q / ctrl-c", "quit"),
        ("", ""),
        ("Lists", ""),
        ("j k ↑ ↓ g G", "move"),
        ("enter", "play / open"),
        ("a", "add to queue"),
        ("esc", "back"),
    ];
    let area = popup(f.area(), 50, rows.len() as u16 + 2);
    f.render_widget(Clear, area);
    let lines: Vec<Line> = rows
        .iter()
        .map(|(k, d)| {
            if d.is_empty() {
                Line::from(Span::styled(*k, Style::new().fg(GOLD).bold()))
            } else {
                Line::from(vec![
                    Span::styled(format!("  {k:<14}"), Style::new().fg(GREEN)),
                    Span::styled(*d, Style::new().fg(FG)),
                ])
            }
        })
        .collect();
    f.render_widget(
        Paragraph::new(lines).block(block("Keys · any key to close")),
        area,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{Api, Device, PlaybackState, Track};
    use crate::app::Msg;
    use crate::jam::{JamSession, Member};
    use ratatui::{Terminal, backend::TestBackend};
    use std::sync::Arc;

    #[tokio::test]
    async fn renders_every_view_at_several_sizes() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let api = Api::new(Arc::new(crate::auth::Auth::for_test()));
        let mut app = App::new(api, None, None, "irie".into(), false, tx);
        let track = Track {
            name: "Three Little Birds".into(),
            uri: "spotify:track:1".into(),
            duration_ms: 180_000,
            ..Default::default()
        };
        app.on_msg(Msg::Playback(Some(PlaybackState {
            is_playing: true,
            progress_ms: Some(42_000),
            item: Some(track.clone()),
            repeat_state: "off".into(),
            device: Some(Device {
                id: Some("d".into()),
                name: "Phone".into(),
                is_active: true,
                ..Default::default()
            }),
            ..Default::default()
        })));
        app.on_msg(Msg::Top {
            artists: vec![],
            tracks: vec![track.clone(); 30],
            recent: vec![track.clone(); 5],
        });
        app.analyzer.bands = (0..40)
            .map(|i| ((i as f32) * 0.37).sin().abs() * 0.9)
            .collect();
        app.analyzer.peaks = vec![0.9; 40];

        if let Some(pb) = &mut app.playback {
            pb.is_playing = false;
        }
        for (w, h) in [(200, 60), (100, 30), (60, 16), (20, 8)] {
            let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
            for tab in Tab::ALL {
                app.tab = tab;
                for mode in [Mode::Bars, Mode::Mirror, Mode::Wave] {
                    app.viz_mode = mode;
                    term.draw(|f| draw(f, &mut app)).unwrap();
                }
            }
            app.jam = Some(JamSession {
                session_id: "s".into(),
                join_token: "t".into(),
                is_host: true,
                members: vec![Member {
                    name: "me".into(),
                    is_host: true,
                }],
            });
            app.tab = Tab::Jam;
            app.help = true;
            app.devices = Some((
                vec![Device {
                    name: "irie".into(),
                    ..Default::default()
                }],
                TableState::default(),
            ));
            term.draw(|f| draw(f, &mut app)).unwrap();
            app.help = false;
            app.devices = None;
            app.jam = None;
        }
        if std::env::var_os("IRIE_DUMP").is_some() {
            let mut term = Terminal::new(TestBackend::new(110, 30)).unwrap();
            for tab in [Tab::Now, Tab::Profile] {
                app.tab = tab;
                app.viz_mode = Mode::Bars;
                term.draw(|f| draw(f, &mut app)).unwrap();
                let buf = term.backend().buffer().clone();
                for y in 0..buf.area.height {
                    let line: String = (0..buf.area.width)
                        .map(|x| buf[(x, y)].symbol().to_string())
                        .collect();
                    println!("{line}");
                }
            }
        }
    }
}
