# 端到端自动验证：不靠人说话，把整条链路真的跑一遍。
#
# 做法：
#   1. 起一个假目标窗口（真实前台 Win32 窗口 + 文本框，内容定时落到文件里）
#   2. 用 YANCHU_WAV_INPUT 让言出拿一段 wav 冒充麦克风
#   3. 按当前输入模式合成触发键（默认右 Alt）：
#      - hold  ：按住右 Alt，时长覆盖整段音频，再松开
#      - toggle：按一下右 Alt 开始，等音频，再按一下（或 Esc）结束
#   4. 读回目标窗口的内容，断言文字确实落进去了
#
# 用法： powershell -ExecutionPolicy Bypass -File tools\e2e-test.ps1 [-Wav <路径>] [-HoldMs <毫秒>] [-Mode hold|toggle] [-StopWithEsc]

param(
  [string]$Wav = '',
  [int]$HoldMs = 0,
  [ValidateSet('hold', 'toggle')][string]$Mode = 'hold',
  [switch]$StopWithEsc
)

$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [Text.Encoding]::UTF8

$root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
$appExe = Join-Path $root 'app\src-tauri\target\debug\yanchu.exe'
$targetSrc = Join-Path $root 'spike\target-app.cs'
$targetExe = Join-Path $root 'tools\.e2e-target.exe'
$dump = Join-Path $root 'tools\.e2e-dump.txt'
$appLog = Join-Path $root 'tools\.e2e-app.log'
$triggerPath = Join-Path $root 'app\src-tauri\models\trigger.json'
$triggerBackup = "$triggerPath.e2e-bak"

if ($StopWithEsc -and $Mode -ne 'toggle') {
  throw '-StopWithEsc 只能和 -Mode toggle 一起用（按住模式松手就结束）'
}
if (-not $Wav) {
  $Wav = Join-Path $root 'bench\corpus\01.wav'
}

if (-not (Test-Path $appExe)) { throw "还没编译: $appExe（先 cargo build）" }
if (-not (Test-Path $Wav)) { throw "找不到 wav: $Wav" }

# ---------------------------------------------------------------- Win32

$nativeCs = @'
using System;
using System.Runtime.InteropServices;
public static class E2E {
    [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
    [DllImport("user32.dll")] public static extern bool IsWindow(IntPtr h);
    [DllImport("user32.dll")] public static extern bool BringWindowToTop(IntPtr h);
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int cmd);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
    [DllImport("user32.dll")] public static extern bool AttachThreadInput(uint a, uint b, bool f);
    [DllImport("user32.dll")] public static extern void keybd_event(byte vk, byte scan, uint flags, UIntPtr extra);
    [DllImport("user32.dll")] public static extern int GetWindowTextLength(IntPtr h);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern int GetWindowText(IntPtr h, System.Text.StringBuilder sb, int max);

    public static uint ThreadOf(IntPtr h) { uint p; return GetWindowThreadProcessId(h, out p); }

    public static string Title(IntPtr h) {
        int n = GetWindowTextLength(h);
        var sb = new System.Text.StringBuilder(n + 2);
        GetWindowText(h, sb, sb.Capacity);
        return sb.ToString();
    }

    public static bool ForceForeground(IntPtr h) {
        IntPtr fg = GetForegroundWindow();
        if (fg == h) return true;
        uint a = ThreadOf(fg), b = ThreadOf(h);
        bool attached = false;
        if (a != b && a != 0) attached = AttachThreadInput(a, b, true);
        try { ShowWindow(h, 9); BringWindowToTop(h); SetForegroundWindow(h); }
        finally { if (attached) AttachThreadInput(a, b, false); }
        return GetForegroundWindow() == h;
    }
}
'@
Add-Type -TypeDefinition $nativeCs -Language CSharp

$VK_RMENU = 0xA5
$VK_ESCAPE = 0x1B

function Get-TargetText {
  if (Test-Path $dump) {
    try { return [IO.File]::ReadAllText($dump, [Text.Encoding]::UTF8) } catch { return '' }
  }
  return ''
}

function Tap-Key([byte]$vk, [int]$DownMs = 60) {
  [E2E]::keybd_event($vk, 0, 0, [UIntPtr]::Zero)
  Start-Sleep -Milliseconds $DownMs
  [E2E]::keybd_event($vk, 0, 2, [UIntPtr]::Zero)
  Start-Sleep -Milliseconds 120
}

# ---------------------------------------------------------------- 准备

if (-not (Test-Path $targetExe)) {
  Write-Host '编译假目标窗口...'
  $cs = [IO.File]::ReadAllText($targetSrc, [Text.Encoding]::UTF8)
  Add-Type -TypeDefinition $cs -OutputAssembly $targetExe -OutputType WindowsApplication `
    -ReferencedAssemblies 'System', 'System.Core', 'System.Windows.Forms', 'System.Drawing'
}

# toggle 模式要临时改触发方式；跑完恢复用户原来的设置。
$triggerExisted = Test-Path $triggerPath
if ($Mode -eq 'toggle') {
  if ($triggerExisted) { Copy-Item $triggerPath $triggerBackup -Force }
  $json = '{"keys":[165],"mode":"toggle","long_press_ms":0}'
  [IO.File]::WriteAllText($triggerPath, $json, (New-Object Text.UTF8Encoding($false)))
  Write-Host '触发方式已临时切到 toggle（完成会恢复）'
}

Remove-Item $dump, $appLog -ErrorAction SilentlyContinue

$target = Start-Process -FilePath $targetExe -ArgumentList $dump -PassThru
Start-Sleep -Milliseconds 1200

$hwnd = [IntPtr]::Zero
for ($i = 0; $i -lt 40; $i++) {
  $target.Refresh()
  if ($target.MainWindowHandle -ne [IntPtr]::Zero) { $hwnd = $target.MainWindowHandle; break }
  Start-Sleep -Milliseconds 100
}
if ($hwnd -eq [IntPtr]::Zero) { throw '假目标窗口没起来' }

$env:YANCHU_WAV_INPUT = $Wav
$app = Start-Process -FilePath $appExe -PassThru -RedirectStandardError $appLog -WindowStyle Hidden
Start-Sleep -Seconds 3

# 现在 wav 是按真实节奏喂进采集缓冲的，按住时长必须覆盖整段音频，
# 否则松手时只录到前面一小截。
# 时长 = data 块大小 / 每秒字节数。扫描 "data" 而不是固定偏移，
# 因为文件里可能夹着 LIST 之类的块。
function Get-WavSeconds([string]$Path) {
  try {
    $bytes = [IO.File]::ReadAllBytes($Path)
    if ($bytes.Length -lt 64) { return 3.0 }
    $byteRate = [BitConverter]::ToInt32($bytes, 28)
    $idx = -1
    for ($i = 12; $i -lt $bytes.Length - 8; $i++) {
      if ($bytes[$i] -eq 0x64 -and $bytes[$i + 1] -eq 0x61 -and
          $bytes[$i + 2] -eq 0x74 -and $bytes[$i + 3] -eq 0x61) { $idx = $i; break }
    }
    if ($idx -lt 0 -or $byteRate -le 0) { return 3.0 }
    $dataSize = [BitConverter]::ToInt32($bytes, $idx + 4)
    if ($dataSize -le 0) { return 3.0 }
    return $dataSize / [double]$byteRate
  } catch {
    return 3.0
  }
}

if ($HoldMs -le 0) {
  $hold = [int]((Get-WavSeconds $Wav) * 1000) + 500
} else {
  $hold = $HoldMs
}
Write-Host ("模式: {0}；时长: {1}ms（音频 {2:N1}s）" -f $Mode, $hold, (Get-WavSeconds $Wav))

try {
  $focused = $false
  for ($i = 0; $i -lt 10; $i++) {
    if ([E2E]::ForceForeground($hwnd)) { $focused = $true; break }
    Start-Sleep -Milliseconds 300
  }
  if (-not $focused) { throw '抢不到焦点' }
  Start-Sleep -Milliseconds 400

  if ($Mode -eq 'hold') {
    Write-Host '按下右 Alt...'
    [E2E]::keybd_event($VK_RMENU, 0, 0, [UIntPtr]::Zero)
    Start-Sleep -Milliseconds $hold
    Write-Host '松开右 Alt...'
    [E2E]::keybd_event($VK_RMENU, 0, 2, [UIntPtr]::Zero)
  } else {
    Write-Host '按一下右 Alt 开始录音...'
    Tap-Key $VK_RMENU
    Start-Sleep -Milliseconds $hold
    if ($StopWithEsc) {
      Write-Host '按 Esc 结束录音...'
      Tap-Key $VK_ESCAPE
    } else {
      Write-Host '再按一下右 Alt 结束录音...'
      Tap-Key $VK_RMENU
    }
  }

  $got = ''
  $deadline = (Get-Date).AddSeconds(25)
  while ((Get-Date) -lt $deadline) {
    $got = Get-TargetText
    if ($got.Length -gt 0) { break }
    Start-Sleep -Milliseconds 300
  }

  Write-Host ''
  Write-Host '--- 结果 ---'
  if ($got.Length -gt 0) {
    Write-Host ("落字成功，目标窗口内容: {0}" -f $got) -ForegroundColor Green
  } else {
    Write-Host '没有落字' -ForegroundColor Red
  }

  if ($got.Length -eq 0) {
    Write-Host ''
    Write-Host '--- 应用日志（尾部） ---'
    if (Test-Path $appLog) { Get-Content $appLog -Tail 30 }
    exit 1
  }

  Write-Host ''
  Write-Host '--- 应用日志（尾部） ---'
  if (Test-Path $appLog) { Get-Content $appLog -Tail 25 }
} finally {
  Stop-Process -Id $app.Id -Force -ErrorAction SilentlyContinue
  Stop-Process -Id $target.Id -Force -ErrorAction SilentlyContinue
  Remove-Item Env:\YANCHU_WAV_INPUT -ErrorAction SilentlyContinue
  Remove-Item $dump -ErrorAction SilentlyContinue

  if ($Mode -eq 'toggle') {
    if ($triggerExisted -and (Test-Path $triggerBackup)) {
      Move-Item $triggerBackup $triggerPath -Force
    } elseif (-not $triggerExisted) {
      Remove-Item $triggerPath -ErrorAction SilentlyContinue
    }
  }
}
