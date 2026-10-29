<#
.SYNOPSIS
  Builds dist\Downpour-Setup.exe: one file holding the app, the browser
  helper, the command line tool, and the signed Firefox add-on.

.PARAMETER AmoCredentials
  Path to an env file with JWT_issuer / JWT_secret (addons.mozilla.org API
  keys). When given, the Firefox add-on is signed first (unlisted channel).
  Without it, an already signed dist\extensions\downpour-firefox.xpi of the
  current version is used.

.EXAMPLE
  .\scripts\build-installer.ps1 -AmoCredentials "path\to\mozilla.env"
#>
param(
    [string] $AmoCredentials
)
$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
Push-Location $root
# The compiler writes source paths into panic messages. Released programs
# should not carry the build machine's folders or user name, so they are
# rewritten to neutral prefixes.
$cargoHome = if ($env:CARGO_HOME) { $env:CARGO_HOME } else { Join-Path $env:USERPROFILE ".cargo" }
$previousRustFlags = $env:RUSTFLAGS
$env:RUSTFLAGS = (@($previousRustFlags) + @(
    "--remap-path-prefix=$cargoHome=cargo",
    "--remap-path-prefix=$root=downpour",
    "--remap-path-prefix=$env:USERPROFILE=home"
) | Where-Object { $_ }) -join " "
try {
    Write-Host "Building the app and the browser helper (release)..."
    cargo build --release -p downpour-app -p downpour-host -p downpour-cli
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }

    & (Join-Path $PSScriptRoot "package-extensions.ps1")

    $signed = Join-Path $root "dist\extensions\downpour-firefox.xpi"
    if ($AmoCredentials) {
        node (Join-Path $PSScriptRoot "sign-firefox.mjs") --credentials $AmoCredentials
        if ($LASTEXITCODE -ne 0) { throw "signing the Firefox add-on failed" }
    }
    if (-not (Test-Path $signed)) {
        throw "No signed Firefox add-on at $signed. Run with -AmoCredentials <mozilla.env> once."
    }
    # Release Firefox refuses unsigned add-ons, and a stale signed copy would
    # install old code: the signed file must match the manifest's version.
    $wanted = (Get-Content (Join-Path $root "extension\firefox\manifest.json") -Raw | ConvertFrom-Json).version
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $zip = [System.IO.Compression.ZipFile]::OpenRead($signed)
    try {
        $entry = $zip.GetEntry("manifest.json")
        $reader = New-Object System.IO.StreamReader($entry.Open())
        $signedVersion = ($reader.ReadToEnd() | ConvertFrom-Json).version
        $reader.Close()
        $isSigned = $null -ne $zip.GetEntry("META-INF/mozilla.rsa")
    } finally {
        $zip.Dispose()
    }
    if (-not $isSigned) { throw "$signed is not signed by Mozilla." }
    if ($signedVersion -ne $wanted) {
        throw "The signed add-on is version $signedVersion but the manifest says $wanted. Sign again with -AmoCredentials."
    }

    $stage = Join-Path $root "target\installer-payload"
    if (Test-Path $stage) { Remove-Item -Recurse -Force $stage }
    New-Item -ItemType Directory -Force (Join-Path $stage "extensions") | Out-Null
    Copy-Item (Join-Path $root "target\release\downpour.exe") $stage
    Copy-Item (Join-Path $root "target\release\downpour-host.exe") $stage
    Copy-Item (Join-Path $root "target\release\downpour-cli.exe") $stage
    Copy-Item $signed (Join-Path $stage "extensions\downpour-firefox.xpi")

    Write-Host "Building the installer..."
    $env:DOWNPOUR_PAYLOAD_DIR = $stage
    try {
        cargo build --release -p downpour-setup
        if ($LASTEXITCODE -ne 0) { throw "cargo build of the installer failed" }
    } finally {
        Remove-Item Env:DOWNPOUR_PAYLOAD_DIR
    }
    $installer = Join-Path $root "dist\Downpour-Setup.exe"
    Copy-Item (Join-Path $root "target\release\Downpour-Setup.exe") $installer -Force
    $size = [math]::Round((Get-Item $installer).Length / 1MB, 1)
    Write-Host "Installer: $installer ($size MB)"
} finally {
    $env:RUSTFLAGS = $previousRustFlags
    Pop-Location
}
