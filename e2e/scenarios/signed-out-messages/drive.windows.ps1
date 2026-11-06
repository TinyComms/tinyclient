# signed-out-messages on Windows. The lab runs this in the console session it is already filming.
$ErrorActionPreference = "Stop"
python "$PSScriptRoot\drive.py"
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
