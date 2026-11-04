# 焦点丢失降级验证：按一下模式开始录音 → 切到别的窗口 → Esc 结束
# 预期：目标窗口不落字，文字进剪贴板。
$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [Text.Encoding]::UTF8

$root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
$appExe = Join-Path $root 'app\src-tauri\target\debug\yanchu.exe'
$targetExe = Join-Path $root 'tools\.e2e-target.exe'
$dump = Join-Path $root 'tools\.focus-target.txt'
$dump2 = Join-Path $root 'tools\.focus-target2.txt'
$appLog = Join-Path $root 'tools\.focus-app.log'
$triggerPath = Join-Path $root 'app\src-tauri\models\trigger.json'
$backup = "$triggerPath.focus-bak"
$wav = Join-Path $root 'bench\corpus\01.wav'

$nativeCs = @'
using System;
using System.Runtime.InteropServices;
public static class F {
    [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int c);
    [DllImport("user32.dll")] public static extern bool BringWindowToTop(IntPtr h);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
    [DllImport("user32.dll")] public static extern bool AttachThreadInput(uint a, uint b, bool f);
    [DllImport("user32.dll")] public static extern void keybd_event(byte vk, byte scan, uint flags, UIntPtr extra);

    public static uint ThreadOf(IntPtr h) { uint p; return GetWindowThreadProcessId(h, out p); }

    public static void DismissShell() {
        IntPtr fg = GetForegroundWindow();
        if (fg == IntPtr.Zero) return;
        uint pid; GetWindowThreadProcessId(fg, out pid);
        string name = "";
        try { name = System.Diagnostics.Process.GetProcessById((int)pid).ProcessName; } catch {}
        if (name == "SearchHost" || name == "ShellExperienceHost" || name == "StartMenuExperienceHost") {
            ShowWindow(fg, 0);
        }
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

function Tap([byte]$vk) {
  [F]::keybd_event($vk, 0, 0, [UIntPtr]::Zero)
  Start-Sleep -Milliseconds 60
  [F]::keybd_event($vk, 0, 2, [UIntPtr]::Zero)
  Start-Sleep -Milliseconds 120
}

$existed = Test-Path $triggerPath
if ($existed) { Copy-Item $triggerPath $backup -Force }
[IO.File]::WriteAllText($triggerPath, '{"keys":[165],"mode":"toggle","long_press_ms":0}', (New-Object Text.UTF8Encoding($false)))
Get-Process -Name yanchu -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep -Milliseconds 400
Remove-Item $dump, $appLog -ErrorAction SilentlyContinue
Set-Clipboard -Value 'FOCUS-BEFORE'

$target = Start-Process -FilePath $targetExe -ArgumentList $dump -PassThru
Start-Sleep -Milliseconds 1200
$hwnd = [IntPtr]::Zero
for ($i = 0; $i -lt 40; $i++) {
  $target.Refresh()
  if ($target.MainWindowHandle -ne [IntPtr]::Zero) { $hwnd = $target.MainWindowHandle; break }
  Start-Sleep -Milliseconds 100
}
if ($hwnd -eq [IntPtr]::Zero) { throw '假目标窗口没起来' }

$env:YANCHU_WAV_INPUT = $wav
$app = Start-Process -FilePath $appExe -PassThru -RedirectStandardError $appLog -WindowStyle Hidden
Start-Sleep -Seconds 3
$target2 = $null
try {
  $focused = $false
  for ($i = 0; $i -lt 10; $i++) {
    [F]::DismissShell()
    if ([F]::ForceForeground($hwnd)) { $focused = $true; break }
    Start-Sleep -Milliseconds 300
  }
  if (-not $focused) { throw '抢不到目标窗口焦点' }
  Start-Sleep -Milliseconds 400

  Tap 0xA5
  Start-Sleep -Milliseconds 1800

  # 再起一个同款窗口当前台，模拟用户中途点了别的应用
  $target2 = Start-Process -FilePath $targetExe -ArgumentList $dump2 -PassThru
  Start-Sleep -Milliseconds 1000
  $nh = [IntPtr]::Zero
  for ($i = 0; $i -lt 40; $i++) {
    $target2.Refresh()
    if ($target2.MainWindowHandle -ne [IntPtr]::Zero) { $nh = $target2.MainWindowHandle; break }
    Start-Sleep -Milliseconds 100
  }
  if ($nh -eq [IntPtr]::Zero -or -not [F]::ForceForeground($nh)) {
    throw '抢不到第二个窗口的焦点'
  }
  Write-Host '焦点已切到另一个窗口'

  Tap 0x1B
  Start-Sleep -Milliseconds 2500

  $clip = Get-Clipboard -Raw
  $targetText = if (Test-Path $dump) { [IO.File]::ReadAllText($dump, [Text.Encoding]::UTF8) } else { '' }
  Write-Host ("剪贴板: {0}" -f $clip)
  Write-Host ("目标窗口: {0}" -f $targetText)
  if ($clip -match '占位识别结果' -and $targetText.Length -eq 0) {
    Write-Host '焦点丢失降级通过：已复制到剪贴板，目标窗口未落字' -ForegroundColor Green
  } else {
    Write-Host '焦点丢失降级失败' -ForegroundColor Red
    exit 1
  }
} finally {
  if ($target2) { Stop-Process -Id $target2.Id -Force -ErrorAction SilentlyContinue }
  Stop-Process -Id $app.Id -Force -ErrorAction SilentlyContinue
  Stop-Process -Id $target.Id -Force -ErrorAction SilentlyContinue
  Remove-Item Env:\YANCHU_WAV_INPUT -ErrorAction SilentlyContinue
  Remove-Item $dump -ErrorAction SilentlyContinue
  Remove-Item $dump2 -ErrorAction SilentlyContinue
  if ($existed -and (Test-Path $backup)) { Move-Item $backup $triggerPath -Force }
  elseif (-not $existed) { Remove-Item $triggerPath -ErrorAction SilentlyContinue }
}
