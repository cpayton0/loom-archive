<#
.SYNOPSIS
  Downloads the pinned LGPL FFmpeg build that ships with Loom Archive.

.DESCRIPTION
  Fetches BtbN's static win64 LGPL build, checks its SHA-256, and places
  ffmpeg.exe where Tauri's externalBin expects it:
  src-tauri/binaries/ffmpeg-x86_64-pc-windows-msvc.exe

  The build is configured without --enable-gpl or --enable-nonfree and with
  --enable-version3, so it is licensed LGPL v3 or later. See
  THIRD-PARTY-NOTICES.md before changing the pin.

  Re-running is cheap: an existing binary with the expected hash is kept.
#>
[CmdletBinding()]
param(
  # Overrides the download location, for example a mirror of the same file.
  [string]$Url = $null
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$Release = 'autobuild-2026-11-20-12-40'
$Asset = 'ffmpeg-n9.0.2-61-gd59078794d-win64-lgpl-9.0.zip'
$ZipSha256 = '814c62c35ca7e2b15d85b0a2a5b79bbf21783176520154f26a6df1e9df72761e'
if (-not $Url) { $Url = "https://github.com/BtbN/FFmpeg-Builds/releases/download/$Release/$Asset" }

$Root = Split-Path -Parent $PSScriptRoot
$BinDir = Join-Path $Root 'src-tauri\binaries'
$Target = Join-Path $BinDir 'ffmpeg-x86_64-pc-windows-msvc.exe'
$Stamp = "$Target.sha256"

New-Item -ItemType Directory -Force -Path $BinDir | Out-Null
if ((Test-Path $Target) -and (Test-Path $Stamp)) {
  $expected = (Get-Content $Stamp -Raw).Trim()
  $actual = (Get-FileHash $Target -Algorithm SHA256).Hash.ToLowerInvariant()
  if ($expected -eq $actual) {
    Write-Host "FFmpeg already present: $Target"
    exit 0
  }
}

$Work = Join-Path ([System.IO.Path]::GetTempPath()) ("loom-ffmpeg-" + [guid]::NewGuid())
New-Item -ItemType Directory -Path $Work | Out-Null
try {
  $Zip = Join-Path $Work $Asset
  Write-Host "Downloading $Url"
  $ProgressPreference = 'SilentlyContinue'
  Invoke-WebRequest -Uri $Url -OutFile $Zip -UseBasicParsing

  $hash = (Get-FileHash $Zip -Algorithm SHA256).Hash.ToLowerInvariant()
  if ($hash -ne $ZipSha256) {
    throw "SHA-256 mismatch for $Asset`n  expected $ZipSha256`n  actual   $hash"
  }

  Expand-Archive -Path $Zip -DestinationPath $Work
  $Exe = Get-ChildItem -Path $Work -Recurse -Filter 'ffmpeg.exe' | Select-Object -First 1
  if (-not $Exe) { throw "ffmpeg.exe not found in $Asset" }

  # Refuse a build that is not LGPL, whatever the pin says.
  $conf = & $Exe.FullName -hide_banner -buildconf 2>&1 | Out-String
  if ($conf -match '--enable-gpl|--enable-nonfree') {
    throw "The pinned FFmpeg build is not LGPL-only; refusing to bundle it."
  }
  $protocols = & $Exe.FullName -hide_banner -protocols 2>&1 | Out-String
  foreach ($needed in 'https', 'tls', 'crypto') {
    if ($protocols -notmatch "(?m)^\s*$needed\s*$") { throw "The pinned FFmpeg build lacks the '$needed' protocol." }
  }

  Copy-Item $Exe.FullName $Target -Force
  (Get-FileHash $Target -Algorithm SHA256).Hash.ToLowerInvariant() | Set-Content $Stamp -NoNewline
  Write-Host "FFmpeg ready: $Target"
}
finally {
  Remove-Item -Recurse -Force $Work -ErrorAction SilentlyContinue
}
