<#
.SYNOPSIS
  Assembles the Firefox add-on from extension/common plus
  extension/firefox/manifest.json into dist/extensions/firefox/, and packs it
  as dist/extensions/downpour-firefox-unsigned.xpi.

.DESCRIPTION
  The folder is what scripts/sign-firefox.mjs submits for signing. The
  unsigned .xpi is for trying the add-on in Firefox Developer Edition or
  Nightly. The signed add-on, dist/extensions/downpour-firefox.xpi, is written
  only by sign-firefox.mjs, so packaging never replaces it with an unsigned one.
#>
$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
$common = Join-Path $root "extension\common"
$manifest = Join-Path $root "extension\firefox\manifest.json"
$out = Join-Path $root "dist\extensions"
$target = Join-Path $out "firefox"
$archive = Join-Path $out "downpour-firefox-unsigned.xpi"

if (Test-Path $target) { Remove-Item -Recurse -Force $target }
New-Item -ItemType Directory -Force $target | Out-Null
Get-ChildItem $common -Exclude "*.test.js" | Copy-Item -Destination $target -Recurse
Copy-Item $manifest $target

# An .xpi is a zip. Entries are written by hand because Compress-Archive in
# Windows PowerShell 5.1 stores folders with backslashes, which Firefox rejects.
if (Test-Path $archive) { Remove-Item -Force $archive }
Add-Type -AssemblyName System.IO.Compression
Add-Type -AssemblyName System.IO.Compression.FileSystem
$zip = [System.IO.Compression.ZipFile]::Open($archive, [System.IO.Compression.ZipArchiveMode]::Create)
try {
    $prefix = (Resolve-Path $target).Path.TrimEnd("\") + "\"
    foreach ($file in Get-ChildItem $target -Recurse -File) {
        $entry = $file.FullName.Substring($prefix.Length).Replace("\", "/")
        [System.IO.Compression.ZipFileExtensions]::CreateEntryFromFile(
            $zip, $file.FullName, $entry, [System.IO.Compression.CompressionLevel]::Optimal) | Out-Null
    }
} finally {
    $zip.Dispose()
}

Write-Host "firefox -> $target"
Write-Host "           $archive"
