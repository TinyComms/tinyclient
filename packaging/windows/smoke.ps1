# Installs dist\tinyclient-<Label>-windows-x86_64-setup.exe silently for the current user, checks
# that the programs run, the Start menu entry and the Apps & features entry exist, then uninstalls
# silently and checks that nothing is left. Then checks the portable zip. The release workflow
# runs it after packaging/windows.ps1.
param(
    [Parameter(Mandatory)][string]$Label,
    [Parameter(Mandatory)][string]$Version
)
$ErrorActionPreference = "Stop"
$root = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
$name = "tinyclient-$Label-windows-x86_64"
$programs = "tinyclient", "chatd", "mediad", "tinyclient-login"

function Assert-Answers([string]$Dir) {
    foreach ($program in $programs) {
        $out = (& (Join-Path $Dir "$program.exe") --version | Out-String).Trim()
        Write-Host "${program}: $out"
        if ($out -ne "$program $Version") { throw "expected '$program $Version', got '$out'" }
    }
}

$setup = Join-Path $root "dist\$name-setup.exe"
$log = Join-Path $env:RUNNER_TEMP "tinyclient-setup.log"
$p = Start-Process -FilePath $setup -ArgumentList "/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART", "/CURRENTUSER", "/LOG=`"$log`"" -Wait -PassThru
if ($p.ExitCode -ne 0) { Get-Content $log; throw "setup exited $($p.ExitCode)" }

# Per user: no administrator, so it lands under %LOCALAPPDATA%\Programs.
$app = Join-Path $env:LOCALAPPDATA "Programs\tinyclient"
Assert-Answers $app
$shortcut = Join-Path $env:APPDATA "Microsoft\Windows\Start Menu\Programs\tinyclient.lnk"
if (-not (Test-Path $shortcut)) { throw "no Start menu entry at $shortcut" }
$target = (New-Object -ComObject WScript.Shell).CreateShortcut($shortcut).TargetPath
if ($target -ne (Join-Path $app "tinyclient.exe")) { throw "the Start menu entry opens $target" }
$key = "HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\{8C1B5E0A-4F3D-4C1E-9B7A-2D6E5F1A7C3B}_is1"
$entry = Get-ItemProperty $key
Write-Host "Apps & features: $($entry.DisplayName) $($entry.DisplayVersion), $($entry.UninstallString)"
if ($entry.DisplayVersion -ne $Version) { throw "Apps & features lists version $($entry.DisplayVersion)" }

# The uninstaller copies itself to a temporary folder and returns at once, so wait for the files to go.
$p = Start-Process -FilePath (Join-Path $app "unins000.exe") -ArgumentList "/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART" -Wait -PassThru
if ($p.ExitCode -ne 0) { throw "uninstall exited $($p.ExitCode)" }
for ($i = 0; $i -lt 60 -and (Test-Path $app); $i++) { Start-Sleep -Seconds 1 }
if (Test-Path $app) { Get-ChildItem -Recurse $app; throw "$app is still there after uninstalling" }
if (Test-Path $shortcut) { throw "the Start menu entry is still there after uninstalling" }
if (Test-Path $key) { throw "the Apps & features entry is still there after uninstalling" }
Write-Host "installer: installed, ran and uninstalled"

# The portable zip: the four programs, run where they are unpacked.
$portable = Join-Path $env:RUNNER_TEMP "tinyclient-portable"
Expand-Archive -Force (Join-Path $root "dist\$name.zip") $portable
Assert-Answers $portable
Write-Host "zip: ran"
