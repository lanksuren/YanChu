# 验证气泡窗口的透明是否真的生效。
#
# 做法：垫一个纯红窗口在下面 → 触发气泡 → 抓屏采样。
#   圆角外侧的像素如果是红色 → 透明生效
#   如果是灰白 → WebView2 的底色还盖着
#
# 用法： powershell -ExecutionPolicy Bypass -File tools\check-transparency.ps1 [-HoldMs 20000]

param([int]$HoldMs = 20000)

$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [Text.Encoding]::UTF8

# 必须在任何 UI 调用之前声明 DPI 感知，否则拿到的坐标是被虚拟化过的
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class Dpi {
    [DllImport("shcore.dll")] public static extern int SetProcessDpiAwareness(int v);
    [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
    public static void Enable() { try { SetProcessDpiAwareness(2); } catch { SetProcessDPIAware(); } }
}
'@
[Dpi]::Enable()

Add-Type -AssemblyName System.Drawing

# 纯红底窗：必须在 C# 里起线程，PowerShell 直接 new Thread 会歧义
Add-Type -TypeDefinition @'
using System;
using System.Drawing;
using System.Threading;
using System.Windows.Forms;

public static class RedWin
{
    private static Form form;
    public static IntPtr Start()
    {
        IntPtr handle = IntPtr.Zero;
        var ready = new ManualResetEvent(false);
        var t = new Thread(() =>
        {
            form = new Form();
            form.Text = "YANCHU_RED_BACKDROP";
            form.FormBorderStyle = FormBorderStyle.None;
            form.StartPosition = FormStartPosition.Manual;
            // 铺满整个屏幕：气泡贴在屏幕底部，尺寸小了根本盖不住
            form.Location = new Point(0, 0);
            form.Size = Screen.PrimaryScreen.Bounds.Size;
            form.BackColor = Color.FromArgb(255, 0, 0);
            // 必须置顶：否则会被前台的其它窗口盖住，采样到的是别人的颜色
            form.TopMost = true;
            form.Show();
            handle = form.Handle;
            ready.Set();
            Application.Run(form);
        });
        t.SetApartmentState(ApartmentState.STA);
        t.IsBackground = true;
        t.Start();
        ready.WaitOne(4000);
        return handle;
    }

    public static void Stop()
    {
        try { if (form != null) form.Invoke(new Action(() => form.Close())); } catch { }
    }
}
'@ -ReferencedAssemblies System.Windows.Forms, System.Drawing

Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class Win {
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
    [DllImport("user32.dll")] public static extern void keybd_event(byte vk, byte scan, uint flags, UIntPtr extra);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }

    private delegate bool EnumProc(IntPtr h, IntPtr l);
    [DllImport("user32.dll")] private static extern bool EnumWindows(EnumProc cb, IntPtr l);
    [DllImport("user32.dll")] private static extern int GetWindowTextLength(IntPtr h);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] private static extern int GetWindowText(IntPtr h, System.Text.StringBuilder sb, int max);

    // 隐藏窗口也能按 PID + 标题找到
    public static IntPtr FindByPid(uint targetPid, string title) {
        IntPtr found = IntPtr.Zero;
        EnumWindows((h, l) => {
            uint pid; GetWindowThreadProcessId(h, out pid);
            if (pid != targetPid) return true;
            int n = GetWindowTextLength(h);
            if (n == 0) return true;
            var sb = new System.Text.StringBuilder(n + 2);
            GetWindowText(h, sb, sb.Capacity);
            if (sb.ToString() == title) { found = h; return false; }
            return true;
        }, IntPtr.Zero);
        return found;
    }
}
'@

$root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
$exe = Join-Path $root 'app\src-tauri\target\debug\yanchu.exe'
$wav = if ($env:YANCHU_CHECK_WAV) { $env:YANCHU_CHECK_WAV } else { Join-Path $root 'bench\corpus\01.wav' }

if (-not (Test-Path $exe)) { throw "还没编译: $exe" }
if (-not (Test-Path $wav)) { throw "找不到 wav: $wav" }

$hwndRed = [RedWin]::Start()
if ($hwndRed -eq [IntPtr]::Zero) { throw '红色底窗没起来' }
Start-Sleep -Milliseconds 700

[void][Win]::SetForegroundWindow($hwndRed)
Start-Sleep -Milliseconds 500

$env:YANCHU_WAV_INPUT = $wav
$app = Start-Process -FilePath $exe -PassThru -WindowStyle Hidden
Start-Sleep -Seconds 4

try {
  [void][Win]::SetForegroundWindow($hwndRed)
  Start-Sleep -Milliseconds 300
  [Win]::keybd_event(0xA5, 0, 0, [UIntPtr]::Zero)
  Start-Sleep -Milliseconds $HoldMs  # 等文字攒够，验证换行

  $bubble = [Win]::FindByPid([uint32]$app.Id, '言出')
  if ($bubble -eq [IntPtr]::Zero) { throw '找不到气泡窗口' }

  $rect = New-Object Win+RECT
  [void][Win]::GetWindowRect($bubble, [ref]$rect)
  $w = [int]$rect.Right - [int]$rect.Left
  $h = [int]$rect.Bottom - [int]$rect.Top
  "气泡窗口: $($rect.Left),$($rect.Top) ${w}x${h}"

  $bmp = New-Object System.Drawing.Bitmap $w, $h
  $g = [System.Drawing.Graphics]::FromImage($bmp)
  $g.CopyFromScreen([int]$rect.Left, [int]$rect.Top, 0, 0, $bmp.Size)
  $g.Dispose()

  # 采样：四角内侧 4px 处应当透出底下的红色；中心应当是浅色气泡
  # 注意括号：PowerShell 里逗号比减号结合得紧，@($w - 5, 4) 会被解析成 $w - (5,4)
  $corners = @(
    @(4, 4), @(($w - 5), 4), @(4, ($h - 5)), @(($w - 5), ($h - 5))
  )
  $samples = @()
  foreach ($c in $corners) {
    $px = $bmp.GetPixel($c[0], $c[1])
    $samples += ("({0},{1})={2},{3},{4}" -f $c[0], $c[1], $px.R, $px.G, $px.B)
  }
  $center = $bmp.GetPixel([int]($w / 2), [int]($h / 2))

  "四角像素: " + ($samples -join '  ')
  "中心像素: $($center.R),$($center.G),$($center.B)"

  $out = Join-Path $root 'tools\bubble-shot.png'
  $bmp.Save($out, [System.Drawing.Imaging.ImageFormat]::Png)

  $transparent = $true
  foreach ($c in $corners) {
    $px = $bmp.GetPixel($c[0], $c[1])
    # 允许抗锯齿误差：红通道明显压过绿蓝就算透出来了
    if (-not ($px.R -gt 120 -and $px.G -lt 110 -and $px.B -lt 110)) { $transparent = $false }
  }
  $bmp.Dispose()

  [Win]::keybd_event(0xA5, 0, 2, [UIntPtr]::Zero)
  Start-Sleep -Milliseconds 300

  "截图: $out"
  if ($transparent) {
    Write-Host '✓ 圆角外侧是透明的' -ForegroundColor Green
  } else {
    Write-Host '✗ 圆角外侧还有底色' -ForegroundColor Red
    exit 1
  }
} finally {
  Remove-Item Env:\YANCHU_WAV_INPUT -ErrorAction SilentlyContinue
  Get-Process -Id $app.Id -ErrorAction SilentlyContinue | Stop-Process -Force
  [RedWin]::Stop()
}
