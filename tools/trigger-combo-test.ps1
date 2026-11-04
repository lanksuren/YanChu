# 组合键触发验证：Ctrl+Shift+Space，可选长按阈值。
# 用法： powershell -ExecutionPolicy Bypass -File tools\trigger-combo-test.ps1 [-LongPressMs 350]
param([int]$LongPressMs = 0)

$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [Text.Encoding]::UTF8

$root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
$appExe = Join-Path $root 'app\src-tauri\target\debug\yanchu.exe'
$targetExe = Join-Path $root 'tools\.e2e-target.exe'
$dump = Join-Path $root 'tools\.combo-target.txt'
$appLog = Join-Path $root 'tools\.combo-app.log'
$triggerPath = Join-Path $root 'app\src-tauri\models\trigger.json'
$backup = "$triggerPath.combo-bak"
$wav = Join-Path $root 'bench\corpus\01.wav'

$nativeCs = @'
using System;
using System.Runtime.InteropServices;
public static class C {
    [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int c);
    [DllImport("user32.dll")] public static extern bool BringWindowToTop(IntPtr h);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
    [DllImport("user32.dll")] public static extern bool AttachThreadInput(uint a, uint b, bool f);
    [DllImport("user32.dll")] public static extern void keybd_event(byte vk, byte scan, uint flags, UIntPtr extra);
    public static uint T(IntPtr h) { uint p; return GetWindowThreadProcessId(h, out p); }
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
        uint a = T(fg), b = T(h);
        bool at = a != b && a != 0 ? AttachThreadInput(a, b, true) : false;
        try { ShowWindow(h, 9); BringWindowToTop(h); SetForegroundWindow(h); }
        finally { if (at) AttachThreadInput(a, b, false); }
        return GetForegroundWindow() == h;
    }
}
'@
Add-Type -TypeDefinition $nativeCs -Language CSharp

function Get-WavSeconds([string]$Path) {
  $bytes = [IO.File]::ReadAllBytes($Path)
  $byteRate = [BitConverter]::ToInt32($bytes, 28)
  $idx = -1
  for ($i = 12; $i -lt $bytes.Length - 8; $i++) {
    if ($bytes[$i] -eq 0x64 -and $bytes[$i + 1] -eq 0x61 -and
        $bytes[$i + 2] -eq 0x74 -and $bytes[$i + 3] -eq 0x61) { $idx = $i; break }
  }
  if ($idx -lt 0 -or $byteRate -le 0) { return 3.0 }
  return ([BitConverter]::ToInt32($bytes, $idx + 4)) / [double]$byteRate
}

$existed = Test-Path $triggerPath
if ($existed) { Copy-Item $triggerPath $backup -Force }
$json = '{"keys":[162,160,32],"mode":"hold","long_press_ms":' + $LongPressMs + '}'
[IO.File]::WriteAllText($triggerPath, $json, (New-Object Text.UTF8Encoding($false)))
Get-Process -Name yanchu -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep -Milliseconds 400
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

$env:YANCHU_WAV_INPUT = $wav
$app = Start-Process -FilePath $appExe -PassThru -RedirectStandardError $appLog -WindowStyle Hidden
Start-Sleep -Seconds 3
try {
  $focused = $false
  for ($i = 0; $i -lt 10; $i++) {
    [C]::DismissShell()
    if ([C]::ForceForeground($hwnd)) { $focused = $true; break }
    Start-Sleep -Milliseconds 300
  }
  if (-not $focused) { throw '抢不到目标窗口焦点' }
  Start-Sleep -Milliseconds 400

  $hold = [int]((Get-WavSeconds $wav) * 1000) + $LongPressMs + 900
  Write-Host ("按下 Ctrl+Shift+Space，长按阈值 {0}ms，保持 {1}ms..." -f $LongPressMs, $hold)
  [C]::keybd_event(0xA2, 0, 0, [UIntPtr]::Zero)
  Start-Sleep -Milliseconds 40
  [C]::keybd_event(0xA0, 0, 0, [UIntPtr]::Zero)
  Start-Sleep -Milliseconds 40
  [C]::keybd_event(0x20, 0, 0, [UIntPtr]::Zero)
  Start-Sleep -Milliseconds $hold
  [C]::keybd_event(0x20, 0, 2, [UIntPtr]::Zero)
  Start-Sleep -Milliseconds 40
  [C]::keybd_event(0xA0, 0, 2, [UIntPtr]::Zero)
  Start-Sleep -Milliseconds 40
  [C]::keybd_event(0xA2, 0, 2, [UIntPtr]::Zero)

  $got = ''
  $deadline = (Get-Date).AddSeconds(25)
  while ((Get-Date) -lt $deadline) {
    if (Test-Path $dump) { $got = [IO.File]::ReadAllText($dump, [Text.Encoding]::UTF8) }
    if ($got.Length -gt 0) { break }
    Start-Sleep -Milliseconds 300
  }

  Write-Host ("落字: {0}" -f $got)
  if ($got -match '占位识别结果') {
    Write-Host '组合键触发通过' -ForegroundColor Green
  } else {
    Write-Host '组合键触发失败' -ForegroundColor Red
    if (Test-Path $appLog) { Get-Content $appLog -Tail 30 }
    exit 1
  }
} finally {
  Stop-Process -Id $app.Id -Force -ErrorAction SilentlyContinue
  Stop-Process -Id $target.Id -Force -ErrorAction SilentlyContinue
  Remove-Item Env:\YANCHU_WAV_INPUT -ErrorAction SilentlyContinue
  Remove-Item $dump -ErrorAction SilentlyContinue
  if ($existed -and (Test-Path $backup)) { Move-Item $backup $triggerPath -Force }
  elseif (-not $existed) { Remove-Item $triggerPath -ErrorAction SilentlyContinue }
}
