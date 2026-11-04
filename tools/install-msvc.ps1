# Installs MSVC build tools (VS Build Tools + Windows SDK) with retries.
# ASCII-only on purpose: this runs via powershell.exe -File, and an unbom'd
# non-ASCII .ps1 gets mis-decoded by Windows PowerShell 5.1.
#
# Usage (elevated): powershell -ExecutionPolicy Bypass -File install-msvc.ps1

$ErrorActionPreference = 'Continue'
$log = Join-Path $env:TEMP 'yanchu-msvc.log'
$done = Join-Path $env:TEMP 'yanchu-msvc.done'

function Log([string]$m) {
  $line = "{0} {1}" -f (Get-Date -Format 'HH:mm:ss'), $m
  Write-Host $line
  Add-Content -Path $log -Value $line
}

Set-Content -Path $log -Value "=== install starts $(Get-Date) ==="

$vsWhere = 'C:\Program Files (x86)\Microsoft Visual Studio\Installer\vswhere.exe'
$setup = 'C:\Program Files (x86)\Microsoft Visual Studio\Installer\setup.exe'
$argsList = '--quiet --wait --norestart --nocache --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended'

function Test-MsvcReady {
  $link = Get-ChildItem 'C:\Program Files (x86)\Microsoft Visual Studio' -Recurse -Filter link.exe -ErrorAction SilentlyContinue | Select-Object -First 1
  $sdk = Get-ChildItem 'C:\Program Files (x86)\Windows Kits\10\Lib' -ErrorAction SilentlyContinue | Select-Object -First 1
  return ($null -ne $link -and $null -ne $sdk)
}

# ---------------------------------------------------------------- attempt 1: bootstrapper
$boot = Join-Path $env:TEMP 'vs_BuildTools.exe'
if (-not (Test-Path $boot)) {
  Log 'downloading vs_BuildTools.exe'
  Invoke-WebRequest -Uri 'https://aka.ms/vs/17/release/vs_BuildTools.exe' -OutFile $boot -UseBasicParsing
}

Log 'attempt 1: vs_BuildTools.exe'
try {
  $p = Start-Process -FilePath $boot -ArgumentList $argsList -Wait -PassThru
  Log "bootstrapper exit=$($p.ExitCode)"
} catch {
  Log "bootstrapper threw: $($_.Exception.Message)"
}

if (-not (Test-MsvcReady) -and (Test-Path $setup)) {
  Log 'attempt 1b: setup.exe directly'
  try {
    $p = Start-Process -FilePath $setup -ArgumentList $argsList -Wait -PassThru
    Log "setup exit=$($p.ExitCode)"
  } catch {
    Log "setup threw: $($_.Exception.Message)"
  }
}

# ---------------------------------------------------------------- attempt 2: standalone installer
if (-not (Test-MsvcReady)) {
  Log 'attempt 2: fresh vs_installer.exe'
  $vi = Join-Path $env:TEMP 'vs_installer.exe'
  try {
    Invoke-WebRequest -Uri 'https://aka.ms/vs/17/release/installer' -OutFile $vi -UseBasicParsing
    Log "downloaded vs_installer.exe $((Get-Item $vi).Length) bytes"
    $p = Start-Process -FilePath $vi -ArgumentList '--quiet --wait --norestart --nocache' -Wait -PassThru
    Log "vs_installer exit=$($p.ExitCode)"

    if (Test-Path $setup) {
      $p = Start-Process -FilePath $setup -ArgumentList $argsList -Wait -PassThru
      Log "setup(2) exit=$($p.ExitCode)"
    }
  } catch {
    Log "attempt 2 threw: $($_.Exception.Message)"
  }
}

# ---------------------------------------------------------------- verify
$ready = Test-MsvcReady
$link = Get-ChildItem 'C:\Program Files (x86)\Microsoft Visual Studio' -Recurse -Filter link.exe -ErrorAction SilentlyContinue | Select-Object -First 1
Log "MSVC ready: $ready"
if ($link) { Log "link.exe: $($link.FullName)" }

"ready=$ready" | Set-Content -Path $done -Encoding ASCII
Log '=== install ends ==='
