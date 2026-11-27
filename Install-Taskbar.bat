@echo off
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0scripts\install-taskbar.ps1" %*
pause
