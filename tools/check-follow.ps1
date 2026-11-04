# 验证指示器窗口真的跟着鼠标走。
#
# 做法：起应用 → 合成按下右 Alt 让它显示 → 把鼠标挪两个位置，
# 分别读指示器窗口的屏幕坐标，看它有没有跟着动。

$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [Text.Encoding]::UTF8

$exe = 'C:\02WORK\01-MyProject\YanChu\app\src-tauri\target\debug\yanchu.exe'
$log = 'C:\02WORK\01-MyProject\YanChu\tools\.follow.log'

$cs = @'
using System;
using System.Runtime.InteropServices;
using System.Text;

public static class W {
    [StructLayout(LayoutKind.Sequential)]
    public struct RECT { public int Left, Top, Right, Bottom; }

    public delegate bool EnumProc(IntPtr hWnd, IntPtr lParam);

    [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc cb, IntPtr p);
    [DllImport("user32.dll")] public static extern int GetWindowTextLength(IntPtr h);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern int GetWindowText(IntPtr h, StringBuilder sb, int max);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
    [DllImport("user32.dll")] public static extern void keybd_event(byte vk, byte scan, uint flags, UIntPtr extra);
    [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);

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
}
'@
Add-Type -TypeDefinition $cs -Language CSharp

Get-Process -Name yanchu -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep -Milliseconds 400
Remove-Item $log -ErrorAction SilentlyContinue

$app = Start-Process -FilePath $exe -PassThru -RedirectStandardError $log -WindowStyle Hidden
Start-Sleep -Seconds 4

try {
  $indicator = [W]::FindByTitle('言出指示器', $app.Id)
  $bubble = [W]::FindByTitle('言出', $app.Id)
  "指示器窗口: $indicator"
  "气泡窗口  : $bubble"

  if ($indicator -eq [IntPtr]::Zero) { throw '找不到指示器窗口' }

  '按下右 Alt...'
  [W]::keybd_event(0xA5, 0, 0, [UIntPtr]::Zero)
  Start-Sleep -Milliseconds 900

  $positions = @(@(300, 260), @(900, 640))
  $rects = @()
  foreach ($p in $positions) {
    [void][W]::SetCursorPos($p[0], $p[1])
    Start-Sleep -Milliseconds 500
    $r = New-Object W+RECT
    if ([W]::GetWindowRect($indicator, [ref]$r)) {
      $rects += ,@($r.Left, $r.Top)
      "鼠标 ({0},{1}) -> 指示器 ({2},{3})  可见={4}" -f $p[0], $p[1], $r.Left, $r.Top, [W]::IsWindowVisible($indicator)
    }
  }

  [W]::keybd_event(0xA5, 0, 2, [UIntPtr]::Zero)
  Start-Sleep -Milliseconds 600

  if ($rects.Count -eq 2) {
    $dx = [math]::Abs($rects[1][0] - $rects[0][0])
    $dy = [math]::Abs($rects[1][1] - $rects[0][1])
    "位移: dx=$dx dy=$dy"
    if ($dx -gt 400 -and $dy -gt 200) {
      Write-Host '✓ 指示器跟随鼠标' -ForegroundColor Green
    } else {
      Write-Host '✗ 指示器没有跟随' -ForegroundColor Red
      $script:bad = $true
    }
  }
} finally {
  Get-Process -Id $app.Id -ErrorAction SilentlyContinue | Stop-Process -Force
  Remove-Item $log -ErrorAction SilentlyContinue
  if ($script:bad) { exit 1 }
}
