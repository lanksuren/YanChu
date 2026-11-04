# 在桌面（和开始菜单）创建「言出」快捷方式。
#
# 用法： powershell -ExecutionPolicy Bypass -File tools\make-shortcut.ps1
#        powershell ... -Exe <自定义 exe 路径>

param(
  [string]$Exe = ''
)

$ErrorActionPreference = 'Stop'

$root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)

if (-not $Exe) {
  $candidates = @(
    (Join-Path $root 'app\src-tauri\target\release\yanchu.exe'),
    (Join-Path $root 'app\src-tauri\target\debug\yanchu.exe')
  )
  $Exe = $candidates | Where-Object { Test-Path $_ } | Select-Object -First 1
}

if (-not $Exe -or -not (Test-Path $Exe)) {
  throw "找不到 yanchu.exe，先编译（tools\build.bat build --features asr）"
}

$icon = Join-Path $root 'app\src-tauri\icons\icon.ico'
$workDir = Split-Path -Parent $Exe

$shell = New-Object -ComObject WScript.Shell

$targets = @(
  [Environment]::GetFolderPath('Desktop'),
  (Join-Path $env:APPDATA 'Microsoft\Windows\Start Menu\Programs')
)

foreach ($dir in $targets) {
  if (-not $dir -or -not (Test-Path $dir)) { continue }
  $lnkPath = Join-Path $dir '言出.lnk'

  $lnk = $shell.CreateShortcut($lnkPath)
  $lnk.TargetPath = $Exe
  $lnk.WorkingDirectory = $workDir
  if (Test-Path $icon) { $lnk.IconLocation = "$icon,0" }
  $lnk.Description = '言出 —— 按住右 Alt 说话，松手落字'
  $lnk.WindowStyle = 7   # 最小化启动，不要弹黑框
  $lnk.Save()

  Write-Host ("已创建: {0}" -f $lnkPath) -ForegroundColor Green
}

Write-Host ("目标: {0}" -f $Exe)
