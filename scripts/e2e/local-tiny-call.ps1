<#
.SYNOPSIS
  Local end-to-end Tiny call: Postgres + tinyserver (with SFU media) + a desktop tinyclient,
  plus session files for an Android emulator and an iOS simulator.

.DESCRIPTION
  Dev-only test infrastructure. Nothing here touches a real community.

  -Phase server   Postgres container (tiny-e2e-pg, 127.0.0.1:54351), a fresh database, the
                  OIDC discovery stub, `tinyserver serve` on 0.0.0.0:8091 with media on UDP/TCP
                  50100, and the seed (owner Alice + members Bea, Cy, room "Proof").
                  Writes <State>\seed.json and session files for each person.
  -Phase desktop  Starts this checkout's tinyclient GUI signed in as Alice with an isolated
                  TINYCLIENT_DIR (never the real keyring or %APPDATA%\tinyclient), test audio
                  from mediad (TINYCLIENT_MEDIAD_AUDIO), and joins the room through UI Automation.
  -Phase android  Writes Bea's session into the installed debug app (run-as) for emulator-5554.
  -Phase all      server, desktop, android.
  -Stop           Stops only the processes this script started (pids in <State>\pids.json) and
                  removes the Postgres container.

.EXAMPLE
  pwsh scripts/e2e/local-tiny-call.ps1 -Phase all
#>
param(
    [ValidateSet('server', 'desktop', 'android', 'all')]
    [string]$Phase = 'all',
    [switch]$Stop,
    # A clean tinyserver checkout. Default: a detached worktree of tinyserver's origin/main next
    # to it, made on first run, so a dirty feature branch in the main checkout does not matter.
    [string]$TinyServerDir = '',
    [string]$TinyServerRef = 'origin/main',
    [string]$State = (Join-Path $env:LOCALAPPDATA 'tiny-e2e\run'),
    [string]$Audio = 'onset',
    [int]$HttpPort = 8091,
    [int]$MediaPort = 50100,
    [int]$PgPort = 54351,
    [int]$OidcPort = 54352,
    [string]$Serial = 'emulator-5554'
)
$ErrorActionPreference = 'Stop'
$Repo = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
# The project space: the folder holding the tinyclient and tinyserver checkouts.
$Space = Split-Path (Split-Path (git -C $Repo rev-parse --path-format=absolute --git-common-dir) -Parent) -Parent
if (-not $TinyServerDir) {
    $TinyServerDir = Join-Path $Space '_wt-e2e-server'
    if (-not (Test-Path (Join-Path $TinyServerDir 'Cargo.toml'))) {
        git -C (Join-Path $Space 'tinyserver') fetch origin
        git -C (Join-Path $Space 'tinyserver') worktree add --detach $TinyServerDir $TinyServerRef
    }
}
$TinyServerDir = (Resolve-Path $TinyServerDir).Path
New-Item -ItemType Directory -Force $State | Out-Null
$PidFile = Join-Path $State 'pids.json'
$Logs = Join-Path $State 'logs'
New-Item -ItemType Directory -Force $Logs | Out-Null

function Save-Pid([string]$Name, [int]$Id) {
    $pids = @{}
    if (Test-Path $PidFile) { (Get-Content $PidFile -Raw | ConvertFrom-Json).psobject.Properties | ForEach-Object { $pids[$_.Name] = $_.Value } }
    $pids[$Name] = $Id
    $pids | ConvertTo-Json | Set-Content $PidFile
}

function Stop-Ours {
    if (Test-Path $PidFile) {
        (Get-Content $PidFile -Raw | ConvertFrom-Json).psobject.Properties | ForEach-Object {
            $p = Get-Process -Id $_.Value -ErrorAction SilentlyContinue
            if ($p) { Write-Host "stop $($_.Name) ($($_.Value))"; Stop-Process -Id $_.Value -Force -ErrorAction SilentlyContinue }
        }
        Remove-Item $PidFile
    }
    # chatd/mediad children of our desktop client live under our TINYCLIENT_DIR only.
    $dir = Join-Path $State 'desktop-a'
    Get-CimInstance Win32_Process -Filter "Name='chatd.exe' or Name='mediad.exe'" | Where-Object {
        $_.ExecutablePath -and $_.ExecutablePath.StartsWith($Repo, [StringComparison]::OrdinalIgnoreCase) -and
        ((Get-Process -Id $_.ParentProcessId -ErrorAction SilentlyContinue) -eq $null -or $_.CommandLine -match [regex]::Escape($dir))
    } | ForEach-Object { Write-Host "stop orphan $($_.Name) ($($_.ProcessId))"; Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }
    docker rm -f tiny-e2e-pg 2>$null | Out-Null
}

if ($Stop) { Stop-Ours; return }

$MigrateUrl = "postgres://app_owner:app_owner@127.0.0.1:$PgPort/tiny_e2e?sslmode=disable"
$RuntimeUrl = "postgres://app_runtime:app_runtime@127.0.0.1:$PgPort/tiny_e2e?sslmode=disable"

function Wait-Http([string]$Url, [int]$Seconds = 60) {
    $deadline = (Get-Date).AddSeconds($Seconds)
    while ((Get-Date) -lt $deadline) {
        try { return Invoke-RestMethod -Uri $Url -TimeoutSec 3 } catch { Start-Sleep -Milliseconds 500 }
    }
    throw "timed out waiting for $Url"
}

function Start-Server {
    # Postgres, our own container and port; never the tinyserver-postgres-1 dev database.
    if (-not (docker ps -q -f name=^tiny-e2e-pg$)) {
        docker rm -f tiny-e2e-pg 2>$null | Out-Null
        # A Windows checkout may have CRLF in the init script, which sh then fails on.
        $init = Join-Path $State 'pg-init'
        New-Item -ItemType Directory -Force $init | Out-Null
        $roles = (Get-Content -Raw "$TinyServerDir\docker\postgres\init\01-roles.sh") -replace "`r`n", "`n"
        [IO.File]::WriteAllText((Join-Path $init '01-roles.sh'), $roles)
        docker run -d --name tiny-e2e-pg -e POSTGRES_USER=postgres -e POSTGRES_PASSWORD=postgres `
            -e POSTGRES_DB=tinyserver -e APP_OWNER_PASSWORD=app_owner -e APP_RUNTIME_PASSWORD=app_runtime `
            -p "127.0.0.1:${PgPort}:5432" -v "${init}:/docker-entrypoint-initdb.d:ro" `
            postgres:17-alpine | Out-Null
    }
    $ready = $false
    for ($i = 0; $i -lt 60 -and -not $ready; $i++) {
        docker exec tiny-e2e-pg psql -U app_owner -d tinyserver -c 'select 1' 2>$null | Out-Null
        if ($LASTEXITCODE -eq 0) { $ready = $true } else { Start-Sleep 1 }
    }
    if (-not $ready) { throw 'postgres did not come up' }
    # A fresh database every run: the seed makes the owner.
    docker exec tiny-e2e-pg psql -U postgres -d tinyserver -c 'DROP DATABASE IF EXISTS tiny_e2e WITH (FORCE)' | Out-Null
    docker exec tiny-e2e-pg psql -U postgres -d tinyserver -c 'CREATE DATABASE tiny_e2e OWNER app_owner' | Out-Null
    docker exec tiny-e2e-pg psql -U postgres -d tiny_e2e -c 'ALTER SCHEMA public OWNER TO app_owner; GRANT USAGE ON SCHEMA public TO app_runtime; GRANT CONNECT ON DATABASE tiny_e2e TO app_runtime;' | Out-Null
    # Default privileges are per database; repeat what 01-roles.sh does for `tinyserver`.
    docker exec tiny-e2e-pg psql -U postgres -d tiny_e2e -c 'ALTER DEFAULT PRIVILEGES FOR ROLE app_owner IN SCHEMA public GRANT SELECT, INSERT, UPDATE, DELETE ON TABLES TO app_runtime; ALTER DEFAULT PRIVILEGES FOR ROLE app_owner IN SCHEMA public GRANT USAGE, SELECT ON SEQUENCES TO app_runtime; ALTER DEFAULT PRIVILEGES FOR ROLE app_owner IN SCHEMA public GRANT EXECUTE ON FUNCTIONS TO app_runtime;' | Out-Null

    # Seed (builds against tinyserver's db crate).
    $seedDir = Join-Path $env:LOCALAPPDATA 'tiny-e2e\seed'
    New-Item -ItemType Directory -Force $seedDir | Out-Null
    $ts = $TinyServerDir -replace '\\', '/'
    $main = (Join-Path $PSScriptRoot 'tiny-seed\main.rs') -replace '\\', '/'
    @"
[package]
name = "tiny-seed"
version = "0.0.0"
edition = "2021"
publish = false

[[bin]]
name = "tiny-seed"
path = "$main"

[dependencies]
db = { path = "$ts/crates/db" }
serde_json = "1"
sha2 = { version = "0.10", default-features = false }
sqlx = { version = "0.8", default-features = false, features = ["runtime-tokio", "tls-none", "postgres"] }
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }

[workspace]
"@ | Set-Content (Join-Path $seedDir 'Cargo.toml')
    if (-not (Test-Path (Join-Path $seedDir 'Cargo.lock'))) { Copy-Item (Join-Path $TinyServerDir 'Cargo.lock') $seedDir }
    cargo build -q --manifest-path (Join-Path $seedDir 'Cargo.toml')
    if ($LASTEXITCODE) { throw 'tiny-seed build failed' }
    cargo build -q -p tinyserver --manifest-path (Join-Path $TinyServerDir 'Cargo.toml')
    if ($LASTEXITCODE) { throw 'tinyserver build failed' }

    $env:DATABASE_URL_MIGRATE = $MigrateUrl
    $env:DATABASE_URL = $RuntimeUrl
    $env:TINY_E2E_BASE_URL = "http://127.0.0.1:$HttpPort"
    $seedJson = & (Join-Path $seedDir 'target\debug\tiny-seed.exe')
    if ($LASTEXITCODE) { throw 'seed failed' }
    $seedJson | Set-Content (Join-Path $State 'seed.json')
    $seed = $seedJson | ConvertFrom-Json

    # One file per person, in the format chatd's Tiny adapter saves.
    foreach ($k in 'a', 'b', 'c') {
        $s = $seed.sessions.$k
        $s | ConvertTo-Json -Compress | Set-Content (Join-Path $State "session-$k.json")
    }

    # OIDC discovery stub so `serve` starts; sign-in through it is impossible.
    $oidc = Start-Process -PassThru -WindowStyle Hidden python -ArgumentList '-I', (Join-Path $PSScriptRoot 'fake-oidc.py'), $OidcPort
    Save-Pid 'fake-oidc' $oidc.Id
    Wait-Http "http://127.0.0.1:$OidcPort/.well-known/openid-configuration" | Out-Null

    $tailnet = (tailscale ip -4 2>$null | Select-Object -First 1)
    $lan = (Get-NetIPAddress -AddressFamily IPv4 -InterfaceAlias 'Ethernet', 'Wi-Fi' -ErrorAction SilentlyContinue | Select-Object -First 1).IPAddress
    $env:BIND = "0.0.0.0:$HttpPort"
    $env:TINYSERVER_PUBLIC_URL = "http://127.0.0.1:$HttpPort"
    # The advertised media address. The tailnet address is reachable from this PC, the Android
    # emulator (through its NAT) and a Mac on the tailnet. LAN_IP adds the home network address.
    $env:TINYSERVER_PUBLIC_IP = if ($tailnet) { $tailnet } else { $lan }
    if ($lan) { $env:TINYSERVER_LAN_IP = $lan }
    $env:TINYSERVER_MEDIA_PORT = "$MediaPort"
    $env:TINYSERVER_OIDC_ISSUER = "http://127.0.0.1:$OidcPort"
    $env:TINYSERVER_OIDC_CLIENT_ID = 'tinyserver'
    $env:TINYSERVER_OIDC_CLIENT_SECRET = 'dev-only'
    $env:TINYSERVER_ALLOW_FAKE_SCAN = '1'
    $env:RUST_LOG = 'info,sfu=debug,api::live=debug'
    $exe = Join-Path $TinyServerDir 'target\debug\tinyserver.exe'
    $srv = Start-Process -PassThru -WindowStyle Hidden $exe -ArgumentList 'serve' -WorkingDirectory $State `
        -RedirectStandardOutput (Join-Path $Logs 'tinyserver.out.log') -RedirectStandardError (Join-Path $Logs 'tinyserver.log')
    Save-Pid 'tinyserver' $srv.Id
    $health = Wait-Http "http://127.0.0.1:$HttpPort/health" 90
    Write-Host "tinyserver up: $($health | ConvertTo-Json -Compress)"
    Write-Host "room: $($seed.room.name) $($seed.room.id)"
}

if ($Phase -in 'server', 'all') { Start-Server }
