# Installs dist\tinyclient-<Label>-windows-<Arch>-setup.exe silently for the current user, checks
# that the programs run (tinyclient.exe as a windowed program that still prints --version), the Start menu entry and the Apps & features entry exist, then uninstalls
# silently and checks that nothing is left. Then checks the portable zip. The release workflow
# runs it after packaging/windows.ps1.
param(
    [Parameter(Mandatory)][string]$Label,
    [Parameter(Mandatory)][string]$Version,
    [ValidateSet('x86_64', 'aarch64')][string]$Arch = "x86_64"
)
$ErrorActionPreference = "Stop"
$root = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
$name = "tinyclient-$Label-windows-$Arch"
$programs = "tinyclient", "chatd", "mediad", "tinyclient-login"
$machine = if ($Arch -eq "aarch64") { 0xAA64 } else { 0x8664 }
$runnerArch = if ($Arch -eq "aarch64") { "Arm64" } else { "X64" }
if ([System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture -ne $runnerArch) {
    throw "the $Arch smoke test requires a native $runnerArch system"
}

function Assert-Answers([string]$Dir) {
    foreach ($program in $programs) {
        $exe = Join-Path $Dir "$program.exe"
        $bytes = [System.IO.File]::ReadAllBytes($exe)
        $pe = [BitConverter]::ToInt32($bytes, 0x3C)
        if ([BitConverter]::ToUInt32($bytes, $pe) -ne 0x4550 -or [BitConverter]::ToUInt16($bytes, $pe + 4) -ne $machine) {
            throw "$exe is not a native $Arch PE executable"
        }
        $out = (& $exe --version | Out-String).Trim()
        Write-Host "${program}: $out"
        if ($out -ne "$program $Version") { throw "expected '$program $Version', got '$out'" }
    }
}

# The Start menu entry opens no console beside the window: tinyclient.exe is a Windows GUI
# program. The PE optional header, 24 bytes past "PE\0\0", has Subsystem at 68: 2 GUI, 3 console.
function Assert-Windowed([string]$Dir) {
    $exe = Join-Path $Dir "tinyclient.exe"
    $bytes = [System.IO.File]::ReadAllBytes($exe)
    $pe = [BitConverter]::ToInt32($bytes, 0x3C)
    if ([BitConverter]::ToUInt32($bytes, $pe) -ne 0x4550) { throw "$exe is not a PE file" }
    $subsystem = [BitConverter]::ToUInt16($bytes, $pe + 24 + 68)
    Write-Host "tinyclient.exe subsystem: $subsystem"
    if ($subsystem -ne 2) { throw "tinyclient.exe is not a GUI program (subsystem $subsystem)" }

    # Run in a console with nothing redirected, --version still prints there: it borrows the
    # console it was started from. A PowerShell in a console of its own runs it, then copies
    # what its console shows to a file.
    $out = Join-Path $env:RUNNER_TEMP "tinyclient-console.txt"
    $script = Join-Path $env:RUNNER_TEMP "tinyclient-console.ps1"
    Remove-Item -Force $out -ErrorAction SilentlyContinue
    Set-Content -Path $script -Value @'
param([string]$Exe, [string]$Out)
& $Exe --version
# PowerShell does not wait for a GUI program whose output it does not read.
Start-Sleep -Seconds 3
$raw = $Host.UI.RawUI
$rect = New-Object System.Management.Automation.Host.Rectangle 0, 0, ($raw.BufferSize.Width - 1), $raw.CursorPosition.Y
$cells = $raw.GetBufferContents($rect)
$lines = for ($y = 0; $y -le $cells.GetUpperBound(0); $y++) {
    -join (0..$cells.GetUpperBound(1) | ForEach-Object { $cells[$y, $_].Character })
}
Set-Content -Path $Out -Value $lines
'@
    $shell = (Get-Process -Id $PID).Path
    Start-Process -FilePath $shell -ArgumentList "-NoProfile", "-File", "`"$script`"", "`"$exe`"", "`"$out`"" -Wait
    $shown = @(Get-Content $out | ForEach-Object { $_.Trim() } | Where-Object { $_ })
    Write-Host "tinyclient.exe --version in a console: $($shown -join ' | ')"
    if ($shown -notcontains "tinyclient $Version") { throw "tinyclient.exe --version printed nothing to the console it ran in" }
}

$setup = Join-Path $root "dist\$name-setup.exe"
$log = Join-Path $env:RUNNER_TEMP "tinyclient-setup.log"
$p = Start-Process -FilePath $setup -ArgumentList "/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART", "/CURRENTUSER", "/LOG=`"$log`"" -Wait -PassThru
if ($p.ExitCode -ne 0) { Get-Content $log; throw "setup exited $($p.ExitCode)" }

# Per user: no administrator, so it lands under %LOCALAPPDATA%\Programs.
$app = Join-Path $env:LOCALAPPDATA "Programs\tinyclient"
Assert-Answers $app
Assert-Windowed $app
if ($Arch -eq "aarch64") {
    $runtime = 'Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}'
    # WebView2 registers in the machine's 32-bit view or the current user's view.
    $keys = "HKLM:\SOFTWARE\WOW6432Node\$runtime", "HKCU:\SOFTWARE\$runtime"
    $versions = Get-ItemPropertyValue -LiteralPath $keys -Name pv -ErrorAction SilentlyContinue
    if (-not ($versions | Where-Object { $_ -match '^\d+\.\d+\.\d+\.\d+$' -and [version]$_ -gt [version]'0.0.0.0' })) {
        throw "WebView2 Runtime is missing on the Arm64 smoke runner"
    }
    & python (Join-Path $root "packaging/ci/microphone-smoke.py") (Join-Path $app "mediad.exe")
    if ($LASTEXITCODE) { throw "microphone smoke failed ($LASTEXITCODE)" }
}
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
