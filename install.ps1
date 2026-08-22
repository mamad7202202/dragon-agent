# Dragon Agent installer - Windows 10/11
#   irm https://raw.githubusercontent.com/mamad7202202/dragon-agent/main/install.ps1 | iex

$ErrorActionPreference = "Stop"

$repo = "mamad7202202/dragon-agent"
$url = "https://github.com/$repo/releases/download/latest/dragon-x86_64-windows.exe"
$dir = Join-Path $env:LOCALAPPDATA "Programs\dragon"
$exe = Join-Path $dir "dragon.exe"

Write-Host ""
Write-Host "dragon installer" -ForegroundColor Yellow
Write-Host "  from : $url"
Write-Host "  to   : $exe"

New-Item -ItemType Directory -Force -Path $dir | Out-Null
Invoke-WebRequest -Uri $url -OutFile $exe

$userPath = [Environment]::GetEnvironmentVariable("Path", "User")
if ($userPath -notlike "*$dir*") {
    [Environment]::SetEnvironmentVariable("Path", "$userPath;$dir", "User")
    Write-Host ""
    Write-Host "added $dir to your user PATH" -ForegroundColor Green
}

Write-Host ""
Write-Host "installed -> $exe" -ForegroundColor Green
Write-Host ""
Write-Host "open a NEW terminal and run: dragon"
Write-Host "(the app walks you through adding your API key on first start)"
