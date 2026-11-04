# Qwen3-ASR 0.6B 延迟优化方案（含实测）

> 日期：2026-11-04（P0/P1 已落地）
> 背景：`models/asr.json` 切到 `qwen3` 后体感比 SenseVoice Small 慢。
> 机器：Intel Core Ultra 5 338H（12 核 12 线程）+ Arc B370 核显（24G 共享显存），Windows。
> 口径：文中所有数字都是本机实测，不是估算；复现命令见文末。

## 一、结论先说

1. **慢的不是「离线编码器做流式」这件事**。既有管线已经把定稿段固定下来，只重算尾巴，计算量不随流增长。
2. **Qwen3 单次解码的真实成本**：`143ms + 69ms × 音频秒数`（4 线程）。其中 encoder 占 41ms/秒，decoder 约 10ms/字，固定开销 ~160ms/次。
3. **松手 → 出字 的构成变了**：ASR 定稿只占 ~300ms，**大润色（LLM）占 500–1100ms**。Qwen3 时代 800ms 预算的瓶颈已经从 ASR 转移到润色。
4. 能靠调度拿回来的那一部分已经拿了：**P95 1172 → 970ms**，定稿解码从 205–511ms 收敛到 185–201ms。
5. 剩下的路只有三条：**增量大润色**（收益最大，不属 ASR）、**GPU（DirectML）**、或者**回 SenseVoice 上屏 + Qwen3 后台定稿**。

## 二、P0：把一次解码拆成三块

`yanchu.exe --bench-lat <wav>`（新增，见 `app/src-tauri/src/bench_lat.rs`）。
同一段音频取 1/2/4/6/8/12/16s 前缀，分别解「真人说话」和「等长静音」：
静音那次的 encoder 成本一样、decoder 只出 1 个 token，于是两组的斜率差就是 decoder。

| 音频长度 | 说话 | 静音 | 差值 | 字数 |
|---:|---:|---:|---:|---:|
| 1s | 239ms | 215ms | 24ms | 3 |
| 2s | 294ms | 250ms | 44ms | 3 |
| 4s | 417ms | 350ms | 67ms | 9 |
| 6s | 517ms | 430ms | 87ms | 17 |
| 8s | 671ms | 494ms | 177ms | 20 |
| 12s | 1009ms | 671ms | 338ms | 32 |
| 16s | 1258ms | 837ms | 421ms | 41 |

拟合：

```text
说话: 143ms + 69ms/秒
静音: 174ms + 41ms/秒   ← encoder
```

**判读**：encoder 是斜率的大头（41/69 ≈ 59%），固定开销 143–174ms 占短句成本的三分之一。
所以「缩短要解码的音频」和「减少解码次数」是两个独立的钱袋，都得管。

### 线程数：4 是对的，别加

| threads | 截距 | 每秒斜率 |
|---:|---:|---:|
| 2 | 187ms | 91ms |
| **4** | **170ms** | **71ms** |
| 6 | 454ms | 55ms |
| 8 | 703ms | 79ms |

4 线程以上截距暴涨（混合架构把线程甩到 E 核，每个算子的 barrier 等最慢那根）。
`default_threads()` 的 `min(核数, 4)` 已经是最优，不用改；`YANCHU_ASR_THREADS` 别乱调。

## 三、P1：调度层（已落地，commit `perf: 流式识别调度按模型分档`）

三处改动：

1. **刷新间隔自适应**：`间隔 = clamp(1.15 × 上一拍解码耗时, 地板, 1500ms)`。
   Qwen3 地板 700ms（450ms 时只是多烧 25% CPU，延迟一点没变），SenseVoice 仍 450ms。
2. **尾巴上限按模型分档**：Qwen3 6s（12s 尾巴一次要 ~1s），SenseVoice 保持 12s。
3. **尾巴没语音就不解码**：切点落在静音末尾，刚定稿完那 0.2–0.5s 的尾巴
   以前会白跑一次 ~200ms 解码，拿到「。」再被丢掉。

### 端到端效果（`tools/lat-bench.ps1`，03.wav 11.4s，各 4 次）

| | 改前 | 改后 |
|---|---:|---:|
| 松手→出字 P50 | 988ms | **907ms** |
| 松手→出字 P95 | 1172ms | **970ms** |
| 定稿解码 | 205–511ms | **185–201ms** |
| 识别总耗时（11.4s 音频） | 6.8s | 7.1s |
| 大润色 | 515ms | 544ms |

识别总耗时没降反升一点，是因为定稿解码变短了、整体更稳 —— 用 CPU 换确定性，值得。

### 长口述（34s 拼接口述，2 次）

| | 改前 | 改后 |
|---|---:|---:|
| 松手→出字 P50 | 1600ms | 1698ms |
| 大润色 | 1094ms | 1118ms |
| 识别总耗时 | 19.5s | 22.2s |

长口述**没有变好，略差一点**（在噪声范围内，主要被 1.1s 的大润色淹没）。
6s 尾巴上限让它多切了两刀（9 → 11 次提交），换来的是单次解码有界。
如果哪天觉得长口述的字跳得厉害，`YANCHU_TAIL_SECS=12` 就回到旧行为。

## 四、钱花在哪：一次「松手」的账

11.4s 的一次口述（P50）：

```text
松手 → 定稿解码 185-201ms → 大润色 544ms → 写入 25ms → 出字
        ^^^^^^^^^^^^^^^^      ^^^^^^^^^^
        31%                   55%
```

34s 的长口述：定稿 386ms + 大润色 1118ms + 写入 25ms。

**ASR 已经不是主要矛盾了。** 继续抠 ASR 调度最多再省几十毫秒；
要进 800ms，得动大润色（产品定义 v2 里写的「增量大润色」）——
每句闭合就把该句+上文润好缓存，松手时只润最后一句，500ms 能降到 100–200ms。

## 五、还剩哪些路

| 方案 | 预期 | 代价 | 建议 |
|---|---|---|---|
| **增量大润色**（缓存每句润色结果，松手只跑最后一句） | 松手延迟 −350～900ms，直接进 800ms | 产品定义 v2 已经写好，属实现欠账 | **优先做**，比 ASR 优化的收益大一个量级 |
| **GPU（DirectML / Arc B370）** | 单次解码 3–5×，长口述的单次 1s → 0.2–0.3s；CPU 也让出来给 LLM | 要自建 sherpa-onnx + ONNX Runtime DirectML（Rust crate 只发 CPU 预编译包，见 `target/sherpa-onnx-prebuilt/…win-x64-static-MT-Release`），再用 `SHERPA_ONNX_LIB_DIR` 指过去；1–2 周 | 长口述/多任务场景值得试 |
| **双模型：SenseVoice 出灰字 + Qwen3 定稿** | 灰字延迟回到 90ms 级；定稿仍要等 Qwen3 | 定稿替换灰字会有一次可见改写；两套模型常驻内存 | 只有在「灰字要跟上说话」是硬指标时才做 |
| **换因果流式塔 `qfuxa/qwen3-asr-0.6b-streaming`** | 英文+GPU 下 RTF 0.16 | **英文专用**（FLEURS 中文 CER 11.4→85.7）；CPU 只有 fp32，RTF 0.82，比现在慢 6 倍；Python 运行时 | **不要用**，对中文是负优化 |
| 换纯 Rust 引擎 `huanglizhuo/QwenASR` | Apple Silicon 上 46× 实时 | 优化针对 NEON/Accelerate/AMX，Windows 走无 BLAS 路径，倍数不可预期 | Windows 上收益未验证，别抱期待 |

## 六、复现命令

```powershell
# 拆 encoder / decoder 成本（7 档长度 × 说话/静音，约 1 分钟）
app\src-tauri\target\debug\yanchu.exe --bench-lat bench\corpus\03.wav

# 端到端「松手 → 出字」分布（起假目标窗口 + wav 冒充麦克风，跑 N 次）
powershell -ExecutionPolicy Bypass -File tools\lat-bench.ps1 -Runs 4
powershell -ExecutionPolicy Bypass -File tools\lat-bench.ps1 -Runs 2 -Wav tools\.lat-long.wav

# 扫调度参数（不重编译）
$env:YANCHU_TICK_MS = "700"; $env:YANCHU_TAIL_SECS = "6"
```

> `tools\.lat-long.wav`（34s）是本机拼出来的一次性素材（corpus 的 03+01+02 直接接 PCM），
> 没进仓库；要重做就用同样的办法拼一段 30s 以上的口述。

## 七、注意

- 语料是 Windows TTS 合成的，比真人干净；真人录音下 decoder 的 token 数会变，`ms/字` 要重测。
- 大润色跑的是本地 Ollama（qwen3.5:2b），耗时随文本长度线性涨：11.4s 口述 544ms，34s 口述 1118ms。
- 端到端样本量小（各 2–4 次），P95 的置信区间不宽；要更硬的结论就加到 10 次以上。
