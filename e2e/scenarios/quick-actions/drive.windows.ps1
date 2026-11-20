# quick-actions, Windows. Runs inside the console session of a lab host, with a recorder filming.
# A launcher's quick action is an argument a second launch carries (`--dock-action <index>`, what
# the jump list gpui writes launches), handed to the running window over the single-instance pipe
# the way an invite link is; the second process exits. New Message opens the "to whom" switcher
# (the picture changes), Inbox is handed over, and Quit ends the app. On a build without the
# actions the second launch ignores the argument and opens its own window, so it never exits and
# the steps fail.
#
# The jump list itself is written by the shell for the app's identity; clicking it is not
# scriptable headlessly. The registered items and their arguments are proven by the tinyclient
# tests (crates/tinyclient/src/quick.rs); this driver proves what the arguments do.
#
# Env: E2E_T0, E2E_OUT, E2E_APP_DIR (tinyclient.exe, chatd.exe, mediad.exe), E2E_SRC_DIR (the
# source tree, for scripts\e2e\win-window.ps1).
$ErrorActionPreference = 'Stop'
$T0 = [double]::Parse($env:E2E_T0, [Globalization.CultureInfo]::InvariantCulture)
$Out = $env:E2E_OUT
$AppDir = $env:E2E_APP_DIR
$WinWindow = Join-Path $env:E2E_SRC_DIR 'scripts\e2e\win-window.ps1'
$Data = Join-Path (Split-Path $Out -Parent) 'tinyclient-data'
$utf8 = New-Object System.Text.UTF8Encoding($false)
$StepsFile = Join-Path $Out 'steps.jsonl'
$failed = $false
$started = @()   # PIDs this driver started: the only ones it ever stops

function Now { [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds() / 1000.0 }
function Step([string]$words, [bool]$ok, [string]$shot = '') {
    $line = [ordered]@{ t = [math]::Round((Now) - $T0, 1); step = $words; ok = $ok }
    if ($shot) { $line.shot = $shot }
    [IO.File]::AppendAllText($StepsFile, (($line | ConvertTo-Json -Compress) + "`n"), $utf8)
    Write-Host ("[{0,6:N1}s] {1}: {2}" -f $line.t, $(if ($ok) { 'ok    ' } else { 'FAILED' }), $words)
    if (-not $ok) { $script:failed = $true }
}
function Win([string]$action, [hashtable]$more = @{}) {
    $args = @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', $WinWindow, '-ProcessId', $script:App.Id, '-Action', $action)
    foreach ($k in $more.Keys) { $args += "-$k"; $args += "$($more[$k])" }
    & powershell.exe @args 2>&1 | Out-String
}
function ShotHash([string]$name) {
    $file = Join-Path $Out "$name.png"
    Win 'shot' @{ Path = $file } | Out-Null
    (Get-FileHash $file -Algorithm MD5).Hash
}
# A second launch carrying one quick action. Returns its exit code, 125 if it never handed over
# and had to be killed (what a build without the actions does: it opens its own window).
function ActionLaunch([string]$flag, [string]$value) {
    $p = Start-Process -PassThru -FilePath (Join-Path $AppDir 'tinyclient.exe') -ArgumentList $flag, $value -WorkingDirectory $Data
    $script:started += $p.Id
    if (-not $p.WaitForExit(20000)) {
        Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue
        return 125
    }
    return $p.ExitCode
}
function Stop-Mine {
    foreach ($id in $script:started) { Stop-Process -Id $id -Force -ErrorAction SilentlyContinue }
}

try {
    # Invented data only, in a folder of this run.
    Remove-Item -Recurse -Force $Data -ErrorAction SilentlyContinue
    New-Item -ItemType Directory -Force $Data | Out-Null
    $env:TINYCLIENT_DIR = $Data
    $env:TINYCLIENT_LOG_LEVEL = 'debug'
    $chatd = Start-Process -PassThru -WindowStyle Hidden -FilePath (Join-Path $AppDir 'chatd.exe') -ArgumentList '--seed-demo' `
        -WorkingDirectory $Data -RedirectStandardOutput (Join-Path $Out 'chatd.out.log') -RedirectStandardError (Join-Path $Out 'chatd.err.log')
    $started += $chatd.Id
    $ready = $false
    $text = Join-Path $Out 'inbox.txt'
    for ($i = 0; $i -lt 90 -and -not $chatd.HasExited; $i++) {
        $ask = Start-Process -Wait -PassThru -WindowStyle Hidden -FilePath (Join-Path $AppDir 'tinyclient.exe') -ArgumentList '--text' `
            -RedirectStandardOutput $text -RedirectStandardError (Join-Path $Out 'inbox.err.txt')
        if ($ask.ExitCode -eq 0 -and (Get-Content $text -ErrorAction SilentlyContinue) -match [regex]::Escape('Yusuf Demirci')) { $ready = $true; break }
        Start-Sleep -Seconds 1
    }
    Step 'seeded demo data ready' $ready
    if (-not $ready) { throw 'chatd never answered' }

    # The window, on the primary display, filling it.
    $script:App = Start-Process -PassThru -FilePath (Join-Path $AppDir 'tinyclient.exe') -WorkingDirectory $Data
    $started += $script:App.Id
    $shown = $false
    for ($i = 0; $i -lt 60 -and -not $script:App.HasExited; $i++) {
        try { Win 'max' | Out-Null; $shown = $true; break } catch { Start-Sleep -Seconds 1 }
    }
    if ($shown) { Win 'front' | Out-Null; Start-Sleep -Seconds 4; Win 'front' | Out-Null; Start-Sleep -Seconds 2 }
    Step 'window on screen' $shown 'window'
    if (-not $shown) { throw 'no window' }

    # New Message: the second launch hands the action over and exits; the switcher opens.
    $before = ShotHash 'inbox'
    $rc = ActionLaunch '--dock-action' '0'
    Step ("New Message handed to the running window (second launch exit {0})" -f $rc) ($rc -eq 0)
    $switcher = $false
    for ($i = 0; $i -lt 16; $i++) {
        Start-Sleep -Milliseconds 500
        if ((ShotHash 'switcher') -ne $before) { $switcher = $true; break }
    }
    Step 'the window shows the new-message switcher' $switcher 'switcher'

    # Inbox: handed over the same way.
    $rc = ActionLaunch '--dock-action' '1'
    Step ("Inbox handed to the running window (second launch exit {0})" -f $rc) ($rc -eq 0)

    # Quit: handed over, and the running app ends by itself with status 0.
    $rc = ActionLaunch '--dock-action' '2'
    Step ("Quit handed to the running window (second launch exit {0})" -f $rc) ($rc -eq 0)
    $quit = $script:App.WaitForExit(15000)
    $code = if ($quit) { $script:App.ExitCode } else { -1 }
    $script:App = $null
    Step ("the app quit (exit {0})" -f $code) ($quit -and $code -eq 0)
}
catch {
    Write-Host "driver: $($_.Exception.Message)"
    $failed = $true
}
finally {
    Copy-Item (Join-Path $Data 'logs') (Join-Path $Out 'logs') -Recurse -ErrorAction SilentlyContinue
    Stop-Mine
}
if ($failed) { exit 1 }
exit 0
