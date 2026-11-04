# 下载言出用到的模型。
#
# 三个来源，按顺序回退：
#   1. GitHub release（国内通常最稳）
#   2. huggingface.co
#   3. hf-mirror.com（HuggingFace 国内镜像）
#
# 用法： powershell -ExecutionPolicy Bypass -File tools\download-models.ps1

$ErrorActionPreference = 'Continue'
$ProgressPreference = 'SilentlyContinue'

$root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
$models = Join-Path $root 'app\src-tauri\models'
New-Item -ItemType Directory -Force -Path $models | Out-Null

$senseVoiceDir = Join-Path $models 'sherpa-onnx-sense-voice-zh-en-ja-ko-yue-2024-07-17'
$vadPath = Join-Path $models 'silero_vad.onnx'

function Get-RemoteBytes {
  param([string[]]$Urls, [string]$OutFile, [long]$MinBytes)
  if ((Test-Path $OutFile) -and (Get-Item $OutFile).Length -ge $MinBytes) {
    Write-Host ("  已存在，跳过: {0}" -f (Split-Path -Leaf $OutFile))
    return $true
  }
  foreach ($u in $Urls) {
    Write-Host ("  尝试 {0}" -f $u)
    # -C - 断点续传，-L 跟随跳转，超时给足
    & curl.exe -L -C - --retry 3 --retry-delay 5 --connect-timeout 60 --max-time 1800 -o $OutFile $u 2>&1 | Select-Object -Last 2
    if ($LASTEXITCODE -eq 0 -and (Test-Path $OutFile) -and (Get-Item $OutFile).Length -ge $MinBytes) {
      return $true
    }
  }
  return $false
}

Write-Host '=== SenseVoice int8 (≈230MB) ==='
New-Item -ItemType Directory -Force -Path $senseVoiceDir | Out-Null

$tar = Join-Path $models 'sensevoice.tar.bz2'
$got = Get-RemoteBytes -Urls @(
  'https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-sense-voice-zh-en-ja-ko-yue-2024-07-17.tar.bz2'
) -OutFile $tar -MinBytes 100MB

if ($got) {
  Write-Host '  解包...'
  Push-Location $models
  & tar.exe -xjf $tar
  $code = $LASTEXITCODE
  Pop-Location
  if ($code -eq 0) { Remove-Item $tar -Force -ErrorAction SilentlyContinue }
} else {
  # GitHub 不通就退回直连
  $base = 'https://huggingface.co/csukuangfj/sherpa-onnx-sense-voice-zh-en-ja-ko-yue-2024-07-17/resolve/main'
  [void](Get-RemoteBytes -Urls @("$base/model.int8.onnx", $base.Replace('huggingface.co','hf-mirror.com') + '/model.int8.onnx') -OutFile (Join-Path $senseVoiceDir 'model.int8.onnx') -MinBytes 100MB)
  [void](Get-RemoteBytes -Urls @("$base/tokens.txt", $base.Replace('huggingface.co','hf-mirror.com') + '/tokens.txt') -OutFile (Join-Path $senseVoiceDir 'tokens.txt') -MinBytes 100KB)
}

Write-Host '=== Silero VAD (0.6MB) ==='
[void](Get-RemoteBytes -Urls @('https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/silero_vad.onnx') -OutFile $vadPath -MinBytes 100KB)

Write-Host ''
Write-Host '=== 结果 ==='
Get-ChildItem $models -Recurse -File | Select-Object @{n='文件';e={$_.FullName.Replace($models+'\','')}}, @{n='MB';e={[math]::Round($_.Length/1MB,1)}} | Format-Table -AutoSize

$model = Join-Path $senseVoiceDir 'model.int8.onnx'
if ((Test-Path $model) -and (Get-Item $model).Length -gt 100MB) {
  Write-Host '模型就绪' -ForegroundColor Green
} else {
  Write-Host '模型不完整，换个网络环境再跑一次本脚本（支持断点续传）' -ForegroundColor Yellow
}
