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
                  from mediad (TINYCLIENT_MEDIAD_AUDIO). With -Join it posts window messages that open
                  the room and join (GPUI has no UI Automation tree on Windows); -Camera turns the camera on.
  -Phase android  Writes Bea's session into the installed debug app (run-as) for emulator-5554.
  -Phase all      server, desktop, android.
  -Phase call     The whole proof with nothing on this PC's screen: server, then Alice on the
                  tiny-linux VM's X11 desktop (vm-desktop.sh: built there, TINYCLIENT_DEBUG_JOIN,
                  screen share, 440 Hz tone, ffmpeg recording), then Bea on the emulator by a debug
                  launch (tiny.join, camera on, speaker, 660 Hz tone), -CallSeconds in the call,
                  then logs, recording and both ends' received audio into -Proof with
                  tone-check.py's verdict, and everything stopped.
                  pwsh scripts/e2e/local-tiny-call.ps1 -Phase call -InstallApk -Proof <dir>
  -Stop           Stops only the processes this script started (pids in <State>\pids.json) and
                  removes the Postgres container.

.EXAMPLE
  pwsh scripts/e2e/local-tiny-call.ps1 -Phase all
#>
param(
    [ValidateSet('server', 'desktop', 'android', 'all', 'call', 'serve', 'vm', 'phone', 'collect')]
    [string]$Phase = 'all',
    [switch]$Stop,
    # Desktop: post clicks that open the community, the room, and Join channel; -Camera also
    # turns the camera on (a real webcam if one is attached).
    [switch]$Join,
    [switch]$Camera,
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
    [string]$Serial = 'emulator-5554',
    # -Phase call: the VM that runs the desktop participant (scripts/e2e/vm-desktop.sh).
    [string]$VmHost = 'tiny-linux',
    [string]$VmDir = '~/tiny-e2e',
    [switch]$SkipVmBuild,
    [switch]$InstallApk,
    [int]$CallSeconds = 60,
    [string]$Proof = (Join-Path $env:LOCALAPPDATA 'tiny-e2e\proof')
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
$ServerTarget = Join-Path $env:LOCALAPPDATA 'tiny-e2e\server-target'
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
    if ($Phase -in 'call', 'collect' -or $Stop) {
        ssh $VmHost "bash $VmDir/src/scripts/e2e/vm-desktop.sh stop $VmDir/run 2>/dev/null; true" | Out-Null
        adb -s $Serial shell am force-stop dev.tinyclient.app 2>$null
    }
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
    # Our own target directory: the shared worktree's target/ has been cleaned under a running test.
    cargo build -q -p tinyserver --manifest-path (Join-Path $TinyServerDir 'Cargo.toml') --target-dir $ServerTarget
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

    Start-TinyServer
    Write-Host "room: $($seed.room.name) $($seed.room.id)"
}

# `tinyserver serve` on the database Start-Server made; also -Phase serve after a crash. Run under
# cmd so its exit code lands in the log: a debug tinyserver has died silently when a call ended.
function Start-TinyServer {
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
    $exe = Join-Path $ServerTarget 'debug\tinyserver.exe'
    $env:DATABASE_URL_MIGRATE = $MigrateUrl
    $env:DATABASE_URL = $RuntimeUrl
    $log = Join-Path $Logs 'tinyserver.log'
    $wrap = Join-Path $State 'tinyserver.cmd'
    "@`"$exe`" serve >> `"$log.out`" 2>> `"$log`"`r`n@echo tinyserver exited with code %errorlevel% >> `"$log`"`r`n" | Set-Content -NoNewline $wrap
    $srv = Start-Process -PassThru -WindowStyle Hidden cmd -WorkingDirectory $State -ArgumentList '/c', "`"$wrap`""
    Save-Pid 'tinyserver-cmd' $srv.Id
    $health = Wait-Http "http://127.0.0.1:$HttpPort/health" 90
    $child = Get-CimInstance Win32_Process -Filter "ParentProcessId=$($srv.Id) and Name='tinyserver.exe'"
    Save-Pid 'tinyserver' $child.ProcessId
    Write-Host "tinyserver up: $($health | ConvertTo-Json -Compress)"
}

function Get-Seed { Get-Content (Join-Path $State 'seed.json') -Raw | ConvertFrom-Json }

function Start-Desktop {
    $seed = Get-Seed
    $exe = Join-Path $Repo 'target\debug\tinyclient.exe'
    if (-not (Test-Path $exe)) { throw "build first: cargo build -p tinyclient --features gui -p chatd -p mediad" }
    $dir = Join-Path $State 'desktop-a'
    Remove-Item -Recurse -Force $dir -ErrorAction SilentlyContinue
    New-Item -ItemType Directory -Force $dir | Out-Null
    # TINYCLIENT_DIR turns the keyring off and makes chatd read the session from this variable
    # (crates/chatd/src/session.rs); the name carries the network label, space and all.
    $session = $seed.sessions.a | ConvertTo-Json -Compress
    $vars = @{
        'TINYCLIENT_DIR'               = $dir
        'TINYCLIENT_SESSION_TINY SERVER' = $session
        'TINYCLIENT_MEDIAD_AUDIO'      = $Audio
        'TINYCLIENT_MEDIAD_SINK'       = (Join-Path $dir 'played.pcm')
        'TINYCLIENT_LOG_LEVEL'         = 'debug'
    }
    foreach ($k in $vars.Keys) { [Environment]::SetEnvironmentVariable($k, $vars[$k], 'Process') }
    $p = Start-Process -PassThru $exe -WorkingDirectory $dir
    foreach ($k in $vars.Keys) { [Environment]::SetEnvironmentVariable($k, $null, 'Process') }
    Save-Pid 'tinyclient-a' $p.Id
    # Maximized on the primary display keeps the positions below stable (MoveWindow to a fixed
    # size misbehaves across monitors with different scaling). Then it goes behind every other
    # window so it does not cover someone working at the desk; PrintWindow still captures it.
    $win = Join-Path $PSScriptRoot 'win-window.ps1'
    for ($i = 0; $i -lt 60; $i++) {
        try { & $win -ProcessId $p.Id -Action max; break } catch { Start-Sleep 1 }
    }
    Start-Sleep 3
    & $win -ProcessId $p.Id -Action back
    Write-Host "desktop tinyclient pid $($p.Id), TINYCLIENT_DIR $dir, logs $dir\logs"
    if ($Join) {
        # Window pixels for a maximized window on a 1920x1080 primary display at 100% scale,
        # measured from shots (win-window.ps1 -Action shot). Posted messages, not real input.
        $steps = @(
            @{ what = 'community'; x = 43; y = 195 },
            @{ what = 'room Proof'; x = 135; y = 102 },
            @{ what = 'Join channel'; x = 1037; y = 625 }
        )
        if ($Camera) { $steps += @{ what = 'Turn camera on'; x = 1010; y = 991 } }
        foreach ($s in $steps) {
            Start-Sleep 3
            & $win -ProcessId $p.Id -Action post -X $s.x -Y $s.y
            Write-Host "posted click: $($s.what)"
        }
        Start-Sleep 5
        & $win -ProcessId $p.Id -Action shot -Path (Join-Path $State 'desktop-a-joined.png')
        Get-Content (Join-Path $dir 'logs\chatd.log') | Select-String 'call state' | Select-Object -Last 2
    }
}

function Push-AndroidSession {
    $seed = Get-Seed
    $b = $seed.sessions.b.psobject.Copy()
    # The emulator reaches the host's loopback at 10.0.2.2.
    $b.base_url = "http://10.0.2.2:$HttpPort"
    $json = $b | ConvertTo-Json -Compress
    $entry = [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($json))
    $file = Join-Path $State 'android-sessions.json'
    [IO.File]::WriteAllText($file, (@{ 'tinyclient/Tiny server' = $entry } | ConvertTo-Json -Compress))
    $pkg = 'dev.tinyclient.app'
    adb -s $Serial shell am force-stop $pkg
    adb -s $Serial push $file /data/local/tmp/tiny-sessions.json | Out-Null
    adb -s $Serial shell "run-as $pkg sh -c 'mkdir -p files && cp /data/local/tmp/tiny-sessions.json files/sessions.json && chmod 600 files/sessions.json && ls -l files'"
    adb -s $Serial shell rm /data/local/tmp/tiny-sessions.json
    Write-Host "Bea's session written to $pkg files/sessions.json (base_url $($b.base_url))"
}

function Get-LanIp {
    (Get-NetIPAddress -AddressFamily IPv4 -InterfaceAlias 'Ethernet', 'Wi-Fi' -ErrorAction SilentlyContinue | Select-Object -First 1).IPAddress
}

# The desktop participant on a Linux VM's own X11 desktop (scripts/e2e/vm-desktop.sh), so the
# test never opens a window on this PC. Alice, joined by TINYCLIENT_DEBUG_JOIN, sharing the VM's
# screen, sending a 440 Hz tone and keeping what she hears in played.pcm; ffmpeg records the screen.
function Start-Vm {
    $seed = Get-Seed
    $lan = Get-LanIp
    if (-not $SkipVmBuild) {
        $tar = Join-Path $State 'src.tar.gz'
        git -C $Repo archive --format=tar.gz -o $tar HEAD
        scp -q $tar "${VmHost}:/tmp/tiny-e2e-src.tar.gz"
        ssh $VmHost "rm -rf $VmDir/src && mkdir -p $VmDir/src && tar xzf /tmp/tiny-e2e-src.tar.gz -C $VmDir/src && rm /tmp/tiny-e2e-src.tar.gz"
        ssh $VmHost "bash $VmDir/src/scripts/e2e/vm-desktop.sh build $VmDir/src > $VmDir/build.log 2>&1; echo build exit `$?"
    }
    # The VM must reach this PC's server: health over the LAN.
    ssh $VmHost "curl -s --max-time 5 http://${lan}:$HttpPort/health" | Write-Host
    $a = $seed.sessions.a.psobject.Copy()
    $a.base_url = "http://${lan}:$HttpPort"
    $session = ($a | ConvertTo-Json -Compress) -replace "'", "'\''"
    $join = "|$($seed.room.name)|share"
    ssh $VmHost "bash $VmDir/src/scripts/e2e/vm-desktop.sh stop $VmDir/run >/dev/null 2>&1; rm -rf $VmDir/run; bash $VmDir/src/scripts/e2e/vm-desktop.sh start $VmDir/src $VmDir/run '$session' '$join' tone:440"
}

# Bea on the emulator: her session, the debug APK, then a debug launch that opens the room and
# joins with the camera on and the sound on the speaker; mediad sends a 660 Hz tone and keeps
# what it hears in the app's files/played.pcm. No taps.
function Start-Android {
    Push-AndroidSession
    $apk = Join-Path $Repo 'mobile\android\app\build\outputs\apk\debug\app-debug.apk'
    if ($InstallApk) {
        adb -s $Serial install -r $apk | Write-Host
        Push-AndroidSession
    }
    $seed = Get-Seed
    $pkg = 'dev.tinyclient.app'
    adb -s $Serial shell pm grant $pkg android.permission.CAMERA
    adb -s $Serial shell pm grant $pkg android.permission.RECORD_AUDIO
    adb -s $Serial shell pm grant $pkg android.permission.POST_NOTIFICATIONS 2>$null
    adb -s $Serial logcat -c
    $join = "|$($seed.room.name)|audio=tone:660|sink=/data/user/0/$pkg/files/played.pcm"
    adb -s $Serial shell "am start -n $pkg/.TinyActivity --es tiny.join '$join' --ez tiny.camera true --ez tiny.speaker true" | Write-Host
}

# Logs, the VM's recording and both ends' received audio into $Proof, then the 440/660 Hz check.
function Save-Evidence {
    New-Item -ItemType Directory -Force $Proof | Out-Null
    Copy-Item (Join-Path $Logs '*') $Proof -Force
    Copy-Item (Join-Path $State 'seed.json') $Proof -Force
    adb -s $Serial logcat -d > (Join-Path $Proof 'android-logcat.txt')
    # PowerShell's > is text; cmd's keeps the bytes.
    cmd /c "adb -s $Serial exec-out run-as dev.tinyclient.app cat files/played.pcm > `"$(Join-Path $Proof 'android-played.pcm')`""
    ssh $VmHost "bash $VmDir/src/scripts/e2e/vm-desktop.sh stop $VmDir/run"
    ssh $VmHost "cd $VmDir/run && tar czf /tmp/tiny-e2e-vm.tgz screen.mp4 played.pcm tinyclient.log ffmpeg.log data/logs 2>/dev/null; true"
    scp -q "${VmHost}:/tmp/tiny-e2e-vm.tgz" (Join-Path $Proof 'vm.tgz')
    tar xzf (Join-Path $Proof 'vm.tgz') -C $Proof
    Remove-Item (Join-Path $Proof 'vm.tgz')
    python -I (Join-Path $PSScriptRoot 'tone-check.py') (Join-Path $Proof 'played.pcm') (Join-Path $Proof 'android-played.pcm') | Tee-Object (Join-Path $Proof 'tone-check.txt')
}

if ($Phase -in 'server', 'all', 'call') { Start-Server }
if ($Phase -in 'desktop', 'all') { Start-Desktop }
if ($Phase -in 'android', 'all') { Push-AndroidSession }
if ($Phase -eq 'serve') { Start-TinyServer }
if ($Phase -eq 'vm') { Start-Vm }
if ($Phase -eq 'phone') { Start-Android }
if ($Phase -eq 'collect') { Save-Evidence }
if ($Phase -eq 'call') {
    Start-Vm
    Start-Android
    Write-Host "in the call for $CallSeconds s"
    Start-Sleep $CallSeconds
    Save-Evidence
    Stop-Ours
    Write-Host "evidence in $Proof"
}

