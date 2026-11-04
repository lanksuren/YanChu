# 对比不同小模型的润色延迟与效果。
#
# 直接打 Ollama 的 /api/chat，用的 system prompt 与应用内 llm.rs 保持一致。
# 目的只有一个：量出「这次润色要花多久」，以及它有没有守住「不改实词」。
#
# 用法： powershell -ExecutionPolicy Bypass -File tools\test-polish.ps1 -Model qwen2.5:0.5b

param(
  [string]$Model = 'qwen2.5:0.5b',
  [string]$Url = 'http://127.0.0.1:11434',
  # Qwen3/3.5 默认会「想一下」再回答，润色场景必须关掉，否则延迟翻几倍
  [switch]$NoThink
)

$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [Text.Encoding]::UTF8

$system = @'
你是中文语音输入的书面化整理器。请把用户的口述整理成可以直接发出去的书面文字。

必须遵守：
1. 删除语气词和口头禅：嗯、呃、啊、那个、就是说、然后就是、你知道吧 等；
2. 补全并规范标点，把一句话断开的地方接好；
3. 保持中英混排原样，专业术语（如 helper、staging、timeout、JSON）一个字母都不要改；
4. 不得增加、删减或替换任何实词，不得改变原意，不得添加解释；
5. 只输出整理后的文本本身，不要加引号、不要加说明。
'@

$samples = @(
  '嗯，这个 bug 在 tging 环境复现不了，只在 production 出现。',
  '啊，那个，我想问一下这个接口为什么返回空。',
  '把这段逻辑抽成一个 helper 函数放到 utils 里。',
  '就是说我们这个方案，那个，呃，还需要再讨论一下。',
  '我需要你把 timeout 从三十秒改成六十秒，然后，那个，重新跑一遍测试。',
  '这个函数的入参类型改成 optional，嗯，就是这样。'
)

$results = @()

foreach ($text in $samples) {
  $payload = @{
    model    = $Model
    stream   = $false
    options  = @{ temperature = 0.1; num_predict = 256 }
    messages = @(
      @{ role = 'system'; content = $system },
      @{ role = 'user'; content = $text }
    )
  }
  if ($NoThink) { $payload['think'] = $false }
  $body = $payload | ConvertTo-Json -Depth 6 -Compress

  $sw = [Diagnostics.Stopwatch]::StartNew()
  try {
    $bytes = [Text.Encoding]::UTF8.GetBytes($body)
    $resp = Invoke-RestMethod -Uri "$Url/api/chat" -Method Post -Body $bytes -ContentType 'application/json; charset=utf-8' -TimeoutSec 120
    $ms = $sw.ElapsedMilliseconds
    $out = $resp.message.content.Trim()
  } catch {
    $ms = $sw.ElapsedMilliseconds
    $out = "（失败: $($_.Exception.Message)）"
  }

  $results += [pscustomobject]@{ Ms = $ms; In = $text; Out = $out }

  Write-Host ''
  Write-Host ("[{0}ms] {1}" -f $ms, $text) -ForegroundColor DarkGray
  Write-Host ("   → {0}" -f $out) -ForegroundColor Green
}

$ms = $results | ForEach-Object { $_.Ms } | Sort-Object
$p50 = $ms[[int]($ms.Count * 0.5)]
$p95 = $ms[[math]::Min($ms.Count - 1, [int]($ms.Count * 0.95))]

Write-Host ''
Write-Host ('=== {0} ===' -f $Model) -ForegroundColor Cyan
Write-Host ("P50 {0}ms   P95 {1}ms" -f $p50, $p95)
Write-Host ("预算 600ms 内：{0}" -f $(if ($p95 -le 600) { '是' } else { '否' }))
