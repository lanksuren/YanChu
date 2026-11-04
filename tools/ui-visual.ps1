# 视觉检查：把控制面板、气泡、指示器各截一张图，人工看效果。
#
# 用法： powershell -ExecutionPolicy Bypass -File tools\ui-visual.ps1
# 产物： tools\.shot-panel.png / .shot-live.png / .shot-done.png
#
# 注意：脚本先把自己切成 per-monitor DPI aware，否则在 200%+ 缩放下
# 截到的是左上角一块放大的画面。

$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [Text.Encoding]::UTF8

$root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
$exe = Join-Path $root 'app\src-tauri\target\debug\yanchu.exe'
$log = Join-Path $root 'tools\.ui-visual.log'

$cs = @'
using System;
using System.Drawing;
using System.Runtime.InteropServices;
using System.Text;

public static class V {
    public delegate bool EnumProc(IntPtr hWnd, IntPtr lParam);
    [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc cb, IntPtr p);
    [DllImport("user32.dll")] public static extern int GetWindowTextLength(IntPtr h);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern int GetWindowText(IntPtr h, StringBuilder sb, int max);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
    [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr h, IntPtr after, int x, int y, int cx, int cy, uint flags);
    [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
    [DllImport("user32.dll")] public static extern void keybd_event(byte vk, byte scan, uint flags, UIntPtr extra);
    [DllImport("user32.dll")] public static extern bool SetProcessDpiAwarenessContext(IntPtr value);
    [DllImport("user32.dll")] public static extern int GetSystemMetrics(int index);

    public static IntPtr FindByTitle(string title, uint pid) {
        IntPtr found = IntPtr.Zero;
        EnumWindows((h, p) => {
            uint wpid; GetWindowThreadProcessId(h, out wpid);
            if (wpid != pid) return true;
            int n = GetWindowTextLength(h);
            if (n == 0) return true;
            var sb = new StringBuilder(n + 2);
            GetWindowText(h, sb, sb.Capacity);
            if (sb.ToString() == title) { found = h; return false; }
            return true;
        }, IntPtr.Zero);
        return found;
    }

    public static void Screen(string path) {
        int w = GetSystemMetrics(0), h = GetSystemMetrics(1);
        using (var bmp = new Bitmap(w, h))
        using (var g = Graphics.FromImage(bmp)) {
            g.CopyFromScreen(0, 0, 0, 0, new Size(w, h));
            bmp.Save(path, System.Drawing.Imaging.ImageFormat.Png);
        }
    }
}
'@
Add-Type -TypeDefinition $cs -Language CSharp -ReferencedAssemblies System.Drawing

[void][V]::SetProcessDpiAwarenessContext([IntPtr](-4))  # PER_MONITOR_AWARE_V2

Get-Process -Name yanchu -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep -Milliseconds 500

$env:YANCHU_OPEN_PANEL = '1'
$app = Start-Process -FilePath $exe -PassThru -RedirectStandardError $log -WindowStyle Hidden
Start-Sleep -Seconds 5

try {
  $panel = [V]::FindByTitle('言出 · 控制面板', $app.Id)
  if ($panel -ne [IntPtr]::Zero) {
    [void][V]::SetWindowPos($panel, [IntPtr]::Zero, 60, 60, 0, 0, 0x0001 -bor 0x0004) # NOSIZE | NOZORDER
    [void][V]::SetForegroundWindow($panel)
    Start-Sleep -Milliseconds 900
    [V]::Screen((Join-Path $root 'tools\.shot-panel.png'))
    'panel shot ok'
    [void][V]::SetWindowPos($panel, [IntPtr]::Zero, -4000, 60, 0, 0, 0x0001 -bor 0x0004)
  }

  # 让指示器出现在固定位置，再合成一次按住说话
  $w = [V]::GetSystemMetrics(0)
  $h = [V]::GetSystemMetrics(1)
  [void][V]::SetCursorPos([int]($w * 0.42), [int]($h * 0.42))
  [V]::keybd_event(0xA5, 0, 0, [UIntPtr]::Zero)
  Start-Sleep -Milliseconds 1200
  [V]::Screen((Join-Path $root 'tools\.shot-live.png'))
  'live shot ok'
  [V]::keybd_event(0xA5, 0, 2, [UIntPtr]::Zero)
  Start-Sleep -Milliseconds 1500
  [V]::Screen((Join-Path $root 'tools\.shot-done.png'))
  'done shot ok'
} finally {
  $env:YANCHU_OPEN_PANEL = $null
  Get-Process -Id $app.Id -ErrorAction SilentlyContinue | Stop-Process -Force
}
