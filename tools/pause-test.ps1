# 暂停提示：听写关掉时按右 Alt，气泡要出现「听写已暂停 · 托盘里可恢复」。
#
# 用法： powershell -ExecutionPolicy Bypass -File tools\pause-test.ps1

$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [Text.Encoding]::UTF8
Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes

$root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
$exe = Join-Path $root 'app\src-tauri\target\debug\yanchu.exe'
$state = Join-Path $root 'app\src-tauri\models\state.json'
$log = Join-Path $root 'tools\.pause-test.log'
$script:failed = $false

$cs = @'
using System;
using System.Runtime.InteropServices;
using System.Text;
public static class PZ {
    public delegate bool EnumProc(IntPtr h, IntPtr p);
    [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc cb, IntPtr p);
    [DllImport("user32.dll")] public static extern int GetWindowTextLength(IntPtr h);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern int GetWindowText(IntPtr h, StringBuilder sb, int max);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
    [DllImport("user32.dll")] public static extern void keybd_event(byte vk, byte scan, uint flags, UIntPtr extra);
    public static IntPtr Find(string title, uint pid) {
        IntPtr f = IntPtr.Zero;
        EnumWindows((h, p) => {
            uint wp; GetWindowThreadProcessId(h, out wp);
            if (wp != pid) return true;
            int n = GetWindowTextLength(h);
            if (n == 0) return true;
            var sb = new StringBuilder(n + 2);
            GetWindowText(h, sb, sb.Capacity);
            if (sb.ToString() == title) { f = h; return false; }
            return true;
        }, IntPtr.Zero);
        return f;
    }
}
'@
Add-Type -TypeDefinition $cs

function Check($ok, $msg) {
  if ($ok) { Write-Host "  OK   $msg" -ForegroundColor Green }
  else { Write-Host "  FAIL $msg" -ForegroundColor Red; $script:failed = $true }
}

Get-Process -Name yanchu -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep -Milliseconds 1200
[IO.File]::WriteAllText($state, '{"dictation":false,"polish":true,"deep":false}')

$env:YANCHU_OPEN_PANEL = '1'
$app = Start-Process -FilePath $exe -PassThru -RedirectStandardError $log -WindowStyle Hidden
Start-Sleep -Seconds 5

# 单实例插件：如果上一个测试的进程还没退干净，这次启动会直接退出，
# 拿到的是旧进程的窗口。确认自己活着，否则重启一次。
if ($app.HasExited) {
  Get-Process -Name yanchu -ErrorAction SilentlyContinue | Stop-Process -Force
  Start-Sleep -Milliseconds 1200
  $app = Start-Process -FilePath $exe -PassThru -RedirectStandardError $log -WindowStyle Hidden
  Start-Sleep -Seconds 5
}
Check (-not $app.HasExited) '应用已启动（单实例未误杀）'

try {
  [PZ]::keybd_event(0xA5, 0, 0, [UIntPtr]::Zero)

  $bubble = [IntPtr]::Zero
  for ($i = 0; $i -lt 8; $i++) {
    Start-Sleep -Milliseconds 200
    $bubble = [PZ]::Find('言出', $app.Id)
    if ($bubble -ne [IntPtr]::Zero -and [PZ]::IsWindowVisible($bubble)) { break }
  }
  Check ($bubble -ne [IntPtr]::Zero -and [PZ]::IsWindowVisible($bubble)) '暂停提示时气泡可见'

  $tip = $null
  if ($bubble -ne [IntPtr]::Zero) {
    $bel = [System.Windows.Automation.AutomationElement]::FromHandle($bubble)
    # WebView2 的可访问性树偶尔晚几百毫秒才建好，重试几次
    for ($i = 0; $i -lt 14 -and $null -eq $tip; $i++) {
      Start-Sleep -Milliseconds 150
      $tip = $bel.FindFirst([System.Windows.Automation.TreeScope]::Descendants,
        [System.Windows.Automation.PropertyCondition]::new(
          [System.Windows.Automation.AutomationElement]::NameProperty,
          '听写已暂停 · 托盘里可恢复'))
    }
  }
  Check ($null -ne $tip) '气泡里出现「听写已暂停」'

  [PZ]::keybd_event(0xA5, 0, 2, [UIntPtr]::Zero)
  Start-Sleep -Milliseconds 300
} finally {
  $env:YANCHU_OPEN_PANEL = $null
  Get-Process -Id $app.Id -ErrorAction SilentlyContinue | Stop-Process -Force
}

# 收尾：恢复默认开关
[IO.File]::WriteAllText($state, '{"dictation":true,"polish":true,"deep":false}')

if ($script:failed) { exit 1 } else { Write-Host '全部通过' -ForegroundColor Green }
