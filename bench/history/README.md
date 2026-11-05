# 真人语料（历史会话回收）

> **只留在本机。** 这个目录里除了本 README，`NN.wav` 和 `NN.txt` 都是本机录音的
> 产物——音频是你自己的声音，文本是当时的识别原文。两者都被 `.gitignore` 排除，
> 不会进仓库，也不会被推到远程。

这 20 段来自 `devlog/` 里的真实会话——真麦克风、真口音、真停顿，
不是 `bench/corpus` 那批 TTS 合成音。

## 为什么文本还不能直接信

`NN.txt` 里现在放的是**当时的识别结果**，也就是模型的输出，不是人写的真值。
拿它当参考去跑 `--bench`，等于让模型和自己比，CER 会接近 0 —— 看起来很好，毫无意义。

所以这批语料的正确用法是：**先听、再改、后跑**。

## 怎么用

1. 播放 `NN.wav`，把 `NN.txt` 改成你实际说的那句话
   - 标点不必还原，`cer_of` 会忽略标点
   - 数字写阿拉伯数字（30）还是汉字（三十）都行，两边等价
   - 英文术语照你念的写
2. 跑评分：

```powershell
app\src-tauri\target\debug\yanchu.exe --bench bench\history
```

## 素材怎么来的

```powershell
powershell -ExecutionPolicy Bypass -File tools\make-history-corpus.ps1
```

只挑真人会话（排除 e2e 注入的测试音频），按长度降序、按文本去重，
所以这 20 段是「说过的不同内容里，最长的那些」——覆盖长句、口语、中英混说。

重新生成会覆盖已改好的文本，除非加 `-Force` 以外的保护：
脚本默认跳过已存在的 `.txt`，只有 `-Force` 才覆盖。
