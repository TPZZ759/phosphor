' Starts the Phosphor server (hidden) if it isn't running, then opens the app window.
' Respects data\deploying, avoids spawning duplicates if phosphor-server.exe is already starting,
' and logs to unified data\server-rs.log.
Set sh = CreateObject("WScript.Shell")
Set fso = CreateObject("Scripting.FileSystemObject")
dir = fso.GetParentFolderName(WScript.ScriptFullName)

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
  http.setTimeouts 1000, 1000, 1500, 1500
  http.open "GET", "http://127.0.0.1:7878/api/status", False
  http.send
  If Err.Number = 0 Then
    If http.status = 200 Or http.status = 503 Then Up = True
  End If
  On Error GoTo 0
End Function

Function ServerProcessExists()
  ServerProcessExists = False
  On Error Resume Next
  Set wmi = GetObject("winmgmts:\\.\root\cimv2")
  If wmi.ExecQuery("select * from Win32_Process where Name='phosphor-server.exe'").Count > 0 Then ServerProcessExists = True
  On Error GoTo 0
End Function

' If an idle deploy is mid-swap, wait up to 15s for it to finish instead of racing it
waited = 0
Do While IsDeploying() And waited < 15
  WScript.Sleep 1000
  waited = waited + 1
Loop

running = Up()

If Not running Then
  If ServerProcessExists() Then
    ' Server is already starting up: wait up to 10s instead of spawning a duplicate
    For i = 1 To 10
      WScript.Sleep 1000
      If Up() Then
        running = True
        Exit For
      End If
      If Not ServerProcessExists() Then Exit For
    Next
  End If

  If Not running And Not IsDeploying() And Not ServerProcessExists() Then
    sh.CurrentDirectory = dir
    If fso.FileExists(dir & "\phosphor-server.exe") Then
      sh.Run "cmd /c """ & dir & "\phosphor-server.exe"" >> data\server-rs.log 2>&1", 0, False
    Else
      sh.Run "cmd /c node server.js >> data\server-rs.log 2>&1", 0, False
    End If
    ' Wait up to 10s for the newly started server to answer before opening the window
    For i = 1 To 20
      WScript.Sleep 500
      If Up() Then Exit For
    Next
  End If
End If

edge = sh.ExpandEnvironmentStrings("%ProgramFiles(x86)%") & "\Microsoft\Edge\Application\msedge.exe"
If Not fso.FileExists(edge) Then edge = sh.ExpandEnvironmentStrings("%ProgramFiles%") & "\Microsoft\Edge\Application\msedge.exe"
If fso.FileExists(edge) Then
  sh.Run """" & edge & """ --app=http://127.0.0.1:7878 --start-maximized", 1, False
Else
  sh.Run "http://127.0.0.1:7878", 1, False
End If
