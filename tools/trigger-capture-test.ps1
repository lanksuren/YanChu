# 触发键录入 + 保存的端到端检查（开发期人工核对）：
# 打开设置 → 点「触发键」→ 按 Ctrl+Shift+Space → 截图 → 点保存 → 读回 trigger.json → 恢复原配置。
$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [Text.Encoding]::UTF8

$root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
$exe = Join-Path $root 'app\src-tauri\target\debug\yanchu.exe'
$log = Join-Path $root 'tools\.trigger-capture.log'
$shotCaptured = Join-Path $root 'tools\.trigger-captured.png'
$shotSaved = Join-Path $root 'tools\.trigger-saved.png'
$triggerPath = Join-Path $root 'app\src-tauri\models\trigger.json'
$backup = "$triggerPath.capture-bak"

$cs = @'
using System;
using System.Drawing;
using System.Runtime.InteropServices;
using System.Text;

public static class TC {
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
    [DllImport("user32.dll")] public static extern void keybd_event(byte vk, byte scan, uint flags, UIntPtr extra);
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
        System.Threading.Thread.Sleep(260);
    }

    public static void Key(byte vk, bool down) {
        keybd_event(vk, 0, down ? 0u : 2u, UIntPtr.Zero);
        System.Threading.Thread.Sleep(45);
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
    }

    public static void Topmost(IntPtr h, bool on) {
        if (on) ShowWindow(h, 9);
        SetWindowPos(h, (IntPtr)(on ? -1 : -2), 0, 0, 0, 0, 0x0001 | 0x0002 | (on ? 0x0040u : 0u));
    }
}
'@
Add-Type -TypeDefinition $cs -Language CSharp -ReferencedAssemblies System.Drawing
[void][TC]::SetProcessDpiAwarenessContext([IntPtr](-4))

$existed = Test-Path $triggerPath
if ($existed) { Copy-Item $triggerPath $backup -Force }
Get-Process -Name yanchu -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep -Milliseconds 400
Remove-Item $log -ErrorAction SilentlyContinue

$env:YANCHU_OPEN_PANEL = '1'
$app = Start-Process -FilePath $exe -PassThru -RedirectStandardError $log -WindowStyle Hidden
Start-Sleep -Seconds 5

$panel = [IntPtr]::Zero
try {
    $panel = [TC]::FindByTitle('言出 · 控制面板', $app.Id)
    if ($panel -eq [IntPtr]::Zero) { throw '没找到控制面板' }
    [TC]::Topmost($panel, $true)
    Start-Sleep -Milliseconds 600

    $r = New-Object TC+RECT
    [void][TC]::GetWindowRect($panel, [ref]$r)
    $w = $r.Right - $r.Left
    $h = $r.Bottom - $r.Top
    $px = { param($fx, $fy) [int]($r.Left + $w * $fx), [int]($r.Top + $h * $fy) }

    # 控制面板默认就打开在「设置」页，不用再点页签
    $p = & $px 0.813 0.358
    Write-Host ("点击触发键按钮: {0},{1}" -f $p[0], $p[1])
    [TC]::ClickAt($p[0], $p[1])
    Start-Sleep -Milliseconds 500

    [TC]::Key(0xA2, $true)   # Ctrl
    [TC]::Key(0xA0, $true)   # Shift
    [TC]::Key(0x20, $true)   # Space
    [TC]::Key(0x20, $false)
    [TC]::Key(0xA0, $false)
    [TC]::Key(0xA2, $false)
    Start-Sleep -Milliseconds 900

    [TC]::Shot($panel, $shotCaptured)
    Write-Host "录入后截图: $shotCaptured"

    $p = & $px 0.096 0.552
    Write-Host ("点击保存: {0},{1}" -f $p[0], $p[1])
    [TC]::ClickAt($p[0], $p[1])
    Start-Sleep -Milliseconds 1500
    [TC]::Shot($panel, $shotSaved)
    Write-Host "保存后截图: $shotSaved"

    if (Test-Path $triggerPath) {
        Write-Host 'trigger.json:'
        Get-Content $triggerPath -Raw
    } else {
        Write-Host 'trigger.json 不存在（保存可能没发生）'
    }

    if (Test-Path $log) {
        $err = Get-Content $log -Raw
        if ($err -match 'panicked|thread .* panic') { Write-Host '日志出现 panic' -ForegroundColor Red }
    }
} finally {
    if ($panel -ne [IntPtr]::Zero) { [TC]::Topmost($panel, $false) }
    $env:YANCHU_OPEN_PANEL = $null
    Get-Process -Id $app.Id -ErrorAction SilentlyContinue | Stop-Process -Force
    if ($existed -and (Test-Path $backup)) {
        Move-Item $backup $triggerPath -Force
    } elseif (-not $existed) {
        Remove-Item $triggerPath -ErrorAction SilentlyContinue
    }
}
