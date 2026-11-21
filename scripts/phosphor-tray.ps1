# Phosphor system tray companion (lightweight WinForms NotifyIcon).
# Double-click opens Phosphor in your default browser; right-click controls mpv, OLED display restore,
# logon auto-start, and dynamically binds global Windows media keys ONLY while mpv is playing.
Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing

$createdNew = $false
$mutex = New-Object System.Threading.Mutex($true, "Local\PhosphorAppTray", [ref]$createdNew)
if (-not $createdNew) { exit 0 }

if (-not ([System.Management.Automation.PSTypeName]'PhosphorMediaHotkeyWindow').Type) {
  Add-Type -ReferencedAssemblies System.Windows.Forms -TypeDefinition @"
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Windows.Forms;

public class PhosphorHotkeyEventArgs : EventArgs {
    private readonly int _id;
    public int Id { get { return _id; } }
    public PhosphorHotkeyEventArgs(int id) { _id = id; }
}

public class PhosphorMediaHotkeyWindow : NativeWindow, IDisposable {
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

    public event EventHandler<PhosphorHotkeyEventArgs> HotkeyPressed;

    public bool IsActive { get { return _active; } }

    public PhosphorMediaHotkeyWindow() {
        CreateParams cp = new CreateParams();
        cp.Caption = "PhosphorMediaHotkeys";
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
            EventHandler<PhosphorHotkeyEventArgs> handler = HotkeyPressed;
            if (handler != null) {
                handler(this, new PhosphorHotkeyEventArgs(m.WParam.ToInt32()));
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

$base = "http://127.0.0.1:7878"
$startupDir = Join-Path $env:APPDATA "Microsoft\Windows\Start Menu\Programs\Startup"
$startupLnk = Join-Path $startupDir "Phosphor Tray.lnk"
$vbsPath = Join-Path $PSScriptRoot "Start Phosphor Tray.vbs"

# Draw a crisp 32x32 Phosphor icon (pure black + warm amber #ffb547)
$bmp = New-Object System.Drawing.Bitmap(32, 32)
$g = [System.Drawing.Graphics]::FromImage($bmp)
$g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
$g.Clear([System.Drawing.Color]::Transparent)
$bgBrush = New-Object System.Drawing.SolidBrush([System.Drawing.Color]::FromArgb(255, 8, 9, 12))
$amber = [System.Drawing.Color]::FromArgb(255, 255, 181, 71)
$pen = New-Object System.Drawing.Pen($amber, 2.5)
$g.FillRectangle($bgBrush, 2, 5, 28, 22)
$g.DrawRectangle($pen, 2, 5, 28, 22)
$tri = [System.Drawing.Point[]]@(
  (New-Object System.Drawing.Point(12, 10)),
  (New-Object System.Drawing.Point(23, 16)),
  (New-Object System.Drawing.Point(12, 22))
)
$playBrush = New-Object System.Drawing.SolidBrush($amber)
$g.FillPolygon($playBrush, $tri)
$g.Dispose()
$icon = [System.Drawing.Icon]::FromHandle($bmp.GetHicon())

$tray = New-Object System.Windows.Forms.NotifyIcon
$tray.Icon = $icon
$tray.Text = "Phosphor: Starting..."
$tray.Visible = $true

$menu = New-Object System.Windows.Forms.ContextMenuStrip
$miStatus = $menu.Items.Add("Phosphor: Checking...")
$miStatus.Enabled = $false
$menu.Items.Add("-") | Out-Null
$miOpen = $menu.Items.Add("Open Phosphor")
$menu.Items.Add("-") | Out-Null
$miPause = $menu.Items.Add("Pause / Play (mpv)")
$miSkip = $menu.Items.Add("Skip Intro (mpv)")
$miStop = $menu.Items.Add("Stop mpv")
$menu.Items.Add("-") | Out-Null
$miRestore = $menu.Items.Add("Restore OLED Display (Hz / HDR)")
$miAutoStart = New-Object System.Windows.Forms.ToolStripMenuItem("Start Tray at Windows Logon")
$miAutoStart.Checked = Test-Path -LiteralPath $startupLnk
$menu.Items.Add($miAutoStart) | Out-Null
$menu.Items.Add("-") | Out-Null
$miQuit = $menu.Items.Add("Quit Tray Icon")
$tray.ContextMenuStrip = $menu

$hk = New-Object PhosphorMediaHotkeyWindow

function Post-Api($path, $bodyObj) {
  try {
    $json = $bodyObj | ConvertTo-Json -Compress
    return Invoke-RestMethod -Uri "$base$path" -Method Post -ContentType "application/json" -Body $json -TimeoutSec 2
  } catch {
    return $null
  }
}

$openApp = {
  Start-Process $base
}
$tray.Add_DoubleClick($openApp)
$miOpen.Add_Click($openApp)
$miPause.Add_Click({ Post-Api "/api/mpv/control" @{ cmd = "toggle" } | Out-Null })
$miSkip.Add_Click({ Post-Api "/api/mpv/control" @{ cmd = "skip" } | Out-Null })
$miStop.Add_Click({
  $hk.SetActive($false)
  Post-Api "/api/mpv/stop" @{} | Out-Null
  & $refresh
})
$miRestore.Add_Click({
  Post-Api "/api/display/restore" @{} | Out-Null
  $tray.ShowBalloonTip(2000, "Phosphor", "Display refresh rate and HDR restored.", [System.Windows.Forms.ToolTipIcon]::Info)
})

$menu.Add_Opening({
  $miAutoStart.Checked = Test-Path -LiteralPath $startupLnk
})

$miAutoStart.Add_Click({
  try {
    if (Test-Path -LiteralPath $startupLnk) {
      Remove-Item -LiteralPath $startupLnk -Force -ErrorAction Stop
      $miAutoStart.Checked = $false
      $tray.ShowBalloonTip(2000, "Phosphor", "Logon auto-start disabled.", [System.Windows.Forms.ToolTipIcon]::Info)
    } else {
      if (-not (Test-Path -LiteralPath $startupDir)) {
        New-Item -ItemType Directory -Path $startupDir -Force | Out-Null
      }
      $wsh = New-Object -ComObject WScript.Shell
      try {
        $sc = $wsh.CreateShortcut($startupLnk)
        $sc.TargetPath = "$env:WINDIR\System32\wscript.exe"
        $sc.Arguments = "`"$vbsPath`""
        $sc.WorkingDirectory = Split-Path -Parent $vbsPath
        $sc.Description = "Phosphor System Tray Companion"
        $sc.Save()
      } finally {
        [System.Runtime.InteropServices.Marshal]::ReleaseComObject($wsh) | Out-Null
      }
      $miAutoStart.Checked = Test-Path -LiteralPath $startupLnk
      $tray.ShowBalloonTip(2000, "Phosphor", "Phosphor Tray will start automatically at Windows logon.", [System.Windows.Forms.ToolTipIcon]::Info)
    }
  } catch {
    $miAutoStart.Checked = Test-Path -LiteralPath $startupLnk
    $tray.ShowBalloonTip(2500, "Phosphor", "Could not update Startup shortcut.", [System.Windows.Forms.ToolTipIcon]::Warning)
  }
})

$miQuit.Add_Click({
  $timer.Stop()
  $hk.SetActive($false)
  $hk.Dispose()
  $tray.Visible = $false
  $tray.Dispose()
  [System.Windows.Forms.Application]::Exit()
})

$hk.Add_HotkeyPressed({
  param($sender, $e)
  switch ($e.Id) {
    1001 {
      $res = Post-Api "/api/mpv/control" @{ cmd = "toggle" }
      if (-not $res -or -not $res.ok) { & $refresh }
    }
    1002 {
      $res = Post-Api "/api/mpv/control" @{ cmd = "seek"; v = 15 }
      if (-not $res -or -not $res.ok) { & $refresh }
    }
    1003 {
      $res = Post-Api "/api/mpv/control" @{ cmd = "seek"; v = -15 }
      if (-not $res -or -not $res.ok) { & $refresh }
    }
    1004 {
      $hk.SetActive($false)
      Post-Api "/api/mpv/stop" @{} | Out-Null
      & $refresh
    }
  }
})

$timer = New-Object System.Windows.Forms.Timer
$timer.Interval = 3000
$refresh = {
  try {
    $st = Invoke-RestMethod -Uri "$base/api/status" -TimeoutSec 2
    $mp = Invoke-RestMethod -Uri "$base/api/mpv" -TimeoutSec 2
    $mpvPlaying = ($mp.playing -eq $true)
    $hk.SetActive($mpvPlaying)
    if ($mpvPlaying -and $mp.session) {
      $t = ($mp.session.title -as [string])
      if ($t.Length -gt 40) { $t = $t.Substring(0, 37) + "..." }
      $txt = "Phosphor (mpv): $t"
      $miPause.Enabled = $true; $miSkip.Enabled = $true; $miStop.Enabled = $true
    } elseif ($mpvPlaying) {
      $txt = "Phosphor (mpv): Playing"
      $miPause.Enabled = $true; $miSkip.Enabled = $true; $miStop.Enabled = $true
    } elseif ($st.streaming) {
      $txt = "Phosphor: Streaming in browser"
      $miPause.Enabled = $false; $miSkip.Enabled = $false; $miStop.Enabled = $false
    } else {
      $txt = "Phosphor: Ready"
      $miPause.Enabled = $false; $miSkip.Enabled = $false; $miStop.Enabled = $false
    }
    $miStatus.Text = $txt
    $tray.Text = if ($txt.Length -gt 63) { $txt.Substring(0, 63) } else { $txt }
  } catch {
    $hk.SetActive($false)
    $miStatus.Text = "Phosphor: Server offline"
    $tray.Text = "Phosphor: Server offline"
    $miPause.Enabled = $false; $miSkip.Enabled = $false; $miStop.Enabled = $false
  }
}
$timer.Add_Tick($refresh)
& $refresh
$timer.Start()

try {
  [System.Windows.Forms.Application]::Run()
} finally {
  $timer.Stop()
  $hk.SetActive($false)
  $hk.Dispose()
  $tray.Visible = $false
  $tray.Dispose()
  try { $mutex.ReleaseMutex() } catch {}
  $mutex.Dispose()
}
