$ErrorActionPreference = 'Stop'
& python (Join-Path $PSScriptRoot 'drive.py')
exit $LASTEXITCODE
