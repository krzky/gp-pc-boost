# Self-elevating 1-Click Full Rollback (restores Explorer, Taskbar, Search, TaskMgr, tweaks, power plan)
$isAdmin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
if (-not $isAdmin) {
    Start-Process powershell.exe -Verb RunAs -ArgumentList "-NoProfile -ExecutionPolicy Bypass -File `"$PSCommandPath`""
    exit
}

$exe = "D:\Project\gp-pc-boost\target\release\gp-pc-boost.exe"
if (-not (Test-Path $exe)) {
    Write-Error "Binary not found: $exe."
    pause
    exit 1
}

Write-Host "=== Rolling back all WinRust replacements and optimizations ===" -ForegroundColor Cyan
& $exe replace revert all
& $exe tune undo
& $exe power restore

Write-Host "`n=== Ensuring Windows Explorer shell is active ===" -ForegroundColor Cyan
$shell = Get-CimInstance Win32_Process -Filter "Name='explorer.exe'" | Where-Object { $_.CommandLine -match '(?i)explorer\.exe"?(\s+/NoUACCheck)?\s*$' }
if (-not $shell) {
    Start-Process "runas.exe" -ArgumentList "/trustlevel:0x20000", "C:\Windows\explorer.exe" -WindowStyle Hidden
    Write-Host "Restarted explorer.exe shell." -ForegroundColor Green
} else {
    Write-Host "explorer.exe shell is already running." -ForegroundColor Green
}

Write-Host "`n=== Mapped Network Drives Check ===" -ForegroundColor Cyan
net.exe use

Write-Host "`nRollback complete!" -ForegroundColor Green
pause
