# Headless daemon IPC/live-event regression; no native UI proof.
$ErrorActionPreference = 'Stop'
$drive = Join-Path $PSScriptRoot 'drive.py'
if (Get-Command py -ErrorAction SilentlyContinue) {
    & py -3 $drive
    exit $LASTEXITCODE
}
if (Get-Command python -ErrorAction SilentlyContinue) {
    & python $drive
    exit $LASTEXITCODE
}
if (Get-Command python3 -ErrorAction SilentlyContinue) {
    & python3 $drive
    exit $LASTEXITCODE
}
Write-Host 'python is not on PATH (tried py -3, python, python3)'
exit 1
