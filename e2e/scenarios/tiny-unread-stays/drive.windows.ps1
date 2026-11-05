# tiny-unread-stays on Windows. The lab runs this in the console session it is already filming.
# Python is the standard-library fixture and the driver; chatd and tinyclient come from E2E_APP_DIR.
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
