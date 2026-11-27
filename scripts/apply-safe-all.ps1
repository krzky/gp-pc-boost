# Self-elevating Safe-All WinRust preset (zero workbench / shell disruption)
$isAdmin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
if (-not $isAdmin) {
    Start-Process powershell.exe -Verb RunAs -ArgumentList "-NoProfile -ExecutionPolicy Bypass -File `"$PSCommandPath`""
    exit
}

$exe = "D:\Project\gp-pc-boost\target\release\gp-pc-boost.exe"
if (-not (Test-Path $exe)) {
    Write-Error "Binary not found: $exe. Run cargo build --release first."
    pause
    exit 1
}

Write-Host "=== Step 1: Pre-change backup ===" -ForegroundColor Cyan
& $exe backup create

Write-Host "`n=== Step 2: Power plan -> High performance & Standby RAM purge ===" -ForegroundColor Cyan
& $exe power set high
& $exe mem purge

Write-Host "`n=== Step 3: Replacing Task Manager, Settings, Photo Viewer, WizTree + Reversible Debloat ===" -ForegroundColor Cyan
& $exe replace apply safe-all

Write-Host "`n=== Step 4: Optional Irreversible Junk Appx Purge ===" -ForegroundColor Yellow
& $exe bloat preview
$ansJunk = Read-Host "`nPermanently purge Tier 1 Pure Junk Appx apps + telemetry tasks (keeps Teams, Outlook, OneDrive, Edge)? [y/N]"
if ($ansJunk -match '^(y|yes)$') {
    $ansOd = Read-Host "Also uninstall OneDrive (currently empty on this PC)? [y/N]"
    if ($ansOd -match '^(y|yes)$') {
        & $exe bloat purge --tier junk --onedrive --confirm-irreversible
    } else {
        & $exe bloat purge --tier junk --confirm-irreversible
    }
}

Write-Host "`nDone! Run scripts\rollback-all.ps1 anytime to revert WinRust components." -ForegroundColor Green
pause
