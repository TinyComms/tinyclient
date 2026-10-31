#!/usr/bin/env pwsh
# Starts `chatd --seed-demo` against a scratch TINYCLIENT_DIR and prints the environment the
# shell needs. This is the manual demo process from crates/chatd/src/demo.rs, automated:
#
#   TINYCLIENT_DIR=/tmp/tinyclient-demo chatd --seed-demo=live
#
#   .\scripts\mobile\seed-demo.ps1                    # live, in %TEMP%\tinyclient-demo
#   .\scripts\mobile\seed-demo.ps1 -Mode many
#   .\scripts\mobile\seed-demo.ps1 -Dir C:\tmp\tiny-demo -Mode expired,live
#   .\scripts\mobile\seed-demo.ps1 -NoStart           # only print the variables
#
# The daemon stays running in the background; the script prints how to stop it.
[CmdletBinding()]
param(
    [string]$Dir = (Join-Path ([System.IO.Path]::GetTempPath()) 'tinyclient-demo'),

    # A comma-separated list of the modes chatd knows: expired, live, many, attention,
    # reconnecting. `live` makes things keep happening, which is what a demo wants.
    [string]$Mode = 'live',

    [int]$TimeoutSeconds = 30,

    # Do not start chatd; just print what to export.
    [switch]$NoStart
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$root = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)

function Fail([string]$message) {
    Write-Host "seed-demo: $message" -ForegroundColor Red
    exit 1
}

# Rust is installed outside PATH on some machines (rustup's default on Windows).
$cargoBin = Join-Path $env:USERPROFILE '.cargo\bin'
if (-not (Get-Command cargo -ErrorAction SilentlyContinue) -and (Test-Path (Join-Path $cargoBin 'cargo.exe'))) {
    $env:PATH = "$cargoBin;$env:PATH"
}
if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    Fail "cargo is not on PATH. Install Rust from https://rustup.rs, or add $cargoBin to PATH."
}

$scratch = [System.IO.Path]::GetFullPath($Dir)
New-Item -ItemType Directory -Force -Path $scratch | Out-Null

Write-Host "seed-demo: TINYCLIENT_DIR=$scratch"

if ($NoStart) {
    Write-Host ''
    Write-Host 'Run the daemon (it seeds on start):'
    Write-Host "  `$env:TINYCLIENT_DIR = `"$scratch`""
    Write-Host "  cargo run -p chatd -- --seed-demo=$Mode"
    Write-Host ''
    Write-Host 'Then, in another shell:'
    Write-Host "  `$env:TINYCLIENT_DIR = `"$scratch`""
    Write-Host '  cargo run -p tinyclient --features gui'
    exit 0
}

# --- Build and start chatd -------------------------------------------------

Push-Location $root
try {
    & cargo build -p chatd
    if ($LASTEXITCODE -ne 0) { Fail "cargo build -p chatd failed with exit code $LASTEXITCODE" }
} finally {
    Pop-Location
}

$exe = Join-Path $root 'target/debug/chatd.exe'
if (-not (Test-Path $exe)) { Fail "expected $exe after a successful build." }

$log = Join-Path $scratch 'chatd.log'
if (Test-Path $log) { Remove-Item $log -Force }

# chatd refuses --seed-demo without TINYCLIENT_DIR, by design, so that a demo can never write
# into the real store. Windows PowerShell 5.1 has no Start-Process -Environment, so set it on
# this process for the child and then restore it.
$previous = $env:TINYCLIENT_DIR
$env:TINYCLIENT_DIR = $scratch
try {
    $process = Start-Process -FilePath $exe `
        -ArgumentList "--seed-demo=$Mode" `
        -WorkingDirectory $root `
        -RedirectStandardError $log `
        -RedirectStandardOutput (Join-Path $scratch 'chatd.out.log') `
        -WindowStyle Hidden `
        -PassThru
} finally {
    if ($null -eq $previous) { Remove-Item Env:TINYCLIENT_DIR -ErrorAction SilentlyContinue }
    else { $env:TINYCLIENT_DIR = $previous }
}

# Seeding writes the database before the socket is bound, so wait for chatd's own line rather
# than for a path: on Windows the "socket" is a named pipe and there is no file to poll.
$deadline = (Get-Date).AddSeconds($TimeoutSeconds)
$listening = $false
while ((Get-Date) -lt $deadline) {
    if ($process.HasExited) { break }
    if ((Test-Path $log) -and (Select-String -Path $log -Pattern 'chatd listening' -Quiet)) {
        $listening = $true
        break
    }
    Start-Sleep -Milliseconds 250
}

if (-not $listening) {
    $tail = if (Test-Path $log) { Get-Content $log -Tail 20 | Out-String } else { '' }
    if ($process.HasExited) {
        Fail "chatd exited before it listened. Its output:`n$tail`nIf it says another chatd is already running, stop that one or pass -Dir with a different directory."
    }
    Fail "chatd did not report listening within $TimeoutSeconds s. Its output so far:`n$tail"
}

Write-Host "seed-demo: chatd pid $($process.Id), seeded '$Mode', log $log"
Write-Host ''
Write-Host 'In the shell that runs tinyclient (PowerShell):'
Write-Host "  `$env:TINYCLIENT_DIR = `"$scratch`""
Write-Host '  cargo run -p tinyclient --features gui'
Write-Host ''
Write-Host 'Same, in bash/zsh:'
Write-Host "  export TINYCLIENT_DIR=`"$scratch`""
Write-Host '  cargo run -p tinyclient --features gui'
Write-Host ''
Write-Host "Stop the daemon with:  Stop-Process -Id $($process.Id)"
Write-Host "Re-seed from scratch:  remove $scratch and run this script again."
