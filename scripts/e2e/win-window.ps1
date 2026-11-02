<#
.SYNOPSIS
  Drives a desktop tinyclient window for tests: GPUI exposes no UI Automation tree on Windows,
  so this works in window pixels.

  win-window.ps1 -ProcessId <pid> -Action rect
  win-window.ps1 -ProcessId <pid> -Action shot -Path out.png       # PrintWindow, works when covered
  win-window.ps1 -ProcessId <pid> -Action click -X 120 -Y 340      # window-relative pixels
  win-window.ps1 -ProcessId <pid> -Action place -X 40 -Y 40 -W 1280 -H 800
#>
param(
    [Parameter(Mandatory)][int]$ProcessId,
    [ValidateSet('rect', 'shot', 'click', 'place', 'front', 'max', 'post', 'back')][string]$Action = 'rect',
    [string]$Path,
    [int]$X, [int]$Y, [int]$W = 1280, [int]$H = 800
)
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing
if (-not ('TinyWin2' -as [type])) {
    Add-Type @'
using System;
using System.Runtime.InteropServices;
public static class TinyWin2 {}
public static class TinyWin {
    public delegate bool EnumProc(IntPtr h, IntPtr l);
    [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc f, IntPtr l);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int cmd);
    [DllImport("user32.dll")] public static extern bool MoveWindow(IntPtr h, int x, int y, int w, int hh, bool repaint);
    [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr dc, uint flags);
    [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
    [DllImport("user32.dll")] public static extern void mouse_event(uint f, uint dx, uint dy, uint d, UIntPtr e);
    [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
    [StructLayout(LayoutKind.Sequential)] public struct POINT { public int X, Y; }
    [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr h, IntPtr after, int x, int y, int w, int hh, uint f);
    [DllImport("user32.dll")] public static extern bool ClientToScreen(IntPtr h, ref POINT p);
    [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint m, IntPtr w, IntPtr l);
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
[TinyWin]::SetProcessDPIAware() | Out-Null
$h = [TinyWin]::Find([uint32]$ProcessId)
if ($h -eq [IntPtr]::Zero) { throw "no visible window for pid $ProcessId" }
$r = New-Object TinyWin+RECT
[TinyWin]::GetWindowRect($h, [ref]$r) | Out-Null
switch ($Action) {
    'rect' { [pscustomobject]@{ hwnd = $h; left = $r.L; top = $r.T; width = $r.R - $r.L; height = $r.B - $r.T } }
    # Shown but behind everything and never activated, so it does not cover someone at the desk.
    'back' { [TinyWin]::ShowWindow($h, 4) | Out-Null; [TinyWin]::SetWindowPos($h, [IntPtr]1, 0, 0, 0, 0, 0x13) | Out-Null }
    'max' { [TinyWin]::ShowWindow($h, 3) | Out-Null }
    'front' { [TinyWin]::ShowWindow($h, 9) | Out-Null; [TinyWin]::SetForegroundWindow($h) | Out-Null }
    'place' { [TinyWin]::ShowWindow($h, 9) | Out-Null; [TinyWin]::MoveWindow($h, $X, $Y, $W, $H, $true) | Out-Null }
    'shot' {
        $bmp = New-Object System.Drawing.Bitmap ($r.R - $r.L), ($r.B - $r.T)
        $g = [System.Drawing.Graphics]::FromImage($bmp)
        $dc = $g.GetHdc()
        # PW_RENDERFULLCONTENT (2) captures GPU-composited windows.
        [TinyWin]::PrintWindow($h, $dc, 2) | Out-Null
        $g.ReleaseHdc($dc); $g.Dispose()
        $bmp.Save($Path, [System.Drawing.Imaging.ImageFormat]::Png); $bmp.Dispose()
        Write-Host "saved $Path ($($r.R - $r.L)x$($r.B - $r.T))"
    }
    'post' {
        # Window messages, not real input: the cursor and focus stay with whoever is at the
        # desk. X/Y are window pixels, as in a shot; converted to client coordinates here.
        $pt = New-Object TinyWin+POINT
        [TinyWin]::ClientToScreen($h, [ref]$pt) | Out-Null
        $cx = $r.L + $X - $pt.X; $cy = $r.T + $Y - $pt.Y
        $lp = [IntPtr](($cy -shl 16) -bor ($cx -band 0xffff))
        [TinyWin]::PostMessage($h, 0x200, [IntPtr]::Zero, $lp) | Out-Null   # WM_MOUSEMOVE
        Start-Sleep -Milliseconds 120
        [TinyWin]::PostMessage($h, 0x201, [IntPtr]1, $lp) | Out-Null       # WM_LBUTTONDOWN
        Start-Sleep -Milliseconds 80
        [TinyWin]::PostMessage($h, 0x202, [IntPtr]::Zero, $lp) | Out-Null  # WM_LBUTTONUP
    }
    'click' {
        [TinyWin]::SetForegroundWindow($h) | Out-Null
        Start-Sleep -Milliseconds 150
        [TinyWin]::SetCursorPos($r.L + $X, $r.T + $Y) | Out-Null
        Start-Sleep -Milliseconds 80
        [TinyWin]::mouse_event(0x2, 0, 0, 0, [UIntPtr]::Zero)
        Start-Sleep -Milliseconds 60
        [TinyWin]::mouse_event(0x4, 0, 0, 0, [UIntPtr]::Zero)
    }
}



