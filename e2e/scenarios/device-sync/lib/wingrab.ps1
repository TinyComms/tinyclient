<#
.SYNOPSIS
  Films app windows on the coordinator without filming the coordinator: PrintWindow of each named process's
  window, a few times a second, into numbered PNG frames, until <OutDir>\stop exists. Then
  lib/frames2mp4.py turns them into one captioned clip.

.DESCRIPTION
  the coordinator is the user's own PC in daily use, and the proof's recordings may be published, so the
  desktop is never recorded (the lab's gdigrab desktop recorder is for winbuild's console session).
  PrintWindow (PW_RENDERFULLCONTENT) captures a GPU-composited window even when it is covered
  (the same call scripts/e2e/win-window.ps1 -Action shot uses), so the windows stay "back" and never
  take focus. The frames of all windows are tiled side by side into one picture per tick.

  wingrab.ps1 -OutDir <dir> -PidFile <file> [-Fps 4]
  <file> holds one "<pid> <label>" line per window and is read again on every frame, so a device
  started mid-run (Ada B after the history) joins the picture.
#>
param(
    [Parameter(Mandatory)][string]$OutDir,
    [Parameter(Mandatory)][string]$PidFile,
    [double]$Fps = 4
)
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing
if (-not ('DlGrab' -as [type])) {
    Add-Type @'
using System;
using System.Runtime.InteropServices;
public static class DlGrab {
    public delegate bool EnumProc(IntPtr h, IntPtr l);
    [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc f, IntPtr l);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr dc, uint flags);
    [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
    public static IntPtr Find(uint want) {
        IntPtr found = IntPtr.Zero; long best = 0;
        EnumWindows((h, l) => {
            uint pid; GetWindowThreadProcessId(h, out pid);
            if (pid == want && IsWindowVisible(h)) {
                RECT r; GetWindowRect(h, out r);
                long area = (long)(r.R - r.L) * (r.B - r.T);
                if (area > best) { best = area; found = h; }
            }
            return true;
        }, IntPtr.Zero);
        return found;
    }
}
'@
}
[DlGrab]::SetProcessDPIAware() | Out-Null
New-Item -ItemType Directory -Force $OutDir | Out-Null
$stop = Join-Path $OutDir 'stop'
$index = Join-Path $OutDir 'frames.txt'   # frame file + unix time, for the captioner
$font = New-Object System.Drawing.Font('Segoe UI', 14, [System.Drawing.FontStyle]::Bold)
$n = 0
$period = [int](1000 / $Fps)
while (-not (Test-Path $stop)) {
    $t = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds() / 1000.0
    $lines = @(Get-Content $PidFile -ErrorAction SilentlyContinue | Where-Object { $_.Trim() })
    if (-not $lines.Count) { Start-Sleep -Milliseconds $period; continue }
    $ids = @($lines | ForEach-Object { [uint32]($_.Trim() -split ' ', 2)[0] })
    $names = @($lines | ForEach-Object { ($_.Trim() -split ' ', 2)[1] })
    $tiles = @()
    for ($i = 0; $i -lt $ids.Count; $i++) {
        $h = [DlGrab]::Find($ids[$i])
        if ($h -eq [IntPtr]::Zero) { $tiles += $null; continue }
        $r = New-Object DlGrab+RECT
        [DlGrab]::GetWindowRect($h, [ref]$r) | Out-Null
        $w = [Math]::Max(1, $r.R - $r.L); $hh = [Math]::Max(1, $r.B - $r.T)
        $bmp = New-Object System.Drawing.Bitmap $w, $hh
        $g = [System.Drawing.Graphics]::FromImage($bmp)
        $dc = $g.GetHdc()
        [DlGrab]::PrintWindow($h, $dc, 2) | Out-Null
        $g.ReleaseHdc($dc); $g.Dispose()
        $tiles += $bmp
    }
    # One picture: tiles side by side, each fitted (aspect kept) into 960x1080, its label on top.
    # The frame size stays the same for the whole clip: room for up to 4 windows.
    $tw = 960; $th = 1080; $lab = 30; $slots = 4
    $frame = New-Object System.Drawing.Bitmap ($tw * $slots), ($th + $lab)
    $g = [System.Drawing.Graphics]::FromImage($frame)
    $g.Clear([System.Drawing.Color]::FromArgb(24, 24, 28))
    for ($i = 0; $i -lt [Math]::Min($ids.Count, $slots); $i++) {
        $g.DrawString($names[$i], $font, [System.Drawing.Brushes]::White, ($tw * $i + 8), 2)
        if ($tiles[$i]) {
            $sc = [Math]::Min($tw / $tiles[$i].Width, $th / $tiles[$i].Height)
            $g.DrawImage($tiles[$i], ($tw * $i), $lab, [int]($tiles[$i].Width * $sc), [int]($tiles[$i].Height * $sc))
            $tiles[$i].Dispose()
        } else {
            $g.DrawString('(no window)', $font, [System.Drawing.Brushes]::Gray, ($tw * $i + 40), 200)
        }
    }
    $g.Dispose()
    $file = 'f{0:D6}.png' -f $n
    $frame.Save((Join-Path $OutDir $file), [System.Drawing.Imaging.ImageFormat]::Png)
    $frame.Dispose()
    Add-Content -Path $index -Value ("{0} {1:F3}" -f $file, $t)
    $n++
    $spent = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds() - [long]($t * 1000)
    if ($spent -lt $period) { Start-Sleep -Milliseconds ($period - $spent) }
}
Write-Host "wingrab: $n frames"
