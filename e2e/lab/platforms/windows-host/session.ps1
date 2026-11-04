# Runs INSIDE winbuild's console session (started by run-session.ps1 through a one-shot scheduled
# task), because only there can a recorder see the screen and an app have a window. Starts the
# recorder, then the driver, stops the recorder with "q" so the mp4 is finished, and leaves
# everything in <RunDir>\out. Writes out\session.json and then out\done, in that order.
param([Parameter(Mandatory)][string]$RunDir)
$ErrorActionPreference = 'Stop'
$cfg = Get-Content (Join-Path $RunDir 'run.json') -Raw | ConvertFrom-Json
$out = Join-Path $RunDir 'out'
New-Item -ItemType Directory -Force $out | Out-Null
$utf8 = New-Object System.Text.UTF8Encoding($false)
function Now { [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds() / 1000.0 }
$result = [ordered]@{ recorder_started = $false; driver_exit = $null; seconds = 0; t0 = 0; note = '' }
$rec = $null
try {
    $video = Join-Path $out 'recording.mp4'
    $progress = Join-Path $out 'ffmpeg-progress.txt'
    $ffout = $out.Replace('\', '/').Replace(':', '\:')
    $env:FFREPORT = "file=$ffout/ffmpeg.log:level=32"
    $psi = New-Object System.Diagnostics.ProcessStartInfo
    $psi.FileName = $cfg.ffmpeg
    $psi.Arguments = "-hide_banner -y -f gdigrab -framerate 10 -draw_mouse 1 -i desktop -c:v libx264 -preset ultrafast -pix_fmt yuv420p -progress `"$progress`" -stats_period 0.25 `"$video`""
    $psi.UseShellExecute = $false
    $psi.RedirectStandardInput = $true
    $psi.CreateNoWindow = $true
    $rec = [System.Diagnostics.Process]::Start($psi)
    # The recording starts at its first frame: that is time zero for the driver's step times.
    $deadline = (Get-Date).AddSeconds(20)
    $first = $false
    while ((Get-Date) -lt $deadline -and -not $rec.HasExited) {
        if ((Test-Path $progress) -and ((Get-Content $progress -Raw -ErrorAction SilentlyContinue) -match 'frame=[1-9]')) { $first = $true; break }
        Start-Sleep -Milliseconds 100
    }
    if (-not $first) { throw 'the recorder produced no frame within 20 s' }
    $t0 = Now
    $result.recorder_started = $true
    $result.t0 = $t0

    $env:E2E_T0 = '{0:F3}' -f $t0
    $env:E2E_OUT = $out
    $env:E2E_APP_DIR = $cfg.app_dir
    $env:E2E_SRC_DIR = $cfg.src_dir
    $env:E2E_RUN_DIR = $RunDir
    if ($cfg.sabotage) { $env:E2E_SABOTAGE = '1' } else { Remove-Item Env:\E2E_SABOTAGE -ErrorAction SilentlyContinue }
    $driver = Start-Process -PassThru -WindowStyle Hidden -FilePath powershell.exe `
        -ArgumentList '-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', "`"$($cfg.driver)`"" `
        -RedirectStandardOutput (Join-Path $out 'driver.log') -RedirectStandardError (Join-Path $out 'driver.err.log')
    $null = $driver.Handle   # without this a process started with redirected output reports no exit code
    if (-not $driver.WaitForExit([int]$cfg.driver_timeout * 1000)) {
        $result.note = "the driver ran past $($cfg.driver_timeout) s and was stopped"
        Stop-Process -Id $driver.Id -Force -ErrorAction SilentlyContinue
        $result.driver_exit = 124
    } else {
        $result.driver_exit = $driver.ExitCode
    }
    # Whatever the driver left running in this run's folder (and nothing else), by PID.
    Get-CimInstance Win32_Process | Where-Object { $_.ExecutablePath -and $_.ExecutablePath.StartsWith($RunDir, [StringComparison]::OrdinalIgnoreCase) } |
        ForEach-Object { Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue; $result.note += " stopped stray $($_.Name) $($_.ProcessId)" }
    Start-Sleep -Seconds 1   # the last frame shows the end state
} catch {
    $result.note = "session: $($_.Exception.Message)"
} finally {
    if ($rec) {
        try { $rec.StandardInput.WriteLine('q'); $rec.StandardInput.Flush() } catch {}
        if (-not $rec.WaitForExit(20000)) { Stop-Process -Id $rec.Id -Force -ErrorAction SilentlyContinue; $result.note += ' recorder had to be killed' }
        if ($result.t0 -gt 0) { $result.seconds = [math]::Round((Now) - $result.t0, 2) }
    }
    [IO.File]::WriteAllText((Join-Path $out 'session.json'), ($result | ConvertTo-Json -Compress), $utf8)
    [IO.File]::WriteAllText((Join-Path $out 'done'), 'done', $utf8)
}
