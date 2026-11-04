# 开关状态测试：持久化 / 面板与托盘同源 / 深度档置灰 / 暂停提示。
#
# 用法： powershell -ExecutionPolicy Bypass -File tools\state-test.ps1
#
# 依赖 WebView2 的 UI Automation 暴露 aria-label 与文本。
# 脚本先把自己切成 per-monitor DPI aware，否则 200%+ 缩放下点击坐标会偏。

$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [Text.Encoding]::UTF8
Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes

$root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
$exe = Join-Path $root 'app\src-tauri\target\debug\yanchu.exe'
$state = Join-Path $root 'app\src-tauri\models\state.json'
$log = Join-Path $root 'tools\.state-test.log'
$script:failed = $false

$cs = @'
using System;
using System.Drawing;
using System.Runtime.InteropServices;
using System.Text;
public static class S {
    public delegate bool EnumProc(IntPtr h, IntPtr p);
    [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc cb, IntPtr p);
    [DllImport("user32.dll")] public static extern int GetWindowTextLength(IntPtr h);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern int GetWindowText(IntPtr h, StringBuilder sb, int max);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
    [DllImport("user32.dll")] public static extern void keybd_event(byte vk, byte scan, uint flags, UIntPtr extra);
    [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
    [DllImport("user32.dll")] public static extern void mouse_event(uint f, uint dx, uint dy, uint data, UIntPtr extra);
    [DllImport("user32.dll")] public static extern bool SetProcessDpiAwarenessContext(IntPtr value);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] public static extern uint GetDpiForWindow(IntPtr h);
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
    [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr h, IntPtr after, int x, int y, int cx, int cy, uint flags);
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
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
    public static void Click(int x, int y) {
        SetCursorPos(x, y);
        System.Threading.Thread.Sleep(80);
        mouse_event(0x0002, 0, 0, 0, UIntPtr.Zero);
        mouse_event(0x0004, 0, 0, 0, UIntPtr.Zero);
    }
    public static int Pixel(int x, int y) {
        using (var b = new Bitmap(1, 1))
        using (var g = Graphics.FromImage(b)) {
            g.CopyFromScreen(x, y, 0, 0, new Size(1, 1));
            var c = b.GetPixel(0, 0);
            return (c.R << 16) | (c.G << 8) | c.B;
        }
    }
}
'@
Add-Type -TypeDefinition $cs -ReferencedAssemblies System.Drawing
[void][S]::SetProcessDpiAwarenessContext([IntPtr](-4))

function Check($ok, $msg) {
  if ($ok) { Write-Host "  OK   $msg" -ForegroundColor Green }
  else { Write-Host "  FAIL $msg" -ForegroundColor Red; $script:failed = $true }
}

function Find-ByName([IntPtr]$hwnd, [string]$name) {
  $el = [System.Windows.Automation.AutomationElement]::FromHandle($hwnd)
  $cond = [System.Windows.Automation.PropertyCondition]::new(
    [System.Windows.Automation.AutomationElement]::NameProperty, $name)
  return $el.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $cond)
}

function Find-CheckBox([IntPtr]$hwnd, [string]$name) {
  $el = [System.Windows.Automation.AutomationElement]::FromHandle($hwnd)
  $cond = [System.Windows.Automation.AndCondition]::new(
    [System.Windows.Automation.PropertyCondition]::new(
      [System.Windows.Automation.AutomationElement]::NameProperty, $name),
    [System.Windows.Automation.PropertyCondition]::new(
      [System.Windows.Automation.AutomationElement]::ControlTypeProperty,
      [System.Windows.Automation.ControlType]::CheckBox))
  return $el.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $cond)
}

function Toggle-State($el) {
  $p = $el.GetCurrentPattern([System.Windows.Automation.TogglePattern]::Pattern)
  return $p.Current.ToggleState
}

function Click-Element($el) {
  $r = $el.Current.BoundingRectangle
  [S]::Click([int]($r.X + $r.Width / 2), [int]($r.Y + $r.Height / 2))
}

function Find-Text([IntPtr]$hwnd, [string]$name) {
  $el = [System.Windows.Automation.AutomationElement]::FromHandle($hwnd)
  $cond = [System.Windows.Automation.AndCondition]::new(
    [System.Windows.Automation.PropertyCondition]::new(
      [System.Windows.Automation.AutomationElement]::NameProperty, $name),
    [System.Windows.Automation.PropertyCondition]::new(
      [System.Windows.Automation.AutomationElement]::ControlTypeProperty,
      [System.Windows.Automation.ControlType]::Text))
  return $el.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $cond)
}

# 开关的 input 是 0×0（视觉上藏在 slider 下面），点它的 bounding rect 会点到 (0,0)。
# 所以按「行文字 + 面板右边缘」算开关中心：右侧留 20(body)+14(card)+20(半个开关)。
function Click-Switch([IntPtr]$panel, [string]$label) {
  $text = Find-Text $panel $label
  if ($null -eq $text) { throw "找不到开关文字: $label" }
  $r = New-Object S+RECT
  [void][S]::GetWindowRect($panel, [ref]$r)
  $scale = [S]::GetDpiForWindow($panel) / 96.0
  $tr = $text.Current.BoundingRectangle
  $x = [int]($r.R - 54 * $scale)
  $y = [int]($tr.Y + $tr.Height / 2)
  [S]::Click($x, $y)
}

function Click-Row([IntPtr]$panel, [string]$label) {
  $text = Find-Text $panel $label
  if ($null -eq $text) { throw "找不到开关文字: $label" }
  $r = $text.Current.BoundingRectangle
  [S]::Click([int]($r.X + $r.Width / 2), [int]($r.Y + $r.Height / 2))
}

# 直接读开关中心的像素：亮蓝=开，浅灰=关。
# WebView2 的 UIA ToggleState 在这里不可靠（开着的开关也报 Off），所以用像素兜底。
function Switch-On([IntPtr]$panel, [string]$label) {
  $text = Find-Text $panel $label
  if ($null -eq $text) { throw "找不到开关文字: $label" }
  $r = New-Object S+RECT
  [void][S]::GetWindowRect($panel, [ref]$r)
  $scale = [S]::GetDpiForWindow($panel) / 96.0
  $tr = $text.Current.BoundingRectangle
  $x = [int]($r.R - 54 * $scale)
  $y = [int]($tr.Y + $tr.Height / 2)
  $rgb = [S]::Pixel($x, $y)
  $blue = $rgb -band 0xFF
  $red = ($rgb -shr 16) -band 0xFF
  return ($blue -gt 150 -and $red -lt 120)
}

# 备选：让 WebView 自己把焦点放到 checkbox 上，再敲空格（不依赖点击坐标）
function Activate-CheckBox($el) {
  try { $el.SetFocus() } catch { }
  Start-Sleep -Milliseconds 300
  [S]::keybd_event(0x20, 0, 0, [UIntPtr]::Zero)
  Start-Sleep -Milliseconds 80
  [S]::keybd_event(0x20, 0, 2, [UIntPtr]::Zero)
}

function Start-App {
  $env:YANCHU_OPEN_PANEL = '1'
  $p = Start-Process -FilePath $exe -PassThru -RedirectStandardError $log -WindowStyle Hidden
  Start-Sleep -Seconds 5
  $panel = [S]::Find('言出 · 控制面板', $p.Id)
  return @($p, $panel)
}

function Stop-App($p) {
  $env:YANCHU_OPEN_PANEL = $null
  Get-Process -Id $p.Id -ErrorAction SilentlyContinue | Stop-Process -Force
  Start-Sleep -Milliseconds 600
}

function Log-Has([string]$needle) {
  if (-not (Test-Path $log)) { return $false }
  $text = Get-Content -LiteralPath $log -Raw -Encoding UTF8
  return $text.Contains($needle)
}

Get-Process -Name yanchu -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep -Milliseconds 500

# 先把状态写成「听写关、深度开」，验证能读出来、且深度因为没云端 API 而置灰
New-Item -ItemType Directory -Force -Path (Split-Path $state) | Out-Null
[IO.File]::WriteAllText($state, '{"dictation":false,"polish":true,"deep":true}')

$r = Start-App
$app = $r[0]; $panel = $r[1]
try {
  Check ($panel -ne [IntPtr]::Zero) '控制面板已打开'
  if ($panel -eq [IntPtr]::Zero) { throw '面板没打开，后面的断言没有意义' }
  [void][S]::SetForegroundWindow($panel)
  Start-Sleep -Milliseconds 700

  $dict = Find-CheckBox $panel '语音听写'
  Check ($null -ne $dict) '找到「语音听写」开关'
  Check (Log-Has '读取状态：听写=false') '启动时读到了持久化的「关」（不是默认的开）'

  $deep = Find-CheckBox $panel '深度整理'
  Check ($null -ne $deep) '找到「深度整理」开关'
  Check (-not $deep.Current.IsEnabled) '没有云端 API 时深度开关置灰'

  # 在面板里点开听写：应写回 state.json，并把托盘勾选同步成开
  [void][S]::SetWindowPos($panel, [IntPtr](-1), 0, 0, 0, 0, 0x0001 -bor 0x0002)
  [void][S]::SetForegroundWindow($panel)
  Start-Sleep -Milliseconds 300

  # 懒创建的面板第一次显示时，第一次点击可能只用来激活窗口，不会落进 WebView。
  # 先点标题栏激活，再点整行。
  $wr = New-Object S+RECT
  [void][S]::GetWindowRect($panel, [ref]$wr)
  [S]::Click($wr.L + 40, $wr.T + 20)
  Start-Sleep -Milliseconds 400

  # 鼠标点在整行上；偶发没落到 WebView，重试一次。
  $saved = $null
  for ($try = 0; $try -lt 2; $try++) {
    Click-Row $panel '语音听写'
    for ($i = 0; $i -lt 5; $i++) {
      Start-Sleep -Milliseconds 300
      $saved = Get-Content -LiteralPath $state -Raw | ConvertFrom-Json
      if ($saved.dictation -eq $true) { break }
    }
    if ($saved.dictation -eq $true) { break }
  }
  Check (Log-Has '状态已同步：听写=true') '面板改开关后状态已广播/同步'
  Check ($saved.dictation -eq $true) '面板改开关后写回了 state.json'
} finally {
  Stop-App $app
}

# 重启：应保持「开」
$r2 = Start-App
$app2 = $r2[0]; $panel2 = $r2[1]
try {
  Start-Sleep -Seconds 2
  Check (Log-Has '读取状态：听写=true') '重启后控制面板读到的是「开」'
} finally {
  Stop-App $app2
}

# 收尾：恢复默认
[IO.File]::WriteAllText($state, '{"dictation":true,"polish":true,"deep":false}')

if ($script:failed) { exit 1 } else { Write-Host '全部通过' -ForegroundColor Green }
