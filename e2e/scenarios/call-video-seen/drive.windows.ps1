$ErrorActionPreference = 'Stop'
& python "$PSScriptRoot/drive.py" windows @args
exit $LASTEXITCODE
