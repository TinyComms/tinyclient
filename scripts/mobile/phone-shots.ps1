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
$carpool = @('--tab', 'inbox', '--open', '1')
$phone = @(
    @{ n = '01-needs-me';            a = @('--tab', 'needs') },
    @{ n = '02-inbox';               a = @('--tab', 'inbox') },
    @{ n = '03-search-all';          a = @('--tab', 'search') },
    @{ n = '04-search-query';        a = @('--tab', 'search', '--phone-search', 'trail') },
    @{ n = '05-you';                 a = @('--tab', 'you') },
    @{ n = '06-scope-sheet';         a = @('--tab', 'inbox', '--scope') },
    @{ n = '07-conversation-bottom'; a = $carpool },
    @{ n = '08-conversation-top';    a = $carpool; v = @{ TINYCLIENT_SHOT_AT = 'm01' } },
    @{ n = '09-conversation-link-image'; a = $carpool; v = @{ TINYCLIENT_SHOT_AT = 'm08' } },
    @{ n = '10-conversation-code-reply-file'; a = $carpool; v = @{ TINYCLIENT_SHOT_AT = 'm12' } },
    @{ n = '11-composer-text';       a = $carpool + @('--compose', 'Running 10 min late, save me a coffee') },
    @{ n = '12-message-actions';     a = $carpool; v = @{ TINYCLIENT_SHOT_HOVER = 'm25' } },
    @{ n = '13-message-menu';        a = $carpool; v = @{ TINYCLIENT_SHOT_MENU = 'm25' } },
    # The person's card over the direct conversation: the one people surface the phone draws.
    @{ n = '14-profile-card';        a = @('--tab', 'inbox', '--open', '2'); v = @{ TINYCLIENT_S6 = 'd-card' } },
    @{ n = '15-first-run';           a = @('--demo-empty') },
    @{ n = '16-sign-in-sheet';       a = @('--demo-empty'); v = @{ TINYCLIENT_SIGNIN_SHOT = 'phone' } },
    # Every sign-in flow as a phone page (`gui/signin/page.rs`), with made-up steps.
    @{ n = '16a-sign-in-telegram';   a = @('--demo-empty'); v = @{ TINYCLIENT_SIGNIN_SHOT = 'qr' } },
    @{ n = '16b-sign-in-code-wrong'; a = @('--demo-empty'); v = @{ TINYCLIENT_SIGNIN_SHOT = 'code-wrong' } },
    @{ n = '16c-sign-in-password';   a = @('--demo-empty'); v = @{ TINYCLIENT_SIGNIN_SHOT = 'password' } },
    @{ n = '16d-sign-in-tiny';       a = @('--demo-empty'); v = @{ TINYCLIENT_SIGNIN_SHOT = 'tiny' } },
    @{ n = '16e-sign-in-tiny-error'; a = @('--demo-empty'); v = @{ TINYCLIENT_SIGNIN_SHOT = 'tiny-error' } },
    @{ n = '16f-sign-in-discord';    a = @('--demo-empty'); v = @{ TINYCLIENT_SIGNIN_SHOT = 'discord' } },
    @{ n = '16g-sign-in-discord-waiting'; a = @('--demo-empty'); v = @{ TINYCLIENT_SIGNIN_SHOT = 'discord-waiting' } },
    @{ n = '16h-sign-in-slack';      a = @('--demo-empty'); v = @{ TINYCLIENT_SIGNIN_SHOT = 'slack' } },
    @{ n = '16i-sign-in-teams';      a = @('--demo-empty'); v = @{ TINYCLIENT_SIGNIN_SHOT = 'teams' } }
)
# Settings on a phone is a page of the one window (`gui/settings/phone_page.rs`); the shot draws
# that page in a window of its own at the phone's size. `list` is the list of sections.
$settingsShots = @(
    @{ n = '17-settings';            a = @('--settings', 'accounts') },
    @{ n = '17a-settings-list';      a = @('--settings', 'list') },
    @{ n = '17b-settings-appearance'; a = @('--settings', 'appearance') },
    @{ n = '17c-settings-messages';  a = @('--settings', 'messages') },
    @{ n = '17d-settings-privacy';   a = @('--settings', 'privacy') },
    @{ n = '17e-settings-storage';   a = @('--settings', 'storage') },
    @{ n = '17f-settings-about';     a = @('--settings', 'about') },
    @{ n = '17g-settings-confirm';   a = @('--settings', 'about', '--settings-confirm', 'reset') },
    @{ n = '17h-settings-search';    a = @('--settings', 'list', '--settings-search', 'text') }
)

try {
    foreach ($size in @('390x844', '360x780')) {
        Write-Host "phone-shots: phone at $size"
        foreach ($shot in $phone) {
            $vars = if ($shot.ContainsKey('v')) { $shot.v } else { @{} }
            Shot $shot.n $size $shot.a $vars
        }
        # Settings: drawn at the phone's size beside a held main window.
        foreach ($shot in $settingsShots) {
            $settings = Join-Path $Out "$($shot.n)-$size.png"
            Remove-Item $settings -ErrorAction SilentlyContinue
            foreach ($var in $hookVars) { Remove-Item "Env:$var" -ErrorAction SilentlyContinue }
            $throwaway = Join-Path $scratch 'settings-main.png'
            $all = @('--shot', $throwaway, '--size', $size, '--demo', '--tab', 'you', '--hold', '3') + $shot.a + @('--settings-shot', $settings, '--settings-size', $size)
            & $exe @all 2>&1 | Out-Null
            if (Test-Path $settings) { $made.Add((Split-Path -Leaf $settings)); Write-Host "  $(Split-Path -Leaf $settings)" }
            else { $missing.Add((Split-Path -Leaf $settings)); Write-Host "  $(Split-Path -Leaf $settings): not written" -ForegroundColor Yellow }
        }
    }

    Write-Host 'phone-shots: 150% text'
    Shot '18-inbox-text150' '390x844' @('--tab', 'inbox', '--text-scale', '1.5')
    Shot '19-conversation-text150' '390x844' ($carpool + @('--text-scale', '1.5'))

    Write-Host 'phone-shots: desktop, for reference'
    Shot 'desktop-inbox' '1280x800' @()
    Shot 'desktop-conversation' '1280x800' @('--open', '1')
    # The call dock lives in the desktop sidebar; the phone form does not draw it.
    Shot 'desktop-call-dock' '1280x800' @('--open', '1') @{ TINYCLIENT_SHOT_CALL = 'joined' }
    Shot 'desktop-first-run' '1280x800' @('--demo-empty')
    Shot 'desktop-sign-in-sheet' '1280x800' @('--demo-empty') @{ TINYCLIENT_SIGNIN_SHOT = 'phone' }
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
