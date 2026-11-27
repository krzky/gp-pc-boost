@echo off
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0scripts\uninstall-taskbar.ps1" %*
pause
