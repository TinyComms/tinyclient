# Runs on winbuild. OpenSSH is session 0, so the first call schedules this
# same script into the logged-on console and waits. The console call does the work.
param([Parameter(Position = 0)][string]$Command = "doctor")
$ErrorActionPreference = "Stop"
$root = Join-Path $env:USERPROFILE "tiny-call-e2e"
$e2e = Join-Path $root "e2e"
$art = Join-Path $e2e "artifacts"
New-Item -ItemType Directory -Force -Path $art | Out-Null
$exitFile = Join-Path $art "desk.exit"
$log = Join-Path $art "desk.log"
$session = [System.Diagnostics.Process]::GetCurrentProcess().SessionId
$console = (Get-Process explorer -ErrorAction SilentlyContinue | Select-Object -First 1).SessionId

if ($null -eq $console -or $session -ne $console) {
  Remove-Item -Force -ErrorAction SilentlyContinue $exitFile
  $taskCmd = Join-Path $e2e "scripts\desk-task.cmd"
  $tr = '"' + $taskCmd + '" ' + $Command
  # schtasks warns when the one-time start is already past. The warning is not a failure, and
  # Stop would turn that stderr line into a terminating error before /Run.
  $kept = $ErrorActionPreference
  $ErrorActionPreference = "Continue"
  $create = & schtasks.exe /Create /F /TN TinyCallE2e /SC ONCE /ST 00:00 /RL LIMITED /IT /TR $tr 2>&1 | Out-String
  $createCode = $LASTEXITCODE
  $run = ""
  $runCode = 0
  if ($createCode -eq 0) {
    $run = & schtasks.exe /Run /TN TinyCallE2e 2>&1 | Out-String
    $runCode = $LASTEXITCODE
  }
  $ErrorActionPreference = $kept
  if ($createCode -ne 0) {
    Set-Content -Path $log -Value $create -Encoding ascii
    exit $createCode
  }
  if ($runCode -ne 0) {
    Set-Content -Path $log -Value ($create + $run) -Encoding ascii
    exit $runCode
  }
  $i = 0
  while (-not (Test-Path $exitFile) -and $i -lt 180) {
    Start-Sleep -Seconds 5
    $i++
  }
  if (-not (Test-Path $exitFile)) {
    Set-Content -Path $log -Value "console task produced no exit file" -Encoding ascii
    exit 1
  }
  exit [int]((Get-Content $exitFile -TotalCount 1).Trim())
}

$ErrorActionPreference = "Continue"
$env:Path = [System.Environment]::GetEnvironmentVariable("Path", "Machine") + ";" + [System.Environment]::GetEnvironmentVariable("Path", "User")
Set-Location $e2e
$machine = Join-Path $env:USERPROFILE ".config\tiny-e2e\env.ps1"
if (Test-Path $machine) { . $machine }
$forwarded = Join-Path $e2e "desk-env.ps1"
if (Test-Path $forwarded) { . $forwarded }
& npm.cmd ci
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
$ffmpeg = @(
  (Join-Path (Get-Location) "node_modules\ffmpeg-static\ffmpeg.exe"),
  (Join-Path (Get-Location) "node_modules\ffmpeg-static\ffmpeg")
) | Where-Object { Test-Path $_ } | Select-Object -First 1
if (-not $ffmpeg) {
  & node.exe .\node_modules\ffmpeg-static\install.js
  if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
}
$listed = & node.exe .\node_modules\appium\index.js driver list --installed 2>&1 | Out-String
if ($listed -notmatch "wincore") {
  & node.exe .\node_modules\appium\index.js driver install --source=npm appium-wincore-driver
  if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
}
& npm.cmd run build
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
$env:TINY_E2E_DATA = $art
& node.exe .\dist\cli.js $Command
$code = 0
if ($null -ne $LASTEXITCODE) { $code = $LASTEXITCODE }
Set-Content -Path $exitFile -Value $code -Encoding ascii
exit $code
