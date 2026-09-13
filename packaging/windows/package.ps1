param(
    [Parameter(Mandatory = $true)]
    [string] $Version
)

$ErrorActionPreference = "Stop"
$root = (Resolve-Path (Join-Path $PSScriptRoot "..\..")).Path
$release = Join-Path $root "target\release"
$dist = Join-Path $root "dist"
$work = Join-Path $root "target\package\windows"
$bundleName = "IronTSC-$Version-windows-x64"
$bundle = Join-Path $work $bundleName
$binary = Join-Path $release "irontsc.exe"

if (-not (Test-Path $binary -PathType Leaf)) {
    throw "Missing release binary: $binary"
}

if (Test-Path $work) {
    Remove-Item $work -Recurse -Force
}
New-Item $bundle -ItemType Directory -Force | Out-Null
New-Item $dist -ItemType Directory -Force | Out-Null
Copy-Item $binary $bundle
Copy-Item (Join-Path $root "README.md") $bundle

$zip = Join-Path $dist "$bundleName.zip"
if (Test-Path $zip) {
    Remove-Item $zip -Force
}
Compress-Archive -Path $bundle -DestinationPath $zip -CompressionLevel Optimal

$match = [regex]::Match($Version, '^(\d+)\.(\d+)\.(\d+)')
if (-not $match.Success) {
    throw "Version '$Version' must begin with MAJOR.MINOR.PATCH"
}
$major = [int]$match.Groups[1].Value
$minor = [int]$match.Groups[2].Value
$patch = [int]$match.Groups[3].Value
if ($major -gt 255 -or $minor -gt 255 -or $patch -gt 65535) {
    throw "Version '$Version' exceeds Windows Installer's 255.255.65535 limit"
}
$productVersion = "$major.$minor.$patch"

$wixRoots = @()
if ($env:WIX_BIN) {
    $wixRoots += $env:WIX_BIN
}
$wixRoots += @(
    "${env:ProgramFiles(x86)}\WiX Toolset v3.14\bin",
    "${env:ProgramFiles(x86)}\WiX Toolset v3.11\bin"
)
$wix = $wixRoots | Where-Object { Test-Path (Join-Path $_ "candle.exe") } | Select-Object -First 1
if (-not $wix) {
    throw "WiX Toolset v3 was not found"
}

$wixObject = Join-Path $work "irontsc.wixobj"
$candle = Join-Path $wix "candle.exe"
$light = Join-Path $wix "light.exe"
& $candle -nologo -arch x64 `
    "-dBinDir=$release" `
    "-dSourceDir=$root" `
    "-dProductVersion=$productVersion" `
    -out $wixObject `
    (Join-Path $root "packaging\windows\irontsc.wxs")
if ($LASTEXITCODE -ne 0) { throw "candle.exe failed with exit code $LASTEXITCODE" }

$msi = Join-Path $dist "$bundleName.msi"
& $light -nologo -out $msi $wixObject
if ($LASTEXITCODE -ne 0) { throw "light.exe failed with exit code $LASTEXITCODE" }

Write-Host "Created $zip"
Write-Host "Created $msi"
