# 界面冒烟测试：控制面板的「关闭 = 收进托盘」+ 气泡/指示器还能不能出来。
#
# 用法： powershell -ExecutionPolicy Bypass -File tools\ui-smoke.ps1
#
# 断言：
#   1. 应用能起来，托盘初始化不报错；
#   2. YANCHU_OPEN_PANEL=1 能打开控制面板；
#   3. 发 WM_CLOSE 后窗口**仍然存在**（只是隐藏），进程不退出；
#   4. 隐藏的窗口还能再次显示（模拟托盘再次打开）；
#   5. 合成右 Alt 后指示器/气泡窗口能被显示出来。

$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [Text.Encoding]::UTF8

$root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
$exe = Join-Path $root 'app\src-tauri\target\debug\yanchu.exe'
$log = Join-Path $root 'tools\.ui-smoke.log'
$shot = Join-Path $root 'tools\.panel-after.png'
$script:failed = $false

$cs = @'
using System;
using System.Drawing;
using System.Runtime.InteropServices;
using System.Text;

public static class UI {
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }
    public delegate bool EnumProc(IntPtr hWnd, IntPtr lParam);

    [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc cb, IntPtr p);
    [DllImport("user32.dll")] public static extern int GetWindowTextLength(IntPtr h);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern int GetWindowText(IntPtr h, StringBuilder sb, int max);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
    [DllImport("user32.dll")] public static extern bool IsWindow(IntPtr h);
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int cmd);
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
    [DllImport("user32.dll")] public static extern IntPtr SendMessage(IntPtr h, uint msg, IntPtr w, IntPtr l);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
    [DllImport("user32.dll")] public static extern void keybd_event(byte vk, byte scan, uint flags, UIntPtr extra);

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

    public static void Shot(IntPtr h, string path) {
        RECT r; GetWindowRect(h, out r);
        int w = r.Right - r.Left, ht = r.Bottom - r.Top;
        if (w <= 0 || ht <= 0) return;
        using (var bmp = new Bitmap(w, ht))
        using (var g = Graphics.FromImage(bmp)) {
            g.CopyFromScreen(r.Left, r.Top, 0, 0, new Size(w, ht));
            bmp.Save(path, System.Drawing.Imaging.ImageFormat.Png);
        }
    }
}
'@
Add-Type -TypeDefinition $cs -Language CSharp -ReferencedAssemblies System.Drawing

function Check($ok, $msg) {
  if ($ok) { Write-Host "  OK   $msg" -ForegroundColor Green }
  else { Write-Host "  FAIL $msg" -ForegroundColor Red; $script:failed = $true }
}

Get-Process -Name yanchu -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep -Milliseconds 500
Remove-Item $log -ErrorAction SilentlyContinue

$env:YANCHU_OPEN_PANEL = '1'
$app = Start-Process -FilePath $exe -PassThru -RedirectStandardError $log -WindowStyle Hidden
Start-Sleep -Seconds 5

try {
  Check (-not $app.HasExited) "应用已启动（pid=$($app.Id)）"

  $panel = [UI]::FindByTitle('言出 · 控制面板', $app.Id)
  Check ($panel -ne [IntPtr]::Zero) '找到控制面板窗口'
  Check ([UI]::IsWindowVisible($panel)) '控制面板可见'
  [void][UI]::SetForegroundWindow($panel)
  Start-Sleep -Milliseconds 900
  [UI]::Shot($panel, $shot)
  Check (Test-Path $shot) "截图已保存: $shot"

  # 关闭（等价于点 ×）：应被拦截为 hide
  [void][UI]::SendMessage($panel, 0x0010, [IntPtr]::Zero, [IntPtr]::Zero)  # WM_CLOSE
  Start-Sleep -Milliseconds 800
  Check ([UI]::IsWindow($panel)) '关闭后窗口仍存在（没有被销毁）'
  Check (-not [UI]::IsWindowVisible($panel)) '关闭后窗口已隐藏'
  Check (-not $app.HasExited) '关闭面板后进程仍活着（常驻托盘）'

  # 再次显示（托盘 open_panel 走的就是 show + set_focus）
  [void][UI]::ShowWindow($panel, 5)  # SW_SHOW
  Start-Sleep -Milliseconds 600
  Check ([UI]::IsWindowVisible($panel)) '隐藏的窗口可以再次显示'
  [void][UI]::ShowWindow($panel, 0)  # SW_HIDE，别挡着后面的测试

  # 合成按下右 Alt：指示器 + 气泡应被显示
  [UI]::keybd_event(0xA5, 0, 0, [UIntPtr]::Zero)
  Start-Sleep -Milliseconds 900
  $indicator = [UI]::FindByTitle('言出指示器', $app.Id)
  $bubble = [UI]::FindByTitle('言出', $app.Id)
  Check ($indicator -ne [IntPtr]::Zero -and [UI]::IsWindowVisible($indicator)) '指示器已显示'
  Check ($bubble -ne [IntPtr]::Zero -and [UI]::IsWindowVisible($bubble)) '气泡已显示'
  [UI]::keybd_event(0xA5, 0, 2, [UIntPtr]::Zero)
  Start-Sleep -Milliseconds 1200

  if (Test-Path $log) {
    $err = Get-Content $log -Raw
    if ($err -match 'panicked|thread .* panic') {
      Check $false "日志里出现 panic: $err"
    } else {
      Write-Host '  OK   日志无 panic' -ForegroundColor Green
    }
  }
} finally {
  $env:YANCHU_OPEN_PANEL = $null
  Get-Process -Id $app.Id -ErrorAction SilentlyContinue | Stop-Process -Force
}

if ($script:failed) { exit 1 } else { Write-Host '全部通过' -ForegroundColor Green }
