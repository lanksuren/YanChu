# 分片下载：代理环境下长连接容易卡死，改成每片一个独立请求 + 断点续传。
#
# 用法: powershell -ExecutionPolicy Bypass -File tools\fetch-chunked.ps1 -Url <url> -Out <path>

param(
  [Parameter(Mandatory = $true)][string]$Url,
  [Parameter(Mandatory = $true)][string]$Out,
  [int]$ChunkMB = 8,
  [int]$MaxRetries = 6,
  [string]$Proxy = 'http://127.0.0.1:7897'
)

$ErrorActionPreference = 'Continue'
$chunk = $ChunkMB * 1MB

# 目标大小
$headers = & curl.exe -s -L -I --proxy $Proxy --connect-timeout 30 --max-time 60 $Url 2>$null
$total = 0
foreach ($line in $headers) {
  if ($line -match '(?i)^content-length:\s*(\d+)') { $total = [int64]$matches[1] }
}
Write-Host "目标大小: $([math]::Round($total/1MB,1)) MB"

$dir = Split-Path -Parent $Out
if ($dir -and -not (Test-Path $dir)) { New-Item -ItemType Directory -Force -Path $dir | Out-Null }

$pos = 0
if (Test-Path $Out) { $pos = (Get-Item $Out).Length }
Write-Host "已有: $([math]::Round($pos/1MB,1)) MB"

if ($total -gt 0 -and $pos -ge $total) {
  Write-Host '已经下载完整' -ForegroundColor Green
  exit 0
}

$fs = [IO.File]::Open($Out, [IO.FileMode]::Append, [IO.FileAccess]::Write)
$stuck = 0
try {
  while ($true) {
    if ($total -gt 0 -and $pos -ge $total) { break }

    $end = $pos + $chunk - 1
    if ($total -gt 0 -and $end -ge $total) { $end = $total - 1 }
    $tmp = Join-Path $env:TEMP ("chunk-" + [guid]::NewGuid().ToString('n') + ".part")

    $ok = $false
    for ($try = 1; $try -le $MaxRetries -and -not $ok; $try++) {
      & curl.exe -s -L --proxy $Proxy --connect-timeout 30 --max-time 180 -r "$pos-$end" -o $tmp $Url 2>$null
      $len = 0
      if (Test-Path $tmp) { $len = (Get-Item $tmp).Length }

      if ($LASTEXITCODE -eq 0 -and $len -gt 0) {
        $bytes = [IO.File]::ReadAllBytes($tmp)
        $fs.Write($bytes, 0, $bytes.Length)
        $fs.Flush()
        $pos += $bytes.Length
        $ok = $true
        $pct = if ($total -gt 0) { [math]::Round($pos * 100.0 / $total, 1) } else { 0 }
        Write-Host ("{0} MB / {1} MB ({2}%)" -f [math]::Round($pos/1MB,1), [math]::Round($total/1MB,1), $pct)
      } else {
        Start-Sleep -Milliseconds (800 * $try)
      }
      Remove-Item $tmp -Force -ErrorAction SilentlyContinue
    }

    if (-not $ok) {
      $stuck++
      Write-Warning "这一片重试 $MaxRetries 次都失败（已连续失败 $stuck 次）"
      if ($stuck -ge 3) { break }
    } else {
      $stuck = 0
    }
  }
} finally {
  $fs.Close()
}

$final = (Get-Item $Out).Length
Write-Host ("完成: {0} MB" -f [math]::Round($final/1MB,1))
if ($total -gt 0 -and $final -lt $total) {
  Write-Warning "未下完（$final / $total），再跑一次本脚本会接着下"
  exit 1
}
exit 0
