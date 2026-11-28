@echo off
setlocal
rem Gracefully stops the Phosphor server (flushing state and restoring OLED display),
rem falling back to taskkill if it does not exit within a few seconds.

echo Requesting graceful TV shutdown...
curl.exe -s -m 4 -X POST http://127.0.0.1:7878/api/shutdown >nul 2>&1

rem Wait up to 4 seconds for port 7878 to stop listening
for /l %%i in (1,1,4) do (
  netstat -ano | findstr ":7878 " | findstr LISTENING >nul 2>&1
  if errorlevel 1 goto :stopped
  timeout /t 1 /nobreak >nul
)

echo Graceful shutdown timed out; forcing stop...
for /f "tokens=5" %%p in ('netstat -ano ^| findstr ":7878 " ^| findstr LISTENING') do taskkill /PID %%p /T /F >nul 2>&1
taskkill /IM phosphor-server.exe /T /F >nul 2>&1

:stopped
echo Phosphor stopped.
echo Note: Task Scheduler "Phosphor server (keep running)" automatically restarts the server every 5 minutes unless disabled.
timeout /t 3 >nul
