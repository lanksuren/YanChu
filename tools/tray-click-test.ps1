# 托盘左键测试：点托盘里的「言出」图标，控制面板应该打开。
#
# 步骤：起应用 → 展开隐藏图标 → 找到「言出」→ Invoke（等价左键）→ 断言面板可见。
# 用法： powershell -ExecutionPolicy Bypass -File tools\tray-click-test.ps1

$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [Text.Encoding]::UTF8
Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes

$root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
$exe = Join-Path $root 'app\src-tauri\target\debug\yanchu.exe'
$log = Join-Path $root 'tools\.tray-click.log'
$script:failed = $false

$cs = @'
using System;
using System.Runtime.InteropServices;
using System.Text;
public static class T {
    public delegate bool EnumProc(IntPtr h, IntPtr p);
    [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc cb, IntPtr p);
    [DllImport("user32.dll")] public static extern int GetWindowTextLength(IntPtr h);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern int GetWindowText(IntPtr h, StringBuilder sb, int max);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern IntPtr FindWindow(string cls, string name);
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
Start-Sleep -Milliseconds 500
$app = Start-Process -FilePath $exe -PassThru -RedirectStandardError $log -WindowStyle Hidden
Start-Sleep -Seconds 5

try {
  Write-Host '展开隐藏图标...'
  $tray = [System.Windows.Automation.AutomationElement]::FromHandle([T]::FindWindow('Shell_TrayWnd', $null))
  $chevron = $tray.FindFirst([System.Windows.Automation.TreeScope]::Descendants,
    [System.Windows.Automation.PropertyCondition]::new(
      [System.Windows.Automation.AutomationElement]::NameProperty, '显示隐藏的图标'))
  Check ($null -ne $chevron) '找到「显示隐藏的图标」'
  if ($null -eq $chevron) { throw '没有隐藏图标按钮' }
  $chevron.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke()
  Start-Sleep -Milliseconds 1000

  # Windows 11 的溢出面板是 XAML island，没有独立的 NotifyIconOverflowWindow 句柄；
  # 直接从桌面根节点找按钮，按名字过滤。
  $desktop = [System.Windows.Automation.AutomationElement]::RootElement
  $all = $desktop.FindAll([System.Windows.Automation.TreeScope]::Descendants,
    [System.Windows.Automation.PropertyCondition]::new(
      [System.Windows.Automation.AutomationElement]::ControlTypeProperty,
      [System.Windows.Automation.ControlType]::Button))
  Write-Host ("  桌面按钮 {0} 个，按名字找「言出」..." -f $all.Count)
  $target = $null
  foreach ($b in $all) {
    if ($b.Current.Name -like '*言出*') { $target = $b; break }
  }
  Check ($null -ne $target) '在托盘里找到「言出」图标'

  if ($null -ne $target) {
    $target.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke()
    Start-Sleep -Milliseconds 1000
    $panel = [T]::Find('言出 · 控制面板', $app.Id)
    Check ($panel -ne [IntPtr]::Zero) '面板窗口存在'
    Check ([T]::IsWindowVisible($panel)) '左键点击托盘后控制面板已显示'
  }
} finally {
  Get-Process -Id $app.Id -ErrorAction SilentlyContinue | Stop-Process -Force
}

if ($script:failed) { exit 1 } else { Write-Host '全部通过' -ForegroundColor Green }
