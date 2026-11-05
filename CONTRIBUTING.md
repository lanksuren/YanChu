# 参与开发

先谢谢你看这个项目。下面这些不是规矩，是省时间的路径——按着走能少踩几个已知的坑。

## 动大改之前先开个 issue

修 bug、补文档、改 UI 细节，直接提 PR 就行。**要改行为、加依赖、动落字链路或隐私边界**，先开 issue 说一句，免得写完发现方向不对。

## 环境

- Windows 10/11（只有 Windows 验证过）
- Rust 工具链 + MSVC 链接器（Visual Studio Build Tools 的「使用 C++ 的桌面开发」）
- WebView2 运行时（Win11 自带）

**不要直接 `cargo build`**：rustc 找不到 `link.exe` 会报错。走 `tools\build.bat`，它会先进 vcvars64：

```powershell
tools\build.bat                       # 占位识别器，只验链路
tools\build.bat build --features asr  # 接真实识别，需先下载模型
tools\build.bat run --features asr
```

模型在仓库外按需下载：`tools\download-models.ps1`。

前端是纯 HTML/CSS/JS，**没有构建步骤**，但资源在编译时被嵌进 exe——改完 `app/src/` 里的文件必须重新 build，否则跑的还是旧界面。

## 改完先跑这些

```powershell
# Rust 单测：188 项，几秒钟
cargo test --lib --manifest-path app\src-tauri\Cargo.toml

# 编译检查
cargo check --lib --manifest-path app\src-tauri\Cargo.toml
```

动了落字链路（`insert/`）或气泡显示，再补跑：

```powershell
powershell -ExecutionPolicy Bypass -File spike\insert-spike.ps1   # 5/5
powershell -ExecutionPolicy Bypass -File tools\e2e-test.ps1
powershell -ExecutionPolicy Bypass -File tools\ui-smoke.ps1
```

## 隐私红线（最重要的一条）

这个项目的用户会**对着麦克风说话**，所以仓库里绝不出现：

- 任何音频（`*.wav` / `*.mp3` / `*.m4a` …）
- `devlog/`（开发模式采集：整段 wav + 每句切片 + 全链路 jsonl）
- `bench/history/` 里的 `NN.txt`（真实会话的识别原文）
- `bench/replay/` 的真人语料和回放报告（正文含「原文 / 规则层 / 成品」）

本地启用钩子，提交前自动拦：

```powershell
git config core.hooksPath .githooks
```

CI 里还有一道 `no-audio.yml`，PR 带了这些文件会直接失败。要自己造语料测试，用
`bench\make-corpus.ps1`（TTS 合成）或 `tools\record-corpus.ps1`（本地录，产物已在 gitignore 里）。

## 提交信息

沿用现有风格：`type(scope): 中文摘要`，正文说清「为什么」和「验证了什么」。

```
feat(ui): 定稿只做颜色过渡，气泡与指示器支持配色皮肤
fix(insert): 剪贴板还原过早导致显示已插入但输入框没有
perf(stream): 流式识别调度按模型分档，松手延迟 P95 1172→970ms
docs: 新增优化清单（OP-001..007）
```

常用 type：`feat` / `fix` / `perf` / `refactor` / `docs` / `test` / `chore`。

## PR

- 一个 PR 只做一件事，描述里写清楚：**问题是什么、怎么改的、怎么验证的**
- 换行符统一 LF（`.gitattributes` 已配，别改 `core.autocrlf` 制造整文件 diff）
- 提交保持线性历史，合并时用 squash
- 新增依赖请在 PR 里说明理由和体积影响
- 代码风格：Rust 跑 `cargo fmt`；前端保持无框架、无构建

## 许可

提交即表示同意以本仓库的 [MIT License](LICENSE) 授权。
