# TV

A fast, self-hosted, Netflix-style desktop streaming front-end for **Xtream Codes IPTV** subscriptions on Windows — powered by a single-binary **Rust (`axum`/`tokio`)** backend, a pure-black **OLED-tuned web UI**, and an optional cinema-grade **`mpv`** player with neural GPU upscaling.

> **Disclaimer:** This project is a **media player client only**. It does not include, host, link to, or distribute any streams, playlists, channels, or copyrighted content. You must provide your own Xtream Codes credentials from a legitimate provider.

---

## Features

### Streaming & Playback Engine
- **Built for single-connection accounts:** Connection preflight guard, automatic stream handoff, background worker yielding, and a **900 MB in-memory read-ahead spool** so seeks and brief network drops never stall playback or trigger duplicate connections.
- **Fast Browser Player (MSE fMP4):** Sub-second in-buffer seeks, seamless quality/version switching, automatic fallback chain across alternate copies, and source health tracking.
- **Live TV Timeshift & Guide:** 384 MB live MPEG-TS ring buffer (pause, rewind, and jump back to live), multi-feed channel deduplication, XMLTV TV Guide + Catch-up, **in-player Mini-Guide & Live Scores drawer (`G`)**, and broadcast reminders.
- **Self-Correcting Subtitles & Online Search:** Embedded subtitle extraction plus free zero-key OpenSubtitles search directly inside the player, automatically aligned to speech using **WebRTC VAD + `alass`**.
- **Audio-Fingerprinted Intro & Credits Skip:** Detects show intros and end credits locally from playback audio for automatic Skip Intro (`S`) and Up Next episode countdowns.
- **"What Did They Just Say?" (`R`):** Rewinds 8 seconds and temporarily enables English subtitles (works in both the browser player and `mpv`).

### Cinema `mpv` Integration & OLED Tuning
- **OLED True Black Pipeline:** Automatic refresh-rate matching (`24/25/30/50/60 Hz` multiples on Windows via native `QueryDisplayConfig` / `ChangeDisplaySettingsEx`), true 0-nit blacks, 10-bit HDR10 / Dolby Vision tone-mapping via `libplacebo`, and automatic display restore on exit.
- **Custom Pure-Black OLED Lua HUD (`mpv-scripts/tv_osd.lua`):** Netflix-style controls, live Clear Voice / Night Mode (`dynaudnorm`), Skip Intro (`S`), clickable Up Next, auto black-bar crop (`B`), and 8s replay with subs (`R`).
- **GPU Neural Upscaling Shaders (`mpv-shaders/`):** Bundled `Anime4K` (Fast & HQ), `FSRCNNX`, and `KrigBilateral` GLSL chains switchable live with `Ctrl+0–3`, plus optional NVIDIA RTX Video Super Resolution.

### Rich Discovery, Categorisation & Hubs
- **Movies & Series Hubs:** Trending rows (TMDB), IMDb/Rotten Tomatoes scores (OMDb), franchise collections, cast/director pages, hover previews, and quick category/platform jump bars.
- **Dedicated Documentaries Hub (`#/docs`):** Auto-classifies documentary films and series across **9 topics** (*True Crime, Nature & Wildlife, History & War, Science & Space, Sports, Music & Arts, Biography, Society & Culture, Food & Travel*), 14 curated rows, and full faceted filtering.
- **English-Dubbed Anime Hub (`#/anime`):** Episode-level audio track verification (`ffprobe` cache), automatic filtering of sub-only episodes, **"Notify when dubbed"** alerts, and **Anime Filler Badges** (`FILLER` / `MIXED`) with automatic **Skip Filler** support.
- **In-Depth Search, Browse & Rich Data View:** Multi-facet Browse across *All / Movies / Series / Documentaries / Anime* (`ANY/ALL` genre matching, platforms, countries, vote tiers, series status, runtime, 9 sorts, live facet counts), categorised Global Search (title + synopsis/topic + EPG matching), and a **`⊞ Grid` / `☰ Data`** view toggle.
- **Watch-Night Concierge (`🎲`):** Pick by type (*Movies, Series, Documentaries, Anime, Anything*), mood/topic, length, rating, 4K, and unwatched status for 3 instant non-repeating picks.
- **Sports, Couch Mode, Phone Remote & Sync:** Live ESPN scoreboards & team kickoff alerts, arrow-key/gamepad couch navigation, Wi-Fi phone remote (`:7880` with QR pairing), Letterboxd `.zip` import, two-way Trakt sync, DLNA casting, and a Windows system tray companion with dynamic global media keys.

---

## Prerequisites

- **Windows 10 / 11** (64-bit)
- **Rust toolchain** — install from https://rustup.rs/
- **FFmpeg (`ffmpeg` & `ffprobe`)** on `PATH` — e.g.:
  ```powershell
  winget install Gyan.FFmpeg
  ```
- **mpv** *(optional, strongly recommended for 4K HDR / Dolby Vision & neural shaders)*:
  ```powershell
  winget install shinchiro.mpv
  ```

---

## Quick Start

1. **Clone the repository:**
   ```powershell
   git clone https://github.com/TPZZ759/tv.git
   cd tv
   ```

2. **Run the setup script:**
   ```powershell
   powershell -ExecutionPolicy Bypass -File .\setup.ps1
   ```
   This checks dependencies, compiles the Rust server (`tv-server.exe`), stages the launcher scripts, and creates `config.json` from `config.example.json`.

3. **Configure `config.json`:**
   Open `config.json` and fill in your Xtream Codes details:
   ```json
   {
     "servers": ["http://your-provider-host.example"],
     "username": "YOUR_USERNAME",
     "password": "YOUR_PASSWORD",
     "port": 7878,
     "tmdbKey": "OPTIONAL_FREE_TMDB_API_KEY",
     "omdbKey": "",
     "serverTimezone": "Europe/Amsterdam",
     "mpvPath": "C:\\Program Files\\MPV Player\\mpv.exe",
     "catalogRefreshHours": 12
   }
   ```
   - **TMDB key (recommended, free):** Create an account at https://www.themoviedb.org/settings/api to unlock trending rows, rich metadata, cast pages, and smart search (can also be pasted inside **Settings** in the UI).
   - **OMDb key (optional, free):** https://www.omdbapi.com/apikey.aspx for IMDb / Rotten Tomatoes / Metacritic badges.

4. **Launch:**
   - Double-click **`Open TV.vbs`** (starts `tv-server.exe` hidden on `http://localhost:7878` and opens the app window).
   - *(Optional)* Double-click **`Start TV Tray.vbs`** for the Windows tray companion (adds global Play/Pause/Next/Prev media keys while `mpv` is playing and a toggle to start at Windows logon).
   - Double-click **`Stop TV.cmd`** to stop the background server anytime.

---

## Keyboard Shortcuts

| Context | Key | Action |
|---|---|---|
| Navigation | `Arrows` / `Enter` / `Backspace` | Spatial couch/gamepad navigation |
| Navigation | `/` | Focus global search |
| Player (Browser & `mpv`) | `Space` / `K` | Play / Pause (pauses Live TV buffer too) |
| Player (Browser & `mpv`) | `←` / `→` | Seek ±10 seconds |
| Player (Browser & `mpv`) | `R` | **"What did they just say?"** (8s replay with temporary EN subs) |
| Player (Browser & `mpv`) | `S` | Skip Intro |
| Player (Browser & `mpv`) | `C` | Toggle / cycle subtitles |
| Player (Browser & `mpv`) | `N` | Next episode |
| Player (Browser & `mpv`) | `F` / `M` / `I` | Fullscreen / Mute / Playback stats overlay |
| Browser Player | `P` | Hand off current title to `mpv` |
| Browser Player | `G` / `H` | Subtitle timing ±250 ms (`G` opens Mini-Guide on Live TV) |
| Browser Player | `[` / `]` | Audio timing ±100 ms |
| `mpv` OLED HUD | `B` | Auto-crop black bars (`cropdetect`) |
| `mpv` OLED HUD | `Ctrl+0` / `1` / `2` / `3` | Live GPU shaders: Off / Film (`FSRCNNX`) / `Anime4K` Fast / `Anime4K` HQ |
| Live TV | `G` / `L` / `↑↓` / `End` | Mini-Guide & Live Scores / Last Channel / Zap / Jump to Live |

---

## Project Layout

```text
├── setup.ps1                  # One-command Windows build & setup script
├── config.example.json        # Config template (copy to config.json; gitignored)
├── server/                    # Rust backend (axum, tokio, reqwest, webrtc-vad, alass)
│   ├── Cargo.toml
│   ├── Cargo.lock
│   ├── deploy-when-idle.ps1   # Zero-interruption idle server swapper
│   └── src/
├── public/                    # Vanilla JS + CSS OLED web UI & mobile remote
├── mpv-scripts/               # Custom pure-black OLED Lua OSD (tv_osd.lua)
├── mpv-shaders/               # Bundled GLSL neural shaders + third-party licenses
└── scripts/                   # Windows launchers, watchdog & tray companion
```

## License

MIT — see [LICENSE](LICENSE). Third-party GLSL shaders in `mpv-shaders/` carry their own upstream licenses (see [mpv-shaders/THIRD_PARTY_LICENSES.md](mpv-shaders/THIRD_PARTY_LICENSES.md)).
