# Swaps in a newly built tv-server.exe once nothing has been playing through the app for ~2 minutes.
param(
  [string]$AppDir = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path,
  [string]$NewExe = (Join-Path $PSScriptRoot 'target\release\tv.exe')
)
$d = $AppDir
$new = $NewExe
$idle = 0
for ($i = 0; $i -lt 1440; $i++) {   # up to 12 hours, checking every 30 s
  try { $s = (Invoke-WebRequest http://127.0.0.1:7878/api/status -UseBasicParsing -TimeoutSec 5).Content | ConvertFrom-Json } catch { $s = $null }
  try { $m = (Invoke-WebRequest http://127.0.0.1:7878/api/mpv -UseBasicParsing -TimeoutSec 5).Content | ConvertFrom-Json } catch { $m = $null }
  if ($s -and -not $s.streaming -and (-not $m -or -not $m.playing)) { $idle++ } else { $idle = 0 }
  if ($idle -ge 4) {
    # pause the watchdog (tv-watchdog.vbs) so it doesn't relaunch the old exe mid-swap
    Set-Content "$d\data\deploying" (Get-Date -Format s)
    Get-Process tv-server -ErrorAction SilentlyContinue | Stop-Process -Force
    Start-Sleep 2
    Copy-Item $new "$d\tv-server.exe" -Force
    Start-Sleep 3
    # detached & hidden (via WMI ProcessStartupInformation.ShowWindow = 0): no blank cmd window
    $si = New-CimInstance -ClassName Win32_ProcessStartup -Property @{ ShowWindow = [uint16]0 } -ClientOnly
    Invoke-CimMethod -ClassName Win32_Process -MethodName Create -Arguments @{ CommandLine = "cmd /c `"$d\tv-server.exe`" >> data\server-rs.log 2>&1"; CurrentDirectory = $d; ProcessStartupInformation = $si } | Out-Null
    Start-Sleep 10
    Remove-Item "$d\data\deploying" -ErrorAction SilentlyContinue
    "deployed at $(Get-Date -Format s)"
    exit 0
  }
  Start-Sleep 30
}
"gave up: app was busy the whole time"
