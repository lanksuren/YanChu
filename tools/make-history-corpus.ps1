# Build a real-voice corpus out of the sessions YanChu already recorded.
#
# bench/corpus is TTS-synthesized. devlog/ has real microphone audio from actual
# usage, but no human-written reference text: the only text is what the ASR
# produced, which would make every comparison circular.
#
# So this script produces a *draft* corpus: audio + the ASR output as the first
# draft of the reference. You then play each clip and fix the .txt.
# Until the .txt is corrected, CER numbers are meaningless (they measure the
# model against itself).
#
# Usage:
#   powershell -ExecutionPolicy Bypass -File tools\make-history-corpus.ps1
#   powershell -ExecutionPolicy Bypass -File tools\make-history-corpus.ps1 -Count 30
#   powershell -ExecutionPolicy Bypass -File tools\make-history-corpus.ps1 -Force   # overwrite corrected text

param(
    [string]$OutDir  = "bench\history",
    [int]$Count      = 20,
    [double]$MinSec  = 5,
    [double]$MaxSec  = 30,
    [switch]$Force
)

$ErrorActionPreference = "Stop"

$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

$eventsPath = Join-Path $root "devlog\events.jsonl"
if (-not (Test-Path $eventsPath)) {
    Write-Host "No devlog found: $eventsPath" -ForegroundColor Red
    Write-Host "Turn on YANCHU_DEV_LOG=1 first, or point this script at another log dir."
    exit 1
}

$outPath = Join-Path $root $OutDir
if (-not (Test-Path $outPath)) { New-Item -ItemType Directory -Path $outPath | Out-Null }

# devlog 是 UTF-8，PS 5.1 默认按 ANSI 读会把中文读坏
$lines = Get-Content $eventsPath -Encoding UTF8
$events = foreach ($line in $lines) {
    if ($line.Trim() -eq "") { continue }
    try { $line | ConvertFrom-Json } catch { }
}

# 只留真人会话：排除 e2e 注入的测试音频，排除太短/太长的
$real = @($events | Where-Object {
    $_.target -ne ".e2e-target.exe" -and
    -not $_.aborted -and
    $_.audio_secs -ge $MinSec -and
    $_.audio_secs -le $MaxSec -and
    $_.raw -and $_.raw.Trim() -ne ""
})

if ($real.Count -eq 0) {
    Write-Host "No usable real sessions in $eventsPath" -ForegroundColor Red
    exit 1
}

# 长句优先 + 文本去重。
# 真实会话里最短的那批（4 秒左右）反复说的其实是同一句话，直接按时间取会得到
# 一堆重复样本；按长度降序再按文本去重，拿到的是更自然、更有信息量的口述。
$seen = @{}
$unique = @()
foreach ($e in ($real | Sort-Object audio_secs -Descending)) {
    $key = $e.raw.Trim()
    if ($seen.ContainsKey($key)) { continue }
    $seen[$key] = $true
    $unique += $e
}

if ($unique.Count -eq 0) {
    Write-Host "No usable real sessions in $eventsPath" -ForegroundColor Red
    exit 1
}

$picked = @($unique | Select-Object -First $Count)

Write-Host ""
Write-Host "Found $($real.Count) real sessions, picking $($picked.Count)"
Write-Host "Output -> $outPath"
Write-Host ""

$i = 0
foreach ($e in $picked) {
    $i++
    $num = "{0:D2}" -f $i
    $date = $e.started_local.Substring(0, 10)
    $srcWav = Join-Path $root ("devlog\sessions\{0}\{1}\audio.wav" -f $date, $e.id)
    if (-not (Test-Path $srcWav)) {
        Write-Host "  [$num] missing audio: $srcWav" -ForegroundColor DarkYellow
        continue
    }

    $dstWav = Join-Path $outPath "$num.wav"
    $dstTxt = Join-Path $outPath "$num.txt"

    Copy-Item $srcWav $dstWav -Force

    if ((Test-Path $dstTxt) -and (-not $Force)) {
        Write-Host "  [$num] kept existing reference (use -Force to overwrite)"
    } else {
        Set-Content -Path $dstTxt -Value $e.raw.Trim() -Encoding UTF8 -NoNewline
        Write-Host ("  [{0}] {1,5:N1}s  {2}" -f $num, $e.audio_secs, $e.raw.Substring(0, [Math]::Min(40, $e.raw.Length)))
    }
}

Write-Host ""
Write-Host "Done. Next:" -ForegroundColor Cyan
Write-Host "  1. Play each .wav and fix the matching .txt (the text is ASR draft, not truth)"
Write-Host "  2. Score:  app\src-tauri\target\debug\yanchu.exe --bench $OutDir"
Write-Host ""
Write-Host "Uncorrected references measure the model against itself (CER near 0),"
Write-Host "which is worse than useless -- it looks like a great result."
