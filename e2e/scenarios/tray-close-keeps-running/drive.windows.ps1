# tray-close-keeps-running, Windows. Runs inside the console session of a lab host, with a
# recorder already filming. The G3 switch is turned on by writing its one line to shell.prefs
# before the window opens (the file the switch itself writes; the row's drawing and search are
# unit-tested, and the recording cannot show a click that happened before the app ran). From
# there everything is what a person does: close the window with its X, click the tray icon to
# come back, right-click it and choose Quit tinyclient.
#
# What it proves: after the close the tinyclient process is alive, its window is gone, chatd
# still answers `tinyclient --text`; the tray icon's click restores the window; and Quit in
# the icon's menu ends tinyclient (exit 0) and the chatd it started.
#
# A build from before the fix fails at the first close: closing the window quits the process.
#
# Env, as smoke: E2E_T0 (unix seconds when the recording began), E2E_OUT, E2E_APP_DIR
# (tinyclient.exe, chatd.exe, mediad.exe), E2E_SRC_DIR (the source tree, for
# scripts\e2e\win-window.ps1).
$ErrorActionPreference = 'Stop'
$T0 = [double]::Parse($env:E2E_T0, [Globalization.CultureInfo]::InvariantCulture)
$Out = $env:E2E_OUT
$AppDir = $env:E2E_APP_DIR
$WinWindow = Join-Path $env:E2E_SRC_DIR 'scripts\e2e\win-window.ps1'
$Data = Join-Path (Split-Path $Out -Parent) 'tinyclient-data'
$utf8 = New-Object System.Text.UTF8Encoding($false)
$StepsFile = Join-Path $Out 'steps.jsonl'
$failed = $false
$started = @()   # PIDs this driver started: the only ones it ever stops

Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes
if (-not ('TinyTray' -as [type])) {
    Add-Type @'
using System;
using System.Runtime.InteropServices;
public static class TinyTray {
    [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
    [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
    [DllImport("user32.dll")] public static extern void mouse_event(uint f, uint dx, uint dy, uint d, UIntPtr e);
    public static void Click(bool right) {
        uint down = right ? 0x8u : 0x2u, up = right ? 0x10u : 0x4u;
        mouse_event(down, 0, 0, 0, UIntPtr.Zero);
        System.Threading.Thread.Sleep(60);
        mouse_event(up, 0, 0, 0, UIntPtr.Zero);
    }
}
'@
}
[TinyTray]::SetProcessDPIAware() | Out-Null

function Now { [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds() / 1000.0 }
function Step([string]$words, [bool]$ok, [string]$shot = '') {
    $line = [ordered]@{ t = [math]::Round((Now) - $T0, 1); step = $words; ok = $ok }
    if ($shot) { $line.shot = $shot }
    [IO.File]::AppendAllText($StepsFile, (($line | ConvertTo-Json -Compress) + "`n"), $utf8)
    Write-Host ("[{0,6:N1}s] {1}: {2}" -f $line.t, $(if ($ok) { 'ok    ' } else { 'FAILED' }), $words)
    if (-not $ok) { $script:failed = $true }
}
function Win([string]$action) {
    & powershell.exe -NoProfile -ExecutionPolicy Bypass -File $WinWindow -ProcessId $script:App.Id -Action $action 2>&1 | Out-String
}
# `tinyclient.exe --text`: what the window's inbox is drawn from, asked of the same chatd.
function ChatdAnswers {
    $file = Join-Path $Out 'text.txt'
    $p = Start-Process -Wait -PassThru -WindowStyle Hidden -FilePath (Join-Path $AppDir 'tinyclient.exe') -ArgumentList '--text' `
        -RedirectStandardOutput $file -RedirectStandardError (Join-Path $Out 'text.err.txt')
    $p.ExitCode -eq 0
}
# The tray icon, as UI Automation sees it: a button named by its tip, "tinyclient". The icon
# sits in the taskbar or behind its chevron; both places hang off the desktop's tree.
function TrayIcon {
    $root = [System.Windows.Automation.AutomationElement]::RootElement
    $name = New-Object System.Windows.Automation.PropertyCondition([System.Windows.Automation.AutomationElement]::NameProperty, 'tinyclient')
    $tray = $root.FindFirst([System.Windows.Automation.TreeScope]::Children, (New-Object System.Windows.Automation.PropertyCondition([System.Windows.Automation.AutomationElement]::ClassNameProperty, 'Shell_TrayWnd')))
    if ($tray) {
        $found = $tray.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $name)
        if ($found) { return $found }
    }
    $root.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $name)
}
function ClickRect([System.Windows.Automation.AutomationElement]$el, [bool]$right) {
    $r = $el.BoundingRectangle
    $x = [int]($r.X + $r.Width / 2); $y = [int]($r.Y + $r.Height / 2)
    [TinyTray]::SetCursorPos($x, $y) | Out-Null
    Start-Sleep -Milliseconds 120
    [TinyTray]::Click($right)
    return @($x, $y)
}
function Stop-Mine {
    foreach ($id in $script:started) { Stop-Process -Id $id -Force -ErrorAction SilentlyContinue }
    # An app-owned chatd the quit never reached: end it too, so the host stays clean.
    Get-CimInstance Win32_Process -Filter "Name = 'chatd.exe'" -ErrorAction SilentlyContinue |
        Where-Object { $_.ParentProcessId -eq $script:App.Id } |
        ForEach-Object { Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }
}

try {
    # Invented data only, in a folder of this run: with TINYCLIENT_DIR the app never reads the
    # credential store, and it starts its own chatd, which is the one Quit has to end.
    Remove-Item -Recurse -Force $Data -ErrorAction SilentlyContinue
    New-Item -ItemType Directory -Force $Data | Out-Null
    $env:TINYCLIENT_DIR = $Data
    $env:TINYCLIENT_LOG_LEVEL = 'debug'
    # The G3 switch, as shell.prefs holds it (tab separated), before the window opens.
    [IO.File]::WriteAllText((Join-Path $Data 'shell.prefs'), "tray`ton`n", $utf8)

    $script:App = Start-Process -PassThru -FilePath (Join-Path $AppDir 'tinyclient.exe') -WorkingDirectory $Data
    $started += $script:App.Id
    $shown = $false
    for ($i = 0; $i -lt 60 -and -not $script:App.HasExited; $i++) {
        try { Win 'max' | Out-Null; $shown = $true; break } catch { Start-Sleep -Seconds 1 }
    }
    if ($shown) { Win 'front' | Out-Null; Start-Sleep -Seconds 4 }
    if ($shown) { Win 'shot' @{ Path = (Join-Path $Out 'window.png') } | Out-Null }
    Step 'window on screen' $shown 'window.png'
    if (-not $shown) { throw 'no window' }

    $chatd = @(Get-CimInstance Win32_Process -Filter "Name = 'chatd.exe'" -ErrorAction SilentlyContinue | Where-Object { $_.ParentProcessId -eq $script:App.Id })
    Step 'tinyclient started its own chatd' ($chatd.Count -eq 1)

    # Close the window the way a person does. The app must stay: that is the whole point of G3.
    $null = $script:App.CloseMainWindow()
    Start-Sleep -Seconds 5
    $alive = -not $script:App.HasExited
    Step 'closing the window leaves tinyclient running' $alive
    $gone = $false
    try { Win 'rect' | Out-Null } catch { $gone = $true }
    Step 'the window is gone from the desktop' $gone
    $answered = $alive -and (ChatdAnswers)
    Step 'chatd still answers with the window closed' $answered
    if (-not ($alive -and $gone -and $answered)) { throw 'the close did not hold in the tray' }

    # The icon's click brings the window back. Searched by name, then clicked where it is.
    $icon = $null
    for ($i = 0; $i -lt 30 -and -not $icon; $i++) {
        Start-Sleep -Seconds 1
        $icon = TrayIcon
    }
    $restored = $false
    if ($icon) {
        ClickRect $icon $false | Out-Null
        for ($i = 0; $i -lt 15 -and -not $restored; $i++) {
            Start-Sleep -Milliseconds 500
            try { Win 'rect' | Out-Null; $restored = $true } catch {}
        }
    }
    Step 'the tray icon reopens the window' $restored
    if ($restored) {
        Win 'front' | Out-Null
        Start-Sleep -Seconds 2
        Win 'shot' @{ Path = (Join-Path $Out 'restored.png') } | Out-Null
    }
    if (-not ($restored -and $icon)) { throw 'no way back in from the tray' }

    # The icon's menu: right click, then Quit tinyclient. The menu is a real one, so its item
    # is found the way the icon was; if this Windows names it differently, the keyboard walks
    # the two items instead (Open, then Quit).
    $at = ClickRect $icon $true
    Start-Sleep -Seconds 2
    $root = [System.Windows.Automation.AutomationElement]::RootElement
    $quit = $null
    for ($i = 0; $i -lt 10 -and -not $quit; $i++) {
        Start-Sleep -Seconds 1
        $quit = $root.FindFirst([System.Windows.Automation.TreeScope]::Descendants, (New-Object System.Windows.Automation.PropertyCondition([System.Windows.Automation.AutomationElement]::NameProperty, 'Quit tinyclient')))
    }
    $asked = $false
    if ($quit) {
        ClickRect $quit $false | Out-Null
        $asked = $true
        Write-Host "quit chosen by click at $($at[0]),$($at[1])"
    } else {
        Add-Type -AssemblyName System.Windows.Forms
        [System.Windows.Forms.SendKeys]::SendWait('{DOWN}{DOWN}{ENTER}')
        $asked = $true
        Write-Host 'quit chosen by keyboard (the menu item was not found by name)'
    }
    Step 'Quit tinyclient asked in the tray menu' $asked

    $done = $script:App.WaitForExit(20000)
    $code = if ($done) { $script:App.ExitCode } else { -1 }
    Step ("Quit in the tray's menu ends tinyclient (exit {0})" -f $code) ($done -and $code -eq 0)
    $chatdDone = $false
    for ($i = 0; $i -lt 20 -and -not $chatdDone; $i++) {
        Start-Sleep -Seconds 1
        $left = @(Get-CimInstance Win32_Process -Filter "Name = 'chatd.exe'" -ErrorAction SilentlyContinue | Where-Object { $_.ParentProcessId -eq $script:App.Id })
        $chatdDone = ($left.Count -eq 0)
    }
    Step 'Quit ends the chatd tinyclient started' $chatdDone
    if (-not ($done -and $chatdDone)) { throw 'the quit did not clean up' }
    Start-Sleep -Seconds 2
}
catch {
    Write-Host "driver: $($_.Exception.Message)"
    $failed = $true
}
finally {
    Copy-Item (Join-Path $Data 'logs') (Join-Path $Out 'logs') -Recurse -ErrorAction SilentlyContinue
    Stop-Mine
}
if ($failed) { exit 1 }
exit 0
