# 言出 YanChu

按住一个键说话，松手，一段**整理好的文字**落进你刚才在用的那个输入框。

不是语音转文字工具 —— 转写只是中间产物，交付的是可以直接发出去的文字。

> 版本与变更记录见 [CHANGELOG.md](CHANGELOG.md)。

## 现状：最小原型 v0.2

| 能力 | 状态 |
|---|---|
| 输入模式 | 按住说话（松手结束）；按一下开始，再按一下或 Esc 结束 |
| 触发键 | 默认右 Alt；控制面板里直接按键录入，支持组合键 / 单键 / 长按触发，保存前检查占用 |
| 采集 | 麦克风**常开** + 350ms 预录，按下的瞬间不丢字 |
| 实时显示 | 说话过程中每 450ms 增量识别；**说完的句子定稿后不再变动**，只有尾巴在变 |
| 跟随鼠标的指示器 | 圆环 + 电平环，四种状态：静默 / 有声 / 润色 / 完成 |
| 底部气泡 | 半透明圆角卡片，圆角外**完全透明**（已验证），最多三行自动换行，12px |
| 落字：修饰键守卫 / 剪贴板条件恢复 / 四态结果 | 已实现，独立 spike 5/5 通过 |
| 焦点丢失 | 自动转成剪贴板降级，提示按 Ctrl+V 粘贴 |
| 托盘 | 听写开关 / 润色开关 / 控制面板 / 复制最近一次 / 开机启动；四态图标，左键开面板 |
| 开关状态 | 托盘与面板同一份状态，改完即写 `models\state.json`，重启恢复 |
| 控制面板 | 设置（运行状态 + 开关 + 输入方式 + 气泡外观 + 试润色）/ 模型（识别模型 + 润色模型 / API）/ 历史（搜索、复制、按需 diff）/ 词表（正确写法 / 会被误读成 + 替换规则） |
| 桌面快捷方式 | 桌面 + 开始菜单，`tools\make-shortcut.ps1` |
| 小润色 | 规则层：语气词、重复词、标点、句中断句、中英间距（22 个单测）；**气泡里逐句生效** |
| 词表层 | 词表 + 编辑距离 + **拼音同音**（≥3 字中文术语，`YANCHU_PINYIN=0` 关） |
| 大润色 | 本地一套 2 条规则的提示词（合并碎句、确定性分点、术语近似修正）；深度档只在接云端 API 时开放；提示词外置成文件 |
| 事实项校验 | 数字/人称/否定变化只记录不拦截，进开发日志 |
| 开发期留档 | 每次按住：整段 wav + 每句切片 wav + 全链路 jsonl，`--dev-report` 出分位数 |
| 热词纠正 | 词表 + 编辑距离模糊匹配，误替换 0/6 |
| VAD 断句 | 已接入但**默认关闭** —— 实测负收益，详见 `bench/RESULTS.md` |

### 大润色怎么开

推荐直接在控制面板 →「模型」里新增配置；下面这种手工放 `llm.json` 的方式仍然兼容
（这个目录已被 gitignore，key 不会进仓库）：

```json
{ "url": "http://localhost:11434/v1", "key": "ollama", "model": "qwen2.5:1.5b" }
```

任何 OpenAI 兼容的 `/chat/completions` 都行。没配就只用规则层，不发任何请求。
超时（900ms）或报错时自动退回规则结果，不会让用户等。

### 模型与资源

- **语音模型（SenseVoice）**：启动时加载进言出进程，约 344MB 工作集；只要进程在就一直驻留。
  关控制面板只是隐藏窗口，不会卸载；从托盘「退出」或结束进程才释放。
  托盘里关「语音听写」只是不触发，模型仍然常驻。
- **润色模型（本地 Ollama）**：不是言出加载的，由 Ollama 进程持有。运行期间言出保持
  `keep_alive: 8h`，并每 3 小时续一次，避免长闲置后被卸载。
  **退出言出时会主动卸载**（`keep_alive: 0`），不让 2–3GB 显存继续占着。
  云端 OpenAI 兼容接口没有“卸载”这回事，退出时不做任何事。
- 如果言出被强杀/崩溃，Ollama 可能还会保留模型最多 8 小时；手动释放：
  `yanchu.exe --unload-llm`

热词表同理：`app/src-tauri/models/glossary.txt`（每行一个词；显式纠错写
`会被误读成=正确写法`，例如 `入餐=入参`），控制面板里可以直接按「正确写法 /
会被误读成」两栏添加。固定替换规则放 `models/rules.txt`（每行 `旧=新`，命中就换，跑在热词表之后）。

验收实测（20 段中英混说语料，8 项门槛过 7 项）：

| 指标 | 门槛 | 实测 |
|---|---|---|
| 字错率 CER | ≤ 8% | 5.9% |
| 英文术语保留率 | ≥ 90% | **81%（未达标）** |
| 单句识别延迟 P95 | ≤ 250ms | 146ms |
| 常驻内存 | ≤ 800MB | 336MB |
| 冷启动到可说话 | ≤ 3s | 1.19s |
| 热词误替换率 | ≤ 2% | 0% |
| 松手 → 文字出现 | P95 ≤ 800ms | 416ms |

术语那道坎换 Qwen3-ASR 能到 96%，但识别延迟会从 146ms 涨到 1131ms。
两条路都已实现，用 `YANCHU_ASR_ENGINE` 切换。详细数据与取舍见 [bench/RESULTS.md](bench/RESULTS.md)。

## 目录

```
docs/            产品定义（v1 原件 + v2 现行）
docs/问题清单.md  发现但不当场修的问题，带状态机和判定标准
devlog/          开发模式采集的产物（音频 + 日志，不进仓库）
app/             Tauri v2 + Rust 应用
  src-tauri/src/
    hotkey.rs    全局热键（低层键盘钩子，能吞按键）
    audio.rs     麦克风采集
    asr.rs       识别层（--features asr 才接模型）
    glossary.rs  词表层：词表 + 编辑距离 + 拼音同音
    rules.rs     替换规则层：用户可编辑的 `旧=新` 表（跑在热词表之后）
    polish.rs    小润色规则层
    llm.rs       大润色：本地一套提示词 / 云端深度档 + 契约校验
    pipeline.rs  词表 → 规则 → 大润色，三段中间结果都留着
    history.rs   落字历史：按天落盘，控制面板做逐字红绿对比
    devlog.rs    开发模式采集与 --dev-report
    stream.rs    流式定稿：冻结区/草稿区 + 断点判据
    ui.rs        气泡外观（宽/高/字号）
    bubble.rs    气泡窗口
    insert/      落字链路：修饰键守卫 / 剪贴板 / 注入
    bench.rs     离线验收：CER、术语保留率、延迟
spike/           落字链路的独立验证脚本（可复现产品约束）
bench/           验收语料生成 + 测量
tools/           构建、模型下载、e2e、开发模式启动
```

## 跑起来

编译需要 Rust 工具链 + MSVC 链接器（Visual Studio Build Tools 的「使用 C++ 的桌面开发」）。

**不要直接 `cargo build`**：rustc 找不到 link.exe 会报错。走 `tools\build.bat`，它会先进 vcvars64 再调 cargo。

> 改前端（`app/src/*.html/css/js`）之后也要重新 build：这些资源在编译时被嵌进 exe，
> 只改文件不重编译，运行起来的还是旧界面。

```bat
tools\build.bat                     :: 占位识别器（只说固定文本），只用来验链路
tools\build.bat build --features asr :: 接真实识别（SenseVoice / Qwen3，需先下载模型）
tools\build.bat run --features asr
```

> **占位识别器不是故障**：默认 feature 不接模型，气泡里永远是「占位识别结果 hello world」。
> 真实使用和验收必须带 `--features asr`；控制面板的「识别模型」会直接标红提示「构建未启用 asr」，
> 避免把占位包当成正式包发出去（YC-016）。

网络受限时依赖下载可能报 `CRYPT_E_REVOCATION_OFFLINE`：
`set CARGO_HTTP_CHECK_REVOKE=false` 再 build。

开发期想连音频一起留证据，用 `tools\dev-run.bat`（等价于设 `YANCHU_DEV_LOG=1` 再启动），
见下面「开发模式：每一句都留档」。

界面调试开关：

```powershell
$env:YANCHU_OPEN_PANEL = "1"      # 启动即打开控制面板
$env:YANCHU_PANEL_TAB = "models"  # 打开面板时直接切到设置 / 模型 / 历史 / 词表
$env:YANCHU_THEME = "dark"        # 强制深色（light 反向），不用改系统设置
$env:YANCHU_REDUCED_MOTION = "1"  # 手动关掉位移/循环动画
$env:YANCHU_NOTIFY_TEST = "1"     # 启动 3 秒后弹一条测试通知
```

> Windows 的 Toast 需要带 AppUserModelID 的快捷方式；**开发期直接跑 exe 可能不弹**，
> 安装包（NSIS）里才正常。通知失败只影响提示，不影响落字。

识别引擎可切换（两个模型都在仓库外按需下载）。**正常使用在控制面板 →「模型」里切换**；
环境变量仍然优先，方便命令行验收：

```powershell
$env:YANCHU_ASR_ENGINE = "sensevoice"   # 默认：P50 92ms / CER 6.6% / 术语 70%
$env:YANCHU_ASR_ENGINE = "qwen3"        # 更准但慢：P50 946ms / CER 3.7% / 术语 93%
```

模型下载到 `app\src-tauri\models\`：

```powershell
powershell -ExecutionPolicy Bypass -File tools\download-models.ps1
```

按住 **右 Alt** 说话，松手落字。触发键、组合键和输入模式在控制面板
「设置 → 输入方式」里改；默认右 Alt，老用户升级后不用重设。日志里能看到每步耗时。

## 触发键与输入模式

**两种模式**（控制面板 → 设置 → 输入方式）：

| 模式 | 怎么用 | 什么时候结束 |
|---|---|---|
| 按住说话 | 按住触发键说，松手 | 松开触发键 |
| 按一下开始 / 停止 | 按一下开始录音 | 再按一下触发键，或按 Esc |

「按一下」模式适合长段口述：不用一直按着键。松手后录音继续，气泡里实时显示识别结果；
结束时如果原来的输入框已经失去焦点，文字会自动复制到剪贴板，按 <kbd>Ctrl</kbd>+<kbd>V</kbd> 粘贴即可。

**触发键**：点「触发键」右侧按钮，焦点进入录入状态后**直接在窗口里按下想要的组合键**。
录键期间全局触发会临时挂起，不会误启动录音。

- 组合键：`Ctrl+Shift+Space`、`Ctrl+Alt+Q` 这类「修饰键 + 一个普通键」；
- 单键：`F1`–`F24`、`CapsLock`、`NumLock`、`ScrollLock`、`Pause`，或沿用默认的右 Alt；
- 长按触发：可以要求按住 200–500ms 才真正开始，避免误触；设为「立即触发」最跟手；
  没到时间就松开会被忽略；
- 触发键被应用接管：单键（如 CapsLock）会被完全吞掉，组合键只吞最后一个普通键，
  Ctrl / Shift / Alt 仍然留给目标应用；
- 保存前会检查：Esc 留给结束录音、单个字母 / 数字会吞掉打字、Win 组合键和常用编辑
  快捷键（Ctrl+C / Ctrl+V 等）会被拒绝；最后再用系统 `RegisterHotKey` 探测一次是否已被别的程序占用；
- 配置存在 `models/trigger.json`，换键立刻生效，不需要重启。

## 模型切换

控制面板新增「模型」页，分两部分：

**语音识别模型**

- **SenseVoice Small**：默认，实时感最好。
- **Qwen3-ASR 0.6B**：英文术语、中文同音词更准，但单段要多等约 0.8–1.1 秒。

点「切换」会重新加载模型并把选择写进 `models/asr.json`，不需要重启应用；
如果正在录音，会先结束当前这一轮。两个模型目录都在 `app/src-tauri/models/`，
状态会显示「已安装 / 未安装 / 自定义目录」。

**润色模型 / API**

可以保存多套配置，点「使用」立即生效：

- **本地 Ollama**：填模型名即可，例如 `qwen3.5:2b`、`qwen3.5:0.8b`；
  点「刷新本地 Ollama 模型」会读取 `/api/tags`，模型名可以直接下拉选。
- **OpenAI 兼容 API**：填地址、模型名和 API Key，例如 DeepSeek / OpenAI / 自建网关。

每套配置都支持「测试连接」（发一个极小请求，返回耗时和回复）、编辑、删除；
列表存在 `models/polish.json`，当前启用项会同时写进 `models/llm.json`，
所以 `llm.rs` 的读取路径不变，切模型也不需要重启。

> 识别模型对比数据见 [bench/ASR-COMPARISON.md](bench/ASR-COMPARISON.md)：
> 20 段同语料上 SenseVoice CER 6.6% / 术语 70% / P50 92ms，
> Qwen3-ASR 3.7% / 93% / P50 946ms。语料是 TTS，结论以真实录音复测为准。

## 两级润色

**小润色（确定性，跟着气泡走）**
句子一闭合就过「词表 + 规则」两层，气泡里看到的就是润色后的样子，不再是一堆「嗯、那个」。
幂等、可单测、每次不到 1ms，因为它的预算是「句末 → 气泡更新 ≤ 300ms」。
它管的是：语气词、重复词、标点整理、句中断句修复、承接词前的句号降级、中英间距、同音术语。

**断句走带重叠区的滑动窗口，不是按停顿硬切**（`stream.rs`）

这是「一句话停顿三次就变成三个句号」的解法：停顿只让窗口前进，不直接写句号。

```
        ┌──── 20% ────┬──────── 60% ────────┬──── 20% ────┐
窗口 i  │  与 i-1 共用  │      本窗口独占       │  与 i+1 共用  │
        └─────────────┴─────────────────────┴─────────────┘
```

- **冻结区**（滑过去的 60%）：永不改变，屏幕上已定型的字靠它稳定；
- **草稿区**（最后 20%）：断点还没定，标点可以再判一次；
- 一段说完先放一个临时逗号，等右文（下一段开头或尾巴的前几个字）出现再定标点。

判据（`stream::decide_break`，按顺序命中）：

| 情况 | 结论 |
|---|---|
| 静音 ≥1.2 秒，且左文 ≥8 字 | 句号（人真的停下来了，这句也说完了） |
| 一小段攒到 ≥26 字还没标点 | 逗号（再不给读者喘不过气） |
| 右文以 然后/所以/但是/如果… 开头 | 逗号（在接着说） |
| 左文正好是 好的/行/对/没问题… | 逗号（话头） |
| 左文不到 8 字 | **不加**（「我们可以 / 测试一下」这种是换气，不是分句） |
| 离上一个标点不到 12 字 | **不加**（防止逗号连珠） |
| 其余 | 逗号 |

默认是**不加**：口语里三五个字一停太常见，每个停顿都给逗号，读起来就是一堆碎片。
拿一段真实停顿很多的口述回放（11 个片段、约 60 字），从原来插 10 个逗号降到 4–6 个。

一直不停顿时，尾巴超过 12 秒会强制找一个低能量点切一刀——否则每 450ms 都要重识别整段，
越说越慢，气泡会彻底跟不上。

**气泡大小可调**：控制面板 →「设置 → 气泡外观」，宽（320–900）、高（80–360）、字号（9–16）都能改，
存 `models/ui.json`，立刻生效。默认 440×100 / 10.5px，大约三行。

**大润色（概率性，落字前）**
本地一套，接云端 API 才多一档：

| | 本地标准（默认） | 云端深度（需配置 API 模型） |
|---|---|---|
| 能做什么 | 合并被停顿切碎的短句、按语义分段、整理编号清单；不换词、不删信息 | 可以换词、可以改写和分点 |
| 提示词 | 短句（<100 字）用 2 条保守规则；长文（≥100 字）改用「重度整理」版：可压缩、要求分段、保持列举条目数。两份都在 `prompts/` 里 | 完整规则 + 示例 |
| 预算 | 2.5–8 秒（按字数） | 4–12 秒 |
| 什么时候用 | 默认 | 托盘里手动开 |
| 契约 | 实词集合基本不变；长度比 0.5–1.5；新增汉字 ≤35%；最多 8 段；英文术语允许 2 个字母以内的识别修正 | 允许改写；长度比放宽到 0.45–2.0；分点上限 15 行 |
| 共同红线 | 英文术语不许丢、不许加前言、不许编内容 | 同左 |
| 失败 | 静默回退规则层 | 回退规则层（提示功能见 [问题清单](docs/问题清单.md) YC-002） |

提示词放在 `app/src-tauri/prompts/`（`local.txt`、`api-deep.txt`），改完不用重编译；
`models/prompts/` 或 `YANCHU_PROMPT_LOCAL` 可以临时覆盖，回放台就是靠这个做 A/B 的。

深度档的输出会被**确定性收口**：模型爱写的 `-`、`*`、`#`、`**` 全部规整成「1. 2. 3.」编号，
所以进输入框的永远是规整列表，不是一堆 markdown 记号。

**事实项校验**：两条通道都会算一遍「数字 / 人称 / 否定词」有没有变，结果只写日志、不拦截 ——
先攒真实数据再决定哪些该拦（YC-001、YC-008）。

> 已知问题与待办都记在 [docs/问题清单.md](docs/问题清单.md)：状态机是
> `open → fixing → verify → done / close`，每条必须有「复现」和「判定标准」。

## 开发模式：每一句都留档

> 控制台窗口已经去掉：开发构建也走窗口子系统，不会在任务栏多一个日志框。
> 运行日志在控制面板 **设置 → 开发者模式 · 日志**（内存最近 400 行）；
> 打开 `YANCHU_DEV_LOG=1` 时同一份日志仍写 `devlog/app.log`。

调润色时不缺观点，缺的是**证据**：这句话识别成了什么、规则层动了哪里、
大润色是没用上还是被契约挡了、那 600ms 花在识别还是模型上。
所以开发期把每一次「按住说话」整段落盘，包含音频：

```bat
tools\dev-run.bat
:: 等价于
set YANCHU_DEV_LOG=1
tools\build.bat run --features asr
```

产物落在仓库根的 `devlog/`（可写 `YANCHU_DEV_LOG_DIR` 换位置，已 gitignore）：

```
devlog/
  app.log                     本次运行的完整日志（和 stderr 同一份）
  events.jsonl                每次按住一行，做统计/导出的入口
  report.md, sessions.csv     --dev-report 的产物
  sessions/2026-11-02/20261102-162154-500/
    session.json              这一次按住的全部事件
    audio.wav                 整段录音，16kHz 单声道
    seg-01.wav                第 1 个定稿句子的音频切片
```

`session.json` 里记的东西，按「排查一个问题需要什么」来放：

| 字段 | 用来回答 |
|---|---|
| `segments[]`（含 `seg-NN.wav` + 该句原文） | 这一句到底是哪段音频、单独重跑识别会不会更好 |
| `asr_calls[]`（partial / commit / final + 耗时） | 实时预览稳不稳、定稿慢在哪一次调用 |
| `stages.{raw, display, corrected, ruled, polished}` | 原文 / 气泡最后一帧 / 三层结果分别是什么；热词命中了什么（`fixes`） |
| `stages.depth` + `stages.llm.{outcome, ms, reject_reason, raw, warnings}` | 走的哪一档、是用了还是回退了、被哪条契约挡的、模型原话是什么、事实项有没有变 |
| `press_to_release_ms` / `release_to_text_ms` / `polish_ms` | 延迟预算花在哪个环节 |
| `target.{process, title}` / `insert` | 落在哪个应用、成功还是降级 |
| `env.*` | 识别引擎、润色模型、VAD、深度档开没开 |

一句一条的汇总 + 分位数 + 改动样例：

```powershell
app\src-tauri\target\debug\yanchu.exe --dev-report            # 默认读 devlog\
app\src-tauri\target\debug\yanchu.exe --dev-report D:\tmp\log
```

报告会直接写出 `report.md`（人看）和 `sessions.csv`（丢进 Excel / pandas 看），
关键几行：**档位分布**、**气泡→落字一致率**（产品定义里的硬指标）、
**事实项告警分布**（数字/人称/否定被改的次数）、**契约复检**（用运行时同一套规则复算）。

单句调试不用说话：

```powershell
# 只跑润色（不碰音频）
yanchu.exe --polish-test "嗯那个，把 time out 从三十秒改成六十秒"
yanchu.exe --polish-test "第一步跑迁移，第二步重启服务" --deep

# 流式自检：拿一段 wav 走「识别 → 断点判定 → 定稿」，不开窗口、不抢焦点
yanchu.exe --stream-test bench\corpus\01.wav

# 当前识别 / 润色配置、切换识别模型、测试当前润色接口
yanchu.exe --models-json
yanchu.exe --set-asr qwen3
yanchu.exe --llm-ping
```

`--stream-test` 是调标点判据的主力工具：会逐段打印识别结果、每个断点的
`静音 / 左文 / 右文 / 判定`，最后给定稿。改判据不用对着麦克风反复说同一句。

深度档也可以直接开一整程：`set YANCHU_DEEP=1`。

> 隐私边界：采集**默认关闭**，产品版不写任何音频。打开后音频只落在本机，
> 但确实是落盘了 —— 这是调试开关，别在别人的机器上随手开。

## 自动化验证

```powershell
# 落字链路（独立于应用）：6 个用例，真实前台窗口
powershell -ExecutionPolicy Bypass -File spike\insert-spike.ps1

# 端到端：默认「按住」模式，合成按住右 Alt，用 wav 冒充麦克风，断言文字落进目标窗口
powershell -ExecutionPolicy Bypass -File tools\e2e-test.ps1

# 端到端：临时切到「按一下」模式，按一下开始、再按一下结束（跑完恢复原设置）
powershell -ExecutionPolicy Bypass -File tools\e2e-test.ps1 -Mode toggle

# 端到端：「按一下」模式用 Esc 结束
powershell -ExecutionPolicy Bypass -File tools\e2e-test.ps1 -Mode toggle -StopWithEsc

# 触发键录入 / 保存：自动点设置页、按 Ctrl+Shift+Space、断言 trigger.json
powershell -ExecutionPolicy Bypass -File tools\trigger-capture-test.ps1

# 组合键触发：Ctrl+Shift+Space；加 -LongPressMs 350 验证长按触发
powershell -ExecutionPolicy Bypass -File tools\trigger-combo-test.ps1 -LongPressMs 350

# 焦点丢失：按一下模式录音中切到别的窗口，按 Esc 结束，断言只进剪贴板
powershell -ExecutionPolicy Bypass -File tools\focus-loss-test.ps1

# 词表新增方向：断言「正确写法 / 会被误读成」保存成「误读=正确」
powershell -ExecutionPolicy Bypass -File tools\glossary-add-test.ps1

# 控制面板截图（设置 / 历史 / 词表）
powershell -ExecutionPolicy Bypass -File tools\panel-shot.ps1 -Tab glossary

# 界面：关闭面板→收进托盘→再打开；气泡/指示器能否显示
powershell -ExecutionPolicy Bypass -File tools\ui-smoke.ps1

# 开关：持久化、深度档置灰（会临时改写 models\state.json）
powershell -ExecutionPolicy Bypass -File tools\state-test.ps1

# 暂停提示：听写关掉时按右 Alt，气泡出现「听写已暂停」
powershell -ExecutionPolicy Bypass -File tools\pause-test.ps1

# 托盘左键：展开隐藏图标→点「言出」→断言面板出现
powershell -ExecutionPolicy Bypass -File tools\tray-click-test.ps1

# 截图：控制面板 / 录音中 / 落字完成（产物在 tools\.shot-*.png）
powershell -ExecutionPolicy Bypass -File tools\ui-visual.ps1
```

## 验收

```powershell
# 1. 生成 20 段中英混说语料（Windows TTS，中英分别用对应音色合成后拼接）
powershell -ExecutionPolicy Bypass -File bench\make-corpus.ps1

# 2. 离线跑一遍，出 CER / 术语保留率 / 延迟分布
tools\build.bat run --features asr -- --bench ..\..\bench\corpus
```

门槛见 `docs/产品定义-v2.md` 第十节：字错率 ≤8%、英文术语保留率 ≥90%、单句 P95 ≤250ms。

> 注意：TTS 合成的语料比真人干净，CER 会偏乐观。它只能证明「管线通、延迟在什么量级」，
> 真实准确率必须用真人录音另测。

## 落字链路的验证

`spike\insert-spike.ps1` 会起一个真实前台窗口，把字真的打进去再读回来断言：

```powershell
powershell -ExecutionPolicy Bypass -File spike\insert-spike.ps1
```

覆盖五个用例：基础粘贴与剪贴板恢复、修饰键一直按住时守卫超时放弃、
修饰键延迟抬起时守卫等待放行、剪贴板被用户改写时不还原、落点在按下瞬间锁定。当前 5/5 通过。

## 已知边界

- 只在 Windows 上验证
- 触发键默认右 Alt，可在设置里换成组合键 / F 键 / CapsLock；除右 Alt 外的单个
  Ctrl / Shift / Alt 和 Win 组合键会被拒绝，避免吞掉正常打字或撞系统快捷键
- 识别默认不走 VAD 断句（实测负收益）
- 气泡显示瞬间 WebView2 会抢一次焦点，已用「抢到就还回去」兜住，但不是零成本
- 麦克风采集只用 wav 冒充验证过，真人录音尚未跑
