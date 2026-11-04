# 量「松手 → 出字」的分布：跑 N 次端到端，汇总 devlog 里的 release_to_text_ms 和识别调用数。
#
# 为什么要单独一个脚本：e2e-test.ps1 只回答「有没有落字」，
# 而调延迟要的是 P50/P95、识别调了几次、每次多少毫秒、润色占了多少。
#
# 用法： powershell -ExecutionPolicy Bypass -File tools\lat-bench.ps1 -Runs 3 -Wav bench\corpus\03.wav
# 前置： tools\build.bat build --features asr（用 debug 版就行，ONNX 是预编译 C++）

param(
  [int]$Runs = 3,
  [string]$Wav = '',
  [string]$SessionRoot = ''
)

$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [Text.Encoding]::UTF8

$root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
if (-not $Wav) { $Wav = Join-Path $root 'bench\corpus\03.wav' }
if (-not $SessionRoot) { $SessionRoot = Join-Path $root 'devlog\sessions' }

$env:YANCHU_DEV_LOG = '1'

$rows = @()
for ($i = 1; $i -le $Runs; $i++) {
  Write-Host "--- 第 $i/$Runs 次 ---"
  # 上一轮残留的实例会让单实例插件直接吞掉新进程，必须等它真的退干净
  Get-Process -Name yanchu -ErrorAction SilentlyContinue | Stop-Process -Force
  for ($w = 0; $w -lt 20; $w++) {
    if (-not (Get-Process -Name yanchu -ErrorAction SilentlyContinue)) { break }
    Start-Sleep -Milliseconds 250
  }

  $startedAt = Get-Date
  $out = (powershell -ExecutionPolicy Bypass -File (Join-Path $root 'tools\e2e-test.ps1') -Wav $Wav 2>&1) -join "`n"
  if ($out -notmatch '落字成功') {
    Write-Host '这次没落字，跳过'
    ($out -split "`n" | Select-Object -Last 8) | Write-Host
    continue
  }

  # 只认这一轮新写的会话
  $latest = Get-ChildItem (Join-Path $SessionRoot '*\*\session.json') |
    Where-Object { $_.LastWriteTime -ge $startedAt.AddSeconds(-2) } |
    Sort-Object LastWriteTime | Select-Object -Last 1
  if (-not $latest) { continue }
  $raw = Get-Content $latest.FullName -Raw

  $release = [int]([regex]::Match($raw, '"release_to_text_ms":\s*(\d+)').Groups[1].Value)
  $polish = [regex]::Match($raw, '"polish_ms":\s*(\d+)')
  $polish = if ($polish.Success) { [int]$polish.Groups[1].Value } else { 0 }
  $insert = [int]([regex]::Match($raw, '"insert"[\s\S]*?"ms":\s*(\d+)').Groups[1].Value)

  # asr_calls 里每一调的毫秒数
  $head = $raw -split '"asr_calls"'
  $msAll = [regex]::Matches($head[-1], '"ms":\s*(\d+)') | ForEach-Object { [int]$_.Groups[1].Value }
  $partials = ([regex]::Matches($raw, '"kind": "partial"')).Count
  $commits = ([regex]::Matches($raw, '"kind": "commit"')).Count
  $finalMs = 0
  $mf = [regex]::Match($raw, '"kind": "final"[\s\S]*?"ms":\s*(\d+)')
  if ($mf.Success) { $finalMs = [int]$mf.Groups[1].Value }

  $sumAsr = ($msAll | Measure-Object -Sum).Sum
  if (-not $sumAsr) { $sumAsr = 0 }

  $rows += [pscustomobject]@{
    run        = $i
    release_ms = $release
    asr_sum_ms = $sumAsr
    final_ms   = $finalMs
    polish_ms  = $polish
    insert_ms  = $insert
    partials   = $partials
    commits    = $commits
    session    = $latest.Directory.Name
  }
  "$release ms（识别共 ${sumAsr}ms / final ${finalMs}ms / 润色 ${polish}ms / partial $partials 次）"
}

if ($rows.Count -eq 0) { throw '一次都没成功' }

Write-Host ''
Write-Host '=== 汇总 ==='
$rows | Format-Table -AutoSize

function Pctl($values, $p) {
  $s = $values | Sort-Object
  $idx = [Math]::Min($s.Count - 1, [Math]::Round(($p / 100.0) * ($s.Count - 1)))
  return $s[$idx]
}

$rel = $rows.release_ms
"松手→出字  P50 {0}ms  P95 {1}ms  min {2}ms  max {3}ms" -f (Pctl $rel 50), (Pctl $rel 95), ($rel | Measure-Object -Minimum).Minimum, ($rel | Measure-Object -Maximum).Maximum
"识别总耗时  平均 {0}ms（partial {1} 次 / commit {2} 次）" -f [int](($rows.asr_sum_ms | Measure-Object -Average).Average), [int](($rows.partials | Measure-Object -Average).Average), [int](($rows.commits | Measure-Object -Average).Average)
"润色        平均 {0}ms" -f [int](($rows.polish_ms | Measure-Object -Average).Average)
