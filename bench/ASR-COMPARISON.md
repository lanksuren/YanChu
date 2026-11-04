# 本地 ASR 对比：SenseVoice Small vs Qwen3-ASR 0.6B

> 日期：2026-11-04
> 语料：`bench/corpus` 的 20 段中英混说 TTS 音频，共 134.2 秒
> 口径：只跑「识别 → 热词表 → 规则层」，**不跑大润色**，避免 LLM 把两种模型的差异抹平。
> 机器：本机 Windows，CPU 推理，线程数 4。

## 怎么跑

```powershell
# SenseVoice Small
$env:YANCHU_ASR_ENGINE = "sensevoice"
app\src-tauri\target\debug\yanchu.exe --bench bench\corpus

# Qwen3-ASR 0.6B
$env:YANCHU_ASR_ENGINE = "qwen3"
app\src-tauri\target\debug\yanchu.exe --bench bench\corpus
```

模型目录：

- SenseVoice：`app/src-tauri/models/sherpa-onnx-sense-voice-zh-en-ja-ko-yue-2024-07-17`
- Qwen3：`app/src-tauri/models/sherpa-onnx-qwen3-asr-0.6B-int8-2026-03-25`

## 结果

| 指标 | SenseVoice Small | Qwen3-ASR 0.6B |
|---|---:|---:|
| 模型体积 | 228.5 MB | 953.8 MB |
| 字错率 CER（规则层后） | 6.6% | **3.7%** |
| 字错率 CER（原始识别） | 7.3% | **6.8%** |
| 英文术语保留率 | 70%（19/27） | **93%（25/27）** |
| 单段识别 P50 | **92 ms** | 946 ms |
| 单段识别 P95 | **133 ms** | 1207 ms |
| 20 段总识别耗时 | **1.907 s** | 18.347 s |
| 平均实时率 RTF | **0.011** | 0.139 |
| 热词纠正 / 误替换 | 3 / 0 | 7 / 0 |
| 冷加载（日志观察） | 约 1 s | 约 3 s |

RTF = 识别耗时 / 音频时长；越低越快。Qwen3 大约是 SenseVoice 的 **9.6 倍**总耗时、**8–10 倍**单段延迟。

## 典型差异

| 样本 | 参考文本 | SenseVoice | Qwen3-ASR |
|---|---|---|---|
| 02 | `把这段逻辑抽成一个 helper 函数放到 utils 里` | `heler`、`il` | ✅ 全对 |
| 03 | `这个 bug 在 staging 环境…` | `tging` | ✅ `staging` |
| 05 | `用 Rust 重写…` | `用 R 重写…` | ✅ `Rust`（规则层统一小写） |
| 07 | `入参`、`optional` | `入餐`、`opional` | ✅ 全对 |
| 09 | `复杂度是 O(n log n) 吗` | `o and log and`，丢「吗」 | `O(N log N).`，也丢「吗」 |
| 10 | `info` → `debug` | `dbug` | ✅ 全对（规则层统一大小写） |
| 17 | `pytest` | `Pt` | `Ptest`（更接近，但仍不对） |
| 19 | `CI 里的 node` | `CI立` | `C I.`（空格拆开 term） |

结论很直接：**Qwen3 的英文术语和中文同音词明显更稳**，尤其适合开发场景里
`helper / utils / staging / pytest / Rust` 这类词；但它每句要等约 1 秒，
已经超过「松手 → 文字出现 P95 ≤ 800ms」的整条链路预算（还没算润色）。

## 怎么选

- **默认继续用 SenseVoice Small**：实时感最好，适合「说完马上出字」。
- **需要准确率时切 Qwen3-ASR 0.6B**：控制面板 → 模型 → 语音识别模型 → 切换。
  代价是单段多等约 0.8–1.1 秒，模型多占约 700MB 磁盘和更多内存。
- 如果后续要接云端 ASR API，这个对比也可以作为「本地兜底」的基线。

## 注意

- 这批语料是 Windows TTS 合成的，比真人干净；CER 会偏乐观。真人录音、口音、
  远场噪声下两种模型的差距可能变化。
- 这里的 CER 是「参考文本 vs 规则层后文本」；原始 CER 也列了，方便看规则层贡献。
- 大润色未参与本对比，避免不同模型经过 LLM 后被“抹平”；端到端体验另外看。
