<#
.SYNOPSIS
  Drives a desktop tinyclient window for tests: GPUI exposes no UI Automation tree on Windows,
  so this works in window pixels. Synthetic mouse/character messages traverse the real GUI;
  they do not invoke chatd IPC. Capture uses only the requested application window.
  Height deliberately has a different name from the HWND variable (PowerShell ignores case).

  windows-ui.ps1 -ProcessId <pid> -Action rect
  windows-ui.ps1 -ProcessId <pid> -Action shot -Path out.png       # PrintWindow, works when covered
  windows-ui.ps1 -ProcessId <pid> -Action click -X 120 -Y 340      # window-relative pixels
  windows-ui.ps1 -ProcessId <pid> -Action place -X 40 -Y 40 -W 1280 -Height 800
#>
param(
    [Parameter(Mandatory)][int]$ProcessId,
    [ValidateSet('rect', 'shot', 'click', 'place', 'front', 'max', 'post', 'back', 'type', 'wheel')][string]$Action = 'rect',
    [string]$Path, [string]$Text, [int]$Delta = -480,
    [int]$X, [int]$Y, [int]$W = 1280, [Alias('H')][int]$Height = 800
)
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing
if (-not ('DlUiNativeMarker' -as [type])) {
    Add-Type @'
using System;
using System.Runtime.InteropServices;
public static class DlUiNativeMarker {}
public static class DlUiNative {
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
    [DllImport("user32.dll")] public static extern IntPtr SendMessage(IntPtr h, uint m, IntPtr w, IntPtr l);
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
[DlUiNative]::SetProcessDPIAware() | Out-Null
$h = [DlUiNative]::Find([uint32]$ProcessId)
if ($h -eq [IntPtr]::Zero) { throw "no visible window for pid $ProcessId" }
$r = New-Object DlUiNative+RECT
[DlUiNative]::GetWindowRect($h, [ref]$r) | Out-Null
switch ($Action) {
    'type' {
        # Native character messages reach the focused GPUI field without changing the desktop focus.
        foreach ($ch in $Text.ToCharArray()) {
            [DlUiNative]::SendMessage($h, 0x102, [IntPtr]([int]$ch), [IntPtr]1) | Out-Null
            Start-Sleep -Milliseconds 15
        }
    }
    'wheel' {
        $lp = [IntPtr]((($r.T + $Y) -shl 16) -bor (($r.L + $X) -band 0xffff))
        [DlUiNative]::PostMessage($h, 0x20a, [IntPtr]($Delta -shl 16), $lp) | Out-Null
    }
    'rect' { [pscustomobject]@{ hwnd = $h; left = $r.L; top = $r.T; width = $r.R - $r.L; height = $r.B - $r.T } }
    # Shown but behind everything and never activated, so it does not cover someone at the desk.
    'back' { [DlUiNative]::ShowWindow($h, 4) | Out-Null; [DlUiNative]::SetWindowPos($h, [IntPtr]1, 0, 0, 0, 0, 0x13) | Out-Null }
    'max' { [DlUiNative]::ShowWindow($h, 3) | Out-Null }
    'front' { [DlUiNative]::ShowWindow($h, 9) | Out-Null; [DlUiNative]::SetForegroundWindow($h) | Out-Null }
    'place' { [DlUiNative]::ShowWindow($h, 9) | Out-Null; [DlUiNative]::MoveWindow($h, $X, $Y, $W, $Height, $true) | Out-Null }
    'shot' {
        $bmp = New-Object System.Drawing.Bitmap ($r.R - $r.L), ($r.B - $r.T)
        $g = [System.Drawing.Graphics]::FromImage($bmp)
        $dc = $g.GetHdc()
        # PW_RENDERFULLCONTENT (2) captures GPU-composited windows.
        [DlUiNative]::PrintWindow($h, $dc, 2) | Out-Null
        $g.ReleaseHdc($dc); $g.Dispose()
        $bmp.Save($Path, [System.Drawing.Imaging.ImageFormat]::Png); $bmp.Dispose()
        Write-Host "saved $Path ($($r.R - $r.L)x$($r.B - $r.T))"
    }
    'post' {
        # Window messages, not real input: the cursor and focus stay with whoever is at the
        # desk. X/Y are window pixels, as in a shot; converted to client coordinates here.
        $pt = New-Object DlUiNative+POINT
        [DlUiNative]::ClientToScreen($h, [ref]$pt) | Out-Null
        $cx = $r.L + $X - $pt.X; $cy = $r.T + $Y - $pt.Y
        $lp = [IntPtr](($cy -shl 16) -bor ($cx -band 0xffff))
        [DlUiNative]::PostMessage($h, 0x200, [IntPtr]::Zero, $lp) | Out-Null   # WM_MOUSEMOVE
        Start-Sleep -Milliseconds 120
        [DlUiNative]::PostMessage($h, 0x201, [IntPtr]1, $lp) | Out-Null       # WM_LBUTTONDOWN
        Start-Sleep -Milliseconds 80
        [DlUiNative]::PostMessage($h, 0x202, [IntPtr]::Zero, $lp) | Out-Null  # WM_LBUTTONUP
    }
    'click' {
        [DlUiNative]::SetForegroundWindow($h) | Out-Null
        Start-Sleep -Milliseconds 150
        [DlUiNative]::SetCursorPos($r.L + $X, $r.T + $Y) | Out-Null
        Start-Sleep -Milliseconds 80
        [DlUiNative]::mouse_event(0x2, 0, 0, 0, [UIntPtr]::Zero)
        Start-Sleep -Milliseconds 60
        [DlUiNative]::mouse_event(0x4, 0, 0, 0, [UIntPtr]::Zero)
    }
}
