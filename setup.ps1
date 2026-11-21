<#
.SYNOPSIS
  One-command Windows setup & build script for TV.
.DESCRIPTION
  1. Checks prerequisites (Rust/cargo, ffmpeg, optional mpv).
  2. Builds the Rust server (`server/target/release/tv.exe`) and copies it to `.\tv-server.exe`.
  3. Copies the launcher & tray helpers into the repo root so double-clicking `Open TV.vbs` works directly.
  4. Creates `config.json` from `config.example.json` if it does not exist yet.
#>
$ErrorActionPreference = 'Stop'
$root = $PSScriptRoot

Write-Host "`n=== TV Setup ===`n" -ForegroundColor Cyan

# 1. Check prerequisites
if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
  Write-Error "Rust/Cargo is required. Install from https://rustup.rs/ and re-run setup.ps1."
}
if (-not (Get-Command ffmpeg -ErrorAction SilentlyContinue)) {
  Write-Warning "ffmpeg was not found on PATH. Install with: winget install Gyan.FFmpeg"
} else {
  Write-Host "[OK] ffmpeg found: $((Get-Command ffmpeg).Source)" -ForegroundColor Green
}

$mpvFound = (Get-Command mpv -ErrorAction SilentlyContinue) -or (Test-Path 'C:\Program Files\MPV Player\mpv.exe')
if ($mpvFound) {
  Write-Host "[OK] mpv found (Cinema OLED player + neural shaders ready)" -ForegroundColor Green
} else {
  Write-Host "[INFO] mpv not found (optional). Install with: winget install shinchiro.mpv  (or from Settings inside the app)" -ForegroundColor Yellow
}

# 2. Build Rust server
Write-Host "`nBuilding Rust server (release mode)..." -ForegroundColor Cyan
Push-Location (Join-Path $root 'server')
try {
  cargo build --release
  if ($LASTEXITCODE -ne 0) { throw "cargo build --release failed" }
} finally {
  Pop-Location
}

# 3. Stage binary and launcher scripts into repo root
Copy-Item (Join-Path $root 'server\target\release\tv.exe') (Join-Path $root 'tv-server.exe') -Force
New-Item -ItemType Directory -Path (Join-Path $root 'data') -Force | Out-Null

foreach ($f in @('Open TV.vbs', 'Start TV Tray.vbs', 'Stop TV.cmd', 'tv-tray.ps1', 'tv-watchdog.vbs')) {
  $src = Join-Path $root "scripts\$f"
  if (Test-Path $src) {
    Copy-Item $src (Join-Path $root $f) -Force
  }
}

# 4. Initialize config.json if missing
$cfgPath = Join-Path $root 'config.json'
if (-not (Test-Path $cfgPath)) {
  Copy-Item (Join-Path $root 'config.example.json') $cfgPath
  Write-Host "`n[ACTION NEEDED] Created config.json from config.example.json." -ForegroundColor Yellow
  Write-Host "  Edit $cfgPath with your Xtream Codes server URL, username, password, and optional free TMDB API key." -ForegroundColor Yellow
} else {
  Write-Host "`n[OK] Existing config.json preserved." -ForegroundColor Green
}

Write-Host "`nDone! Launch the app anytime by double-clicking:" -ForegroundColor Green
Write-Host "  - Open TV.vbs        (starts server hidden + opens app at http://localhost:7878)"
Write-Host "  - Start TV Tray.vbs  (optional Windows tray companion + global media keys)`n"
