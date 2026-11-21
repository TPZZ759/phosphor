@echo off
rem Stops the Phosphor server and any active stream.
for /f "tokens=5" %%p in ('netstat -ano ^| findstr ":7878 " ^| findstr LISTENING') do taskkill /PID %%p /T /F >nul 2>&1
echo Phosphor stopped.
timeout /t 2 >nul
