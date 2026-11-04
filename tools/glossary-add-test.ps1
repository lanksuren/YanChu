# 词表「加一个词」的端到端检查：
# 打开词表 → 输入 正确写法 / 会被误读成 → 添加 → 保存 → 断言文件里是「误读=正确」→ 恢复原文件。
$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [Text.Encoding]::UTF8

$root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
$exe = Join-Path $root 'app\src-tauri\target\debug\yanchu.exe'
$glossary = Join-Path $root 'app\src-tauri\models\glossary.txt'
$backup = "$glossary.add-test-bak"
$log = Join-Path $root 'tools\.glossary-add.log'
$shot = Join-Path $root 'tools\.glossary-add.png'

$cs = @'
using System;
using System.Drawing;
using System.Runtime.InteropServices;
using System.Text;

public static class GA {
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
        System.Threading.Thread.Sleep(350);
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
[void][GA]::SetProcessDpiAwarenessContext([IntPtr](-4))

Copy-Item $glossary $backup -Force
Get-Process -Name yanchu -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep -Milliseconds 400
Remove-Item $log -ErrorAction SilentlyContinue

$env:YANCHU_OPEN_PANEL = '1'
$env:YANCHU_PANEL_TAB = 'glossary'
$app = Start-Process -FilePath $exe -PassThru -RedirectStandardError $log -WindowStyle Hidden
Start-Sleep -Seconds 2

$panel = [IntPtr]::Zero
try {
    for ($i = 0; $i -lt 36 -and $panel -eq [IntPtr]::Zero; $i++) {
        $panel = [GA]::FindByTitle('言出 · 控制面板', $app.Id)
        if ($panel -eq [IntPtr]::Zero) { Start-Sleep -Milliseconds 500 }
    }
    if ($panel -eq [IntPtr]::Zero) { throw '没找到控制面板' }
    [GA]::Topmost($panel, $true)
    Start-Sleep -Milliseconds 900

    $r = New-Object GA+RECT
    [void][GA]::GetWindowRect($panel, [ref]$r)
    $w = $r.Right - $r.Left
    $h = $r.Bottom - $r.Top
    $px = { param($fx, $fy) [int]($r.Left + $w * $fx), [int]($r.Top + $h * $fy) }

    # 正确写法 / 会被误读成（用剪贴板粘贴，避免输入法把字母拆成拼音）
    $p = & $px 0.43 0.335
    [GA]::ClickAt($p[0], $p[1])
    Set-Clipboard -Value 'zzzhelper'
    [GA]::Key(0xA2, $true); [GA]::Key(0x56, $true); [GA]::Key(0x56, $false); [GA]::Key(0xA2, $false)
    Start-Sleep -Milliseconds 300

    $p = & $px 0.71 0.335
    [GA]::ClickAt($p[0], $p[1])
    Set-Clipboard -Value 'zzzhelber'
    [GA]::Key(0xA2, $true); [GA]::Key(0x56, $true); [GA]::Key(0x56, $false); [GA]::Key(0xA2, $false)
    Start-Sleep -Milliseconds 300

    # 添加
    $p = & $px 0.89 0.335
    [GA]::ClickAt($p[0], $p[1])
    Start-Sleep -Milliseconds 600

    # 保存词表
    $p = & $px 0.885 0.49
    [GA]::ClickAt($p[0], $p[1])
    Start-Sleep -Milliseconds 1500

    [GA]::Shot($panel, $shot)
    Write-Host "screenshot: $shot"

    $text = [IO.File]::ReadAllText($glossary, [Text.Encoding]::UTF8)
    if ($text -match '(?m)^zzzhelber=zzzhelper$') {
        Write-Host '词表方向正确：文件里是「误读=正确」' -ForegroundColor Green
    } else {
        Write-Host '词表方向错误：没找到 zzzhelber=zzzhelper' -ForegroundColor Red
        $text -split "`n" | Select-Object -First 6
        exit 1
    }
} finally {
    if ($panel -ne [IntPtr]::Zero) { [GA]::Topmost($panel, $false) }
    $env:YANCHU_OPEN_PANEL = $null
    $env:YANCHU_PANEL_TAB = $null
    Get-Process -Id $app.Id -ErrorAction SilentlyContinue | Stop-Process -Force
    Move-Item $backup $glossary -Force
    Remove-Item $log -Force -ErrorAction SilentlyContinue
}
