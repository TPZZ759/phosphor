<#
.SYNOPSIS
  One-command Windows setup & build script for Phosphor.
.DESCRIPTION
  1. Checks prerequisites (Rust/cargo, ffmpeg, optional mpv).
  2. Builds the Rust server (`server/target/release/phosphor.exe`) and copies it to `.\phosphor-server.exe`.
  3. Stages the Windows launcher & tray helpers into the repo root so double-clicking `Open Phosphor.vbs` works directly.
  4. Creates `config.json` from `config.example.json` if it does not exist yet.
.PARAMETER SkipBuild
  Skip `cargo build --release` (useful when staging a prebuilt `phosphor-server.exe` from GitHub Releases).
#>
param(
  [switch]$SkipBuild
)
$ErrorActionPreference = 'Stop'
$root = $PSScriptRoot

Write-Host "`n=== Phosphor Setup ===`n" -ForegroundColor Yellow

# 1. Check prerequisites
if (-not $SkipBuild -and -not (Get-Command cargo -ErrorAction SilentlyContinue)) {
  Write-Error "Rust/Cargo is required to build from source. Install from https://rustup.rs/ (or download a prebuilt release) and re-run setup.ps1."
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
  Write-Host "[INFO] mpv not found (optional, recommended for 4K HDR / DV & shaders). Install with: winget install shinchiro.mpv" -ForegroundColor Yellow
}

# 2. Build Rust server
if (-not $SkipBuild) {
  Write-Host "`nBuilding Phosphor server (release mode)..." -ForegroundColor Cyan
  Push-Location (Join-Path $root 'server')
  try {
    cargo build --release
    if ($LASTEXITCODE -ne 0) { throw "cargo build --release failed" }
  } finally {
    Pop-Location
  }
  Copy-Item (Join-Path $root 'server\target\release\phosphor.exe') (Join-Path $root 'phosphor-server.exe') -Force
}

# 3. Stage launcher scripts into repo root
New-Item -ItemType Directory -Path (Join-Path $root 'data') -Force | Out-Null

foreach ($f in @('Open Phosphor.vbs', 'Start Phosphor Tray.vbs', 'Stop Phosphor.cmd', 'phosphor-tray.ps1', 'phosphor-watchdog.vbs')) {
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

Write-Host "`nDone! Launch Phosphor anytime by double-clicking:" -ForegroundColor Green
Write-Host "  - Open Phosphor.vbs        (starts server hidden + opens app at http://localhost:7878)"
Write-Host "  - Start Phosphor Tray.vbs  (optional Windows tray companion + global media keys)`n"
