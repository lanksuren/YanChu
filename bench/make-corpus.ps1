# 生成 20 段验收语料：中文部分用 Huihui，英文术语用 Zira，再拼成一段 wav。
#
# 为什么要拆开合成：SAPI 的中文音色读英文术语基本是灾难，
# 直接整句合成会让「英文术语保留率」这项指标失去意义。
# 拆开之后每一段都是一次真实的中英切换，和用户的说话方式更接近。
#
# 局限：合成语音比真人干净得多，CER 会偏乐观。这只用来看「管线通不通」和
# 「延迟什么量级」，不能当成真实场景的准确率。

$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Speech

$root = Split-Path -Parent $MyInvocation.MyCommand.Path
$outDir = Join-Path $root 'corpus'
New-Item -ItemType Directory -Force -Path $outDir | Out-Null

$sentences = @(
  '帮我查一下这个接口返回值为空是为什么',
  '把这段逻辑抽成一个 helper 函数放到 utils 里',
  '这个 bug 在 staging 环境复现不了，只在 production 出现',
  '我需要你把 timeout 从三十秒改成六十秒',
  '用 Rust 重写这个模块，性能应该能提升三倍',
  '帮我生成一个 docker compose 文件',
  '这个函数的入参类型改成 optional',
  '数据库连接池的最大连接数调到五十',
  '这段代码的复杂度是 O(n log n) 吗',
  '把日志级别从 info 改成 debug',
  '部署脚本跑完之后记得检查 health check',
  '这个 API 的 response 里有三个字段是空的',
  '帮我把这个 JSON 格式化一下',
  '前端那个 loading 状态一直不消失',
  '缓存过期时间设成二十四小时',
  '这个 PR 需要 rebase 到 main 分支',
  '用 pytest 跑一下全部的单元测试',
  '这个模型的推理延迟有点高，看看能不能量化成 int8',
  '把 CI 里的 node 版本升到二十',
  '我需要一个能导出 CSV 的接口'
)

# ---------------- WAV 读写（只处理 PCM）

function Read-WavChunks {
  param([string]$Path)
  $bytes = [IO.File]::ReadAllBytes($Path)
  if ([Text.Encoding]::ASCII.GetString($bytes, 0, 4) -ne 'RIFF') { throw "不是 RIFF: $Path" }

  $fmt = $null
  $data = $null
  $i = 12
  while ($i -lt $bytes.Length - 8) {
    $id = [Text.Encoding]::ASCII.GetString($bytes, $i, 4)
    $size = [BitConverter]::ToInt32($bytes, $i + 4)

    if ($id -eq 'fmt ') {
      $fmt = New-Object byte[] $size
      [Array]::Copy($bytes, $i + 8, $fmt, 0, $size)
    } elseif ($id -eq 'data') {
      $data = New-Object byte[] $size
      [Array]::Copy($bytes, $i + 8, $data, 0, $size)
    }

    $i += 8 + $size + ($size % 2)
  }

  if ($null -eq $fmt -or $null -eq $data) { throw "缺 fmt/data: $Path" }
  return @{ Fmt = $fmt; Data = $data }
}

function Write-WavFile {
  param([string]$Path, [byte[]]$Fmt, [byte[]]$Data)
  $ms = New-Object IO.MemoryStream
  $bw = New-Object IO.BinaryWriter($ms)

  $bw.Write([Text.Encoding]::ASCII.GetBytes('RIFF'))
  $bw.Write([int](36 + $Data.Length))
  $bw.Write([Text.Encoding]::ASCII.GetBytes('WAVE'))
  $bw.Write([Text.Encoding]::ASCII.GetBytes('fmt '))
  $bw.Write([int]$Fmt.Length)
  $bw.Write($Fmt)
  $bw.Write([Text.Encoding]::ASCII.GetBytes('data'))
  $bw.Write([int]$Data.Length)
  $bw.Write($Data)
  $bw.Flush()

  [IO.File]::WriteAllBytes($Path, $ms.ToArray())
  $bw.Dispose()
  $ms.Dispose()
}

# ---------------- 合成

function Split-Runs {
  param([string]$Text)
  $runs = New-Object System.Collections.ArrayList
  $cur = New-Object System.Text.StringBuilder
  $curIsLatin = $null

  foreach ($ch in $Text.ToCharArray()) {
    $isLatin = ($ch -match '[A-Za-z0-9]') -or ('.#+/_-' -contains [string]$ch)
    if ($null -eq $curIsLatin) {
      $curIsLatin = $isLatin
    } elseif ($isLatin -ne $curIsLatin) {
      [void]$runs.Add(@{ Text = $cur.ToString(); Latin = $curIsLatin })
      $cur.Clear() | Out-Null
      $curIsLatin = $isLatin
    }
    [void]$cur.Append($ch)
  }
  if ($cur.Length -gt 0) { [void]$runs.Add(@{ Text = $cur.ToString(); Latin = $curIsLatin }) }
  return $runs
}

$zhVoice = New-Object System.Speech.Synthesis.SpeechSynthesizer
$enVoice = New-Object System.Speech.Synthesis.SpeechSynthesizer
$zhVoice.SelectVoice('Microsoft Huihui Desktop')
$enVoice.SelectVoice('Microsoft Zira Desktop')
$zhVoice.Rate = 0
$enVoice.Rate = 0

$tmp = Join-Path $env:TEMP ('yanchu-tts-' + [guid]::NewGuid().ToString('n'))
New-Item -ItemType Directory -Force -Path $tmp | Out-Null

$index = 0
foreach ($sentence in $sentences) {
  $index++
  $name = '{0:d2}' -f $index

  $runs = Split-Runs -Text $sentence
  $fmt = $null
  $all = New-Object System.Collections.Generic.List[byte]

  $runIndex = 0
  foreach ($run in $runs) {
    if ([string]::IsNullOrWhiteSpace($run.Text)) { continue }
    # 只念纯标点的片段会得到一个空 wav，直接跳过
    if ($run.Text -notmatch '[\u4e00-\u9fffA-Za-z0-9]') { continue }
    $runIndex++
    $part = Join-Path $tmp ("$name`_$runIndex.wav")

    $synth = if ($run.Latin) { $enVoice } else { $zhVoice }
    $synth.SetOutputToWaveFile($part)
    $synth.Speak($run.Text)
    $synth.SetOutputToNull()

    try {
      $chunks = Read-WavChunks -Path $part
    } catch {
      Write-Warning ("跳过空片段: {0}" -f $run.Text)
      continue
    }
    if ($null -eq $fmt) { $fmt = $chunks.Fmt }
    $all.AddRange($chunks.Data)
  }

  if ($null -eq $fmt -or $all.Count -eq 0) {
    Write-Warning ("{0} 没有合成出内容，跳过" -f $name)
    continue
  }

  $wavPath = Join-Path $outDir "$name.wav"
  Write-WavFile -Path $wavPath -Fmt $fmt -Data $all.ToArray()
  Set-Content -Path (Join-Path $outDir "$name.txt") -Value $sentence -Encoding UTF8

  $secs = [math]::Round($all.Count / 2.0 / 22050.0, 1)
  Write-Host ("{0}  {1,5}s  {2}" -f $name, $secs, $sentence)
}

$zhVoice.Dispose()
$enVoice.Dispose()
Remove-Item $tmp -Recurse -Force -ErrorAction SilentlyContinue

Write-Host ''
Write-Host ("语料已生成: {0} 段 -> {1}" -f $index, $outDir) -ForegroundColor Green
