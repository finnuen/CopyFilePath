<#
  Builds CopyFilePath and names the result with the version it contains:

      build\CopyFilePath_v<version>.exe

  The version lives in Cargo.toml and nowhere else: build.rs turns it into the
  .exe's file properties, and the app prints the same value. This script bumps it,
  so nobody has to remember to. Two digits (major.minor): a bare run moves the
  minor digit, 1.0 -> 1.1. Three digits only if the project is ever moved to
  them on purpose.

  .\build.ps1                    bump the minor digit, then build
  .\build.ps1 -KeepVersion       build the current version again
  .\build.ps1 -Version 1.2       set the version, then build
#>
[CmdletBinding()]
param(
    [switch]$KeepVersion,
    [string]$Version
)

$ErrorActionPreference = 'Stop'
$root = $PSScriptRoot
$manifest = Join-Path $root 'Cargo.toml'

# ---- cargo, saying which one is missing rather than failing halfway -----------
$cargo = Get-Command cargo -ErrorAction SilentlyContinue
if (-not $cargo) {
    $fallback = Join-Path $env:USERPROFILE '.cargo\bin\cargo.exe'
    if (Test-Path -LiteralPath $fallback) { $cargo = Get-Command $fallback }
}
if (-not $cargo) {
    throw 'cargo was not found. Install the Rust toolchain (https://rustup.rs) and put cargo on PATH.'
}

# ---- version -----------------------------------------------------------------
# The project version is major.minor. Cargo insists on major.minor.patch, so the
# manifest carries the version with a patch digit that stays 0 and is never shown.
$text = [System.IO.File]::ReadAllText($manifest)
$match = [regex]::Match($text, '(?m)^version\s*=\s*"([^"]+)"')
if (-not $match.Success) { throw "No version line in $manifest" }

$numbers = $match.Groups[1].Value.Split('.')
if ($numbers.Count -lt 2) { throw "Cannot read '$($match.Groups[1].Value)' as major.minor." }
$current = $numbers[0..1] -join '.'

if ($Version) {
    if ($Version -notmatch '^\d+\.\d+$') {
        throw "-Version must look like 1.2, not '$Version'."
    }
    $next = $Version
} elseif ($KeepVersion) {
    $next = $current
} else {
    $next = '{0}.{1}' -f $numbers[0], ([int]$numbers[1] + 1)
}

$nextInManifest = "$next.0"
if ($nextInManifest -ne $match.Groups[1].Value) {
    # The build owns the number, so it writes the one place the number is.
    $text = $text.Remove($match.Groups[1].Index, $match.Groups[1].Length).Insert($match.Groups[1].Index, $nextInManifest)
    [System.IO.File]::WriteAllText($manifest, $text)
    Write-Host "Version $current -> $next"
}

# ---- build -------------------------------------------------------------------
Push-Location $root
try {
    # Two steps: the shell handler DLL first, then the exe that embeds it. The exe's
    # build script cannot see a DLL produced by the same cargo run, hence the order.
    & $cargo.Source build --release --lib
    if ($LASTEXITCODE -ne 0) { throw "cargo build --lib failed (exit $LASTEXITCODE)" }

    $builtDll = Join-Path $root 'target\release\copy_file_path.dll'
    if (-not (Test-Path -LiteralPath $builtDll)) { throw "the shell handler DLL was not produced at $builtDll" }
    $staged = Join-Path $root 'target\shell-helper'
    New-Item -ItemType Directory -Force -Path $staged | Out-Null
    Copy-Item -LiteralPath $builtDll -Destination (Join-Path $staged 'CopyFilePathShell.dll') -Force

    & $cargo.Source build --release --bin CopyFilePath
} finally {
    Pop-Location
}
if ($LASTEXITCODE -ne 0) { throw "cargo build failed (exit $LASTEXITCODE)" }

$built = Join-Path $root 'target\release\CopyFilePath.exe'
if (-not (Test-Path -LiteralPath $built)) { throw "cargo succeeded but $built is missing" }

$outDir = Join-Path $root 'build'
New-Item -ItemType Directory -Force -Path $outDir | Out-Null
$out = Join-Path $outDir "CopyFilePath_v$next.exe"
Copy-Item -LiteralPath $built -Destination $out -Force

# The name says the version; the file properties have to agree with it.
$reported = (Get-Item -LiteralPath $out).VersionInfo.ProductVersion
if ($reported -ne $next) {
    throw "The built file reports version '$reported', expected '$next'."
}

$kilobytes = [math]::Round((Get-Item -LiteralPath $out).Length / 1KB)
Write-Host "Built: $out  ($kilobytes KB)"
