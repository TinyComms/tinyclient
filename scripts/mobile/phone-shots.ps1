#!/usr/bin/env pwsh
# Renders every screen of the phone UI with made-up data (`--shot --demo`), for design review
# from screenshots. No chatd is started or asked, and nobody's real chats are read: the window
# answers itself from crates/tinyclient/src/gui/shot_demo.rs, and APPDATA / LOCALAPPDATA point at
# a scratch directory so no saved state of this machine leaks in.
#
#   .\scripts\mobile\phone-shots.ps1
#   .\scripts\mobile\phone-shots.ps1 -Out C:\tmp\shots
#   .\scripts\mobile\phone-shots.ps1 -NoBuild          # reuse target\debug\tinyclient.exe
#
# Files are named NN-state-WxH.png: each phone state at 390x844 and 360x780, the Inbox and the
# conversation again at 150% text, and desktop-*.png at 1280x800 for reference.
[CmdletBinding()]
param(
    [string]$Out = (Join-Path (Split-Path -Parent (Split-Path -Parent (Split-Path -Parent $PSScriptRoot))) '_proof\design\before'),
    [switch]$NoBuild
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$root = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)

function Fail([string]$message) {
    Write-Host "phone-shots: $message" -ForegroundColor Red
    exit 1
}

$cargoBin = Join-Path $env:USERPROFILE '.cargo\bin'
if (-not (Get-Command cargo -ErrorAction SilentlyContinue) -and (Test-Path (Join-Path $cargoBin 'cargo.exe'))) {
    $env:PATH = "$cargoBin;$env:PATH"
}

if (-not $NoBuild) {
    if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
        Fail "cargo is not on PATH. Install Rust from https://rustup.rs, or add $cargoBin to PATH."
    }
    Write-Host 'phone-shots: building tinyclient --features gui-shot'
    Push-Location $root
    try {
        cargo build -p tinyclient --features gui-shot --locked
        if ($LASTEXITCODE -ne 0) { Fail 'the build failed' }
    } finally {
        Pop-Location
    }
}

$exe = Join-Path $root 'target\debug\tinyclient.exe'
if (-not (Test-Path $exe)) { Fail "no $exe; run without -NoBuild" }

$Out = [System.IO.Path]::GetFullPath($Out)
New-Item -ItemType Directory -Force -Path $Out | Out-Null

# A scratch profile: no prefs, no crash log, no sign-ins of this machine.
$scratch = Join-Path ([System.IO.Path]::GetTempPath()) "tinyclient-phone-shots-$PID"
New-Item -ItemType Directory -Force -Path (Join-Path $scratch 'roaming'), (Join-Path $scratch 'local') | Out-Null
$saved = @{ APPDATA = $env:APPDATA; LOCALAPPDATA = $env:LOCALAPPDATA }
$env:APPDATA = Join-Path $scratch 'roaming'
$env:LOCALAPPDATA = Join-Path $scratch 'local'

# The shot hooks some states are reached through. Cleared before every shot.
$hookVars = @(
    'TINYCLIENT_SHOT_AT', 'TINYCLIENT_SHOT_HOVER', 'TINYCLIENT_SHOT_MENU', 'TINYCLIENT_SHOT_STEPS',
    'TINYCLIENT_SHOT_CALL', 'TINYCLIENT_SIGNIN_SHOT', 'TINYCLIENT_S6'
)

$made = New-Object System.Collections.Generic.List[string]
$missing = New-Object System.Collections.Generic.List[string]

function Shot([string]$name, [string]$size, [string[]]$arguments, [hashtable]$vars = @{}) {
    foreach ($var in $hookVars) { Remove-Item "Env:$var" -ErrorAction SilentlyContinue }
    foreach ($key in $vars.Keys) { Set-Item "Env:$key" $vars[$key] }
    $file = Join-Path $Out "$name-$size.png"
    Remove-Item $file -ErrorAction SilentlyContinue
    $all = @('--shot', $file, '--size', $size, '--demo') + $arguments
    # The window quits by itself once the frame is written. Piping its output is what makes
    # PowerShell wait for a GUI program; the output itself is not a result.
    & $exe @all 2>&1 | Out-Null
    foreach ($key in $vars.Keys) { Remove-Item "Env:$key" -ErrorAction SilentlyContinue }
    if (Test-Path $file) {
        $made.Add((Split-Path -Leaf $file))
        Write-Host "  $(Split-Path -Leaf $file)"
    } else {
        $missing.Add((Split-Path -Leaf $file))
        Write-Host "  $(Split-Path -Leaf $file): not written" -ForegroundColor Yellow
    }
}

# Row 1 of the list is "Foray carpool 🍄", the conversation with one of every kind of message;
# row 2 is the direct conversation with Yusuf Demirci; row 3 is Slack's #platform.
$carpool = @('--open', '1')
# The phone is the desktop's rail and sidebar; a conversation and the right pane push over them.
$phone = @(
    @{ n = '01-inbox';               a = @() },
    @{ n = '02-inbox-unread';        a = @('--filter', 'unread') },
    @{ n = '03-inbox-mentions';      a = @('--filter', 'mentions') },
    @{ n = '04-people';              a = @('--mode', 'people') },
    @{ n = '05-discord';             a = @('--mode', 'discord') },
    @{ n = '06-discord-server';      a = @('--mode', 'space', '--space', '1') },
    @{ n = '07-slack';               a = @('--mode', 'slack') },
    @{ n = '08-jump-to';             a = @('--switcher') },
    @{ n = '09-account-sheet';       a = @('--menu', 'account') },
    @{ n = '10-add-sheet';           a = @('--menu', 'add') },
    @{ n = '11-row-sheet';           a = @('--menu', 'row') },
    @{ n = '12-conversation-bottom'; a = $carpool },
    @{ n = '13-conversation-top';    a = $carpool; v = @{ TINYCLIENT_SHOT_AT = 'm01' } },
    @{ n = '14-conversation-link-image'; a = $carpool; v = @{ TINYCLIENT_SHOT_AT = 'm08' } },
    @{ n = '15-conversation-code-reply-file'; a = $carpool; v = @{ TINYCLIENT_SHOT_AT = 'm12' } },
    @{ n = '16-composer-text';       a = $carpool + @('--compose', 'Running 10 min late, save me a coffee') },
    @{ n = '17-message-actions';     a = $carpool; v = @{ TINYCLIENT_SHOT_HOVER = 'm25' } },
    @{ n = '18-message-menu';        a = $carpool; v = @{ TINYCLIENT_SHOT_MENU = 'm25' } },
    # The right pane as a pushed page: a channel's Details and members, a person's profile.
    @{ n = '19-channel-details';     a = @('--open', '3', '--right', 'details') },
    @{ n = '20-channel-members';     a = @('--open', '3', '--right', 'member-list') },
    @{ n = '21-profile';             a = @('--open', '2', '--right', 'details') },
    @{ n = '22-profile-card';        a = @('--open', '2'); v = @{ TINYCLIENT_S6 = 'd-card' } },
    @{ n = '23-first-run';           a = @('--demo-empty') },
    @{ n = '24-sign-in-sheet';       a = @('--demo-empty'); v = @{ TINYCLIENT_SIGNIN_SHOT = 'phone' } }
)

try {
    foreach ($size in @('390x844', '360x780')) {
        Write-Host "phone-shots: phone at $size"
        foreach ($shot in $phone) {
            $vars = if ($shot.ContainsKey('v')) { $shot.v } else { @{} }
            Shot $shot.n $size $shot.a $vars
        }
        # Settings is its own window: drawn at the phone's size beside a held main window.
        $settings = Join-Path $Out "25-settings-$size.png"
        Remove-Item $settings -ErrorAction SilentlyContinue
        foreach ($var in $hookVars) { Remove-Item "Env:$var" -ErrorAction SilentlyContinue }
        $throwaway = Join-Path $scratch 'settings-main.png'
        & $exe --shot $throwaway --size $size --demo --hold 3 --settings accounts --settings-shot $settings --settings-size $size 2>&1 | Out-Null
        if (Test-Path $settings) { $made.Add((Split-Path -Leaf $settings)); Write-Host "  $(Split-Path -Leaf $settings)" }
        else { $missing.Add((Split-Path -Leaf $settings)); Write-Host "  $(Split-Path -Leaf $settings): not written" -ForegroundColor Yellow }
    }

    Write-Host 'phone-shots: 150% text'
    Shot '26-inbox-text150' '390x844' @('--text-scale', '1.5')
    Shot '27-conversation-text150' '390x844' ($carpool + @('--text-scale', '1.5'))

    Write-Host 'phone-shots: desktop, for reference'
    Shot 'desktop-inbox' '1280x800' @()
    Shot 'desktop-conversation' '1280x800' @('--open', '1')
    # The call dock lives in the desktop sidebar; the phone form does not draw it.
    Shot 'desktop-call-dock' '1280x800' @('--open', '1') @{ TINYCLIENT_SHOT_CALL = 'joined' }
} finally {
    $env:APPDATA = $saved.APPDATA
    $env:LOCALAPPDATA = $saved.LOCALAPPDATA
    foreach ($var in $hookVars) { Remove-Item "Env:$var" -ErrorAction SilentlyContinue }
    Remove-Item -Recurse -Force $scratch -ErrorAction SilentlyContinue
}

Write-Host "phone-shots: $($made.Count) written to $Out"
if ($missing.Count -gt 0) {
    Write-Host "phone-shots: not written: $($missing -join ', ')" -ForegroundColor Yellow
    exit 1
}

exit 0
