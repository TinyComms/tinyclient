# Runs over ssh. Lays out one run's folder, then starts session.ps1 in the console session through a
# one-shot scheduled task (/IT, builder, limited: no password, no UAC), waits for out\done, and
# removes the task. Prints "SESSION=<json>"; "SESSION=timeout" and exit 1 if it never finished.
#   <Work>\in\scenario-<Id>\ and run-<Id>.json come from the lab; <Work>\bin\<Sha> from build.ps1.
param(
    [Parameter(Mandatory)][string]$Id,
    [Parameter(Mandatory)][string]$Sha,
    [string]$Work = 'C:\lab',
    [int]$Timeout = 420
)
$ErrorActionPreference = 'Stop'
$runDir = Join-Path $Work "runs\$Id"
$out = Join-Path $runDir 'out'
$name = "lab-$Id"

# A run starts from nothing. (Only this run's own folder is removed.)
Remove-Item -Recurse -Force $runDir -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force $runDir, $out, (Join-Path $runDir 'scenario') | Out-Null
Copy-Item (Join-Path $Work "in\scenario-$Id\*") (Join-Path $runDir 'scenario') -Recurse
Move-Item (Join-Path $Work "in\run-$Id.json") (Join-Path $runDir 'run.json') -Force
Remove-Item (Join-Path $Work "in\scenario-$Id") -Recurse -Force
# The programs run from the run's own folder, so a stray one is recognisable by its path.
Copy-Item (Join-Path $Work "bin\$Sha") (Join-Path $runDir 'app') -Recurse

$action = "powershell.exe -NoProfile -WindowStyle Hidden -ExecutionPolicy Bypass -File `"$Work\host\session.ps1`" -RunDir `"$runDir`""
$finished = $false
try {
    schtasks /create /tn $name /tr $action /sc once /st 23:59 /ru builder /it /rl limited /f | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "schtasks /create failed ($LASTEXITCODE)" }
    schtasks /run /tn $name | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "schtasks /run failed ($LASTEXITCODE)" }
    $deadline = (Get-Date).AddSeconds($Timeout)
    while ((Get-Date) -lt $deadline) {
        if (Test-Path (Join-Path $out 'done')) { $finished = $true; break }
        Start-Sleep -Milliseconds 500
    }
} finally {
    cmd /c "schtasks /end /tn $name >nul 2>&1"
    cmd /c "schtasks /delete /tn $name /f >nul 2>&1"
}
# Old run folders go; the three newest stay for looking at.
Get-ChildItem (Join-Path $Work 'runs') -Directory | Sort-Object LastWriteTime -Descending | Select-Object -Skip 3 |
    ForEach-Object { Remove-Item -Recurse -Force $_.FullName -ErrorAction SilentlyContinue }
# One file to bring back.
& tar.exe -cf (Join-Path $runDir 'out.tar') -C $out .
if (-not $finished) { Write-Host 'SESSION=timeout'; exit 1 }
Write-Host ("SESSION=" + (Get-Content (Join-Path $out 'session.json') -Raw).Trim())
