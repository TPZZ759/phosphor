' Keeps the Phosphor server running: started at logon and every 5 minutes by Task Scheduler ("Phosphor server ...").
' Starts phosphor-server.exe (hidden) if it isn't answering, recovers hung servers after 3 consecutive failed
' health checks across 15s, and rotates data\server-rs.log when > 10 MB. No window, no browser.
Set sh = CreateObject("WScript.Shell")
Set fso = CreateObject("Scripting.FileSystemObject")
dir = fso.GetParentFolderName(WScript.ScriptFullName)
logFile = dir & "\data\server-rs.log"
logBak  = dir & "\data\server-rs.log.1"

Function IsDeploying()
  IsDeploying = False
  If fso.FileExists(dir & "\data\deploying") Then
    On Error Resume Next
    If DateDiff("n", fso.GetFile(dir & "\data\deploying").DateLastModified, Now) < 10 Then IsDeploying = True
    On Error GoTo 0
  End If
End Function

Function Up()
  Up = False
  On Error Resume Next
  Set http = CreateObject("MSXML2.ServerXMLHTTP.6.0")
  http.setTimeouts 2000, 2000, 3000, 3000
  http.open "GET", "http://127.0.0.1:7878/api/status", False
  http.send
  If Err.Number = 0 Then
    If http.status = 200 Or http.status = 503 Then Up = True
  End If
  On Error GoTo 0
End Function

Sub RotateLogIfLarge()
  On Error Resume Next
  If fso.FileExists(logFile) Then
    If fso.GetFile(logFile).Size > 10485760 Then
      If fso.FileExists(logBak) Then fso.DeleteFile logBak, True
      fso.MoveFile logFile, logBak
    End If
  End If
  On Error GoTo 0
End Sub

Sub AppendWatchdogLog(msg)
  On Error Resume Next
  Set ts = fso.OpenTextFile(logFile, 8, True)
  ts.WriteLine "[" & Now & "] [watchdog] " & msg
  ts.Close
  On Error GoTo 0
End Sub

If IsDeploying() Then WScript.Quit

' Healthy fast-path
If Up() Then
  RotateLogIfLarge
  WScript.Quit
End If

' Health check retry: 3 failed checks across ~15 seconds before intervening
For attempt = 2 To 3
  WScript.Sleep 5000
  If IsDeploying() Then WScript.Quit
  If Up() Then WScript.Quit
Next
WScript.Sleep 5000
If IsDeploying() Then WScript.Quit
If Up() Then WScript.Quit

' Still unresponsive after 3 checks across 15s: terminate hung phosphor-server.exe if present
Set wmi = GetObject("winmgmts:\\.\root\cimv2")
Set procs = wmi.ExecQuery("select * from Win32_Process where Name='phosphor-server.exe'")
If procs.Count > 0 Then
  AppendWatchdogLog "phosphor-server.exe unresponsive across 3 checks (15s); terminating hung process."
  On Error Resume Next
  For Each p In procs
    p.Terminate()
  Next
  On Error GoTo 0
  WScript.Sleep 2000
End If

RotateLogIfLarge
sh.CurrentDirectory = dir
If fso.FileExists(dir & "\phosphor-server.exe") Then
  sh.Run "cmd /c """ & dir & "\phosphor-server.exe"" >> data\server-rs.log 2>&1", 0, False
End If
