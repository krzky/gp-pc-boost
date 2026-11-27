# Self-elevating Express WinRust preset (adds topbar + rust-search; keeps explorer.exe shell)
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

Write-Host "=== Express WinRust Replacement (keeps explorer.exe shell) ===" -ForegroundColor Cyan
Write-Host "NOTE: rust-search indexes local NTFS drives (C:, D:, F:, G:) and skips mapped SMB drives (N:-Z:)." -ForegroundColor Yellow
& $exe power set high
& $exe mem purge
& $exe replace apply express

Write-Host "`nDone! Run scripts\rollback-all.ps1 anytime to revert." -ForegroundColor Green
pause
