param(
    [switch]$NoStartup,
    [switch]$NoLaunch
)

$ErrorActionPreference = "Stop"

$repoRoot = Split-Path -Parent $PSScriptRoot
$candidates = @(
    $repoRoot,
    (Join-Path $repoRoot "target\release"),
    $PSScriptRoot
)

$binSrc = $null
foreach ($dir in $candidates) {
    $probe = Join-Path $dir "gp-taskbar.exe"
    if (Test-Path $probe) {
        $binSrc = $dir
        break
    }
}

if (-not $binSrc) {
    throw "gp-taskbar.exe not found next to Install-Taskbar.bat or in target\release. Download a release zip from GitHub Releases or run 'cargo build --release' first."
}

$installDir = Join-Path (Join-Path $env:LOCALAPPDATA "gp-pc-boost") "bin"
$taskbarDst = Join-Path $installDir "gp-taskbar.exe"

Write-Host "Installing gp-taskbar (Windows 12 Fluent 2 Taskbar)..." -ForegroundColor Cyan

# Gracefully stop any running gp-taskbar instance first
if (Test-Path $taskbarDst) {
    Start-Process -FilePath $taskbarDst -ArgumentList "--restore" -Wait -WindowStyle Hidden -ErrorAction SilentlyContinue
}
Get-Process -Name "gp-taskbar" -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
Start-Sleep -Milliseconds 250

New-Item -ItemType Directory -Force -Path $installDir | Out-Null

foreach ($exe in @("gp-taskbar.exe", "gp-pc-boost-ui.exe", "gp-pc-boost.exe")) {
    $srcFile = Join-Path $binSrc $exe
    if (Test-Path $srcFile) {
        Copy-Item -Force $srcFile (Join-Path $installDir $exe)
        Write-Host "  Installed $exe -> $installDir" -ForegroundColor Gray
    }
}

if (-not $NoStartup) {
    reg.exe add "HKCU\Software\Microsoft\Windows\CurrentVersion\Run" /v "gp-taskbar" /t REG_SZ /d "`"$taskbarDst`"" /f | Out-Null
    Write-Host "  Enabled automatic start on Windows login (HKCU\...\Run\gp-taskbar)" -ForegroundColor Gray
}

Write-Host ""
Write-Host "Installed successfully to: $installDir" -ForegroundColor Green
Write-Host "  - Right-click the taskbar or click the gear icon for Settings."
Write-Host "  - Right-click -> 'Restore Stock Taskbar' (or run Uninstall-Taskbar.bat) to restore Windows stock taskbar anytime."

if (-not $NoLaunch) {
    Start-Process -FilePath $taskbarDst
    Write-Host "  Launched gp-taskbar.exe" -ForegroundColor Green
}
