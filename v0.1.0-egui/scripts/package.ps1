<#
.SYNOPSIS
    Build Bamboo Music Player and package the release as a zip.

.DESCRIPTION
    Produces dist\bamboo-player-<version>-windows-x64\ with the exe and README,
    then compresses it to a zip and prints its SHA-256. The build embeds the
    application icon, default cover and version information, so the exe is the
    only runtime file required.

.PARAMETER OutDir
    Output directory, relative to the repository root. Default: dist.

.PARAMETER Portable
    Add an empty portable.txt so the packaged copy stores data next to the exe.

.PARAMETER SkipBuild
    Reuse an existing target\release\bamboo-player.exe instead of running cargo.
#>
[CmdletBinding()]
param(
    [string]$OutDir = "dist",
    [switch]$Portable,
    [switch]$SkipBuild
)

$ErrorActionPreference = "Stop"
$Root = Split-Path -Parent $PSScriptRoot
Set-Location $Root

if (-not $SkipBuild) {
    Write-Host "Building release..." -ForegroundColor Cyan
    cargo build --release
    if ($LASTEXITCODE -ne 0) {
        throw "cargo build --release failed with exit code $LASTEXITCODE"
    }
}

$exe = Join-Path $Root "target\release\bamboo-player.exe"
if (-not (Test-Path $exe)) {
    throw "Missing $exe. Run without -SkipBuild or build the release profile first."
}

$manifest = Get-Content (Join-Path $Root "Cargo.toml") -Raw
if ($manifest -notmatch '(?m)^version\s*=\s*"([^"]+)"') {
    throw "Could not read the package version from Cargo.toml"
}
$version = $Matches[1]

$name = "bamboo-player-$version-windows-x64"
$outRoot = Join-Path $Root $OutDir
$stage = Join-Path $outRoot $name
if (Test-Path $stage) {
    Remove-Item -Recurse -Force $stage
}
New-Item -ItemType Directory -Force -Path $stage | Out-Null

Copy-Item $exe (Join-Path $stage "bamboo-player.exe")
foreach ($file in @("README.md", "HANDOFF.md", "LICENSE")) {
    $source = Join-Path $Root $file
    if (Test-Path $source) {
        Copy-Item $source (Join-Path $stage $file)
    }
}
if ($Portable) {
    Set-Content -Path (Join-Path $stage "portable.txt") -Value "" -NoNewline
}

$zip = Join-Path $outRoot "$name.zip"
if (Test-Path $zip) {
    Remove-Item -Force $zip
}
Compress-Archive -Path $stage -DestinationPath $zip -CompressionLevel Optimal

$exeSize = [math]::Round((Get-Item $exe).Length / 1MB, 1)
$zipSize = [math]::Round((Get-Item $zip).Length / 1MB, 1)
$hash = (Get-FileHash $zip -Algorithm SHA256).Hash

Write-Host ""
Write-Host "Packaged $name" -ForegroundColor Green
Write-Host "  exe:  $exe ($exeSize MB)"
Write-Host "  zip:  $zip ($zipSize MB)"
Write-Host "  sha256: $hash"
if ($Portable) {
    Write-Host "  mode: portable (portable.txt included)"
}
