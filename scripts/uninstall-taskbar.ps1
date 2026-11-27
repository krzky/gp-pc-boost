param(
    [switch]$RemoveConfig
)

$ErrorActionPreference = "Stop"

$installRoot = Join-Path $env:LOCALAPPDATA "gp-pc-boost"
$installDir = Join-Path $installRoot "bin"
$taskbarExe = Join-Path $installDir "gp-taskbar.exe"

Write-Host "Uninstalling gp-taskbar and restoring stock Windows taskbar..." -ForegroundColor Cyan

if (Test-Path $taskbarExe) {
    Start-Process -FilePath $taskbarExe -ArgumentList "--restore" -Wait -WindowStyle Hidden -ErrorAction SilentlyContinue
}
Get-Process -Name "gp-taskbar" -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
Start-Sleep -Milliseconds 300

$prevEap = $ErrorActionPreference
$ErrorActionPreference = "Continue"
reg.exe delete "HKCU\Software\Microsoft\Windows\CurrentVersion\Run" /v "gp-taskbar" /f >$null 2>$null
$ErrorActionPreference = $prevEap

if (Test-Path $installDir) {
    Remove-Item -Recurse -Force $installDir -ErrorAction SilentlyContinue
    Write-Host "  Removed $installDir" -ForegroundColor Gray
}

if ($RemoveConfig) {
    $cfgDir = Join-Path $env:APPDATA "gp-pc-boost"
    if (Test-Path $cfgDir) {
        Remove-Item -Recurse -Force $cfgDir -ErrorAction SilentlyContinue
        Write-Host "  Removed config directory $cfgDir" -ForegroundColor Gray
    }
}

Write-Host ""
Write-Host "Done. Stock Windows taskbar has been restored." -ForegroundColor Green
