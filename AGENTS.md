# AGENTS.md: AI coding agent install and setup guide for Phosphor

If a user asked you to install, configure, or troubleshoot **Phosphor** on Windows 10/11, follow this deterministic checklist.

## Security Rules (Always Enforce)
- **Never commit** `config.json`, `data/`, `*.log`, or any credentials to git. Both `config.json` and `data/` are gitignored.
- **Never echo or log** the user's Xtream Codes `username`, `password`, or `tmdbKey` back into chat transcripts unnecessarily.
- **Never hardcode or recommend** third-party IPTV providers; ask the user for their own Xtream Codes host URL, username, and password.

## Step 1: Verify & Install Prerequisites (Windows 10/11 x64)

Check what is already installed:
```powershell
Get-Command cargo, ffmpeg, ffprobe, mpv -ErrorAction SilentlyContinue | Select-Object Name, Source
```

Install anything missing:
- **Rust toolchain** (only needed if building from source rather than using a GitHub Release `.zip`):
  ```powershell
  winget install --id Rustlang.Rustup -e --accept-package-agreements --accept-source-agreements
  ```
  *(Open a fresh shell or add `$env:USERPROFILE\.cargo\bin` to `PATH` afterward.)*
- **FFmpeg & FFprobe** (required for remuxing, probing, subtitle extraction, and anime dub verification):
  ```powershell
  winget install --id Gyan.FFmpeg -e --accept-package-agreements --accept-source-agreements
  ```
- **mpv** (strongly recommended for 4K HDR10/Dolby Vision, OLED True Black, and neural GPU shaders):
  ```powershell
  winget install --id shinchiro.mpv -e --accept-package-agreements --accept-source-agreements
  ```

## Step 2: Build & Stage Phosphor

From the repository root:
```powershell
powershell -ExecutionPolicy Bypass -File .\setup.ps1
```

What `setup.ps1` does:
1. Compiles `server/` in release mode (`cargo build --release`).
2. Copies `server\target\release\phosphor.exe` to `.\phosphor-server.exe`.
3. Stages Windows launchers (`Open Phosphor.vbs`, `Start Phosphor Tray.vbs`, `Stop Phosphor.cmd`, `phosphor-tray.ps1`, `phosphor-watchdog.vbs`) into the repository root.
4. Copies `config.example.json` to `config.json` if missing.

*(If staging a prebuilt `phosphor-server.exe` from GitHub Releases, run `.\setup.ps1 -SkipBuild`.)*

## Step 3: Configure `config.json`

Ask the user for their Xtream Codes credentials (and optional free TMDB / OMDb API keys), then write `config.json`:

```json
{
  "servers": ["http://your-provider-host.example"],
  "username": "USER_XTREAM_USERNAME",
  "password": "USER_XTREAM_PASSWORD",
  "port": 7878,
  "tmdbKey": "OPTIONAL_FREE_TMDB_API_KEY_OR_READ_TOKEN",
  "omdbKey": "",
  "serverTimezone": "Europe/Amsterdam",
  "mpvPath": "C:\\Program Files\\MPV Player\\mpv.exe",
  "catalogRefreshHours": 12,
  "userAgent": "Mozilla/5.0"
}
```

Notes:
- `servers` accepts one or more Xtream Codes base URLs (automatic failover if multiple mirrors are provided; no trailing slash).
- `mpvPath`: if `mpv` is on `PATH` (e.g., via `winget install shinchiro.mpv`), set `mpvPath` to the resolved `(Get-Command mpv).Source` path or `"mpv"`.
- `tmdbKey` (free from https://www.themoviedb.org/settings/api) unlocks Trending rows, Cinema DNA, Marathons, Release Radar, and rich faceted discovery.

## Step 4: Launch & Verify

Launch Phosphor:
```powershell
wscript.exe ".\Open Phosphor.vbs"
```

Verify the server is healthy (HTTP 200 or 503 while initial catalog sync runs):
```powershell
Invoke-RestMethod http://127.0.0.1:7878/api/status
```

Optional:
- **System tray + global media keys (when `mpv` plays):** `wscript.exe ".\Start Phosphor Tray.vbs"`
- **OLED display check (no changes made):** `.\phosphor-server.exe --display`
- **Stop server:** `.\Stop Phosphor.cmd`

## Useful Environment Overrides
- `PHOSPHOR_ROOT`: override working directory containing `config.json` and `data/`
- `PHOSPHOR_PUBLIC`: override static web UI directory
- `PHOSPHOR_PORT`: override HTTP port (default `7878`)
- `PHOSPHOR_REMOTE_HOST`: bind host for the optional phone remote on port `7880` (default `0.0.0.0`)
