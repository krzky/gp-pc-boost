@echo off
net session >nul 2>&1
if %errorlevel% neq 0 (
    powershell.exe -NoProfile -ExecutionPolicy Bypass -Command "Start-Process -FilePath '%~f0' -Verb RunAs"
    exit /b
)
powershell.exe -NoProfile -ExecutionPolicy Bypass -File "%~dp0scripts\update-topbar-win12.ps1" -Layout win12_split -DockEdge top -Align system -MultiMon system -Theme system
