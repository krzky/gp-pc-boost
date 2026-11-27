param(
    [ValidateSet("win12_split", "unified_top", "unified_bottom")]
    [string]$Layout = "win12_split",
    [ValidateSet("bottom", "top")]
    [string]$DockEdge = "top",
    [ValidateSet("compact", "small", "standard", "large")]
    [string]$DockSize = "compact",
    [ValidateSet("system", "center", "left")]
    [string]$Align = "system",
    [ValidateSet("system", "all", "primary")]
    [string]$MultiMon = "system",
    [ValidateSet("system", "dark", "light")]
    [string]$Theme = "system",
    [switch]$AutoHide,
    [switch]$ResetConfig,
    [string]$IniPath = ""
)

$ErrorActionPreference = "Stop"

if ([string]::IsNullOrWhiteSpace($IniPath)) {
    $IniPath = Join-Path (Join-Path $env:APPDATA "gp-pc-boost") "taskbar.ini"
}

$iniDir = Split-Path -Parent $IniPath
New-Item -ItemType Directory -Force -Path $iniDir | Out-Null

# Preserve existing user taskbar.ini preferences unless -ResetConfig or file does not exist yet
if ($ResetConfig -or (-not (Test-Path $IniPath))) {
    $ahVal = if ($AutoHide) { 1 } else { 0 }
    @"
layout=$Layout
dock_edge=$DockEdge
dock_size=$DockSize
align=$Align
multimon=$MultiMon
theme=$Theme
glass=system
autohide=$ahVal
status_autohide=1
tray_mode=auto6
show_previews=1
show_seconds=0
show_search=1
show_taskview=1
show_telemetry=1
show_desktop_btn=1
"@ | Set-Content -Path $IniPath -Encoding ASCII
} else {
    $existing = Get-Content -Path $IniPath -Raw
    if ($existing -notmatch '(?m)^dock_size=') {
        Add-Content -Path $IniPath -Value "dock_size=$DockSize" -Encoding ASCII
    }
}

$src = "D:\Project\gp-pc-boost\target\release\gp-taskbar.exe"
$payloadDst = "D:\Project\rust-goose\WinRust\payload\topbar.exe"
$userDir = Join-Path (Join-Path $env:LOCALAPPDATA "gp-pc-boost") "bin"
$userDst = Join-Path $userDir "gp-taskbar.exe"
$pfDir = Join-Path $env:ProgramFiles "topbar"
$pfDst = Join-Path $pfDir "topbar.exe"
$bak = Join-Path $pfDir "topbar.exe.winrust.bak"

if (-not (Test-Path $src)) {
    throw "Built binary not found: $src"
}

Write-Host "Hot-swapping active taskbar to Fluent 2 Windows 12 gp-taskbar..." -ForegroundColor Cyan
if (Test-Path $userDst) {
    Start-Process -FilePath $userDst -ArgumentList "--restore" -Wait -WindowStyle Hidden -ErrorAction SilentlyContinue
} elseif (Test-Path $pfDst) {
    Start-Process -FilePath $pfDst -ArgumentList "--restore" -Wait -WindowStyle Hidden -ErrorAction SilentlyContinue
}
Get-Process -Name "topbar","reshell","gp-taskbar" -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
Start-Sleep -Milliseconds 300

$prevEap = $ErrorActionPreference
$ErrorActionPreference = "Continue"
reg.exe delete "HKCU\Software\Microsoft\Windows\CurrentVersion\Run" /v "reshell" /f >$null 2>$null
$ErrorActionPreference = $prevEap

New-Item -ItemType Directory -Force -Path $userDir | Out-Null
Copy-Item -Force $src $userDst

if (Test-Path (Split-Path -Parent $payloadDst)) {
    try { Copy-Item -Force $src $payloadDst } catch { Write-Warning "Skipped payload sync to D: ($($_.Exception.Message))" }
}

$id = [Security.Principal.WindowsIdentity]::GetCurrent()
$pr = New-Object Security.Principal.WindowsPrincipal($id)
$launchExe = $userDst
if ($pr.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    New-Item -ItemType Directory -Force -Path $pfDir | Out-Null
    if ((Test-Path $pfDst) -and (-not (Test-Path $bak))) {
        Copy-Item -Force $pfDst $bak
    }
    Copy-Item -Force $src $pfDst
    $launchExe = $pfDst
}

reg.exe add "HKCU\Software\Microsoft\Windows\CurrentVersion\Run" /v "topbar" /t REG_SZ /d "`"$launchExe`"" /f | Out-Null

Write-Host "Deployed Fluent 2 Windows 12 taskbar ($DockSize) -> $launchExe" -ForegroundColor Green
Start-Process -FilePath $launchExe
