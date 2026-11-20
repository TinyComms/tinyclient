# Measures memory, CPU and size on disk for tinyclient and, when asked, the official apps beside
# it, on Windows. It writes one JSON file the site's numbers are filled in from
# (.github/scripts/site-perf.py). docs/perf-measure.md says when each mode runs.
#
#   -Mode demo      A build in -Bin, started on invented data in a scratch TINYCLIENT_DIR. Needs no
#                   account, so CI runs it on every push to main.
#   -Mode compare   The apps as they are running now, signed in: the official apps and the installed
#                   tinyclient. Run by hand on a desktop where all six are open.
#
# The output names the apps, their versions and the Windows release. It never holds the machine's
# name, its user, its hardware or a path under the user's profile.
param(
    [Parameter(Mandatory)][ValidateSet("demo", "compare")][string]$Mode,
    [Parameter(Mandatory)][string]$Out,
    # demo: the directory holding tinyclient.exe and chatd.exe.
    [string]$Bin,
    # Seconds to let the apps settle before the reading (demo only).
    [int]$Settle = 30,
    # Seconds the CPU reading covers.
    [int]$CpuSeconds = 60,
    # compare: fail when one of the six is not running, instead of leaving it out.
    [switch]$RequireAll,
    # Where the reading was taken, in words fit to publish, such as "a GitHub-hosted Windows
    # runner". Left out, the result only names the Windows release.
    [string]$Where,
    # The commit the build was made from.
    [string]$Commit
)
$ErrorActionPreference = "Stop"

# id, name, the process names that are the app, and where its files are when it is not running.
$official = @(
    @{ id = "teams";    name = "Microsoft Teams"; roots = @("ms-teams") },
    @{ id = "slack";    name = "Slack";           roots = @("slack") },
    @{ id = "discord";  name = "Discord";         roots = @("Discord") },
    @{ id = "signal";   name = "Signal";          roots = @("Signal") },
    @{ id = "telegram"; name = "Telegram";        roots = @("Telegram") }
)
$ours = @{ id = "tinyclient"; name = "tinyclient"; roots = @("tinyclient", "chatd", "mediad", "tinyclient-login") }

function Get-Snapshot {
    # One row per process: its parent, its command line (for WebView2 helpers, which name the
    # program they belong to) and the private working set, the number Task Manager calls Memory.
    $private = @{}
    Get-CimInstance Win32_PerfRawData_PerfProc_Process | ForEach-Object { $private[[int]$_.IDProcess] = [int64]$_.WorkingSetPrivate }
    Get-CimInstance Win32_Process | ForEach-Object {
        [pscustomobject]@{
            Id         = [int]$_.ProcessId
            Parent     = [int]$_.ParentProcessId
            Name       = [System.IO.Path]::GetFileNameWithoutExtension($_.Name)
            Path       = $_.ExecutablePath
            Command    = $_.CommandLine
            Private    = [int64]($private[[int]$_.ProcessId])
            WorkingSet = [int64]$_.WorkingSetSize
            Commit     = [int64]$_.PrivatePageCount
            CpuTicks   = [int64]$_.KernelModeTime + [int64]$_.UserModeTime
        }
    }
}

# The app's processes: the ones with its names, everything they started, and the WebView2
# processes that say they belong to it. -Only keeps just the processes with those ids and what
# they started, so a demo run beside a tinyclient already open on the machine counts only itself.
function Select-App($snapshot, [string[]]$roots, [int[]]$Only) {
    $mine = @{}
    foreach ($p in $snapshot) {
        if ($Only) { if ($Only -notcontains $p.Id) { continue } }
        elseif ($roots -notcontains $p.Name) { continue }
        $mine[$p.Id] = $p
    }
    if ($mine.Count -eq 0) { return @() }
    foreach ($p in $snapshot) {
        if ($Only -or $p.Name -ne "msedgewebview2" -or -not $p.Command) { continue }
        foreach ($root in $roots) {
            if ($p.Command -match [regex]::Escape("--webview-exe-name=$root.exe")) { $mine[$p.Id] = $p }
        }
    }
    do {
        $added = 0
        foreach ($p in $snapshot) {
            if (-not $mine.ContainsKey($p.Id) -and $mine.ContainsKey($p.Parent) -and $p.Name -ne "conhost") { $mine[$p.Id] = $p; $added++ }
        }
    } while ($added -gt 0)
    @($mine.Values)
}

function Get-DirectoryMegabytes([string]$Dir, [string[]]$Skip = @()) {
    if (-not $Dir -or -not (Test-Path -LiteralPath $Dir)) { return $null }
    $bytes = 0L
    foreach ($entry in Get-ChildItem -LiteralPath $Dir -Force -ErrorAction SilentlyContinue) {
        if ($Skip -contains $entry.Name) { continue }
        if ($entry.PSIsContainer) {
            $bytes += (Get-ChildItem -LiteralPath $entry.FullName -Recurse -File -Force -ErrorAction SilentlyContinue | Measure-Object Length -Sum).Sum
        } else {
            $bytes += $entry.Length
        }
    }
    [int][math]::Round($bytes / 1MB)
}

# The directory the app's own files are in: the folder of its main program. Telegram keeps the
# account beside its program, in tdata, which is the person's data and not the app.
function Get-AppFiles($processes, [string[]]$roots) {
    $main = $processes | Where-Object { $roots -contains $_.Name -and $_.Path } | Select-Object -First 1
    if (-not $main) { return $null }
    $dir = Split-Path -Parent $main.Path
    $version = (Get-Item -LiteralPath $main.Path).VersionInfo.ProductVersion
    if ($version) { $version = ($version -split "[+ ]")[0] }
    # tinyclient's programs carry no version resource; each prints its version instead.
    $chatd = Join-Path $dir "chatd.exe"
    if (-not $version -and (Test-Path -LiteralPath $chatd)) { $version = ((& $chatd --version | Out-String).Trim() -split " ")[-1] }
    [pscustomobject]@{ Megabytes = (Get-DirectoryMegabytes $dir @("tdata", "log.txt")); Version = $version }
}

function Measure-Apps($apps, [int[]]$Only) {
    $before = Get-Snapshot
    Start-Sleep -Seconds $CpuSeconds
    $after = Get-Snapshot
    $cores = [Environment]::ProcessorCount
    foreach ($app in $apps) {
        $now = @(Select-App $after $app.roots $Only)
        if ($now.Count -eq 0) {
            if ($RequireAll) { throw "$($app.name) is not running" }
            Write-Host "$($app.name): not running, left out"
            continue
        }
        $then = @{}
        foreach ($p in (Select-App $before $app.roots $Only)) { $then[$p.Id] = $p.CpuTicks }
        $ticks = 0L
        foreach ($p in $now) { if ($then.ContainsKey($p.Id)) { $ticks += $p.CpuTicks - $then[$p.Id] } }
        $files = Get-AppFiles $now $app.roots
        $row = [ordered]@{
            id             = $app.id
            name           = $app.name
            version        = $files.Version
            processes      = $now.Count
            memory_mb      = [int][math]::Round((($now | Measure-Object Private -Sum).Sum) / 1MB)
            working_set_mb = [int][math]::Round((($now | Measure-Object WorkingSet -Sum).Sum) / 1MB)
            commit_mb      = [int][math]::Round((($now | Measure-Object Commit -Sum).Sum) / 1MB)
            # Percent of one core over the reading. Ticks are 100 ns.
            cpu_percent    = [math]::Round(100.0 * ($ticks / 1e7) / $CpuSeconds, 1)
            disk_mb        = $files.Megabytes
            parts          = @($now | Group-Object Name | Sort-Object Name | ForEach-Object {
                    [ordered]@{ name = $_.Name; processes = $_.Count; memory_mb = [int][math]::Round((($_.Group | Measure-Object Private -Sum).Sum) / 1MB) }
                })
        }
        Write-Host ("{0,-16} {1,3} processes  {2,6} MB memory  {3,6} MB working set  {4,5}% cpu  {5,6} MB on disk  {6}" -f $row.name, $row.processes, $row.memory_mb, $row.working_set_mb, $row.cpu_percent, $row.disk_mb, $row.version)
        $row
    }
}

$os = (Get-CimInstance Win32_OperatingSystem).Caption -replace "^Microsoft ", "" -replace " (Pro|Home|Enterprise|Education).*$", ""
$result = [ordered]@{
    schema      = 1
    mode        = $Mode
    measured    = (Get-Date).ToString("yyyy-MM-dd")
    os          = $os
    cpu_seconds = $CpuSeconds
    memory      = "private working set, the Memory column of Task Manager, summed over the app's processes"
}
if ($Where) { $result.where = $Where }
if ($Commit) { $result.commit = $Commit }

if ($Mode -eq "compare") {
    $result.apps = @(Measure-Apps ($official + $ours) @())
} else {
    if (-not $Bin) { throw "-Mode demo needs -Bin" }
    $Bin = (Resolve-Path -LiteralPath $Bin).Path
    foreach ($program in "tinyclient", "chatd") {
        if (-not (Test-Path -LiteralPath (Join-Path $Bin "$program.exe"))) { throw "no $program.exe in $Bin" }
    }
    $scratch = Join-Path ([System.IO.Path]::GetTempPath()) ("tinyclient-perf-" + [guid]::NewGuid().ToString("N").Substring(0, 8))
    New-Item -ItemType Directory -Path $scratch | Out-Null
    $started = @()
    $previous = $env:TINYCLIENT_DIR
    try {
        # TINYCLIENT_DIR moves the socket, the database and the logs there and turns sign-in off,
        # so this never touches a real account, or a tinyclient already running on the machine.
        $env:TINYCLIENT_DIR = $scratch
        # chatd --seed-demo fills the directory and stays up as the daemon the window talks to.
        $started += Start-Process -FilePath (Join-Path $Bin "chatd.exe") -ArgumentList "--seed-demo" -PassThru -WindowStyle Hidden
        Start-Sleep -Seconds 5
        if ($started[0].HasExited) { throw "chatd --seed-demo exited with $($started[0].ExitCode)" }
        $started += Start-Process -FilePath (Join-Path $Bin "tinyclient.exe") -PassThru
        Start-Sleep -Seconds $Settle
        foreach ($p in $started) { if ($p.HasExited) { throw "$($p.ProcessName) exited with $($p.ExitCode) before the reading" } }
        $result.settle_seconds = $Settle
        $result.data = "invented demo data: Discord, two Slack workspaces, Telegram, Teams, Signal and a Tiny server"
        $result.apps = @(Measure-Apps @($ours) @($started | ForEach-Object { $_.Id }))
        if ($result.apps.Count -ne 1) { throw "tinyclient was not running at the reading" }
        # A build directory holds far more than the app, so the size is the four programs.
        $bytes = 0L
        foreach ($program in $ours.roots) {
            $file = Join-Path $Bin "$program.exe"
            if (Test-Path -LiteralPath $file) { $bytes += (Get-Item -LiteralPath $file).Length }
        }
        $result.apps[0].disk_mb = [int][math]::Round($bytes / 1MB)
        $result.apps[0].version = ((& (Join-Path $Bin "chatd.exe") --version | Out-String).Trim() -split " ")[-1]
    } finally {
        $env:TINYCLIENT_DIR = $previous
        # Only what this run started, and what those started: never a tinyclient that was already open.
        if ($started.Count -gt 0) {
            foreach ($p in (Select-App (Get-Snapshot) $ours.roots @($started | ForEach-Object { $_.Id }))) {
                Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue
            }
        }
        Start-Sleep -Seconds 1
        Remove-Item -LiteralPath $scratch -Recurse -Force -ErrorAction SilentlyContinue
    }
}

$json = $result | ConvertTo-Json -Depth 6
# Belt and braces: nothing that names this machine or its user may leave it.
foreach ($private in @($env:COMPUTERNAME, $env:USERNAME, $env:USERPROFILE) | Where-Object { $_ -and $_.Length -gt 2 }) {
    if ($json -match [regex]::Escape($private)) { throw "the result names this machine or its user; nothing was written" }
}
$parent = Split-Path -Parent $Out
if ($parent) { New-Item -ItemType Directory -Force -Path $parent | Out-Null }
[System.IO.File]::WriteAllText($Out, ($json -replace "`r`n", "`n") + "`n", [System.Text.UTF8Encoding]::new($false))
Write-Host "wrote $Out"
