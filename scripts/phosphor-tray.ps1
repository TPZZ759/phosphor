# Unbranded personal TV system tray companion (lightweight WinForms NotifyIcon).
# Zero UI-thread blocking (all HTTP checks/POSTs run on ThreadPool), leak-free GDI icon handling,
# consistent Edge --app launch, graceful shutdown, state backup, and dynamic Windows media keys while mpv plays.
Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing

$createdNew = $false
$mutex = New-Object System.Threading.Mutex($true, "Local\PersonalTvAppTray", [ref]$createdNew)
if (-not $createdNew) { exit 0 }

if (-not ([System.Management.Automation.PSTypeName]'TvMediaHotkeyWindow').Type) {
  Add-Type -ReferencedAssemblies System.Windows.Forms -TypeDefinition @"
using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.IO;
using System.Net;
using System.Runtime.InteropServices;
using System.Text;
using System.Threading;
using System.Windows.Forms;

public static class TvTrayNative {
    [DllImport("user32.dll", SetLastError = true)]
    public static extern bool DestroyIcon(IntPtr hIcon);
}

public class TvStatusSnapshot {
    public int Seq;
    public bool Online;
    public string StatusJson;
    public string MpvJson;
}

public class TvAsyncClient {
    private readonly string _baseUrl;
    private readonly object _lock = new object();
    private int _inflight = 0;
    private int _seq = 0;
    private bool _online = false;
    private string _statusJson = "";
    private string _mpvJson = "";

    public TvAsyncClient(string baseUrl) {
        _baseUrl = baseUrl;
    }

    public void RequestPoll() {
        if (Interlocked.CompareExchange(ref _inflight, 1, 0) != 0) return;
        ThreadPool.QueueUserWorkItem(_ => {
            try {
                string st = GetText(_baseUrl + "/api/status", 2000);
                string mp = st != null ? GetText(_baseUrl + "/api/mpv", 2000) : null;
                lock (_lock) {
                    _online = (st != null);
                    _statusJson = st ?? "";
                    _mpvJson = mp ?? "";
                    _seq++;
                }
            } catch {
                lock (_lock) {
                    _online = false;
                    _statusJson = "";
                    _mpvJson = "";
                    _seq++;
                }
            } finally {
                Interlocked.Exchange(ref _inflight, 0);
            }
        });
    }

    public TvStatusSnapshot GetSnapshot() {
        lock (_lock) {
            return new TvStatusSnapshot {
                Seq = _seq,
                Online = _online,
                StatusJson = _statusJson,
                MpvJson = _mpvJson
            };
        }
    }

    public void PostAsync(string path, string jsonBody) {
        ThreadPool.QueueUserWorkItem(_ => {
            try {
                PostSync(path, jsonBody, 2500);
            } catch { }
            RequestPoll();
        });
    }

    public void RestartServerAsync(string tvDir, string watchdogPath) {
        ThreadPool.QueueUserWorkItem(_ => {
            try {
                PostSync("/api/shutdown", "{}", 3000);
                for (int i = 0; i < 12; i++) {
                    if (Process.GetProcessesByName("tv-server").Length == 0) break;
                    Thread.Sleep(400);
                }
                foreach (var p in Process.GetProcessesByName("tv-server")) {
                    try { p.Kill(); } catch { }
                }
                Thread.Sleep(400);
                if (File.Exists(watchdogPath)) {
                    Process.Start(new ProcessStartInfo {
                        FileName = Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.Windows), "System32", "wscript.exe"),
                        Arguments = "\"" + watchdogPath + "\"",
                        WorkingDirectory = tvDir,
                        UseShellExecute = false
                    });
                } else {
                    string exe = Path.Combine(tvDir, "phosphor-server.exe");
                    if (File.Exists(exe)) {
                        Process.Start(new ProcessStartInfo {
                            FileName = "cmd.exe",
                            Arguments = "/c \"\"" + exe + "\" >> data\\server-rs.log 2>&1\"",
                            WorkingDirectory = tvDir,
                            CreateNoWindow = true,
                            UseShellExecute = false
                        });
                    }
                }
            } catch { }
            Thread.Sleep(1200);
            RequestPoll();
        });
    }

    public bool PostSync(string path, string jsonBody, int timeoutMs) {
        try {
            var req = (HttpWebRequest)WebRequest.Create(_baseUrl + path);
            req.Method = "POST";
            req.ContentType = "application/json";
            req.Timeout = timeoutMs;
            req.ReadWriteTimeout = timeoutMs;
            byte[] data = Encoding.UTF8.GetBytes(string.IsNullOrEmpty(jsonBody) ? "{}" : jsonBody);
            req.ContentLength = data.Length;
            using (var s = req.GetRequestStream()) {
                s.Write(data, 0, data.Length);
            }
            using (var resp = (HttpWebResponse)req.GetResponse()) {
                return ((int)resp.StatusCode >= 200 && (int)resp.StatusCode < 300);
            }
        } catch {
            return false;
        }
    }

    private static string GetText(string url, int timeoutMs) {
        try {
            var req = (HttpWebRequest)WebRequest.Create(url);
            req.Method = "GET";
            req.Timeout = timeoutMs;
            req.ReadWriteTimeout = timeoutMs;
            using (var resp = (HttpWebResponse)req.GetResponse())
            using (var sr = new StreamReader(resp.GetResponseStream(), Encoding.UTF8)) {
                return sr.ReadToEnd();
            }
        } catch (WebException ex) {
            // 503 loading still means the server process is online
            var hr = ex.Response as HttpWebResponse;
            if (hr != null && (int)hr.StatusCode == 503) {
                try {
                    using (var sr = new StreamReader(hr.GetResponseStream(), Encoding.UTF8)) {
                        return sr.ReadToEnd();
                    }
                } catch { return "{}"; }
            }
            return null;
        } catch {
            return null;
        }
    }
}

public class TvHotkeyEventArgs : EventArgs {
    private readonly int _id;
    public int Id { get { return _id; } }
    public TvHotkeyEventArgs(int id) { _id = id; }
}

public class TvMediaHotkeyWindow : NativeWindow, IDisposable {
    [DllImport("user32.dll", SetLastError = true)]
    private static extern bool RegisterHotKey(IntPtr hWnd, int id, uint fsModifiers, uint vk);

    [DllImport("user32.dll", SetLastError = true)]
    private static extern bool UnregisterHotKey(IntPtr hWnd, int id);

    private const int WM_HOTKEY = 0x0312;
    private const uint MOD_NOREPEAT = 0x4000;

    public const int ID_PLAY_PAUSE = 1001;
    public const int ID_NEXT_TRACK = 1002;
    public const int ID_PREV_TRACK = 1003;
    public const int ID_STOP       = 1004;

    private const uint VK_MEDIA_NEXT_TRACK = 0xB0;
    private const uint VK_MEDIA_PREV_TRACK = 0xB1;
    private const uint VK_MEDIA_STOP       = 0xB2;
    private const uint VK_MEDIA_PLAY_PAUSE = 0xB3;

    private readonly HashSet<int> _registeredIds = new HashSet<int>();
    private bool _active = false;
    private bool _disposed = false;

    public event EventHandler<TvHotkeyEventArgs> HotkeyPressed;

    public bool IsActive { get { return _active; } }

    public TvMediaHotkeyWindow() {
        CreateParams cp = new CreateParams();
        cp.Caption = "PersonalTvAppMediaHotkeys";
        CreateHandle(cp);
    }

    public void SetActive(bool enable) {
        if (_disposed) return;
        if (enable) {
            if (!_active) {
                TryReg(ID_PLAY_PAUSE, VK_MEDIA_PLAY_PAUSE);
                TryReg(ID_NEXT_TRACK, VK_MEDIA_NEXT_TRACK);
                TryReg(ID_PREV_TRACK, VK_MEDIA_PREV_TRACK);
                TryReg(ID_STOP,       VK_MEDIA_STOP);
                _active = true;
            }
        } else {
            if (_active || _registeredIds.Count > 0) {
                UnregisterAll();
                _active = false;
            }
        }
    }

    private void TryReg(int id, uint vk) {
        if (_registeredIds.Contains(id) || Handle == IntPtr.Zero) return;
        if (RegisterHotKey(Handle, id, MOD_NOREPEAT, vk)) {
            _registeredIds.Add(id);
        }
    }

    public void UnregisterAll() {
        if (Handle != IntPtr.Zero) {
            foreach (int id in _registeredIds) {
                UnregisterHotKey(Handle, id);
            }
        }
        _registeredIds.Clear();
    }

    protected override void WndProc(ref Message m) {
        if (m.Msg == WM_HOTKEY) {
            EventHandler<TvHotkeyEventArgs> handler = HotkeyPressed;
            if (handler != null) {
                handler(this, new TvHotkeyEventArgs(m.WParam.ToInt32()));
            }
        }
        base.WndProc(ref m);
    }

    public void Dispose() {
        if (_disposed) return;
        _disposed = true;
        UnregisterAll();
        if (Handle != IntPtr.Zero) {
            DestroyHandle();
        }
    }
}
"@
}

$tvDir = Split-Path -Parent $PSScriptRoot
$base = "http://127.0.0.1:7878"
$startupDir = Join-Path $env:APPDATA "Microsoft\Windows\Start Menu\Programs\Startup"
$startupLnk = Join-Path $startupDir "Phosphor Tray.lnk"
$vbsTrayPath = Join-Path $tvDir "Start Phosphor Tray.vbs"
$openVbsPath = Join-Path $tvDir "Open Phosphor.vbs"
$watchdogPath = Join-Path $tvDir "phosphor-watchdog.vbs"
$logPath = Join-Path $tvDir "data\server-rs.log"
$statePath = Join-Path $tvDir "data\state.json"

# Draw a crisp 32x32 unbranded OLED-styled TV icon and release all GDI handles immediately
$bmp = New-Object System.Drawing.Bitmap(32, 32)
$g = [System.Drawing.Graphics]::FromImage($bmp)
$bgBrush = New-Object System.Drawing.SolidBrush([System.Drawing.Color]::FromArgb(255, 10, 12, 16))
$pen = New-Object System.Drawing.Pen([System.Drawing.Color]::FromArgb(255, 255, 181, 71), 2.4)
$playBrush = New-Object System.Drawing.SolidBrush([System.Drawing.Color]::FromArgb(255, 242, 243, 245))
try {
  $g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
  $g.Clear([System.Drawing.Color]::Transparent)
  $g.FillRectangle($bgBrush, 2, 5, 28, 22)
  $g.DrawRectangle($pen, 2, 5, 28, 22)
  $tri = [System.Drawing.Point[]]@(
    (New-Object System.Drawing.Point(12, 10)),
    (New-Object System.Drawing.Point(23, 16)),
    (New-Object System.Drawing.Point(12, 22))
  )
  $g.FillPolygon($playBrush, $tri)
  $hIcon = $bmp.GetHicon()
  try {
    $rawIcon = [System.Drawing.Icon]::FromHandle($hIcon)
    $icon = [System.Drawing.Icon]$rawIcon.Clone()
    $rawIcon.Dispose()
  } finally {
    [TvTrayNative]::DestroyIcon($hIcon) | Out-Null
  }
} finally {
  $playBrush.Dispose()
  $pen.Dispose()
  $bgBrush.Dispose()
  $g.Dispose()
  $bmp.Dispose()
}

$http = New-Object TvAsyncClient($base)
$script:lastSeq = -1
$script:isOnline = $false
$script:isMpv = $false
$script:isStreaming = $false

$tray = New-Object System.Windows.Forms.NotifyIcon
$tray.Icon = $icon
$tray.Text = "TV: Starting..."
$tray.Visible = $true

$menu = New-Object System.Windows.Forms.ContextMenuStrip
$miStatus = $menu.Items.Add("TV: Checking...")
$miStatus.Enabled = $false
$menu.Items.Add("-") | Out-Null
$miOpen = $menu.Items.Add("Open Phosphor")
$miOpen.Font = New-Object System.Drawing.Font($miOpen.Font, [System.Drawing.FontStyle]::Bold)
$menu.Items.Add("-") | Out-Null
$miPause = $menu.Items.Add("Pause / Play")
$miSkip = $menu.Items.Add("Skip Intro")
$miStop = $menu.Items.Add("Stop Playback")
$menu.Items.Add("-") | Out-Null
$miRestore = $menu.Items.Add("Restore OLED Display (Hz / HDR)")
$miRestart = $menu.Items.Add("Restart Server")
$miLog = $menu.Items.Add("Open Live Log")
$miBackup = $menu.Items.Add("Backup State Now")
$menu.Items.Add("-") | Out-Null
$miAutoStart = New-Object System.Windows.Forms.ToolStripMenuItem("Start Tray at Windows Logon")
$miAutoStart.Checked = Test-Path -LiteralPath $startupLnk
$menu.Items.Add($miAutoStart) | Out-Null
$menu.Items.Add("-") | Out-Null
$miQuitTray = $menu.Items.Add("Quit Tray Icon")
$miQuitAll = $menu.Items.Add("Quit All (Stop Server & Tray)")
$tray.ContextMenuStrip = $menu

$hk = New-Object TvMediaHotkeyWindow

function Post-Async($path, $bodyObj) {
  $json = if ($null -eq $bodyObj) { "{}" } else { $bodyObj | ConvertTo-Json -Compress }
  $http.PostAsync($path, $json)
}

function Launch-TvApp {
  if (-not $script:isOnline -and (Test-Path -LiteralPath $openVbsPath)) {
    Start-Process -FilePath "$env:WINDIR\System32\wscript.exe" -ArgumentList "`"$openVbsPath`""
    return
  }
  $edge = Join-Path ${env:ProgramFiles(x86)} "Microsoft\Edge\Application\msedge.exe"
  if (-not (Test-Path -LiteralPath $edge)) {
    $edge = Join-Path $env:ProgramFiles "Microsoft\Edge\Application\msedge.exe"
  }
  if (Test-Path -LiteralPath $edge) {
    Start-Process -FilePath $edge -ArgumentList "--app=$base", "--start-maximized"
  } else {
    Start-Process $base
  }
}

function Stop-TvServerGracefully {
  $http.PostSync("/api/shutdown", "{}", 3000) | Out-Null
  for ($i = 0; $i -lt 10; $i++) {
    if (-not (Get-Process -Name "tv-server" -ErrorAction SilentlyContinue)) { return }
    Start-Sleep -Milliseconds 400
  }
  Get-Process -Name "tv-server" -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
}

$openApp = { Launch-TvApp }
$tray.Add_DoubleClick($openApp)
$miOpen.Add_Click($openApp)

$miPause.Add_Click({
  if ($script:isMpv) { Post-Async "/api/mpv/control" @{ cmd = "toggle" } }
  else { Post-Async "/api/remote/send" @{ cmd = "toggle" } }
})
$miSkip.Add_Click({
  if ($script:isMpv) { Post-Async "/api/mpv/control" @{ cmd = "skip" } }
  else { Post-Async "/api/remote/send" @{ cmd = "skip" } }
})
$miStop.Add_Click({
  $hk.SetActive($false)
  if ($script:isMpv) { Post-Async "/api/mpv/stop" @{} }
  Post-Async "/api/remote/send" @{ cmd = "close" }
  Post-Async "/api/stop" @{}
})
$miRestore.Add_Click({
  Post-Async "/api/display/restore" @{}
  $tray.ShowBalloonTip(2000, "TV", "OLED display refresh rate and HDR restore requested.", [System.Windows.Forms.ToolTipIcon]::Info)
})

$miRestart.Add_Click({
  if ($script:isMpv -or $script:isStreaming) {
    $ans = [System.Windows.Forms.MessageBox]::Show("Playback is currently active. Restart the TV server anyway?", "Restart Phosphor Server", [System.Windows.Forms.MessageBoxButtons]::YesNo, [System.Windows.Forms.MessageBoxIcon]::Warning)
    if ($ans -ne [System.Windows.Forms.DialogResult]::Yes) { return }
  }
  $tray.ShowBalloonTip(2000, "TV", "Restarting TV server cleanly...", [System.Windows.Forms.ToolTipIcon]::Info)
  $http.RestartServerAsync($tvDir, $watchdogPath)
})

$miLog.Add_Click({
  if (Test-Path -LiteralPath $logPath) {
    Start-Process -FilePath "notepad.exe" -ArgumentList "`"$logPath`""
  } else {
    $tray.ShowBalloonTip(2500, "TV", "Live log file not found yet.", [System.Windows.Forms.ToolTipIcon]::Warning)
  }
})

$miBackup.Add_Click({
  try {
    $apiOk = $http.PostSync("/api/backup", "{}", 2500)
    $msg = "State backup saved."
    if (-not $apiOk) {
      if (Test-Path -LiteralPath $statePath) {
        $bakDir = Join-Path $tvDir "data\backups"
        if (-not (Test-Path -LiteralPath $bakDir)) { New-Item -ItemType Directory -Path $bakDir -Force | Out-Null }
        $stamp = Get-Date -Format "yyyyMMdd-HHmmss"
        $dest = Join-Path $bakDir "state-$stamp.json"
        Copy-Item -LiteralPath $statePath -Destination $dest -Force
        Get-ChildItem -LiteralPath $bakDir -Filter "state-*.json" | Sort-Object LastWriteTime -Descending | Select-Object -Skip 10 | Remove-Item -Force -ErrorAction SilentlyContinue
        $msg = "Saved data\backups\state-$stamp.json"
      } else {
        $msg = "data\state.json not found."
      }
    }
    $tray.ShowBalloonTip(2500, "TV", $msg, [System.Windows.Forms.ToolTipIcon]::Info)
  } catch {
    $tray.ShowBalloonTip(2500, "TV", "Could not back up state.", [System.Windows.Forms.ToolTipIcon]::Warning)
  }
})

$menu.Add_Opening({
  $miAutoStart.Checked = Test-Path -LiteralPath $startupLnk
  $http.RequestPoll()
})

$miAutoStart.Add_Click({
  try {
    if (Test-Path -LiteralPath $startupLnk) {
      Remove-Item -LiteralPath $startupLnk -Force -ErrorAction Stop
      $miAutoStart.Checked = $false
      $tray.ShowBalloonTip(2000, "TV", "Logon auto-start disabled.", [System.Windows.Forms.ToolTipIcon]::Info)
    } else {
      if (-not (Test-Path -LiteralPath $startupDir)) {
        New-Item -ItemType Directory -Path $startupDir -Force | Out-Null
      }
      $wsh = New-Object -ComObject WScript.Shell
      try {
        $sc = $wsh.CreateShortcut($startupLnk)
        $sc.TargetPath = "$env:WINDIR\System32\wscript.exe"
        $sc.Arguments = "`"$vbsTrayPath`""
        $sc.WorkingDirectory = Split-Path -Parent $vbsTrayPath
        $sc.Description = "Personal TV System Tray Companion"
        $sc.Save()
      } finally {
        [System.Runtime.InteropServices.Marshal]::ReleaseComObject($wsh) | Out-Null
      }
      $miAutoStart.Checked = Test-Path -LiteralPath $startupLnk
      $tray.ShowBalloonTip(2000, "TV", "TV Tray will start automatically at Windows logon.", [System.Windows.Forms.ToolTipIcon]::Info)
    }
  } catch {
    $miAutoStart.Checked = Test-Path -LiteralPath $startupLnk
    $tray.ShowBalloonTip(2500, "TV", "Could not update Startup shortcut.", [System.Windows.Forms.ToolTipIcon]::Warning)
  }
})

$miQuitTray.Add_Click({
  $timer.Stop()
  $hk.SetActive($false)
  $tray.Visible = $false
  [System.Windows.Forms.Application]::Exit()
})

$miQuitAll.Add_Click({
  $timer.Stop()
  $hk.SetActive($false)
  $tray.Text = "TV: Shutting down..."
  Stop-TvServerGracefully
  $tray.Visible = $false
  [System.Windows.Forms.Application]::Exit()
})

$hk.Add_HotkeyPressed({
  param($sender, $e)
  switch ($e.Id) {
    1001 { Post-Async "/api/mpv/control" @{ cmd = "toggle" } }
    1002 { Post-Async "/api/mpv/control" @{ cmd = "seek"; v = 15 } }
    1003 { Post-Async "/api/mpv/control" @{ cmd = "seek"; v = -15 } }
    1004 {
      $hk.SetActive($false)
      Post-Async "/api/mpv/stop" @{}
    }
  }
})

$applySnapshot = {
  $snap = $http.GetSnapshot()
  if ($snap.Seq -ne $script:lastSeq) {
    $script:lastSeq = $snap.Seq
    if ($snap.Online) {
      $script:isOnline = $true
      $st = $null; $mp = $null
      try { if ($snap.StatusJson) { $st = $snap.StatusJson | ConvertFrom-Json } } catch {}
      try { if ($snap.MpvJson) { $mp = $snap.MpvJson | ConvertFrom-Json } } catch {}
      $mpvPlaying = ($null -ne $mp -and $mp.playing -eq $true)
      $browserStreaming = ($null -ne $st -and $st.streaming -eq $true)
      $script:isMpv = $mpvPlaying
      $script:isStreaming = $browserStreaming
      $hk.SetActive($mpvPlaying)
      if ($mpvPlaying -and $mp.session) {
        $t = ($mp.session.title -as [string])
        if ($t.Length -gt 45) { $t = $t.Substring(0, 42) + "..." }
        $txt = "TV (mpv): $t"
        $miPause.Enabled = $true; $miSkip.Enabled = $true; $miStop.Enabled = $true
      } elseif ($mpvPlaying) {
        $txt = "TV (mpv): Playing"
        $miPause.Enabled = $true; $miSkip.Enabled = $true; $miStop.Enabled = $true
      } elseif ($browserStreaming) {
        $txt = "TV: Streaming in browser"
        $miPause.Enabled = $true; $miSkip.Enabled = $true; $miStop.Enabled = $true
      } else {
        $txt = "TV: Ready"
        $miPause.Enabled = $false; $miSkip.Enabled = $false; $miStop.Enabled = $false
      }
      $miStatus.Text = $txt
      $tray.Text = if ($txt.Length -gt 63) { $txt.Substring(0, 63) } else { $txt }
    } else {
      $script:isOnline = $false
      $script:isMpv = $false
      $script:isStreaming = $false
      $hk.SetActive($false)
      $miStatus.Text = "TV: Server offline"
      $tray.Text = "TV: Server offline"
      $miPause.Enabled = $false; $miSkip.Enabled = $false; $miStop.Enabled = $false
    }
  }
  $http.RequestPoll()
}

$timer = New-Object System.Windows.Forms.Timer
$timer.Interval = 2500
$timer.Add_Tick($applySnapshot)
$http.RequestPoll()
$timer.Start()

try {
  [System.Windows.Forms.Application]::Run()
} finally {
  $timer.Stop()
  $timer.Dispose()
  $hk.SetActive($false)
  $hk.Dispose()
  $tray.Visible = $false
  $tray.Dispose()
  if ($icon) { $icon.Dispose() }
  if ($menu) { $menu.Dispose() }
  try { $mutex.ReleaseMutex() } catch {}
  $mutex.Dispose()
}
