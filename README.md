# irie

A Spotify client for your terminal. irie plays music itself as a Spotify
Connect device and draws a live spectrum of the audio as it plays. You can
browse your playlists, search, manage the queue, check your profile and stats,
and start or join a Spotify Jam without leaving the terminal.

```
 irie    1 Now Playing │ 2 Library │ 3 Search │ 4 Queue │ 5 Profile │ 6 Jam          ♪ irie
╭ Now Playing · spectrum ───────────────────────────────────────────────────────────────╮
│                                  Three Little Birds                                   │
│                                Bob Marley & The Wailers                               │
│                                    Exodus · 1977                                      │
│            ██ ▆▆                   ▆▆ ██                      ██ ▅▅                   │
│         ▆▆ ██ ██                   ██ ██ ▅▅                ▆▆ ██ ██                   │
│      ▃▃ ██ ██ ██ ██             ██ ██ ██ ██ ▂▂          ▃▃ ██ ██ ██ ██             ██ │
│   ▄▄ ██ ██ ██ ██ ██ ██ ▆▆ ▇▇ ██ ██ ██ ██ ██ ██ ██ ▁▁ ██ ██ ██ ██ ██ ██ ██ ▅▅ ██ ██ ██ │
╰───────────────────────────────────────────────────────────────────────────────────────╯
│ ▶ Three Little Birds  Bob Marley & The Wailers              ⇄ shuffle  ↻ repeat  vol 60% │
│    0:42  ━━━━━━━━━━━━━━━━━━━━──────────────────────────────────────────────────── 3:00 │
```

## Features

- **Listen**: irie streams audio itself (via [librespot](https://github.com/librespot-org/librespot))
  at up to 320 kbps, and shows up as a device in every Spotify app, so your
  phone can control it too. You can also move playback to any other device (`d`).
- **Visualize**: a real FFT of the PCM that's playing, in three styles (spectrum
  bars, mirrored bars, oscilloscope), adjusted for the audio output delay so it
  stays in sync with what you hear.
- **Library**: your playlists and Liked Songs. Open one, play from any track, or
  queue tracks.
- **Search**: tracks, artists, albums and playlists.
- **Queue**: see what's up next and add tracks from anywhere with `a`.
- **Profile**: your account, top artists and top tracks (4 weeks / 6 months /
  all time) and recently played.
- **Jam** *(experimental)*: start a Jam and share the invite link, join a
  friend's Jam from a link, see who's listening, and leave or end it.
- **CLI**: one-off commands for scripts and status bars (see below).

## Requirements

- **Spotify Premium**. Spotify only allows Premium accounts to stream through
  third-party Connect clients.
- Rust 1.85 or newer (the repo pins 1.90 via `rust-toolchain.toml`).
- Linux only: ALSA headers (`libasound2-dev` / `alsa-lib-devel`).

## Install

With Homebrew (macOS and Linux):

```sh
brew install jovanfrandika/tap/irie
```

Or download a prebuilt package for macOS, Linux or Windows from the
[latest release](https://github.com/jovanfrandika/irie/releases/latest), unpack
it and put `irie` on your `PATH`:

```sh
# macOS (Apple silicon) example
curl -L https://github.com/jovanfrandika/irie/releases/latest/download/irie-v0.1.0-aarch64-apple-darwin.tar.gz | tar xz
sudo mv irie-*/irie /usr/local/bin/
```

On macOS, if Gatekeeper blocks the binary, run `xattr -d com.apple.quarantine /usr/local/bin/irie`.

Or build from source:

```sh
git clone https://github.com/jovanfrandika/irie && cd irie
cargo install --path . --locked
```

## Usage

```sh
irie          # open the player (logs in through your browser the first time)
irie login    # log in again / switch account
irie logout
```

### Keys

| Key | Action |
| --- | --- |
| `1`–`6`, `tab` | switch view |
| `space` | play / pause |
| `n` / `p` | next / previous |
| `←` `→` (or `,` `.`) | seek 10 s |
| `+` / `-` | volume |
| `s` / `r` | shuffle / cycle repeat |
| `d` | choose playback device |
| `v` | cycle visualizer style |
| `/` | search |
| `j` `k` `↑` `↓` `g` `G` | move in lists |
| `enter` | play / open |
| `a` | add to queue |
| `esc` | back |
| `t` | (Profile) change time range |
| `c` / `i` / `y` / `e` | (Jam) start / join / copy link / end or leave |
| `?` | help |
| `q` | quit |

### Command line

These work against whichever device is active, so they're useful for scripts
and status bars:

```sh
irie status                    # ▶ Song — Artist  [1:02 / 3:45]
irie play                      # resume
irie play redemption song      # search and play the top match
irie pause | next | prev
irie queue                     # show the queue
irie queue add is this love    # search and queue the top match
irie me                        # profile + top artists/tracks
irie playlists
irie devices
irie jam start                 # prints (and copies) the invite link
irie jam join <link>
irie jam status | leave
```

## Configuration

Optional. Copy [`config.example.toml`](config.example.toml) to
`~/Library/Application Support/irie/config.toml` (macOS) or
`~/.config/irie/config.toml` (Linux) to change the device name, bitrate,
volume, cache size and so on. Set `IRIE_CONFIG_DIR` to use a different directory.

Your login token is saved with `0600` permissions in the same directory.

## Using your own Spotify app

By default everything goes through the librespot client ID. If you'd rather the
Web API side (library, search, queue, profile, playback control) run on your
own registered app:

1. Open the [Spotify developer dashboard](https://developer.spotify.com/dashboard),
   log in and accept the terms.
2. **Create app**: any name and description. Under **APIs used**, tick only
   **Web API**. Add both of these **Redirect URIs**:
   - `https://irie.frandika.com/callback`: a page that hands the login back
     to irie, and also works when irie runs over SSH (you paste the link)
   - `http://127.0.0.1:8989/login`: irie's local listener
3. Copy the **Client ID**. You don't need the client secret, because irie uses PKCE.
4. Add it to your config:
   ```toml
   client_id = "your-client-id"
   redirect_uri = "https://irie.frandika.com/callback"   # optional; defaults to the local listener
   ```
5. Run `irie login`, then `irie`. The first time, irie opens a second browser
   tab for a one-time *streaming* login. Spotify only allows audio streaming
   and Jam through its own client IDs, so that part stays on librespot's ID.
   Its credentials are cached and you won't be asked again.

Apps in Spotify's development mode only work for the accounts you add under
**User Management** in the dashboard (your own account works automatically).

## Notes and limitations

- **Visualizer**: Spotify no longer offers audio analysis to third-party apps,
  so irie analyses the audio it decodes itself. That means the visualizer only
  works when irie is the device that's playing. When music plays elsewhere, press
  `d` to move it to irie.
- **Jam**: Spotify has no public API for Jams. irie uses the same internal
  endpoints as the official apps, so this may stop working whenever Spotify
  changes them. Friends join with the invite link from their own Spotify app.
- **Client ID**: by default irie logs in with the Spotify desktop client ID that
  librespot uses, so it works with no setup. To use your own Spotify developer
  app for the Web API, see [Using your own Spotify app](#using-your-own-spotify-app).
- No social features (friend activity, messaging) by design.

## Development

```sh
cargo run            # debug build
cargo test           # unit tests + headless UI render test
cargo clippy --all-targets -- -D warnings
```

Code map: `engine.rs` (librespot session, Connect device, PCM tap) ·
`visualizer.rs` (FFT + smoothing) · `api.rs` (Web API) · `jam.rs` (Jam) ·
`app.rs` (state + input) · `ui.rs` (rendering) · `auth.rs` (OAuth).

## License

MIT
