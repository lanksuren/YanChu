# Record the real-voice corpus for YanChu.
#
# The existing bench/corpus is TTS-synthesized, which is cleaner than a human
# and makes CER look better than it is. This script records 20 real sentences
# so the ASR numbers can be re-measured on actual speech.
#
# Usage:
#   powershell -ExecutionPolicy Bypass -File tools\record-corpus.ps1
#   powershell -ExecutionPolicy Bypass -File tools\record-corpus.ps1 -StartAt 8
#   powershell -ExecutionPolicy Bypass -File tools\record-corpus.ps1 -Seconds 15
#   powershell -ExecutionPolicy Bypass -File tools\record-corpus.ps1 -ScoreOnly
#
# Output: bench\real\01.wav .. 20.wav + matching .txt references,
# which `yanchu.exe --bench bench\real` can score directly.

param(
    [string]$OutDir  = "bench\real",
    [int]$Seconds    = 12,
    [int]$StartAt    = 1,
    [string]$Exe     = "",
    [switch]$ScoreOnly
)

$ErrorActionPreference = "Stop"

$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

if ($Exe -eq "") { $Exe = Join-Path $root "app\src-tauri\target\debug\yanchu.exe" }
if (-not (Test-Path $Exe)) {
    Write-Host "EXE not found: $Exe" -ForegroundColor Red
    Write-Host "Build it first:  tools\build.bat build --features asr"
    exit 1
}

$outPath = Join-Path $root $OutDir
$promptFile = Join-Path $outPath "prompts.txt"

if (-not (Test-Path $promptFile)) {
    Write-Host "Prompt file not found: $promptFile" -ForegroundColor Red
    exit 1
}

$lines = Get-Content $promptFile -Encoding UTF8
$prompts = @()
foreach ($line in $lines) {
    $t = $line.Trim()
    if ($t -eq "") { continue }
    if ($t.StartsWith("#")) { continue }
    $prompts += $t
}

if ($prompts.Count -eq 0) {
    Write-Host "No prompts parsed from $promptFile" -ForegroundColor Red
    exit 1
}

if (-not (Test-Path $outPath)) { New-Item -ItemType Directory -Path $outPath | Out-Null }

$score = {
    Write-Host ""
    Write-Host "=== scoring $OutDir ===" -ForegroundColor Cyan
    & $Exe --bench $outPath 2>&1 | Out-Host
}

if ($ScoreOnly) {
    & $score
    exit 0
}

Write-Host ""
Write-Host "YanChu real-voice corpus" -ForegroundColor Cyan
Write-Host "  $($prompts.Count) sentences, $Seconds s each, output -> $outPath"
Write-Host "  Read each line out loud at a natural pace. Keep the filler words;"
Write-Host "  they are the input we are testing, not noise to hide."
Write-Host ""
Write-Host "  Enter = next | r = redo this one | q = stop and keep what is recorded"
Write-Host ""

$recorded = 0
$i = $StartAt
while ($i -le $prompts.Count) {
    $text = $prompts[$i - 1]
    $num = "{0:D2}" -f $i
    $wav = Join-Path $outPath "$num.wav"
    $txt = Join-Path $outPath "$num.txt"

    Write-Host ""
    Write-Host "[$i / $($prompts.Count)] $num" -ForegroundColor Yellow
    Write-Host ""
    Write-Host "    $text" -ForegroundColor White
    Write-Host ""

    $redo = $true
    $quit = $false
    while ($redo) {
        $redo = $false
        for ($c = 3; $c -ge 1; $c--) {
            Write-Host "  recording in $c ..."
            Start-Sleep -Seconds 1
        }
        Write-Host "  REC ..." -ForegroundColor Red

        $out = & $Exe --record-wav $wav $Seconds 2>&1 | Out-String
        Write-Host $out

        $m = [regex]::Match($out, "RMS\s+([0-9.]+)")
        if ($m.Success) {
            $rms = [double]$m.Groups[1].Value
            if ($rms -lt 0.002) {
                Write-Host "  WARNING: level is nearly zero. Check the mic is not muted;" -ForegroundColor Red
                Write-Host "  a hardware-muted array returns digital silence, and every" -ForegroundColor Red
                Write-Host "  sentence would score as garbage." -ForegroundColor Red
            }
        }

        $answer = Read-Host "  Enter = next | r = redo | q = stop"
        if ($answer -eq "r") {
            $redo = $true
        } elseif ($answer -eq "q") {
            $quit = $true
            $redo = $false
        }
    }

    if ($quit) { break }

    Set-Content -Path $txt -Value $text -Encoding UTF8 -NoNewline
    $recorded++
    $i++
}

Write-Host ""
Write-Host "Recorded $recorded sentence(s) into $outPath" -ForegroundColor Green
Write-Host ""

$answer = Read-Host "Score them now? (y/N)"
if ($answer -eq "y") {
    & $score
}
