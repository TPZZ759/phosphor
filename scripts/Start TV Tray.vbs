' Launches the hidden TV system tray companion (no console window).
Set sh = CreateObject("WScript.Shell")
Set fso = CreateObject("Scripting.FileSystemObject")
dir = fso.GetParentFolderName(WScript.ScriptFullName)
sh.CurrentDirectory = dir
sh.Run "powershell.exe -NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File """ & dir & "\tv-tray.ps1""", 0, False
