@echo off
setlocal
cd /d "%USERPROFILE%\tiny-call-e2e\e2e"
if not exist "%USERPROFILE%\tiny-call-e2e\e2e\artifacts" mkdir "%USERPROFILE%\tiny-call-e2e\e2e\artifacts"
powershell.exe -NoProfile -ExecutionPolicy Bypass -File "%USERPROFILE%\tiny-call-e2e\e2e\scripts\desk.ps1" %1 > "%USERPROFILE%\tiny-call-e2e\e2e\artifacts\desk.log" 2>&1
set rc=%ERRORLEVEL%
> "%USERPROFILE%\tiny-call-e2e\e2e\artifacts\desk.exit" echo %rc%
