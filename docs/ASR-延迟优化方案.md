# Qwen3-ASR 0.6B 延迟优化方案

> 日期：2026-11-04
> 背景：`models/asr.json` 已切到 `qwen3`，实际体感比 SenseVoice Small 慢。本文回答「慢在哪、怎么改」，并按性价比排序。
> 依据：本仓库代码（行号对应 2026-11-04 的 HEAD）、`bench/ASR-COMPARISON.md` 的实测数、`docs/产品定义-v2.md` 的延迟预算。

## 一、账面数字 vs 端到端

| 口径 | SenseVoice Small | Qwen3-ASR 0.6B |
|---|---:|---:|
| 单次 decode（约 6.7s 音频，CPU 4 线程） | 92ms P50 / 133ms P95 | 946ms P50 / 1207ms P95 |
| 单段最高值（产品定义 v2 记为 1131ms） | — | ~1.1s |
| 产品预算：松手 → 文字出现 | P95 ≤ 800ms | 单次 decode 就已击穿 |
| 产品预算：句末 → 气泡更新 | ≤ 300ms | 灰字必然落后 |

**关键认知**：946ms 只是「一次 decode」的账面成本。端到端体感 = 账面成本 × 排队 × 重算次数，Qwen3 在这三项上全都被放大。所以用户感觉到的差距大于 9.6×，也小于「模型参数量必然如此」。

## 二、瓶颈不在「用离线编码器做流式」，在这三处

### 1. 单把互斥锁 + 饱和的 tick（最大头）

- `streaming_loop` 每 `PARTIAL_INTERVAL = 450ms`（`app/src-tauri/src/lib.rs:98`）跑一次；
  Qwen3 单次 decode 946–1131ms → **占空比 > 100%**，循环永远在 decode 里，灰字永远落后 1–2s。
- `transcribe()` 全程持有 `transcriber` 的互斥锁（`lib.rs:1468-1478`）。松手时 `finish()` 的定稿 decode
  **必须排在正在跑的尾巴 decode 后面**；最坏情况那条尾巴是 12s 音频（约 2s）。
- 结果：松手 → 出字 ≈ 排队等待（0–2s）+ 定稿 decode（0.9–2s）+ 润色落字（~0.1s）。

### 2. 尾巴上限 12s 对 0.6B 太宽松

`MAX_TAIL_SAMPLES = 192_000`（12s，`lib.rs:101-105`）、`FORCED_COMMIT_SAMPLES = 160_000`（10s）。
encoder 成本随音频长度线性、decoder 成本随 token 数线性 → 12s 尾巴一次 ≈ 6s 尾巴两次的价钱，
却只换来一次更旧的灰字。SenseVoice 时代 12s 无所谓（92ms），Qwen3 时代它是主要成本项。

### 3. 断点要等满 500ms 静音才开始 decode

`find_commit_point` 要求 5 个静音帧（`lib.rs:837-874`）才返回切点，之后才开始 decode。
这 500ms 是纯等待 —— 静音一旦确认，`[committed_end..silence_start]` 的音频已经齐了，可以提前开跑。

## 三、对检索到的那份回答的核对

| 那条建议 | 核对结果 | 对你的结论 |
|---|---|---|
| 换 `qfuxa/qwen3-asr-0.6b-streaming` 因果塔是「首选」 | 仓库真实存在，但**英文专用**：FLEURS zh CER 11.4 → 85.7（蒸馏只在英文上做）；CPU 只有 fp32 路径 RTF 0.82，而你现在 int8 ONNX 是 0.139；运行时是 Python/torch，sherpa-onnx 没有对应 ONNX 导出 | **负优化**。中文明细度会崩，CPU 上反而慢 6 倍，且等于换引擎不是换权重 |
| vLLM 128 并发 2000× 吞吐、TTFT 92ms | 数字是真的 | 单流本地用不上，是服务端吞吐优化 |
| `huanglizhuo/QwenASR` 纯 Rust 引擎，46× 实时 | 项目真实；46× 是 Apple M5 Pro 上 NEON/Accelerate/AMX 的成绩 | Windows x64 走无 BLAS 路径，倍数不可预期；但它自带 `--stream`，值得抽半天用你的 20 条语料量一次 |
| 「9.2× 差距」「WER 20.8 vs 18.1」「计算量低 3–4 倍」 | 数字分别来自本仓库的 9.6× 对比和该项目的英文评测 | 别当独立结论用，口径不同 |

它的**方向**（低延迟要换范式）对，但**结论**（换因果塔）只对英文+GPU 成立。你的约束是中文、Windows CPU、单流、800ms。

## 四、方案

| 级别 | 做什么 | 改哪里 | 预期（估算） | 风险 |
|---|---|---|---|---|
| **P0** 量化拆分（0.5 天） | 同一段语音裁 1/2/4/8/12s 各跑一次，线性拟合出「每秒音频成本」和「每 token 成本」；devlog 的 `note_asr` 加 `queue_wait_ms` | `bench.rs` 加 `--bench-lat`，`devlog.rs:502` | 判断瓶颈在 encoder 还是 decoder，决定后面走哪条路 | 无 |
| **P1** 调度层（1–2 天，精度无损） | ① 自适应 tick：`next = clamp(1.15 × 实测, 450ms, 1500ms)`；② 尾巴上限按模型分档（qwen3=6s，sensevoice=12s）；③ 松手优先：`AtomicBool release_pending` + 尾巴 decode 用 `try_lock`，松手后直接跳过；④（可选）静音满 200ms 就 speculative 定稿 decode，500ms 判定过了才采用 | `lib.rs:884-1043`、`lib.rs:101-105`、`lib.rs:1468` | 排队从「几乎必然」降到「约一半」，尾巴成本减半：P50 1.5s→~1.0s，P95 2.5s→~1.3s | 低；灰字刷新变慢（1.1s 一次），需看是否可接受 |
| **P2** 结构层（2–3 天，唯一能进 800ms 的低风险路线） | 就是产品定义 v2 已定的「灰字低延迟 + 定稿高精度」：灰字走 SenseVoice（92ms）或真流式 zipformer（sherpa-onnx 原生 online API），定稿才用 Qwen3；松手时若尾巴已在停顿处定稿，则零 decode | `asr.rs` 加第二识别器，`lib.rs` 灰字/定稿分流，`stream.rs` 吸收替换 | P95 ≤ 800ms 可达；术语保留率 96% 保留在定稿上 | 中；定稿替换灰字时会有一次可见改写，需在 `Streamer` 的 soft 区收口 |
| **P3** 硬件层（1–2 周） | sherpa-onnx 的 Rust crate 只发 CPU 预编译包（`target/sherpa-onnx-prebuilt/…-win-x64-static-MT-Release`），要 GPU 得自建 sherpa-onnx + ONNX Runtime DirectML/CUDA，再用 `SHERPA_ONNX_LIB_DIR` 指过去；同时扫 threads 4→8/12、int8 vs fp32、自建 ORT 开 AVX512-VNNI | 构建链，不动业务代码 | 有独显时 0.6B decoder 通常 3–6×，是唯一能把 Qwen3 压进 800ms 的路 | 高；构建成本大 |

## 五、不建议

- **自训中文因果塔**：这是研究项目（要重训音频塔 + 蒸馏 + 导出 ONNX），不是两周的优化。
- **为了 Qwen3 把预算放宽到 1.5s**：先做 P2，它证明过可行（产品定义 v2 的模型组合一行）。
- **直接把 `qfuxa` 塔接进来**：中文会崩，见第三节。

## 六、验收门

| 阶段 | 门 | 怎么量 |
|---|---|---|
| P1 后 | `release_to_text_ms` P95 ≤ 1.3s（Qwen3 档） | devlog 已有该字段，跑 20 次真实口述 |
| P2 后 | P95 ≤ 800ms，且灰字到气泡 ≤ 300ms | 同上 |
| 每一步 | CER / 术语保留率不退化 | `yanchu.exe --bench bench\corpus` 对比基线（Qwen3: 3.7% / 93%） |
