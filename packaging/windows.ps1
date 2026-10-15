# Builds tinyclient's Windows downloads into dist\: the installer (Inno Setup, per user, no
# administrator needed) and the portable zip of the four programs. The release workflow runs it on
# every v* tag (.github/workflows/release.yml).
#
#   packaging\windows.ps1                        build --release, then package as tinyclient-v<version>-windows-x86_64*
#   packaging\windows.ps1 -NoBuild -Label LABEL  package what is already in target\release as tinyclient-LABEL-windows-x86_64*
#
# Building needs protoc on the PATH and WebView2 (current Windows includes it). Packaging needs
# Inno Setup 6 (`choco install innosetup`, or jrsoftware.org); iscc.exe is found on the PATH or in
# its default folder. The installer is not code-signed, so SmartScreen warns before the first run.
param(
    [switch]$NoBuild,
    [string]$Label = ""
)
$ErrorActionPreference = "Stop"

$root = Split-Path -Parent $PSScriptRoot
$release = Join-Path $root "target\release"
$version = (Select-String -Path (Join-Path $root "Cargo.toml") -Pattern '^version = "(.*)"' | Select-Object -First 1).Matches[0].Groups[1].Value
if (-not $Label) { $Label = "v$version" }
$name = "tinyclient-$Label-windows-x86_64"
$dist = Join-Path $root "dist"
$programs = "tinyclient.exe", "chatd.exe", "mediad.exe", "tinyclient-login.exe"

if (-not $NoBuild) {
    cargo build --release -p tinyclient --features tinyclient/gui -p chatd -p mediad -p login
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
}
foreach ($program in $programs) {
    if (-not (Test-Path (Join-Path $release $program))) { throw "windows.ps1: $release\$program is not built" }
}
New-Item -ItemType Directory -Force -Path $dist | Out-Null

# Portable: the four programs and the licences, unzipped anywhere and run in place.
$portable = Join-Path ([System.IO.Path]::GetTempPath()) "tinyclient-portable"
Remove-Item -Recurse -Force $portable -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Path $portable | Out-Null
foreach ($program in $programs) { Copy-Item (Join-Path $release $program) $portable }
Copy-Item (Join-Path $root "LICENSE") (Join-Path $portable "LICENSE.txt")
Copy-Item (Join-Path $root "packaging\AGPL-3.0.txt") $portable
$zip = Join-Path $dist "$name.zip"
Remove-Item -Force $zip -ErrorAction SilentlyContinue
Compress-Archive -Path (Join-Path $portable "*") -DestinationPath $zip
Remove-Item -Recurse -Force $portable

# The installer.
$iscc = (Get-Command iscc.exe -ErrorAction SilentlyContinue).Source
if (-not $iscc) {
    foreach ($dir in "${env:ProgramFiles(x86)}\Inno Setup 6", "$env:ProgramFiles\Inno Setup 6", "$env:LOCALAPPDATA\Programs\Inno Setup 6") {
        if (Test-Path "$dir\ISCC.exe") { $iscc = "$dir\ISCC.exe"; break }
    }
}
if (-not $iscc) { throw "windows.ps1: Inno Setup 6 (iscc.exe) is not installed" }
& $iscc /Q "/DVersion=$version" "/DSource=$release" "/DOutputDir=$dist" "/DOutput=$name-setup" (Join-Path $root "packaging\windows\tinyclient.iss")
if ($LASTEXITCODE -ne 0) { throw "windows.ps1: iscc failed ($LASTEXITCODE)" }

Get-ChildItem (Join-Path $dist "$name*") | Format-Table Name, Length
