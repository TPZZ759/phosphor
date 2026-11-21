' Starts the TV server (hidden) if it isn't running, then opens the app window.
Set sh = CreateObject("WScript.Shell")
Set fso = CreateObject("Scripting.FileSystemObject")
dir = fso.GetParentFolderName(WScript.ScriptFullName)

running = False
On Error Resume Next
Set http = CreateObject("MSXML2.ServerXMLHTTP.6.0")
http.setTimeouts 1000, 1000, 1000, 1000
http.open "GET", "http://127.0.0.1:7878/api/status", False
http.send
If Err.Number = 0 Then
  If http.status = 200 Or http.status = 503 Then running = True
End If
On Error GoTo 0

If Not running Then
  sh.CurrentDirectory = dir
  If fso.FileExists(dir & "\tv-server.exe") Then
    sh.Run "cmd /c """ & dir & "\tv-server.exe"" >> data\server.log 2>&1", 0, False
  Else
    sh.Run "cmd /c node server.js >> data\server.log 2>&1", 0, False
  End If
  WScript.Sleep 2500
End If

edge = sh.ExpandEnvironmentStrings("%ProgramFiles(x86)%") & "\Microsoft\Edge\Application\msedge.exe"
If Not fso.FileExists(edge) Then edge = sh.ExpandEnvironmentStrings("%ProgramFiles%") & "\Microsoft\Edge\Application\msedge.exe"
If fso.FileExists(edge) Then
  sh.Run """" & edge & """ --app=http://127.0.0.1:7878 --start-maximized", 1, False
Else
  sh.Run "http://127.0.0.1:7878", 1, False
End If
