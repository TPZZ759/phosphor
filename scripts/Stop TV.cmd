@echo off
rem Stops the TV server and any stream it is running.
for /f "tokens=5" %%p in ('netstat -ano ^| findstr ":7878 " ^| findstr LISTENING') do taskkill /PID %%p /T /F >nul 2>&1
echo TV stopped.
timeout /t 2 >nul
