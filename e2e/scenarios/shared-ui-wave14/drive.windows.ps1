$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes, System.Windows.Forms
$out = $env:E2E_OUT
$t0 = [double]::Parse($env:E2E_T0, [Globalization.CultureInfo]::InvariantCulture)
$data = Join-Path $out ("profile-" + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $data | Out-Null
$env:TINYCLIENT_DIR = $data
$env:LC_ALL = 'en_US.UTF-8'
$root = [System.Windows.Automation.AutomationElement]
$lastStep = "start with the simulated Telegram account"

function Step([string] $words, [bool] $ok) {
    $line = @{
        t = [math]::Round([DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds() / 1000 - $t0, 2)
        step = $words
        ok = $ok
    } | ConvertTo-Json -Compress
    [IO.File]::AppendAllText((Join-Path $out 'steps.jsonl'), ($line + "`n"), [Text.UTF8Encoding]::new($false))
}
function Nodes {
    $app.Refresh()
    if ($app.MainWindowHandle -eq 0) { return }
    $window = $root::FromHandle($app.MainWindowHandle)
    $window.FindAll([System.Windows.Automation.TreeScope]::Descendants, [System.Windows.Automation.Condition]::TrueCondition)
}
function Find-Node([string] $pattern, [string] $role) {
    foreach ($node in (Nodes)) {
        if ($node.Current.Name -match $pattern -and (!$role -or $node.Current.ControlType.ProgrammaticName -eq $role)) {
            return $node
        }
    }
}
function Await-Node([string] $pattern, [string] $role = '') {
    for ($i = 0; $i -lt 40; $i++) {
        try {
            $node = Find-Node $pattern $role
            if ($node) { return $node }
        } catch [System.Windows.Automation.ElementNotAvailableException] {}
        Start-Sleep -Milliseconds 300
    }
    throw "Accessible element missing: $pattern ($role)"
}
function Press($node) {
    $invoke = $null
    if ($node.TryGetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern, [ref] $invoke)) {
        $invoke.Invoke()
    } else {
        $node.SetFocus()
        Start-Sleep -Milliseconds 150
        [System.Windows.Forms.SendKeys]::SendWait('{ENTER}')
    }
}
function Open-MessageMenu($node) {
    $node.SetFocus()
    Start-Sleep -Milliseconds 200
    [System.Windows.Forms.SendKeys]::SendWait('{ENTER}')
}

try {
    $chatd = Start-Process -PassThru -WindowStyle Hidden -FilePath (Join-Path $env:E2E_APP_DIR 'chatd.exe') -ArgumentList '--seed-demo'
    $app = Start-Process -PassThru -FilePath (Join-Path $env:E2E_APP_DIR 'tinyclient.exe')
    Press (Await-Node '^Telegram\b' 'ControlType.Button')
    Press (Await-Node '^Ines Marchetti(?:,|$)' 'ControlType.ListItem')
    Step $lastStep $true

    $lastStep = 'read three messages with their senders and times'
    $message = Await-Node '^Ines Marchetti, .*\d.*horario del ferry' 'ControlType.ListItem'
    $null = Await-Node '^Ines Marchetti, .*\d.*Uploading them tonight' 'ControlType.ListItem'
    $null = Await-Node '^You, .*\d.*Did the herbarium send the scans' 'ControlType.ListItem'
    $null = Await-Node '^Message$' 'ControlType.Edit'
    Step $lastStep $true

    $lastStep = 'open the message actions and choose Translate'
    Open-MessageMenu $message
    Press (Await-Node '^Translate$' 'ControlType.MenuItem')
    $null = Await-Node 'The ferry schedule changed'
    $null = Await-Node 'Translated from Spanish'
    Step $lastStep $true

    $lastStep = 'Show original removes the translation and leaves the Spanish message'
    Press (Await-Node '^Show original$' 'ControlType.Button')
    $null = Await-Node '^Ines Marchetti, .*horario del ferry' 'ControlType.ListItem'
    for ($i = 0; $i -lt 20 -and (Find-Node 'The ferry schedule changed|^Show original$'); $i++) {
        Start-Sleep -Milliseconds 150
    }
    if (Find-Node 'The ferry schedule changed|^Show original$') { throw 'Translation is still visible' }
    Step $lastStep $true

    $lastStep = 'reply to the message with the keyboard'
    Open-MessageMenu (Await-Node '^Ines Marchetti, .*horario del ferry' 'ControlType.ListItem')
    Press (Await-Node '^Reply$' 'ControlType.MenuItem')
    [System.Windows.Forms.SendKeys]::SendWait('Wave fourteen keyboard reply')
    [System.Windows.Forms.SendKeys]::SendWait('{ENTER}')
    $null = Await-Node '^You, .*Wave fourteen keyboard reply' 'ControlType.ListItem'
    Step $lastStep $true
} catch {
    Step $lastStep $false
    Write-Error $_ -ErrorAction Continue
    exit 1
} finally {
    if ($app -and !$app.HasExited) {
        $app.CloseMainWindow() | Out-Null
        if (!$app.WaitForExit(2000)) { Stop-Process -Id $app.Id }
    }
    if ($chatd -and !$chatd.HasExited) { Stop-Process -Id $chatd.Id }
}
