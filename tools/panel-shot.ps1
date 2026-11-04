# 控制面板截图（开发期人工核对）：settings / models / glossary / history。
# 用法： powershell -ExecutionPolicy Bypass -File tools\panel-shot.ps1 -Tab models
param(
  [ValidateSet('settings', 'models', 'glossary', 'history')][string]$Tab = 'settings',
  [string]$Out = 'tools\.panel-shot.png'
)

$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [Text.Encoding]::UTF8

$root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
$exe = Join-Path $root 'app\src-tauri\target\debug\yanchu.exe'
$log = Join-Path $root 'tools\.panel-shot.log'
if (-not [IO.Path]::IsPathRooted($Out)) { $Out = Join-Path $root $Out }

$cs = @'
using System;
using System.Drawing;
using System.Runtime.InteropServices;
using System.Text;

public static class PS {
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }
    public delegate bool EnumProc(IntPtr hWnd, IntPtr lParam);
    [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc cb, IntPtr p);
    [DllImport("user32.dll")] public static extern int GetWindowTextLength(IntPtr h);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern int GetWindowText(IntPtr h, StringBuilder sb, int max);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
    [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr h, IntPtr after, int x, int y, int cx, int cy, uint flags);
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int cmd);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
    [DllImport("user32.dll")] public static extern void mouse_event(uint flags, int dx, int dy, uint data, UIntPtr extra);
    [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr hdc, uint flags);
    [DllImport("user32.dll")] public static extern bool SetProcessDpiAwarenessContext(IntPtr value);

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

    public static void ClickAt(int x, int y) {
        SetCursorPos(x, y);
        System.Threading.Thread.Sleep(140);
        mouse_event(0x0002, 0, 0, 0, UIntPtr.Zero);
        System.Threading.Thread.Sleep(60);
        mouse_event(0x0004, 0, 0, 0, UIntPtr.Zero);
        System.Threading.Thread.Sleep(900);
    }

    public static void Shot(IntPtr h, string path) {
        RECT r; GetWindowRect(h, out r);
        int w = r.Right - r.Left, ht = r.Bottom - r.Top;
        using (var bmp = new Bitmap(w, ht))
        using (var g = Graphics.FromImage(bmp)) {
            IntPtr hdc = g.GetHdc();
            PrintWindow(h, hdc, 2);
            g.ReleaseHdc(hdc);
            bmp.Save(path, System.Drawing.Imaging.ImageFormat.Png);
        }
        Console.WriteLine(String.Format("shot {0}x{1} rect={2},{3}", w, ht, r.Left, r.Top));
    }

    public static void Topmost(IntPtr h, bool on) {
        if (on) ShowWindow(h, 9);
        SetWindowPos(h, (IntPtr)(on ? -1 : -2), 0, 0, 0, 0, 0x0001 | 0x0002 | (on ? 0x0040u : 0u));
    }
}
'@
Add-Type -TypeDefinition $cs -Language CSharp -ReferencedAssemblies System.Drawing
[void][PS]::SetProcessDpiAwarenessContext([IntPtr](-4))

Get-Process -Name yanchu -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep -Milliseconds 400
Remove-Item $log -ErrorAction SilentlyContinue

$env:YANCHU_OPEN_PANEL = '1'
# 新面板的导航在左侧，坐标点击容易漂；直接让 Rust 侧在页面加载后切页签。
$env:YANCHU_PANEL_TAB = $Tab
$app = Start-Process -FilePath $exe -PassThru -RedirectStandardError $log -WindowStyle Hidden
Start-Sleep -Seconds 2

$panel = [IntPtr]::Zero
try {
    # 首次启动要加载识别模型，面板窗口可能比固定等待更晚出现；轮询到 20 秒。
    for ($i = 0; $i -lt 36 -and $panel -eq [IntPtr]::Zero; $i++) {
        $panel = [PS]::FindByTitle('言出 · 控制面板', $app.Id)
        if ($panel -eq [IntPtr]::Zero) { Start-Sleep -Milliseconds 500 }
    }
    if ($panel -eq [IntPtr]::Zero) {
        $alive = Get-Process -Id $app.Id -ErrorAction SilentlyContinue
        Write-Host "panel not found (pid=$($app.Id), alive=$([bool]$alive))"
        if (Test-Path $log) { Get-Content $log -Tail 20 }
        throw 'panel not found'
    }
    [PS]::Topmost($panel, $true)
    Start-Sleep -Milliseconds 900

    [PS]::Shot($panel, $Out)
    Write-Host "screenshot: $Out ($Tab)"
} finally {
    if ($panel -ne [IntPtr]::Zero) { [PS]::Topmost($panel, $false) }
    $env:YANCHU_OPEN_PANEL = $null
    $env:YANCHU_PANEL_TAB = $null
    Get-Process -Id $app.Id -ErrorAction SilentlyContinue | Stop-Process -Force
    Remove-Item $log -Force -ErrorAction SilentlyContinue
}
