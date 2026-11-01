#!/usr/bin/env pwsh
# Builds tinyclient's Rust library for Android and packages a debug APK.
#
#   .\scripts\mobile\build-android.ps1                 # arm64-v8a + x86_64, debug APK
#   .\scripts\mobile\build-android.ps1 -Profile release
#   .\scripts\mobile\build-android.ps1 -SkipGradle     # only the .so files
#   .\scripts\mobile\build-android.ps1 -Demo           # debug proof build: made-up chats from launch
#
# Prerequisites are checked here rather than left to fail deep inside cargo or Gradle; every
# failure says what to install. See docs/mobile-build.md.
[CmdletBinding()]
param(
    [ValidateSet('debug', 'release')]
    [string]$Profile = 'debug',

    # Matches abiFilters in mobile/android/app/build.gradle.kts.
    [ValidateSet('arm64-v8a', 'x86_64')]
    [string[]]$Abi = @('arm64-v8a', 'x86_64'),

    [switch]$SkipGradle,

    # A proof build with the made-up chatd on from launch (feature `demo`, src/gui/shot_demo.rs).
    # Never for a release: it shows invented data and never reaches the real core.
    [switch]$Demo
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$root = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
$androidDir = Join-Path $root 'mobile/android'
$jniLibs = Join-Path $androidDir 'app/src/main/jniLibs'

function Fail([string]$message) {
    Write-Host "build-android: $message" -ForegroundColor Red
    exit 1
}

function Note([string]$message) {
    Write-Host "build-android: $message"
}

# --- cargo, rustup ---------------------------------------------------------

# Rust is installed outside PATH on some machines (rustup's default on Windows).
$cargoBin = Join-Path $env:USERPROFILE '.cargo\bin'
if (-not (Get-Command cargo -ErrorAction SilentlyContinue) -and (Test-Path (Join-Path $cargoBin 'cargo.exe'))) {
    $env:PATH = "$cargoBin;$env:PATH"
}
if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    Fail "cargo is not on PATH. Install Rust from https://rustup.rs (it lands in $cargoBin), or add $cargoBin to PATH."
}
if (-not (Get-Command rustup -ErrorAction SilentlyContinue)) {
    Fail "rustup is not on PATH; cargo without rustup cannot add the Android targets. Install from https://rustup.rs."
}

$triples = @{ 'arm64-v8a' = 'aarch64-linux-android'; 'x86_64' = 'x86_64-linux-android' }
$wanted = @($Abi | ForEach-Object { $triples[$_] })
$installed = @(& rustup target list --installed)
$missing = @($wanted | Where-Object { $installed -notcontains $_ })
if ($missing.Count -gt 0) {
    Fail ("Rust targets not installed: {0}. Run: rustup target add {1}" -f ($missing -join ', '), ($missing -join ' '))
}

# --- cargo-ndk -------------------------------------------------------------

& cargo ndk --version *> $null
if ($LASTEXITCODE -ne 0) {
    Fail "cargo-ndk is not installed. Run: cargo install cargo-ndk --version 4.1.2 --locked"
}

# --- NDK -------------------------------------------------------------------

function Find-Ndk {
    foreach ($candidate in @($env:ANDROID_NDK_HOME, $env:ANDROID_NDK_ROOT)) {
        if ($candidate -and (Test-Path (Join-Path $candidate 'source.properties'))) { return $candidate }
    }
    $sdkRoots = @(
        $env:ANDROID_HOME,
        $env:ANDROID_SDK_ROOT,
        (Join-Path $env:LOCALAPPDATA 'Android/Sdk')
    ) | Where-Object { $_ -and (Test-Path (Join-Path $_ 'ndk')) }
    foreach ($sdk in $sdkRoots) {
        $newest = Get-ChildItem (Join-Path $sdk 'ndk') -Directory |
            Sort-Object { try { [version]$_.Name } catch { [version]'0.0' } } -Descending |
            Select-Object -First 1
        if ($newest) { return $newest.FullName }
    }
    return $null
}

$ndk = Find-Ndk
if (-not $ndk) {
    Fail "No Android NDK found. Install NDK r30 (30.0.16248370) in Android Studio's SDK Manager, or set ANDROID_NDK_HOME to its directory."
}
$env:ANDROID_NDK_HOME = $ndk
Note "NDK $ndk"
# Gradle needs the SDK too, and finds it only through ANDROID_HOME or local.properties. An NDK
# under <sdk>/ndk/<version> names the SDK, so a machine with neither set still builds.
$ndkParent = Split-Path -Parent (Split-Path -Parent $ndk)
if (-not $env:ANDROID_HOME -and -not $env:ANDROID_SDK_ROOT -and -not (Test-Path (Join-Path $androidDir 'local.properties')) -and (Test-Path (Join-Path $ndkParent 'platforms'))) {
    $env:ANDROID_HOME = $ndkParent
    Note "SDK $ndkParent (from the NDK's location; set ANDROID_HOME to choose another)"
}

# --- Rust library ----------------------------------------------------------

New-Item -ItemType Directory -Force -Path $jniLibs | Out-Null
$cargoArgs = [System.Collections.Generic.List[string]]::new()
$cargoArgs.Add('ndk')
foreach ($a in $Abi) { $cargoArgs.Add('-t'); $cargoArgs.Add($a) }
$cargoArgs.Add('-o'); $cargoArgs.Add($jniLibs)
# cargo-ndk defaults to API 21, where libnativewindow/libandroid's newer symbols are missing
# (`ld.lld: unable to find library -lnativewindow`). Match minSdk in app/build.gradle.kts.
$cargoArgs.Add('--platform'); $cargoArgs.Add('29')
$cargoArgs.Add('build')
if ($Profile -eq 'release') { $cargoArgs.Add('--release') }
# `--lib` because the package also has the desktop binary; `mobile` is the entry point + gui.
$cargoArgs.Add('-p'); $cargoArgs.Add('tinyclient')
if ($Demo -and $Profile -eq 'release') { Fail "-Demo is for debug proof builds only, never a release." }
$features = if ($Demo) { 'mobile demo' } else { 'mobile' }
$cargoArgs.Add('--lib'); $cargoArgs.Add('--features'); $cargoArgs.Add($features)

Note "cargo $($cargoArgs -join ' ')"
Push-Location $root
try {
    & cargo @cargoArgs
    if ($LASTEXITCODE -ne 0) { Fail "cargo ndk failed with exit code $LASTEXITCODE" }
} finally {
    Pop-Location
}

foreach ($a in $Abi) {
    $so = Join-Path $jniLibs "$a/libtinyclient.so"
    if (-not (Test-Path $so)) { Fail "cargo ndk did not write $so (is [lib] crate-type missing cdylib?)" }
    Note ("{0}: {1:N1} MB" -f $so, ((Get-Item $so).Length / 1MB))
}

# --- Emoji font --------------------------------------------------------------

# Android 13+ ships only a COLRv1 emoji font, which GPUI's rasterizer (Swash) cannot draw, so
# gpui_ce_mobile loads a bitmap (CBDT) Noto Color Emoji from the APK at `assets/fonts/
# NotoColorEmoji.ttf` and logs "cannot load bundled emoji font" without it: every emoji is then
# blank. Noto Emoji v2.051 is the last release with the CBDT font in its tree (OFL-1.1). It is
# 10 MB, so it is fetched once into a cache and copied in, pinned by hash, never committed.
$emojiUrl = 'https://raw.githubusercontent.com/googlefonts/noto-emoji/v2.051/fonts/NotoColorEmoji.ttf'
$emojiSha = '72a635cb3d2f3524c51620cdde406b217204e8a6a06c6a096ff8ed4b5fd6e27b'
$emojiCache = Join-Path $env:LOCALAPPDATA "tinyclient-build/NotoColorEmoji-v2.051.ttf"
$emojiAsset = Join-Path $androidDir 'app/src/main/assets/fonts/NotoColorEmoji.ttf'
function Test-Sha([string]$path) {
    (Test-Path $path) -and ((Get-FileHash -Algorithm SHA256 $path).Hash.ToLower() -eq $emojiSha)
}
if (-not (Test-Sha $emojiCache)) {
    Note "fetching the CBDT emoji font: $emojiUrl"
    New-Item -ItemType Directory -Force -Path (Split-Path $emojiCache) | Out-Null
    Invoke-WebRequest -Uri $emojiUrl -OutFile $emojiCache -UseBasicParsing
    if (-not (Test-Sha $emojiCache)) { Fail "the emoji font at $emojiUrl does not match SHA-256 $emojiSha" }
}
if (-not (Test-Sha $emojiAsset)) {
    New-Item -ItemType Directory -Force -Path (Split-Path $emojiAsset) | Out-Null
    Copy-Item $emojiCache $emojiAsset -Force
}

if ($SkipGradle) {
    Note "skipping Gradle (-SkipGradle); jniLibs are in $jniLibs"
    exit 0
}

# --- Gradle ----------------------------------------------------------------

$java = Get-Command java -ErrorAction SilentlyContinue
if (-not $java) { Fail "java is not on PATH. AGP 9 needs JDK 17+ (Android Studio ships one)." }
Note ((& java -version 2>&1 | Select-Object -First 1))

$gradlew = Join-Path $androidDir 'gradlew.bat'
$gradle = $null
if (Test-Path $gradlew) {
    $gradle = $gradlew
} elseif (Get-Command gradle -ErrorAction SilentlyContinue) {
    $gradle = 'gradle'
} else {
    Fail "No Gradle. mobile/android/gradlew.bat is missing because the wrapper jar is not committed, and gradle is not on PATH. Run 'gradle wrapper --gradle-version 9.6' in mobile/android, or install Gradle 9.6+."
}

Push-Location $androidDir
try {
    & $gradle ':app:assembleDebug' --console=plain
    if ($LASTEXITCODE -ne 0) { Fail "Gradle failed with exit code $LASTEXITCODE" }
} finally {
    Pop-Location
}

$apk = Join-Path $androidDir 'app/build/outputs/apk/debug/app-debug.apk'
if (-not (Test-Path $apk)) { Fail "Gradle reported success but $apk does not exist." }
Note "APK $apk"
Note "install with: adb install -r `"$apk`" ; adb shell am start -n dev.tinyclient.app/dev.gpui.mobile.GpuiInputActivity"
Note "16 KB check: llvm-readelf -lW <so> | grep LOAD  (every Align 0x4000), then zipalign -c -P 16 4 <apk>"
