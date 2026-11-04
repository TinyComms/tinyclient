# Unpacks a source tar and builds the shipped programs with the release profile into a cargo
# target folder shared by every SHA, so dependencies build once. Copies the three programs to
# <Work>\bin\<id>. Prints "BINARIES=<dir>" and "TINYCLIENT_SHA256=<hex>" last; exits 1 on a failed build.
param(
    [Parameter(Mandatory)][string]$Tar,
    [Parameter(Mandatory)][string]$Id,
    [string]$Work = 'C:\lab'
)
$ErrorActionPreference = 'Stop'
$src = Join-Path $Work "src\$Id"
$bin = Join-Path $Work "bin\$Id"
$log = Join-Path $Work "logs\build-$Id.log"
New-Item -ItemType Directory -Force (Split-Path $log), (Join-Path $Work 'target') | Out-Null

# Built before (a second run of the same SHA): nothing to do.
if (Test-Path (Join-Path $bin 'tinyclient.exe')) {
    Write-Host 'already built'
    Write-Host "BINARIES=$bin"
    Write-Host "TINYCLIENT_SHA256=$((Get-FileHash (Join-Path $bin 'tinyclient.exe') -Algorithm SHA256).Hash.ToLower())"
    exit 0
}

# A folder per SHA, never reused: cargo trusts file times, and a tar's times are the commit's.
if (-not (Test-Path (Join-Path $src '.unpacked'))) {
    Remove-Item -Recurse -Force $src -ErrorAction SilentlyContinue
    New-Item -ItemType Directory -Force $src | Out-Null
    & tar.exe -xf $Tar -C $src
    if ($LASTEXITCODE -ne 0) { throw "tar failed ($LASTEXITCODE)" }
    New-Item -ItemType File (Join-Path $src '.unpacked') | Out-Null
}

$env:CARGO_TARGET_DIR = Join-Path $Work 'target'
$env:CARGO_TERM_COLOR = 'never'
# protoc for Signal's protobufs; native-tls finds OpenSSL beside Git's usr\bin (as in CI).
$env:PATH = "$Work\tools\protoc\bin;C:\Program Files\Git\usr\bin;C:\Users\builder\.cargo\bin;$env:PATH"
$env:PROTOC = "$Work\tools\protoc\bin\protoc.exe"
Remove-Item Env:\RUSTFLAGS -ErrorAction SilentlyContinue

$started = Get-Date
Push-Location $src
cmd /c "cargo build --release -p tinyclient --features tinyclient/gui -p chatd -p mediad > `"$log`" 2>&1"
$code = $LASTEXITCODE
Pop-Location
Write-Host ("cargo exit {0} after {1:N0}s" -f $code, ((Get-Date) - $started).TotalSeconds)
if ($code -ne 0) { Get-Content $log -Tail 40; exit 1 }

New-Item -ItemType Directory -Force $bin | Out-Null
foreach ($n in 'tinyclient', 'chatd', 'mediad') { Copy-Item (Join-Path $env:CARGO_TARGET_DIR "release\$n.exe") $bin -Force }
# Keep the three newest source folders and binary folders; the target folder is what is cached.
foreach ($d in 'src', 'bin') {
    Get-ChildItem (Join-Path $Work $d) -Directory | Sort-Object LastWriteTime -Descending | Select-Object -Skip 3 |
        ForEach-Object { Remove-Item -Recurse -Force $_.FullName -ErrorAction SilentlyContinue }
}
Remove-Item $Tar -Force -ErrorAction SilentlyContinue
Write-Host "BINARIES=$bin"
Write-Host "TINYCLIENT_SHA256=$((Get-FileHash (Join-Path $bin 'tinyclient.exe') -Algorithm SHA256).Hash.ToLower())"
