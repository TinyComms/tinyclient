<#
.SYNOPSIS
  Throwaway tinyserver for the device-link proof, on the coordinator. Nothing here touches a real community.

.DESCRIPTION
  Adapted from tinyclient scripts/e2e/local-tiny-call.ps1 (-Phase server) of the e2e lab, with its
  own names so it never collides with a lab or share run that may be up at the same time:

    Postgres container  tiny-dl-pg on 127.0.0.1:$PgPort (fresh database tiny_dl every `up`)
    tinyserver          127.0.0.1:$HttpPort only (no firewall prompt), built from -TinyServerDir
                        (default: the device-link server worktree) into its own target dir
    linkproxy.py        0.0.0.0:$ProxyPort -> tinyserver. EVERY device uses the proxy's URL; python
                        already has an inbound firewall rule on the coordinator, tinyserver.exe at a new
                        path would not. The proxy writes wire.jsonl and does the fault injection.
    seed                Ada (owner) with one session per device, Bea (member) with hers.

  All server state lives under $State\server so the canary check (negative\canary-check.ps1) knows
  exactly what to search: TINYSERVER_DATA_DIR = $State\server\data (logs ring, server key),
  TINYSERVER_UPLOAD_DIR = $State\server\data\uploads (media volume), stdout/stderr logs, pg_dump.

  up      build, fresh DB, seed, serve, proxy. Writes $State\seed.json and $State\server.json.
  down    stops only what `up` started (pids in $State\pids.json) and removes tiny-dl-pg.
  status  prints health and capabilities (and whether e2ee_device_link is advertised).
  serve   restarts only tinyserver + proxy on the existing database (after a crash).
  add-cy  adds Cy (a member, two sessions) to the RUNNING server's database, nothing wiped, and
          writes $State\cy-sessions.json (a JSON array of two Tiny sessions on http://localhost:$ProxyPort).
          For the client agent's ignored live test; the proof's own scenarios never use Cy.
          `up` also adds Cy, so the file is rewritten (new tokens) on every fresh database.

.EXAMPLE
  pwsh -File server\dl-server.ps1 up
  pwsh -File server\dl-server.ps1 up -TinyServerDir ..\tinyserver -LinkTtlSeconds 20
#>
param(
    [Parameter(Position = 0)][ValidateSet('up', 'down', 'status', 'serve', 'add-cy')][string]$Action = 'up',
    [string]$TinyServerDir = '',
    # The commit to build, from that checkout's repository, exported with `git archive` so a branch
    # switch or uncommitted work in the checkout changes nothing. '' = the checkout as it is.
    [string]$TinyServerRef = 'feat/device-link',
    [string]$State = (Join-Path $env:LOCALAPPDATA 'tiny-e2e\device-sync'),
    [int]$HttpPort = 8093,
    [int]$ProxyPort = 8094,
    [int]$PgPort = 54361,
    [int]$OidcPort = 54362,
    [ValidateRange(1024,65535)][int]$MediaPort = 50110,
    [string]$PgContainer = 'tiny-device-sync-pg',
    # Short link lifetime for the "expired link" case, in seconds (the server allows 60..3600).
    # 0 = the server's default (300).
    [int]$LinkTtlSeconds = 0,
    # The endpoint every device uses. Default: 127.0.0.1 (or DL_ADVERTISE from private config;
    # remote devices can reach it). Set explicitly from private lab configuration.
    [string]$Advertise = ''
)
$ErrorActionPreference = 'Stop'
$FixtureBase = [IO.Path]::GetFullPath((Join-Path $env:LOCALAPPDATA 'tiny-e2e'))
function Assert-FixturePath([string]$Path) {
    $full = [IO.Path]::GetFullPath($Path)
    if (-not $full.StartsWith($FixtureBase.TrimEnd('\') + '\', [StringComparison]::OrdinalIgnoreCase)) {
        throw "refusing fixture cleanup outside $FixtureBase"
    }
}
Assert-FixturePath $State
$Here = $PSScriptRoot
$Root = Split-Path $Here -Parent                      # _scratch_device_link
$Lab = [IO.Path]::GetFullPath((Join-Path $Root "..\..\.."))
$Space = Split-Path $Lab -Parent
if (-not $TinyServerDir) { $TinyServerDir = Join-Path $Space "tinyserver" }
$TinyServerDir = (Resolve-Path $TinyServerDir).Path
$ServerSha = git -C $TinyServerDir rev-parse HEAD
$ServerBranch = git -C $TinyServerDir branch --show-current
if ($TinyServerRef -and $Action -in 'up', 'serve') {
    $ServerSha = git -C $TinyServerDir rev-parse "$TinyServerRef^{commit}"
    if ($LASTEXITCODE) { throw "no such ref in ${TinyServerDir}: $TinyServerRef" }
    $ServerBranch = $TinyServerRef
    $export = Join-Path $env:LOCALAPPDATA "tiny-e2e\dl-server-src"
    Assert-FixturePath $export
    $stamp = Join-Path $export '.sha'
    if (-not ((Test-Path $stamp) -and (Get-Content $stamp) -eq $ServerSha)) {
        if (Test-Path $export) { Remove-Item -Recurse -Force $export }
        New-Item -ItemType Directory -Force $export | Out-Null
        $tar = Join-Path $env:TEMP "dl-server-$($ServerSha.Substring(0, 12)).tar"
        git -C $TinyServerDir -c core.autocrlf=false archive --format=tar -o $tar $ServerSha
        # Windows' own tar: Git's GNU tar (first on PATH in a Git Bash shell) reads "C:" as a host.
        & (Join-Path $env:SystemRoot 'System32\tar.exe') -x -f $tar -C $export
        if ($LASTEXITCODE) { Remove-Item -Recurse -Force $export; throw "could not unpack $TinyServerRef" }
        Remove-Item $tar
        Set-Content $stamp $ServerSha
    }
    $TinyServerDir = $export
}
$Srv = Join-Path $State 'server'
$Data = Join-Path $Srv 'data'
$Logs = Join-Path $Srv 'logs'
$PidFile = Join-Path $State 'pids.json'
$Target = Join-Path $env:LOCALAPPDATA 'tiny-e2e\dl-server-target'
$SeedDir = Join-Path $env:LOCALAPPDATA 'tiny-e2e\dl-seed'
$Pg = $PgContainer
$MigrateUrl = "postgres://app_owner:app_owner@127.0.0.1:$PgPort/tiny_dl?sslmode=disable"
$RuntimeUrl = "postgres://app_runtime:app_runtime@127.0.0.1:$PgPort/tiny_dl?sslmode=disable"

function Save-Pid([string]$Name, [int]$Id) {
    $pids = @{}
    if (Test-Path $PidFile) { (Get-Content $PidFile -Raw | ConvertFrom-Json).psobject.Properties | ForEach-Object { $pids[$_.Name] = $_.Value } }
    $process = Get-Process -Id $Id -ErrorAction Stop
    try {
        $null = $process.Handle
        $pids[$Name] = @{ pid=$Id; creation_ticks=$process.StartTime.ToUniversalTime().Ticks.ToString(); executable=$process.Path }
    } finally { $process.Dispose() }
    $pids | ConvertTo-Json -Depth 5 | Set-Content $PidFile
}
function Stop-Pids([string[]]$Only = @()) {
    if (-not (Test-Path $PidFile)) { return }
    $keep = @{}
    $failures = [Collections.Generic.List[string]]::new()
    (Get-Content $PidFile -Raw | ConvertFrom-Json).psobject.Properties | ForEach-Object {
        if ($Only.Count -and $_.Name -notin $Only) { $keep[$_.Name] = $_.Value; return }
        $entry = $_.Value
        if ($entry -is [int] -or $entry -is [long]) {
            if (Get-Process -Id $entry -ErrorAction SilentlyContinue) {
                $keep[$_.Name]=$entry
                $failures.Add("refusing live legacy PID without native ownership: $($_.Name)")
            }
            return
        }
        $process = Get-Process -Id $entry.pid -ErrorAction SilentlyContinue
        if (-not $process) { return }
        try {
        $null = $process.Handle
        if (-not $entry.creation_ticks -or -not $entry.executable -or
            $process.StartTime.ToUniversalTime().Ticks.ToString() -ne $entry.creation_ticks -or $process.Path -ne $entry.executable) {
            $keep[$_.Name]=$entry
            $failures.Add("refusing process identity mismatch: $($_.Name)")
            return
        }
        try {
            $process.Kill()
            if (-not $process.WaitForExit(10000)) { throw 'owned process did not exit' }
        } catch {
            $keep[$_.Name]=$entry
            $failures.Add("owned process stop failed: $($_.Name)")
        }
        } finally { $process.Dispose() }
    }
    if ($keep.Count) { $keep | ConvertTo-Json -Depth 5 | Set-Content $PidFile } else { Remove-Item $PidFile }
    if ($failures.Count) { throw ($failures -join '; ') }
}
function Wait-Http([string]$Url, [int]$Seconds = 60) {
    $deadline = (Get-Date).AddSeconds($Seconds)
    while ((Get-Date) -lt $deadline) {
        try { return Invoke-RestMethod -Uri $Url -TimeoutSec 3 } catch { Start-Sleep -Milliseconds 500 }
    }
    throw "timed out waiting for $Url"
}
function Get-Advertise {
    if ($Advertise) { return $Advertise }
    if ($env:DL_ADVERTISE) { return $env:DL_ADVERTISE }
    return '127.0.0.1'
}

function Start-Db {
    Assert-DbOwnership
    if (-not (docker ps -q -f "name=^$Pg$")) {
        docker rm -f $Pg 2>$null | Out-Null
        $init = Join-Path $State 'pg-init'
        New-Item -ItemType Directory -Force $init | Out-Null
        $roles = (Get-Content -Raw "$TinyServerDir\docker\postgres\init\01-roles.sh") -replace "`r`n", "`n"
        [IO.File]::WriteAllText((Join-Path $init '01-roles.sh'), $roles)
        docker run -d --name $Pg -e POSTGRES_USER=postgres -e POSTGRES_PASSWORD=postgres `
            --label 'tiny.e2e.scenario=device-sync' --label "tiny.e2e.state=$State" `
            -e POSTGRES_DB=tinyserver -e APP_OWNER_PASSWORD=app_owner -e APP_RUNTIME_PASSWORD=app_runtime `
            -p "127.0.0.1:${PgPort}:5432" -v "${init}:/docker-entrypoint-initdb.d:ro" postgres:17-alpine | Out-Null
    }
    $ready = $false
    for ($i = 0; $i -lt 60 -and -not $ready; $i++) {
        docker exec $Pg psql -U app_owner -d tinyserver -c 'select 1' 2>$null | Out-Null
        if ($LASTEXITCODE -eq 0) { $ready = $true } else { Start-Sleep 1 }
    }
    if (-not $ready) { throw 'postgres did not come up' }
    docker exec $Pg psql -U postgres -d tinyserver -c 'DROP DATABASE IF EXISTS tiny_dl WITH (FORCE)' | Out-Null
    docker exec $Pg psql -U postgres -d tinyserver -c 'CREATE DATABASE tiny_dl OWNER app_owner' | Out-Null
    docker exec $Pg psql -U postgres -d tiny_dl -c 'ALTER SCHEMA public OWNER TO app_owner; GRANT USAGE ON SCHEMA public TO app_runtime; GRANT CONNECT ON DATABASE tiny_dl TO app_runtime;' | Out-Null
    docker exec $Pg psql -U postgres -d tiny_dl -c 'ALTER DEFAULT PRIVILEGES FOR ROLE app_owner IN SCHEMA public GRANT SELECT, INSERT, UPDATE, DELETE ON TABLES TO app_runtime; ALTER DEFAULT PRIVILEGES FOR ROLE app_owner IN SCHEMA public GRANT USAGE, SELECT ON SEQUENCES TO app_runtime; ALTER DEFAULT PRIVILEGES FOR ROLE app_owner IN SCHEMA public GRANT EXECUTE ON FUNCTIONS TO app_runtime;' | Out-Null
}

function Assert-DbOwnership {
    $existing = docker container inspect $Pg 2>$null
    if ($LASTEXITCODE -eq 0) {
        $record = ($existing | ConvertFrom-Json)[0]
        if ($record.Config.Labels.'tiny.e2e.scenario' -ne 'device-sync' -or $record.Config.Labels.'tiny.e2e.state' -ne $State) {
            throw "refusing to change database container not owned by this fixture: $Pg"
        }
    }
}

function Build-All {
    New-Item -ItemType Directory -Force $SeedDir | Out-Null
    $ts = $TinyServerDir -replace '\\', '/'
    $main = (Join-Path $Root 'seed\main.rs') -replace '\\', '/'
    @"
[package]
name = "dl-seed"
version = "0.0.0"
edition = "2021"
publish = false

[[bin]]
name = "dl-seed"
path = "$main"

[dependencies]
db = { path = "$ts/crates/db" }
serde_json = "1"
sha2 = { version = "0.10", default-features = false }
sqlx = { version = "0.8", default-features = false, features = ["runtime-tokio", "tls-none", "postgres"] }
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }

[workspace]
"@ | Set-Content (Join-Path $SeedDir 'Cargo.toml')
    Copy-Item (Join-Path $TinyServerDir 'Cargo.lock') $SeedDir -Force
    cargo build -q --manifest-path (Join-Path $SeedDir 'Cargo.toml')
    if ($LASTEXITCODE) { throw 'dl-seed build failed' }
    cargo build -q -p tinyserver --manifest-path (Join-Path $TinyServerDir 'Cargo.toml') --target-dir $Target
    if ($LASTEXITCODE) { throw "tinyserver build failed ($TinyServerDir)" }
}

function Start-Serve {
    $adv = Get-Advertise
    $public = "http://${adv}:$ProxyPort"
    New-Item -ItemType Directory -Force $Data, $Logs, (Join-Path $Data 'uploads') | Out-Null
    $env:BIND = "127.0.0.1:$HttpPort"
    # Since tinyserver #33 the public URL must be a hostname (passkeys are bound to it), and it is
    # used only by sign-in pages, which this proof never opens: devices use $public (an address).
    $env:TINYSERVER_PUBLIC_URL = "http://localhost:$ProxyPort"
    $env:TINYSERVER_PUBLIC_IP = $adv
    $env:TINYSERVER_MEDIA_PORT = $MediaPort.ToString()
    # Match fake-oidc.py's discovery issuer exactly.
    $env:TINYSERVER_OIDC_ISSUER = "http://127.0.0.1:$OidcPort"
    $env:TINYSERVER_OIDC_CLIENT_ID = 'tinyserver'
    $env:TINYSERVER_OIDC_CLIENT_SECRET = 'dev-only'
    $env:TINYSERVER_ALLOW_FAKE_SCAN = '1'
    $env:TINYSERVER_DATA_DIR = $Data
    $env:TINYSERVER_UPLOAD_DIR = (Join-Path $Data 'uploads')
    # As much logging as the server will do: the canary check is only as strong as what was logged.
    $env:TINYSERVER_LOG_LEVEL = 'debug'
    $env:RUST_LOG = 'debug,hyper=info,h2=info,sqlx=info,tower=info'
    $env:DATABASE_URL_MIGRATE = $MigrateUrl
    $env:DATABASE_URL = $RuntimeUrl
    $exe = Join-Path $Target 'debug\tinyserver.exe'
    $log = Join-Path $Logs 'tinyserver.log'
    $wrap = Join-Path $Srv 'tinyserver.cmd'
    "@`"$exe`" serve >> `"$log.out`" 2>> `"$log`"`r`n@echo tinyserver exited with code %errorlevel% >> `"$log`"`r`n" | Set-Content -NoNewline $wrap
    $p = Start-Process -PassThru -WindowStyle Hidden cmd -WorkingDirectory $Srv -ArgumentList '/c', "`"$wrap`""
    Save-Pid 'tinyserver-cmd' $p.Id
    $health = Wait-Http "http://localhost:$HttpPort/health" 120
    $child = Get-CimInstance Win32_Process -Filter "ParentProcessId=$($p.Id) and Name='tinyserver.exe'"
    if ($child) { Save-Pid 'tinyserver' $child.ProcessId }

    $proxyOut = Join-Path $State 'wire'
    New-Item -ItemType Directory -Force $proxyOut | Out-Null
    $ctl = Join-Path $State 'proxy-control.json'
    if (-not (Test-Path $ctl)) { '{}' | Set-Content $ctl }
    $px = Start-Process -PassThru -WindowStyle Hidden python -ArgumentList (Join-Path $Root 'lib\linkproxy.py'),
        '--listen', "0.0.0.0:$ProxyPort", '--upstream', "127.0.0.1:$HttpPort", '--out', $proxyOut, '--control', $ctl `
        -RedirectStandardOutput (Join-Path $Logs 'proxy.out.log') -RedirectStandardError (Join-Path $Logs 'proxy.err.log')
    Save-Pid 'linkproxy' $px.Id
    Wait-Http "$public/health" 30 | Out-Null
    Start-Sleep -Milliseconds 500
    if ($px.HasExited) { throw "linkproxy exited (port $ProxyPort taken?): $(Get-Content (Join-Path $Logs 'proxy.err.log') -Tail 2)" }
    # Capabilities need a session: ask as Ada A.
    $tok = (Get-Content (Join-Path $State 'seed.json') -Raw | ConvertFrom-Json).sessions.a_desk_a.token
    $caps = try { Invoke-RestMethod "$public/v1/capabilities" -TimeoutSec 5 -Headers @{ Authorization = "Bearer $tok" } } catch { $null }
    $info = [ordered]@{
        public_url = $public; local_url = "http://localhost:$HttpPort"; proxy_port = $ProxyPort
        tinyserver_dir = $TinyServerDir; tinyserver_sha = $ServerSha
        tinyserver_binary_sha256 = (Get-FileHash -LiteralPath $exe -Algorithm SHA256).Hash.ToLowerInvariant()
        tinyserver_branch = $ServerBranch
        data_dir = $Data; upload_dir = (Join-Path $Data 'uploads'); logs = $Logs; pg = $Pg; pg_db = 'tiny_dl'
        wire = $proxyOut; proxy_control = $ctl; link_ttl_seconds = $LinkTtlSeconds
        e2ee_device_link = [bool]($caps -and $caps.e2ee_device_link)
        health = $health
    }
    $info | ConvertTo-Json -Depth 5 | Set-Content (Join-Path $State 'server.json')
    Write-Host "tinyserver up behind the proxy: $public (e2ee_device_link advertised: $($info.e2ee_device_link))"
}

function Add-Cy {
    $seed = Get-Content (Join-Path $State 'seed.json') -Raw | ConvertFrom-Json
    cargo build -q --manifest-path (Join-Path $SeedDir 'Cargo.toml')
    if ($LASTEXITCODE) { throw 'dl-seed build failed' }
    $env:DATABASE_URL_MIGRATE = $MigrateUrl
    $env:DATABASE_URL = $RuntimeUrl
    # Loopback: the client uploads over plain http only to 127.0.0.1/localhost (chatd upload.rs uploads_to).
    $env:TINY_E2E_BASE_URL = "http://localhost:$ProxyPort"
    $env:DL_ADD_ONLY = 'cy'; $env:DL_ADD_NAME = 'Cy'; $env:DL_ADD_SESSIONS = '2'
    $env:DL_OWNER_ID = $seed.people.ada.user_id; $env:DL_OWNER_ROLE = 'owner'
    $json = & (Join-Path $SeedDir 'target\debug\dl-seed.exe')
    $code = $LASTEXITCODE
    foreach ($n in 'DL_ADD_ONLY', 'DL_ADD_NAME', 'DL_ADD_SESSIONS', 'DL_OWNER_ID', 'DL_OWNER_ROLE') { Remove-Item "env:$n" }
    if ($code) { throw 'adding Cy failed (already added on this database?)' }
    $file = Join-Path $State 'cy-sessions.json'
    [IO.File]::WriteAllText($file, ($json -join "`n"))
    Write-Host "Cy: $file"
}

switch ($Action) {
    'down' {
        Assert-DbOwnership
        Stop-Pids
        docker rm -f $Pg 2>$null | Out-Null
        Write-Host 'down'
    }
    'status' {
        $s = Get-Content (Join-Path $State 'server.json') -Raw | ConvertFrom-Json
        Invoke-RestMethod "$($s.public_url)/health" | ConvertTo-Json -Compress
        Invoke-RestMethod "$($s.public_url)/v1/capabilities" | ConvertTo-Json -Compress
    }
    'add-cy' { Add-Cy }
    'serve' {
        Stop-Pids -Only 'tinyserver-cmd', 'tinyserver', 'linkproxy'
        Start-Serve
    }
    'up' {
        Stop-Pids
        # A stopped tinyserver lets go of its log a moment later.
        for ($i = 0; $i -lt 20 -and (Test-Path $Srv); $i++) {
            try { Remove-Item -Recurse -Force $Srv -ErrorAction Stop } catch { Start-Sleep -Milliseconds 500 }
        }
        if (Test-Path $Srv) { throw "could not clear $Srv" }
        if (Test-Path (Join-Path $State 'wire')) { Remove-Item -Recurse -Force (Join-Path $State 'wire') }
        New-Item -ItemType Directory -Force $State, $Srv, $Logs | Out-Null
        '{}' | Set-Content (Join-Path $State 'proxy-control.json')
        Start-Db
        Build-All
        $env:DATABASE_URL_MIGRATE = $MigrateUrl
        $env:DATABASE_URL = $RuntimeUrl
        $env:TINY_E2E_BASE_URL = "http://$(Get-Advertise):$ProxyPort"
        $seedJson = & (Join-Path $SeedDir 'target\debug\dl-seed.exe')
        if ($LASTEXITCODE) { throw 'seed failed' }
        $seedJson | Set-Content (Join-Path $State 'seed.json')
        # The link lifetime is a column of server_settings (docs/server.md "Device linking", 60..3600 s),
        # set in SQL like the other limits. Only on a server whose migrations have it.
        if ($LinkTtlSeconds -gt 0) {
            docker exec $Pg psql -U postgres -d tiny_dl -v ON_ERROR_STOP=1 -c "UPDATE server_settings SET e2ee_link_ttl_secs = $LinkTtlSeconds" | Out-Null
            if ($LASTEXITCODE) { throw "could not set e2ee_link_ttl_secs (does this server have device linking?)" }
        }
        $oidc = Start-Process -PassThru -WindowStyle Hidden python -ArgumentList '-I', (Join-Path $Lab 'scripts\e2e\fake-oidc.py'), $OidcPort
        Save-Pid 'fake-oidc' $oidc.Id
        Wait-Http "http://localhost:$OidcPort/.well-known/openid-configuration" | Out-Null
        Start-Serve
        Add-Cy
        Write-Host "seed: $(Join-Path $State 'seed.json')"
    }
}
