' Keeps the TV server running: started at logon and every 5 minutes by Task Scheduler ("TV server ...").
' Starts tv-server.exe (hidden) only if it isn't answering. No window, no browser.
Set sh = CreateObject("WScript.Shell")
Set fso = CreateObject("Scripting.FileSystemObject")
dir = fso.GetParentFolderName(WScript.ScriptFullName)

' an update is being installed: leave it alone
If fso.FileExists(dir & "\data\deploying") Then
  If DateDiff("n", fso.GetFile(dir & "\data\deploying").DateLastModified, Now) < 10 Then WScript.Quit
End If

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

If Up() Then WScript.Quit
' maybe still starting up: give it a moment before starting another copy
WScript.Sleep 5000
If Up() Then WScript.Quit
Set wmi = GetObject("winmgmts:\\.\root\cimv2")
If wmi.ExecQuery("select * from Win32_Process where Name='tv-server.exe'").Count > 0 Then WScript.Quit

sh.CurrentDirectory = dir
If fso.FileExists(dir & "\tv-server.exe") Then
  sh.Run "cmd /c """ & dir & "\tv-server.exe"" >> data\server-rs.log 2>&1", 0, False
End If
