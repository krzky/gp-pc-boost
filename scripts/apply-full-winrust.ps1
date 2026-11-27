# Self-elevating Full WinRust preset (replaces explorer.exe shell with rust-explorer + rs-shell)
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

Write-Host "=== FULL WinRust Shell Replacement Warning ===" -ForegroundColor Yellow
Write-Host "This will replace explorer.exe as the Windows shell with topbar + rust-explorer."
Write-Host "Known caveats on this PC:"
Write-Host "  - rust-search (Alt+Space) skips your 8 mapped SMB drives (N:-Z:)."
Write-Host "  - rust-explorer lacks Win+V clipboard history, MTP phone browsing, and Network share discovery."
Write-Host "  - Some Microsoft Store / Microsoft sign-in dialogs need Explorer temporarily."
$confirm = Read-Host "`nType YES to proceed with full WinRust replacement"
if ($confirm -ne "YES") {
    Write-Host "Cancelled. Nothing was changed."
    pause
    exit 0
}

& $exe power set high
& $exe mem purge
& $exe replace apply full --allow-shell-replace

Write-Host "`nFull WinRust stack applied! Run scripts\rollback-all.ps1 anytime to restore Explorer." -ForegroundColor Green
pause
