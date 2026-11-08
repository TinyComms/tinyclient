<#
.SYNOPSIS
  Identity-checked Windows input/capture for this scenario only.
  Foreground and hit-PID must match ExpectedPid before click/type/capture.
  Adapted from the proven composer-input178 / native-history202 helpers.
#>
param(
    [Parameter(Mandatory = $true)][int]$ExpectedPid,
    [Parameter(Mandatory = $true)][ValidateSet(
        'prepare', 'front', 'capture', 'printwindow', 'click', 'type', 'rect'
    )][string]$Action,
    [int]$OtherPid = 0,
    [int]$ScreenX = 0,
    [int]$ScreenY = 0,
    [int]$X = 0,
    [int]$Y = 0,
    [string]$Screenshot,
    [string]$Text
)
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Windows.Forms, System.Drawing
if (-not ('TinyOwnedInput204' -as [type])) {
    Add-Type @'
using System;
using System.Runtime.InteropServices;
public static class TinyOwnedInput204 {
 public delegate bool EnumProc(IntPtr h, IntPtr p);
 [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L,T,R,B; }
 [StructLayout(LayoutKind.Sequential)] public struct POINT { public int X,Y; }
 [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
 [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc f, IntPtr p);
 [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
 [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h,out uint p);
 [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h,out RECT r);
 [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
 [DllImport("user32.dll")] public static extern IntPtr WindowFromPoint(POINT p);
 [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h,int n);
 [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
 [DllImport("user32.dll")] public static extern bool SetCursorPos(int x,int y);
 [DllImport("user32.dll")] public static extern void mouse_event(uint f,uint x,uint y,uint d,UIntPtr e);
 [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr hdcBlt, uint nFlags);
 [DllImport("user32.dll")] public static extern bool ClientToScreen(IntPtr h,ref POINT p);
 public static uint Pid(IntPtr h) { uint p; GetWindowThreadProcessId(h,out p); return p; }
 public static IntPtr Find(int p) {
  IntPtr result=IntPtr.Zero; long area=0;
  EnumWindows((h,l)=>{ if(Pid(h)==p && IsWindowVisible(h)) { RECT r; GetWindowRect(h,out r); long a=(long)(r.R-r.L)*(r.B-r.T); if(a>area){area=a;result=h;} } return true;},IntPtr.Zero);
  return result;
 }
}
'@
}
[TinyOwnedInput204]::SetProcessDPIAware() | Out-Null
$target = [TinyOwnedInput204]::Find($ExpectedPid)
if ($target -eq [IntPtr]::Zero) { throw "No visible owned window for PID $ExpectedPid" }

if ($Action -eq 'prepare') {
    if ($OtherPid -eq $ExpectedPid) { throw 'OtherPid must name a different owned process' }
    if ($OtherPid -gt 0) {
        $other = [TinyOwnedInput204]::Find($OtherPid)
        if ($other -ne [IntPtr]::Zero) { [TinyOwnedInput204]::ShowWindow($other, 6) | Out-Null }
    }
    [TinyOwnedInput204]::ShowWindow($target, 3) | Out-Null
    [TinyOwnedInput204]::SetForegroundWindow($target) | Out-Null
    Start-Sleep -Milliseconds 200
}

if ($Action -in @('prepare', 'front', 'click', 'type', 'capture', 'printwindow')) {
    [TinyOwnedInput204]::ShowWindow($target, 9) | Out-Null
    [TinyOwnedInput204]::SetForegroundWindow($target) | Out-Null
    Start-Sleep -Milliseconds 150
    $foregroundPid = [TinyOwnedInput204]::Pid([TinyOwnedInput204]::GetForegroundWindow())
    if ($foregroundPid -ne $ExpectedPid) {
        throw "Foreground PID $foregroundPid differs from expected $ExpectedPid; no input sent"
    }
}

$r = New-Object TinyOwnedInput204+RECT
[TinyOwnedInput204]::GetWindowRect($target, [ref]$r) | Out-Null

switch ($Action) {
    'rect' {
        [pscustomobject]@{
            hwnd = [int64]$target; left = $r.L; top = $r.T
            width = ($r.R - $r.L); height = ($r.B - $r.T)
        } | ConvertTo-Json -Compress
    }
    'prepare' { Write-Host "prepared pid=$ExpectedPid" }
    'front' { Write-Host "front pid=$ExpectedPid" }
    'capture' {
        if (-not $Screenshot) { throw 'Screenshot path required' }
        $bounds = [Drawing.Rectangle]::FromLTRB($r.L, $r.T, $r.R, $r.B)
        $bmp = New-Object Drawing.Bitmap $bounds.Width, $bounds.Height
        $g = [Drawing.Graphics]::FromImage($bmp)
        $g.CopyFromScreen($bounds.Location, [Drawing.Point]::Empty, $bounds.Size)
        $g.Dispose()
        $bmp.Save($Screenshot, [Drawing.Imaging.ImageFormat]::Png)
        $bmp.Dispose()
        Write-Host "saved $Screenshot"
    }
    'printwindow' {
        if (-not $Screenshot) { throw 'Screenshot path required' }
        $bmp = New-Object Drawing.Bitmap ($r.R - $r.L), ($r.B - $r.T)
        $g = [Drawing.Graphics]::FromImage($bmp)
        $dc = $g.GetHdc()
        [TinyOwnedInput204]::PrintWindow($target, $dc, 2) | Out-Null
        $g.ReleaseHdc($dc); $g.Dispose()
        $bmp.Save($Screenshot, [Drawing.Imaging.ImageFormat]::Png)
        $bmp.Dispose()
        Write-Host "saved $Screenshot"
    }
    'click' {
        $screenX = if ($ScreenX) { $ScreenX } else { $r.L + $X }
        $screenY = if ($ScreenY) { $ScreenY } else { $r.T + $Y }
        $pt = New-Object TinyOwnedInput204+POINT
        $pt.X = $screenX; $pt.Y = $screenY
        $hit = [TinyOwnedInput204]::WindowFromPoint($pt)
        $hitPid = if ($hit -ne [IntPtr]::Zero) { [TinyOwnedInput204]::Pid($hit) } else { 0 }
        if ($hitPid -ne $ExpectedPid) {
            throw "Hit PID $hitPid at $screenX,$screenY differs from expected $ExpectedPid"
        }
        [TinyOwnedInput204]::SetCursorPos($screenX, $screenY) | Out-Null
        [TinyOwnedInput204]::mouse_event(0x0002, 0, 0, 0, [UIntPtr]::Zero) # LEFTDOWN
        Start-Sleep -Milliseconds 30
        [TinyOwnedInput204]::mouse_event(0x0004, 0, 0, 0, [UIntPtr]::Zero) # LEFTUP
        Write-Host "clicked $screenX,$screenY hitPid=$hitPid"
    }
    'type' {
        if (-not $Text) { throw 'Text required' }
        [Windows.Forms.SendKeys]::SendWait($Text)
        Write-Host ("typed length={0}" -f $Text.Length)
    }
}
