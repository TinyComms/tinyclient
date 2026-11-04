# smoke, Windows. Runs inside the console session of a lab host, with a recorder already filming.
# Starts chatd on a scratch TINYCLIENT_DIR seeded with invented demo data, opens the window,
# checks the inbox through the app's own text view, opens a conversation and quits by closing the
# window. One JSON line per step into $E2E_OUT\steps.jsonl. Exits 0 only if every step held.
# Env: E2E_T0 (unix seconds when the recording began), E2E_OUT, E2E_APP_DIR (tinyclient.exe, chatd.exe,
# mediad.exe), E2E_SRC_DIR (the source tree, for scripts\e2e\win-window.ps1), E2E_SABOTAGE=1 (empty data).
$ErrorActionPreference = 'Stop'
$T0 = [double]::Parse($env:E2E_T0, [Globalization.CultureInfo]::InvariantCulture)
$Out = $env:E2E_OUT
$AppDir = $env:E2E_APP_DIR
$Sabotage = $env:E2E_SABOTAGE -eq '1'
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
# `tinyclient.exe --text`: the same lines the window's inbox is drawn from, asked of the same chatd.
function Inbox {
    $file = Join-Path $Out 'inbox.txt'
    $p = Start-Process -Wait -PassThru -WindowStyle Hidden -FilePath (Join-Path $AppDir 'tinyclient.exe') -ArgumentList '--text' `
        -RedirectStandardOutput $file -RedirectStandardError (Join-Path $Out 'inbox.err.txt')
    $all = @(Get-Content $file -ErrorAction SilentlyContinue | Where-Object { $_.Trim() })
    # The account lines and "media ..." (and "call ..." while in one) come first; the rest are conversations.
    $at = [array]::FindIndex([string[]]$all, [Predicate[string]]{ param($l) $l -like 'media *' })
    $lines = if ($at -lt 0 -or $at + 1 -ge $all.Count) { @() } else { @($all[($at + 1)..($all.Count - 1)] | Where-Object { $_ -and $_ -notlike 'call *' }) }
    [pscustomobject]@{ code = $p.ExitCode; lines = $lines }
}
function Stop-Mine {
    foreach ($id in $script:started) { Stop-Process -Id $id -Force -ErrorAction SilentlyContinue }
}

try {
    # Invented data only, in a folder of this run: chatd refuses --seed-demo without TINYCLIENT_DIR,
    # and with it never reads the credential store.
    Remove-Item -Recurse -Force $Data -ErrorAction SilentlyContinue
    New-Item -ItemType Directory -Force $Data | Out-Null
    $env:TINYCLIENT_DIR = $Data
    $env:TINYCLIENT_LOG_LEVEL = 'debug'
    $chatdArgs = if ($Sabotage) { @() } else { @('--seed-demo') }
    $chatdParams = @{ PassThru = $true; WindowStyle = 'Hidden'; FilePath = (Join-Path $AppDir 'chatd.exe'); WorkingDirectory = $Data
                      RedirectStandardOutput = (Join-Path $Out 'chatd.out.log'); RedirectStandardError = (Join-Path $Out 'chatd.err.log') }
    if ($chatdArgs) { $chatdParams.ArgumentList = $chatdArgs }
    $chatd = Start-Process @chatdParams
    $started += $chatd.Id
    $ready = $false
    for ($i = 0; $i -lt 90 -and -not $chatd.HasExited; $i++) {
        if ((Inbox).code -eq 0) { $ready = $true; break }
        Start-Sleep -Seconds 1
    }
    Step $(if ($Sabotage) { 'empty data dir ready, chatd answers (sabotage)' } else { 'seeded demo data ready' }) $ready
    if (-not $ready) { throw 'chatd never answered' }

    # The window, on the primary display, filling it so the pixel positions below stay put.
    $script:App = Start-Process -PassThru -FilePath (Join-Path $AppDir 'tinyclient.exe') -WorkingDirectory $Data
    $started += $script:App.Id
    $shown = $false
    for ($i = 0; $i -lt 60 -and -not $script:App.HasExited; $i++) {
        try { Win 'max' | Out-Null; $shown = $true; break } catch { Start-Sleep -Seconds 1 }
    }
    if ($shown) { Win 'front' | Out-Null; Start-Sleep -Seconds 4 }
    $shot1 = 'window.png'
    if ($shown) { Win 'shot' @{ Path = (Join-Path $Out $shot1) } | Out-Null }
    Step 'window on screen' $shown $shot1
    if (-not $shown) { throw 'no window' }

    # The inbox, from the app's own state: what `--text` prints is what the window lists.
    $inbox = Inbox
    $names = 'Yusuf Demirci'
    $listed = ($inbox.code -eq 0) -and ($inbox.lines.Count -ge 5) -and (($inbox.lines -join "`n") -match [regex]::Escape($names))
    Step ("inbox lists the demo conversations ({0} rows)" -f $inbox.lines.Count) $listed
    if (-not $listed) { throw 'inbox is not the demo inbox' }

    # Open a conversation with a real click on its row (window pixels: the window fills a 1024x768
    # display, so the list is where window.png shows it). Opening one marks it read, which chatd
    # keeps: the row's unread count goes from 1 to nothing in `--text`, which is the app's own word
    # for it. "Vex Morrow" is a direct conversation with one unread message in the demo.
    Start-Sleep -Seconds 2
    $who = 'Vex Morrow'
    $rowOf = { param($lines) $lines | Where-Object { $_ -match [regex]::Escape($who) } | Select-Object -First 1 }
    $before = & $rowOf (Inbox).lines
    $unreadBefore = $before -match '\s1\s+\S+\s+\S'   # "<network> <title> <unread> <age>  <preview>"
    Win 'click' @{ X = 230; Y = 220 } | Out-Null
    $opened = $false
    for ($i = 0; $i -lt 20; $i++) {
        Start-Sleep -Milliseconds 500
        $now = & $rowOf (Inbox).lines
        if ($now -and $now -notmatch '\s1\s+\S+\s+\S') { $opened = $true; break }
    }
    Start-Sleep -Seconds 2
    Win 'shot' @{ Path = (Join-Path $Out 'conversation.png') } | Out-Null
    Step "opens the conversation with $who" ($opened -and [bool]$unreadBefore) 'conversation.png'
    Start-Sleep -Seconds 2
}
catch {
    Write-Host "driver: $($_.Exception.Message)"
    $failed = $true
}
finally {
    # Quitting is what a person does: close the window. The app stops chatd only when it started it
    # itself, so chatd (started above) is stopped by its PID after.
    if ($script:App -and -not $script:App.HasExited) {
        $null = $script:App.CloseMainWindow()
        $done = $script:App.WaitForExit(15000)
        $code = if ($done) { $script:App.ExitCode } else { -1 }
        Step ("app quits cleanly (closing the window, exit {0})" -f $code) ($done -and $code -eq 0)
    }
    Copy-Item (Join-Path $Data 'logs') (Join-Path $Out 'logs') -Recurse -ErrorAction SilentlyContinue
    Stop-Mine
}
if ($failed) { exit 1 }
exit 0
