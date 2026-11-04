//! 识别层。默认是占位实现，`--features asr` 时接 SenseVoice / Qwen3-ASR。
//!
//! 本地模型通过 `models/asr.json` 选择：
//! ```json
//! { "engine": "sensevoice", "model_dir": null, "threads": 4 }
//! ```
//! `engine` 目前支持 `sensevoice` 和 `qwen3`；`model_dir` 留空时用内置目录，
//! 也可以指向自己的模型目录（相对路径按 `models/` 解析）。
//! 环境变量 `YANCHU_ASR_ENGINE` / `YANCHU_ASR_MODEL_DIR` / `YANCHU_ASR_THREADS`
//! 仍然优先，方便命令行验收切模型。

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// SenseVoice int8 模型目录名
const SENSE_VOICE_DIR: &str = "sherpa-onnx-sense-voice-zh-en-ja-ko-yue-2024-07-17";
/// Qwen3-ASR 0.6B int8 模型目录名
const QWEN3_DIR: &str = "sherpa-onnx-qwen3-asr-0.6B-int8-2026-03-25";

/// 识别模型配置文件名（和 state.json / ui.json 放在同一个 models 目录）
pub const ASR_FILE: &str = "asr.json";

pub fn model_root() -> PathBuf {
    if let Ok(dir) = std::env::var("YANCHU_MODEL_DIR") {
        return PathBuf::from(dir);
    }
    // 开发和调试时直接用仓库里的 models/
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("models")
}

pub fn sense_voice_dir() -> PathBuf {
    model_root().join(SENSE_VOICE_DIR)
}

pub fn qwen3_dir() -> PathBuf {
    model_root().join(QWEN3_DIR)
}

fn default_engine() -> String {
    "sensevoice".to_string()
}

/// 本地识别模型配置。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AsrConfig {
    #[serde(default = "default_engine")]
    pub engine: String,
    #[serde(default)]
    pub model_dir: Option<String>,
    #[serde(default)]
    pub threads: Option<i32>,
}

impl Default for AsrConfig {
    fn default() -> Self {
        Self {
            engine: default_engine(),
            model_dir: None,
            threads: None,
        }
    }
}

impl AsrConfig {
    pub fn load(root: &Path) -> Self {
        let path = root.join(ASR_FILE);
        let mut cfg = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str::<AsrConfig>(&text).ok())
            .unwrap_or_default();

        // 环境变量优先，命令行验收/脚本可以临时覆盖
        if let Ok(v) = std::env::var("YANCHU_ASR_ENGINE") {
            if !v.trim().is_empty() {
                cfg.engine = v;
            }
        }
        if let Ok(v) = std::env::var("YANCHU_ASR_MODEL_DIR") {
            if !v.trim().is_empty() {
                cfg.model_dir = Some(v);
            }
        }
        if let Ok(v) = std::env::var("YANCHU_ASR_THREADS") {
            if let Ok(n) = v.parse::<i32>() {
                cfg.threads = Some(n);
            }
        }
        cfg.normalized()
    }

    pub fn save(&self, root: &Path) -> anyhow::Result<()> {
        std::fs::create_dir_all(root)?;
        let body = serde_json::to_string_pretty(&self.clone().normalized())?;
        std::fs::write(root.join(ASR_FILE), body)?;
        Ok(())
    }

    pub fn normalized(mut self) -> Self {
        let engine = self.engine.trim().to_ascii_lowercase();
        self.engine = if matches!(engine.as_str(), "qwen3" | "qwen3-asr" | "qwen3_asr") {
            "qwen3".to_string()
        } else {
            "sensevoice".to_string()
        };
        self.model_dir = self
            .model_dir
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        self.threads = self.threads.map(|n| n.clamp(1, 16));
        self
    }

    pub fn is_qwen3(&self) -> bool {
        self.engine == "qwen3"
    }

    pub fn label(&self) -> &'static str {
        if self.is_qwen3() {
            "Qwen3-ASR 0.6B"
        } else {
            "SenseVoice Small"
        }
    }

    pub fn resolved_dir(&self) -> PathBuf {
        if let Some(dir) = &self.model_dir {
            let path = PathBuf::from(dir);
            if path.is_absolute() {
                return path;
            }
            return model_root().join(path);
        }
        if self.is_qwen3() {
            qwen3_dir()
        } else {
            sense_voice_dir()
        }
    }

    pub fn available(&self) -> bool {
        let dir = self.resolved_dir();
        if self.is_qwen3() {
            [
                "conv_frontend.onnx",
                "encoder.int8.onnx",
                "decoder.int8.onnx",
            ]
            .iter()
            .all(|f| dir.join(f).is_file())
                && dir.join("tokenizer").is_dir()
        } else {
            dir.join("model.int8.onnx").is_file() && dir.join("tokens.txt").is_file()
        }
    }

    pub fn thread_count(&self) -> i32 {
        self.threads.unwrap_or_else(default_threads)
    }
}

/// 控制面板用的模型列表项。
#[derive(Debug, Clone, Serialize)]
pub struct AsrModelInfo {
    pub engine: String,
    pub label: String,
    pub dir: String,
    pub available: bool,
    pub active: bool,
    pub note: String,
}

/// 已支持的本地识别模型（两个内置引擎 + 可选自定义目录）。
pub fn available_models(active: &AsrConfig) -> Vec<AsrModelInfo> {
    let active = active.clone().normalized();
    ["sensevoice", "qwen3"]
        .iter()
        .map(|engine| {
            let cfg = AsrConfig {
                engine: engine.to_string(),
                model_dir: if active.engine == *engine {
                    active.model_dir.clone()
                } else {
                    None
                },
                threads: active.threads,
            }
            .normalized();
            let dir = cfg.resolved_dir();
            let available = cfg.available();
            let note = if !available {
                "缺少模型文件".to_string()
            } else if cfg.model_dir.is_some() {
                "自定义目录".to_string()
            } else {
                "已安装".to_string()
            };
            AsrModelInfo {
                engine: cfg.engine.clone(),
                label: cfg.label().to_string(),
                dir: dir.display().to_string(),
                available,
                active: cfg.engine == active.engine,
                note,
            }
        })
        .collect()
}

// ---------------------------------------------------------------- 占位实现

#[cfg(not(feature = "asr"))]
pub struct Transcriber;

#[cfg(not(feature = "asr"))]
impl Transcriber {
    pub fn load(_cfg: &AsrConfig) -> anyhow::Result<Self> {
        log::warn!("当前是占位识别器（未启用 asr feature），返回固定文本");
        Ok(Self)
    }

    pub fn transcribe(&mut self, _samples: &[f32]) -> anyhow::Result<String> {
        Ok("占位识别结果 hello world".to_string())
    }

    pub fn transcribe_segments(&mut self, _samples: &[f32]) -> anyhow::Result<Vec<String>> {
        Ok(vec!["占位识别结果 hello world".to_string()])
    }
}

// ---------------------------------------------------------------- SenseVoice / Qwen3

#[cfg(feature = "asr")]
pub struct Transcriber {
    recognizer: sherpa_onnx::OfflineRecognizer,
    /// VAD 只负责切句；模型缺失时退化成整段识别
    vad: Option<sherpa_onnx::VoiceActivityDetector>,
    #[allow(dead_code)]
    threads: i32,
}

fn default_threads() -> i32 {
    std::thread::available_parallelism()
        .map(|n| n.get().min(4) as i32)
        .unwrap_or(2)
}

#[cfg(feature = "asr")]
impl Transcriber {
    pub fn load(cfg: &AsrConfig) -> anyhow::Result<Self> {
        let cfg = cfg.clone().normalized();
        let dir = cfg.resolved_dir();
        let threads = cfg.thread_count();

        let (recognizer, threads) = if cfg.is_qwen3() {
            Self::load_qwen3(&dir, threads)?
        } else {
            Self::load_sense_voice(&dir, threads)?
        };

        Ok(Self {
            recognizer,
            vad: Self::load_vad(),
            threads,
        })
    }

    fn load_qwen3(
        dir: &Path,
        threads: i32,
    ) -> anyhow::Result<(sherpa_onnx::OfflineRecognizer, i32)> {
        let conv = dir.join("conv_frontend.onnx");
        let encoder = dir.join("encoder.int8.onnx");
        let decoder = dir.join("decoder.int8.onnx");
        let tokenizer = dir.join("tokenizer");

        anyhow::ensure!(
            conv.is_file(),
            "找不到 Qwen3-ASR conv_frontend: {}",
            conv.display()
        );
        anyhow::ensure!(
            encoder.is_file(),
            "找不到 Qwen3-ASR encoder: {}",
            encoder.display()
        );
        anyhow::ensure!(
            decoder.is_file(),
            "找不到 Qwen3-ASR decoder: {}",
            decoder.display()
        );
        anyhow::ensure!(
            tokenizer.is_dir(),
            "找不到 Qwen3-ASR tokenizer: {}",
            tokenizer.display()
        );

        let mut config = sherpa_onnx::OfflineRecognizerConfig::default();
        config.model_config.qwen3_asr = sherpa_onnx::OfflineQwen3ASRModelConfig {
            conv_frontend: Some(conv.display().to_string()),
            encoder: Some(encoder.display().to_string()),
            decoder: Some(decoder.display().to_string()),
            tokenizer: Some(tokenizer.display().to_string()),
            ..Default::default()
        };
        config.model_config.tokens = Some(String::new());
        config.model_config.num_threads = threads;

        let recognizer = sherpa_onnx::OfflineRecognizer::create(&config)
            .ok_or_else(|| anyhow::anyhow!("Qwen3-ASR 初始化失败"))?;

        log::info!(
            "Qwen3-ASR 已加载: {}（threads={threads}）",
            encoder.display()
        );
        Ok((recognizer, threads))
    }

    fn load_sense_voice(
        dir: &Path,
        threads: i32,
    ) -> anyhow::Result<(sherpa_onnx::OfflineRecognizer, i32)> {
        let model = dir.join("model.int8.onnx");
        let tokens = dir.join("tokens.txt");

        anyhow::ensure!(model.exists(), "找不到模型文件 {}", model.display());
        anyhow::ensure!(tokens.exists(), "找不到 tokens {}", tokens.display());

        let mut config = sherpa_onnx::OfflineRecognizerConfig::default();
        config.model_config.sense_voice.model = Some(model.display().to_string());

        // 这三个参数做成可配置，是为了能不重新编译就做参数扫描（见 bench/RESULTS.md）。
        //
        // 默认值是扫出来的，不是拍脑袋：
        //   lang=zh  混合语料 CER 6.2%、术语 78%（auto 是 7.5% / 74%，en 直接崩到 36.7%）
        //   itn=1    关掉反而更差（CER 9.3%），因为「三十秒」和「30秒」在比对上等价，
        //            但数字化的文本对用户更有用
        //   threads  4 线程比 2 线程 P95 快 70ms，准确率不变
        let language = std::env::var("YANCHU_ASR_LANG").unwrap_or_else(|_| "zh".to_string());
        let use_itn = std::env::var("YANCHU_ASR_ITN")
            .map(|v| v != "0")
            .unwrap_or(true);

        config.model_config.sense_voice.language = Some(language.clone());
        config.model_config.sense_voice.use_itn = use_itn;
        config.model_config.tokens = Some(tokens.display().to_string());
        config.model_config.num_threads = threads;

        let recognizer = sherpa_onnx::OfflineRecognizer::create(&config)
            .ok_or_else(|| anyhow::anyhow!("SenseVoice 初始化失败，检查模型是否完整"))?;

        log::info!(
            "SenseVoice 已加载: {}（lang={language}, itn={use_itn}, threads={threads}）",
            model.display()
        );
        Ok((recognizer, threads))
    }

    fn load_vad() -> Option<sherpa_onnx::VoiceActivityDetector> {
        let path = crate::asr::model_root().join("silero_vad.onnx");
        if !path.exists() {
            log::warn!("没有 silero_vad.onnx，长句不切分: {}", path.display());
            return None;
        }

        let mut config = sherpa_onnx::VadModelConfig::default();
        config.silero_vad.model = Some(path.display().to_string());
        config.silero_vad.threshold = 0.5;
        // 句间停顿 0.4 秒就断开：真人说话换句通常有 0.3–0.6 秒停顿，
        // 设太短会把一句话切成碎片，太长会长句不分。
        config.silero_vad.min_silence_duration = 0.9;
        config.silero_vad.min_speech_duration = 0.25;
        config.silero_vad.max_speech_duration = 30.0;
        config.silero_vad.window_size = 512;
        config.sample_rate = 16_000;

        let vad = sherpa_onnx::VoiceActivityDetector::create(&config, 60.0);
        if vad.is_none() {
            log::warn!("VAD 初始化失败，长句不切分");
        }
        vad
    }

    pub fn transcribe(&mut self, samples: &[f32]) -> anyhow::Result<String> {
        let stream = self.recognizer.create_stream();
        stream.accept_waveform(16_000, samples);
        self.recognizer.decode(&stream);

        let result = stream
            .get_result()
            .ok_or_else(|| anyhow::anyhow!("识别没有返回结果"))?;
        Ok(result.text.trim().to_string())
    }

    /// 用 VAD 把整段录音切成句子，逐句识别。
    ///
    /// **实测是负收益，默认关闭**（见 bench/RESULTS.md「VAD 断句实验」）：
    /// 20 段语料上，断句让 CER 从 7.5% 涨到 8.2%～14.9%、术语保留率从 74% 掉到 56%～70%。
    /// 原因是短音频被切碎之后丢掉上下文，英文术语正好落在切口上。
    ///
    /// 保留这条路径是因为长独白（>30s）理论上需要句读，但那要用真人长录音重测，
    /// 现在的语料证明不了。`YANCHU_VAD=1` 可强制开启做对照实验。
    pub fn transcribe_segments(&mut self, samples: &[f32]) -> anyhow::Result<Vec<String>> {
        const WINDOW: usize = 512; // VAD 要求固定窗口，别改
        const MIN_SEGMENT: usize = 1_600; // 0.1s，过滤咔哒声

        let vad_on = std::env::var("YANCHU_VAD")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("on") || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);

        let Some(vad) = &self.vad else {
            return Ok(vec![self.transcribe(samples)?]);
        };

        if !vad_on {
            return Ok(vec![self.transcribe(samples)?]);
        }

        vad.reset();
        let mut chunks: Vec<Vec<f32>> = Vec::new();

        let drain = |vad: &sherpa_onnx::VoiceActivityDetector, out: &mut Vec<Vec<f32>>| {
            while let Some(seg) = vad.front() {
                let seg_samples = seg.samples().to_vec();
                vad.pop();
                if seg_samples.len() >= MIN_SEGMENT {
                    out.push(seg_samples);
                }
            }
        };

        for chunk in samples.chunks(WINDOW) {
            vad.accept_waveform(chunk);
            drain(vad, &mut chunks);
        }

        // 把尾巴上最后一句吐出来，否则说到一半停住会被吞掉
        vad.flush();
        drain(vad, &mut chunks);

        if chunks.len() <= 1 {
            return Ok(vec![self.transcribe(samples)?]);
        }

        let mut texts = Vec::with_capacity(chunks.len());
        for chunk in chunks {
            texts.push(self.transcribe(&chunk)?);
        }
        Ok(texts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_sensevoice() {
        let cfg = AsrConfig::default();
        assert_eq!(cfg.engine, "sensevoice");
        assert_eq!(cfg.label(), "SenseVoice Small");
        assert!(cfg.resolved_dir().ends_with(SENSE_VOICE_DIR));
    }

    #[test]
    fn normalizes_qwen3_and_custom_dir() {
        let cfg = AsrConfig {
            engine: " Qwen3-ASR ".into(),
            model_dir: Some("  my-model  ".into()),
            threads: Some(99),
        }
        .normalized();
        assert_eq!(cfg.engine, "qwen3");
        assert_eq!(cfg.label(), "Qwen3-ASR 0.6B");
        assert_eq!(cfg.threads, Some(16));
        assert!(cfg.resolved_dir().ends_with("my-model"));
    }

    #[test]
    fn absolute_model_dir_is_kept() {
        let dir = std::env::temp_dir().join("yanchu-asr-test-model");
        let cfg = AsrConfig {
            engine: "sensevoice".into(),
            model_dir: Some(dir.display().to_string()),
            threads: None,
        }
        .normalized();
        assert_eq!(cfg.resolved_dir(), dir);
    }
}
