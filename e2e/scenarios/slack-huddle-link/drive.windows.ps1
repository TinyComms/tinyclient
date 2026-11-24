# slack-huddle-link on Windows. The lab runs this in the console session it is already filming.
$drive = Join-Path $PSScriptRoot 'drive.py'
if (Get-Command py -ErrorAction SilentlyContinue) {
    & py -3 $drive windows
    exit $LASTEXITCODE
}
if (Get-Command python -ErrorAction SilentlyContinue) {
    & python $drive windows
    exit $LASTEXITCODE
}
if (Get-Command python3 -ErrorAction SilentlyContinue) {
    & python3 $drive windows
    exit $LASTEXITCODE
}
Write-Host 'python is not on PATH (tried py -3, python, python3)'
exit 1
