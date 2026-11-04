//! 开发模式采集：把每一次「按住说话」的音频与全链路数据落盘。
//!
//! 产品契约写的是「音频永不出本机、不落盘」，所以这里**默认关闭**，
//! 只有显式打开才写文件 —— 它是调试工具，不是产品行为：
//!
//! ```powershell
//! $env:YANCHU_DEV_LOG = "1"
//! $env:YANCHU_DEV_LOG_DIR = "C:\temp\yanchu-devlog"   # 可选，默认 <仓库根>\devlog
//! ```
//!
//! 目录结构：
//!
//! ```text
//! devlog/
//!   app.log                    应用日志，和 stderr 是同一份
//!   events.jsonl               每次按住一行，做统计/导出用
//!   report.md, sessions.csv    --dev-report 的产物
//!   sessions/2026-11-02/20261102-161230-123/
//!     session.json             这一次按住的完整事件（识别/规则/大润色/落字）
//!     audio.wav                整段录音（16kHz 单声道）
//!     seg-01.wav               第 1 个定稿句子的音频切片
//!     seg-02.wav               第 2 个 …
//! ```
//!
//! 关键点：**每个定稿句子都有自己的 wav 切片**（`seg-NN.wav`），
//! 它和 `session.json` 里 `segments[i].raw` 一一对应。
//! 这样后面想把某一句单独拿去重跑识别、重跑润色、或者做人工标注，都不用再切音频。

use std::collections::VecDeque;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

pub const ENV_FLAG: &str = "YANCHU_DEV_LOG";
pub const ENV_DIR: &str = "YANCHU_DEV_LOG_DIR";

/// 采样率固定 16k —— 送进模型的也是这个，切片和整段可以直接互相比较。
const SAMPLE_RATE: u32 = 16_000;

// ---------------------------------------------------------------- 开关

/// 默认落在仓库根目录的 `devlog/`（`.gitignore` 已排除）。
pub fn default_dir() -> PathBuf {
    if let Ok(dir) = std::env::var(ENV_DIR) {
        if !dir.trim().is_empty() {
            return PathBuf::from(dir);
        }
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("devlog")
}

/// 采集是否开启。只要设了 `YANCHU_DEV_LOG=1` 或 `YANCHU_DEV_LOG_DIR=<目录>` 就算开。
pub fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| {
        if let Some(v) = std::env::var_os(ENV_FLAG) {
            let v = v.to_string_lossy().trim().to_ascii_lowercase();
            if v.is_empty() {
                return false;
            }
            return !matches!(v.as_str(), "0" | "off" | "false" | "no");
        }
        std::env::var(ENV_DIR)
            .map(|d| !d.trim().is_empty())
            .unwrap_or(false)
    })
}

/// 采集根目录；没开采集或者目录建不出来时返回 None。
pub fn root() -> Option<PathBuf> {
    if !enabled() {
        return None;
    }
    let dir = default_dir();
    if let Err(e) = std::fs::create_dir_all(&dir) {
        log::warn!("DEVLOG 目录建不出来（{e}），这次不落盘: {}", dir.display());
        return None;
    }
    Some(dir)
}

// ---------------------------------------------------------------- 日志

/// 最近日志（给控制面板的「开发者模式」看）。不落盘时也能看。
static RECENT: Mutex<VecDeque<String>> = Mutex::new(VecDeque::new());
const RECENT_MAX: usize = 400;

pub fn recent_logs(limit: usize) -> Vec<String> {
    let buf = RECENT.lock().unwrap();
    let n = limit.min(buf.len());
    buf.iter().skip(buf.len() - n).cloned().collect()
}

pub fn clear_logs() {
    RECENT.lock().unwrap().clear();
}

fn push_recent(line: String) {
    let mut buf = RECENT.lock().unwrap();
    if buf.len() >= RECENT_MAX {
        buf.pop_front();
    }
    buf.push_back(line);
}

/// 初始化日志：所有日志进内存环形缓冲（面板可看）；开着采集时再额外写 app.log。
pub fn init_logging() {
    let mut builder =
        env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"));

    let file = root().and_then(|root| {
        match std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(root.join("app.log"))
        {
            Ok(file) => Some(Mutex::new(file)),
            Err(e) => {
                eprintln!("DEVLOG 打不开 app.log（{e}），只写内存日志");
                None
            }
        }
    });

    builder.target(env_logger::Target::Pipe(Box::new(Capture {
        file,
        // GUI 进程从批处理 start 起来时没有控制台，echo 必须是 false
        echo: stderr_available(),
        pending: Vec::new(),
    })));
    // 面板里按纯文本显示，不要 ANSI 颜色码
    builder.write_style(env_logger::WriteStyle::Never);

    // 测试里可能重复初始化，失败了不代表出错
    let _ = builder.try_init();
}

fn stderr_available() -> bool {
    use windows::Win32::System::Console::{GetStdHandle, STD_ERROR_HANDLE};
    unsafe {
        match GetStdHandle(STD_ERROR_HANDLE) {
            Ok(h) => {
                let raw = h.0 as isize;
                raw != 0 && raw != -1
            }
            Err(_) => false,
        }
    }
}

struct Capture {
    /// 开了开发采集时才有
    file: Option<Mutex<std::fs::File>>,
    /// 没有控制台时一律不碰 stderr
    echo: bool,
    /// 一行可能分多次 write，先攒着
    pending: Vec<u8>,
}

impl Write for Capture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        // 文件优先：控制台那边出任何问题都不能影响落盘
        if let Some(file) = &self.file {
            if let Ok(mut f) = file.lock() {
                let _ = f.write_all(buf);
            }
        }
        if self.echo {
            let _ = std::io::stderr().write_all(buf);
        }

        self.pending.extend_from_slice(buf);
        while let Some(pos) = self.pending.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = self.pending.drain(..=pos).collect();
            let text = String::from_utf8_lossy(&line[..line.len().saturating_sub(1)])
                .trim_end_matches('\r')
                .to_string();
            if !text.is_empty() {
                push_recent(text);
            }
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        if let Some(file) = &self.file {
            if let Ok(mut f) = file.lock() {
                let _ = f.flush();
            }
        }
        if self.echo {
            let _ = std::io::stderr().flush();
        }
        Ok(())
    }
}

// ---------------------------------------------------------------- 数据结构

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TargetInfo {
    pub hwnd: isize,
    pub pid: u32,
    /// 进程可执行文件名，例如 `Code.exe`、`chrome.exe`
    pub process: String,
    /// 窗口标题（可能是文档名，用来区分落在哪个应用里）
    pub title: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct AsrCall {
    /// partial / commit / final
    pub kind: String,
    /// 在本次录音 16k 缓冲里的起止采样点
    pub from: usize,
    pub to: usize,
    pub samples: usize,
    pub ms: u64,
    pub chars: usize,
    pub text: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SegmentLog {
    pub index: u32,
    pub from: usize,
    pub to: usize,
    pub samples: usize,
    pub ms: u64,
    /// 切片文件名，写失败时为 None
    pub wav: Option<String>,
    /// 这一句的识别原文（没有经过任何润色）
    pub raw: String,
}

/// 大润色这一次到底干了什么 —— 光看「有没有生效」不够，
/// 回退的原因（超时 / 契约不过 / 没配置）直接决定下一步怎么改提示词。
#[derive(Debug, Clone, Serialize, Default)]
pub struct LlmTrace {
    /// used / skipped / failed
    pub outcome: String,
    /// standard / deep
    pub depth: String,
    pub ms: u64,
    pub reject_reason: Option<String>,
    /// 模型原始输出（被回退的也留下，这是调提示词最有用的东西）
    pub raw: Option<String>,
    /// 事实项告警（数字/人称/否定变化）：只记录、不拦截
    pub warnings: Vec<String>,
}

impl From<&crate::llm::LlmAttempt> for LlmTrace {
    fn from(attempt: &crate::llm::LlmAttempt) -> Self {
        Self {
            outcome: attempt.outcome().to_string(),
            depth: attempt.depth.label().to_string(),
            ms: attempt.ms(),
            reject_reason: attempt.reason.clone(),
            raw: if attempt.raw.is_empty() {
                None
            } else {
                Some(attempt.raw.clone())
            },
            warnings: attempt.warnings.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct InsertInfo {
    /// inserted / partially_inserted / copied_fallback / failed
    pub status: String,
    pub chars: usize,
    pub ms: u64,
    pub warning: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct Finish {
    pub press_to_release_ms: u64,
    pub release_to_text_ms: u64,
    /// 非 None 表示这次没走到落字（录音太短 / 识别失败 / 没听清）
    pub aborted: Option<String>,
    pub raw: String,
    pub tail: String,
    /// 气泡最后一帧（已定稿句子是小润色后的 + 尾巴原文）：
    /// 大润色到底有没有改掉用户看到的措辞，靠它对比
    pub display: String,
    pub corrected: String,
    pub ruled: String,
    pub polished: String,
    pub used_llm: bool,
    pub polish_ms: u64,
    /// standard / deep
    pub depth: String,
    pub fixes: Vec<(String, String)>,
    /// 这次口述里每个断点的判定（逗号/句号/无），调判据时一条条看回来
    pub breaks: Vec<crate::stream::BreakRecord>,
    pub llm: Option<LlmTrace>,
    pub insert: Option<InsertInfo>,
}

#[derive(Debug, Clone, Serialize)]
pub struct EnvSnapshot {
    pub app_version: &'static str,
    pub exe: String,
    pub asr_engine: String,
    pub asr_lang: String,
    pub asr_itn: bool,
    pub vad: bool,
    pub asr_threads: Option<i32>,
    pub polish_enabled: bool,
    /// 是否走了深度整理档
    pub deep_enabled: bool,
    pub llm_model: Option<String>,
    pub llm_url: Option<String>,
    pub llm_provider: Option<String>,
    pub llm_timeout_ms: Option<u64>,
    pub dev_log_dir: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AudioInfo {
    pub samples: usize,
    pub secs: f32,
    pub wav: Option<String>,
    /// 按下的瞬间会把预录缓冲倒进来，这 350ms 也算在录音里
    pub preroll_ms: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct StageInfo {
    pub raw: String,
    /// 松手时最后一段尾巴的识别结果
    pub tail: String,
    /// 气泡最后一帧
    pub display: String,
    pub corrected: String,
    pub ruled: String,
    pub polished: String,
    pub used_llm: bool,
    /// standard / deep
    pub depth: String,
    pub polish_ms: u64,
    pub fixes: Vec<Fix>,
    /// 断点判定记录：调标点判据时靠它复盘
    pub breaks: Vec<crate::stream::BreakRecord>,
    pub llm: Option<LlmTrace>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Fix {
    pub from: String,
    pub to: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionLog {
    pub schema: u32,
    pub id: String,
    pub started_local: String,
    pub ended_local: String,
    pub start_unix_ms: u128,
    pub duration_ms: u64,
    pub target: Option<TargetInfo>,
    pub env: EnvSnapshot,
    pub mock_input: Option<String>,
    pub audio: AudioInfo,
    pub press_to_release_ms: u64,
    pub release_to_text_ms: u64,
    pub aborted: Option<String>,
    pub stages: StageInfo,
    pub insert: Option<InsertInfo>,
    pub asr_calls: Vec<AsrCall>,
    pub segments: Vec<SegmentLog>,
}

/// `events.jsonl` 的一行。字段刻意压到只够做统计，但保留三段文本，
/// 这样报告里能直接看到「规则层改了什么 / 大润色又改了什么」。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub id: String,
    pub started_local: String,
    pub audio_secs: f32,
    pub press_to_release_ms: u64,
    pub release_to_text_ms: u64,
    /// 定稿 + 切句的识别耗时合计（不含实时预览的 partial）
    pub asr_ms: u64,
    pub asr_calls: usize,
    pub commits: usize,
    pub polish_ms: u64,
    /// used / rejected / skipped / off
    pub llm: String,
    pub llm_ms: u64,
    /// standard / deep / ""（没跑润色）
    #[serde(default)]
    pub depth: String,
    /// 事实项告警的种类，例如 ["人称变化"]
    #[serde(default)]
    pub fact_warnings: Vec<String>,
    #[serde(default)]
    pub llm_reject: Option<String>,
    #[serde(default)]
    pub insert: Option<String>,
    #[serde(default)]
    pub insert_chars: usize,
    #[serde(default)]
    pub target: Option<String>,
    pub raw: String,
    pub ruled: String,
    pub polished: String,
    /// 气泡最后一帧：和大润色后的结果对比，能直接量出「气泡→落字一致率」
    #[serde(default)]
    pub display: String,
    /// 这次口述里断成句号 / 逗号的次数
    #[serde(default)]
    pub break_period: usize,
    #[serde(default)]
    pub break_comma: usize,
    #[serde(default)]
    pub aborted: Option<String>,
}

// ---------------------------------------------------------------- 采集器

pub struct Recorder {
    id: String,
    dir: PathBuf,
    root: PathBuf,
    now: LocalNow,
    started: Instant,
    target: Option<TargetInfo>,
    mock_input: Option<String>,
    data: Mutex<Data>,
}

#[derive(Default)]
struct Data {
    asr_calls: Vec<AsrCall>,
    segments: Vec<SegmentLog>,
}

impl Recorder {
    /// 由调用方先把目标窗口信息取好再进来 —— 取目标很便宜，
    /// 建目录、写文件不便宜，所以后者不能在键盘钩子线程上做。
    pub fn start_with(target: Option<TargetInfo>) -> Option<Self> {
        let root = root()?;
        Self::start_in_with(&root, target)
    }

    #[cfg(test)]
    pub(crate) fn start_in(root: &Path) -> Option<Self> {
        Self::start_in_with(root, capture_target())
    }

    fn start_in_with(root: &Path, target: Option<TargetInfo>) -> Option<Self> {
        let now = local_now();
        let id = now.compact.clone();
        let dir = root.join("sessions").join(&now.date).join(&id);
        if let Err(e) = std::fs::create_dir_all(&dir) {
            log::warn!("DEVLOG 建目录失败: {e}");
            return None;
        }

        Some(Self {
            id,
            dir,
            root: root.to_path_buf(),
            started: Instant::now(),
            now,
            target,
            mock_input: std::env::var("YANCHU_WAV_INPUT").ok(),
            data: Mutex::new(Data::default()),
        })
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn target_summary(&self) -> String {
        match &self.target {
            Some(t) if !t.process.is_empty() => {
                format!("{} · {}", t.process, truncate(&t.title, 32))
            }
            Some(_) => "<未知进程>".into(),
            None => "<没有前台窗口>".into(),
        }
    }

    /// 记一次识别调用。partial 也记 —— 实时预览的稳定性要拿这个看。
    pub fn note_asr(&self, kind: &str, from: usize, to: usize, ms: u64, text: &str) {
        let Ok(mut d) = self.data.lock() else {
            return;
        };
        d.asr_calls.push(AsrCall {
            kind: kind.to_string(),
            from,
            to,
            samples: to.saturating_sub(from),
            ms,
            chars: text.chars().count(),
            text: text.to_string(),
        });
    }

    /// 定稿一句话：立刻把它的音频切片写盘，不等整段结束。
    /// 好处是应用中途崩了，已经说完的句子也还在。
    pub fn push_segment(&self, from: usize, to: usize, samples: &[f32], raw: &str, ms: u64) {
        let index = self
            .data
            .lock()
            .map(|d| d.segments.len() as u32 + 1)
            .unwrap_or(1);
        let name = format!("seg-{index:02}.wav");
        let wav = match write_wav(&self.dir.join(&name), samples) {
            Ok(()) => Some(name),
            Err(e) => {
                log::warn!("DEVLOG 写切片失败（{e}）: {}", self.dir.join(&name).display());
                None
            }
        };

        if let Ok(mut d) = self.data.lock() {
            d.segments.push(SegmentLog {
                index,
                from,
                to,
                samples: samples.len(),
                ms,
                wav,
                raw: raw.to_string(),
            });
        }
    }

    /// 收尾：写整段 wav、session.json、events.jsonl。
    pub fn finish(&self, samples: &[f32], f: Finish) {
        let wav = match write_wav(&self.dir.join("audio.wav"), samples) {
            Ok(()) => Some("audio.wav".to_string()),
            Err(e) => {
                log::warn!("DEVLOG 写整段音频失败: {e}");
                None
            }
        };

        let (asr_calls, segments) = match self.data.lock() {
            Ok(d) => (d.asr_calls.clone(), d.segments.clone()),
            Err(_) => (Vec::new(), Vec::new()),
        };

        let ended = local_now();
        let duration_ms = self.started.elapsed().as_millis() as u64;
        let asr_ms: u64 = asr_calls
            .iter()
            .filter(|c| c.kind != "partial")
            .map(|c| c.ms)
            .sum();
        let commits = asr_calls.iter().filter(|c| c.kind == "commit").count();

        let env = env_snapshot(&self.root);
        let log = SessionLog {
            schema: 1,
            id: self.id.clone(),
            started_local: self.now.iso.clone(),
            ended_local: ended.iso.clone(),
            start_unix_ms: unix_ms(),
            duration_ms,
            target: self.target.clone(),
            env,
            mock_input: self.mock_input.clone(),
            audio: AudioInfo {
                samples: samples.len(),
                secs: samples.len() as f32 / SAMPLE_RATE as f32,
                wav: wav.clone(),
                preroll_ms: 350,
            },
            press_to_release_ms: f.press_to_release_ms,
            release_to_text_ms: f.release_to_text_ms,
            aborted: f.aborted.clone(),
            stages: StageInfo {
                raw: f.raw.clone(),
                tail: f.tail.clone(),
                display: f.display.clone(),
                corrected: f.corrected.clone(),
                ruled: f.ruled.clone(),
                polished: f.polished.clone(),
                used_llm: f.used_llm,
                depth: f.depth.clone(),
                polish_ms: f.polish_ms,
                fixes: f
                    .fixes
                    .iter()
                    .map(|(from, to)| Fix {
                        from: from.clone(),
                        to: to.clone(),
                    })
                    .collect(),
                breaks: f.breaks.clone(),
                llm: f.llm.clone(),
            },
            insert: f.insert.clone(),
            asr_calls,
            segments,
        };

        let json = match serde_json::to_string_pretty(&log) {
            Ok(j) => j,
            Err(e) => {
                log::warn!("DEVLOG 序列化失败: {e}");
                return;
            }
        };
        if let Err(e) = std::fs::write(self.dir.join("session.json"), json) {
            log::warn!("DEVLOG 写 session.json 失败: {e}");
        }

        // 一行一份摘要，后续统计只读这一个文件
        let event = Event {
            id: self.id.clone(),
            started_local: self.now.iso.clone(),
            audio_secs: log.audio.secs,
            press_to_release_ms: f.press_to_release_ms,
            release_to_text_ms: f.release_to_text_ms,
            asr_ms,
            asr_calls: log.asr_calls.len(),
            commits,
            polish_ms: f.polish_ms,
            llm: match &f.llm {
                Some(t) => t.outcome.clone(),
                None => "off".to_string(),
            },
            llm_ms: f.llm.as_ref().map(|t| t.ms).unwrap_or(0),
            depth: f.depth.clone(),
            fact_warnings: f
                .llm
                .as_ref()
                .map(|t| t.warnings.clone())
                .unwrap_or_default(),
            llm_reject: f.llm.as_ref().and_then(|t| t.reject_reason.clone()),
            insert: f.insert.as_ref().map(|i| i.status.clone()),
            insert_chars: f.insert.as_ref().map(|i| i.chars).unwrap_or(0),
            target: self.target.as_ref().map(|t| t.process.clone()),
            raw: f.raw.clone(),
            ruled: f.ruled.clone(),
            polished: f.polished.clone(),
            display: f.display.clone(),
            break_period: f.breaks.iter().filter(|b| b.decision == "period").count(),
            break_comma: f.breaks.iter().filter(|b| b.decision == "comma").count(),
            aborted: f.aborted.clone(),
        };

        if let Ok(line) = serde_json::to_string(&event) {
            let path = self.root.join("events.jsonl");
            match std::fs::OpenOptions::new().create(true).append(true).open(&path) {
                Ok(mut file) => {
                    let _ = writeln!(file, "{line}");
                }
                Err(e) => log::warn!("DEVLOG 追加 events.jsonl 失败: {e}"),
            }
        }

        log::info!(
            "DEVLOG 已保存 {}（音频 {:.1}s，识别 {}ms，润色 {}ms，落字 {}ms）: {}",
            self.id,
            log.audio.secs,
            asr_ms,
            f.polish_ms,
            f.insert.as_ref().map(|i| i.ms).unwrap_or(0),
            self.dir.display()
        );
    }
}

// ---------------------------------------------------------------- 环境快照

fn env_snapshot(root: &Path) -> EnvSnapshot {
    let var = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
    // 直接从 llm.json 读，别只看环境变量 —— 模型名决定了这一句是谁润的，
    // 分析「哪个模型更容易违约」时这一列是主键
    let llm = crate::llm::LlmConfig::load(&crate::asr::model_root());
    EnvSnapshot {
        app_version: env!("CARGO_PKG_VERSION"),
        exe: std::env::current_exe()
            .map(|p| p.display().to_string())
            .unwrap_or_default(),
        asr_engine: var("YANCHU_ASR_ENGINE").unwrap_or_else(|| "sensevoice".into()),
        asr_lang: var("YANCHU_ASR_LANG").unwrap_or_else(|| "zh".into()),
        asr_itn: var("YANCHU_ASR_ITN").map(|v| v != "0").unwrap_or(true),
        vad: var("YANCHU_VAD")
            .map(|v| matches!(v.to_ascii_lowercase().as_str(), "1" | "on" | "true"))
            .unwrap_or(false),
        asr_threads: var("YANCHU_ASR_THREADS").and_then(|v| v.parse().ok()),
        polish_enabled: crate::tray::polish_enabled(),
        deep_enabled: crate::tray::deep_enabled(),
        llm_model: llm.as_ref().map(|c| c.model.clone()),
        llm_url: llm.as_ref().map(|c| c.url.clone()),
        llm_provider: llm.as_ref().map(|c| format!("{:?}", c.provider).to_lowercase()),
        llm_timeout_ms: llm.as_ref().map(|c| c.timeout_ms),
        dev_log_dir: Some(root.display().to_string()),
    }
}

// ---------------------------------------------------------------- 时间

#[derive(Debug, Clone)]
pub(crate) struct LocalNow {
    /// 2026-11-02
    pub(crate) date: String,
    /// 2026-11-02T16:12:30.123
    pub(crate) iso: String,
    /// 20261102-161230-123
    pub(crate) compact: String,
}

pub(crate) fn local_now() -> LocalNow {
    let st = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
    let date = format!("{:04}-{:02}-{:02}", st.wYear, st.wMonth, st.wDay);
    let iso = format!(
        "{date}T{:02}:{:02}:{:02}.{:03}",
        st.wHour, st.wMinute, st.wSecond, st.wMilliseconds
    );
    let compact = format!(
        "{:04}{:02}{:02}-{:02}{:02}{:02}-{:03}",
        st.wYear, st.wMonth, st.wDay, st.wHour, st.wMinute, st.wSecond, st.wMilliseconds
    );
    LocalNow {
        date,
        iso,
        compact,
    }
}

fn unix_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

// ---------------------------------------------------------------- 目标窗口

/// 抓当前前台窗口的信息。很便宜（几个 Win32 查询，约几十微秒），
/// 但必须在按下热键的那一刻调用 —— 之后用户可能已经换了窗口。
pub fn capture_target() -> Option<TargetInfo> {
    use windows::Win32::UI::WindowsAndMessaging::{
        GetForegroundWindow, GetWindowTextW, GetWindowThreadProcessId,
    };

    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.0.is_null() {
            return None;
        }

        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid as *mut u32));

        let mut buf = [0u16; 512];
        let n = GetWindowTextW(hwnd, &mut buf);
        let title = if n > 0 {
            String::from_utf16_lossy(&buf[..n as usize])
        } else {
            String::new()
        };

        Some(TargetInfo {
            hwnd: hwnd.0 as isize,
            pid,
            process: process_name(pid).unwrap_or_default(),
            title,
        })
    }
}

fn process_name(pid: u32) -> Option<String> {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };

    if pid == 0 {
        return None;
    }

    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 1024];
        let mut size = buf.len() as u32;
        let res = QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_WIN32,
            windows::core::PWSTR(buf.as_mut_ptr()),
            &mut size,
        );
        let _ = CloseHandle(handle);
        res.ok()?;

        let path = String::from_utf16_lossy(&buf[..size as usize]);
        Some(
            path.rsplit(['\\', '/'])
                .next()
                .unwrap_or(&path)
                .to_string(),
        )
    }
}

// ---------------------------------------------------------------- wav

fn write_wav(path: &Path, samples: &[f32]) -> anyhow::Result<()> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: SAMPLE_RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(path, spec)?;
    for &s in samples {
        let v = (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
        writer.write_sample(v)?;
    }
    writer.finalize()?;
    Ok(())
}

// ---------------------------------------------------------------- 报告

const FILLERS: &[&str] = &[
    "嗯", "呃", "额", "那个", "就是说", "然后就是", "你知道吧", "怎么说呢", "对吧",
];

fn count_fillers(s: &str) -> usize {
    FILLERS.iter().map(|f| s.matches(f).count()).sum()
}

fn pct(values: &[u64], p: f64) -> u64 {
    if values.is_empty() {
        return 0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    let idx = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

fn truncate(s: &str, max_chars: usize) -> String {
    let mut out: String = s.chars().take(max_chars).collect();
    if s.chars().count() > max_chars {
        out.push('…');
    }
    out.replace('\n', " ")
}

fn csv_escape(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

/// `--dev-report`：把 `events.jsonl` 汇总成一张表 + 报告 + CSV。
pub fn report(dir: &Path) -> anyhow::Result<()> {
    let path = dir.join("events.jsonl");
    let raw = std::fs::read_to_string(&path).map_err(|e| {
        anyhow::anyhow!(
            "读不到 {}（{e}）\n先打开采集跑几句：set YANCHU_DEV_LOG=1，再重启言出",
            path.display()
        )
    })?;

    let mut events: Vec<Event> = Vec::new();
    let mut skipped = 0usize;
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match serde_json::from_str::<Event>(line) {
            Ok(e) => events.push(e),
            Err(_) => skipped += 1,
        }
    }

    if events.is_empty() {
        println!("{} 里没有可解析的记录", path.display());
        return Ok(());
    }

    let n = events.len();
    // 旧记录里 started_local 可能不是标准格式，取不到 10 个字符就整条当独立一天
    let days: std::collections::BTreeSet<&str> = events
        .iter()
        .map(|e| e.started_local.get(..10).unwrap_or(&e.started_local))
        .collect();
    let total_secs: f32 = events.iter().map(|e| e.audio_secs).sum();

    let collect = |f: &dyn Fn(&Event) -> u64| -> Vec<u64> { events.iter().map(f).collect() };
    let lat = collect(&|e| e.release_to_text_ms);
    let asr = collect(&|e| e.asr_ms);
    let polish = collect(&|e| e.polish_ms);

    let aborted = events.iter().filter(|e| e.aborted.is_some()).count();
    let rule_changed = events
        .iter()
        .filter(|e| e.aborted.is_none() && e.raw.trim() != e.ruled.trim())
        .count();
    let llm_used = events.iter().filter(|e| e.llm == "used").count();
    let llm_failed = events.iter().filter(|e| e.llm == "failed").count();
    let llm_skipped = events.iter().filter(|e| e.llm == "skipped").count();
    let llm_off = events.iter().filter(|e| e.llm == "off").count();
    let llm_changed = events
        .iter()
        .filter(|e| e.llm == "used" && e.ruled != e.polished)
        .count();
    let violations = events
        .iter()
        .filter(|e| {
            if e.llm != "used" {
                return false;
            }
            let depth = crate::llm::Depth::from_label(&e.depth);
            crate::llm::contract_violation_for(depth, &e.ruled, &e.polished).is_some()
        })
        .count();
    let inserted = events
        .iter()
        .filter(|e| e.insert.as_deref() == Some("inserted"))
        .count();

    let deep_runs = events.iter().filter(|e| e.depth == "deep").count();
    let fact_warned = events
        .iter()
        .filter(|e| !e.fact_warnings.is_empty())
        .count();

    let fillers_raw: usize = events.iter().map(|e| count_fillers(&e.raw)).sum();
    let fillers_out: usize = events
        .iter()
        .filter(|e| e.aborted.is_none())
        .map(|e| count_fillers(&e.polished))
        .sum();

    let mut out = String::new();
    let line = |s: &str| s.to_string();
    let mut push = |s: String| {
        out.push_str(&s);
        out.push('\n');
    };

    push(line("言出 · 开发日志报告"));
    push(format!("目录     : {}", dir.display()));
    push(format!(
        "会话数   : {n}（{} 天），失败/丢弃 {aborted}",
        days.len()
    ));
    push(format!(
        "总音频   : {:.1} 分钟（平均 {:.1}s/次）",
        total_secs / 60.0,
        total_secs / n as f32
    ));
    if skipped > 0 {
        push(format!("跳过行   : {skipped}（格式不认识，多半是旧版本写的）"));
    }
    push(String::new());

    push(line("延迟（ms）"));
    push(format!(
        "  松手→落字    P50 {:>5}   P95 {:>5}   最大 {:>5}",
        pct(&lat, 0.5),
        pct(&lat, 0.95),
        lat.iter().copied().max().unwrap_or(0)
    ));
    push(format!(
        "  识别（定稿） P50 {:>5}   P95 {:>5}",
        pct(&asr, 0.5),
        pct(&asr, 0.95)
    ));
    push(format!(
        "  润色         P50 {:>5}   P95 {:>5}",
        pct(&polish, 0.5),
        pct(&polish, 0.95)
    ));
    push(String::new());

    push(line("润色"));
    push(format!(
        "  规则层改动   {rule_changed}/{n}（{:.0}%）",
        rule_changed as f32 / n as f32 * 100.0
    ));
    push(format!(
        "  大润色       采用 {llm_used}   回退 {llm_failed}   没配置 {llm_skipped}   关闭 {llm_off}"
    ));
    push(format!(
        "  档位         深度 {deep_runs}   标准 {}",
        events
            .iter()
            .filter(|e| e.depth == "standard")
            .count()
    ));
    push(format!(
        "  大润色改动措辞 {}（采用里 {}%）",
        llm_changed,
        if llm_used == 0 {
            0
        } else {
            llm_changed * 100 / llm_used
        }
    ));
    push(format!(
        "  契约复检      {violations} 条会被判违规（violates_contract，与运行时同一套规则）"
    ));
    // 产品定义里的硬指标：气泡最后一帧 = 落字结果
    let bubble_same = events
        .iter()
        .filter(|e| {
            e.aborted.is_none()
                && !e.display.is_empty()
                && e.display.trim() == e.polished.trim()
        })
        .count();
    let landed = events.iter().filter(|e| e.aborted.is_none()).count();
    push(format!(
        "  气泡→落字     {bubble_same}/{landed} 一致（{}%）（大润色改了气泡措辞的会掉在这里）",
        if landed == 0 {
            100
        } else {
            bubble_same * 100 / landed
        }
    ));
    push(format!(
        "  事实项告警     {fact_warned} 条（只记录不拦截，见 docs/问题清单.md YC-001）"
    ));
    push(format!("  口头语残留    原文 {fillers_raw} 处 → 落字 {fillers_out} 处"));
    let break_periods: usize = events.iter().map(|e| e.break_period).sum();
    let break_commas: usize = events.iter().map(|e| e.break_comma).sum();
    push(format!(
        "  断句         句号 {break_periods}   逗号 {break_commas}（判据见 stream.rs::decide_break）"
    ));
    push(format!(
        "  落字成功      {inserted}/{n}（其余走降级或失败）"
    ));
    push(String::new());

    if fact_warned > 0 {
        push(line("事实项告警分布"));
        let mut kinds: std::collections::BTreeMap<&str, usize> = Default::default();
        for e in events.iter().filter(|e| !e.fact_warnings.is_empty()) {
            for w in &e.fact_warnings {
                *kinds.entry(w.as_str()).or_default() += 1;
            }
        }
        for (k, c) in kinds {
            push(format!("  {c}× {k}"));
        }
        push(String::new());
    }

    if llm_failed > 0 {
        push(line("回退原因"));
        let mut reasons: std::collections::BTreeMap<&str, usize> = Default::default();
        for e in events.iter().filter(|e| e.llm == "failed") {
            if let Some(r) = &e.llm_reject {
                *reasons.entry(r.as_str()).or_default() += 1;
            }
        }
        for (r, c) in reasons {
            push(format!("  {c}× {r}"));
        }
        push(String::new());
    }

    push(line("样例（原文 → 规则层 → 落字）"));
    let mut shown = 0;
    for e in events.iter().rev() {
        if e.aborted.is_some() || e.polished.trim() == e.raw.trim() {
            continue;
        }
        if shown >= 12 {
            break;
        }
        shown += 1;
        push(format!("  [{}{}]", e.id, if e.llm == "used" { " · 大润色" } else { "" }));
        push(format!("    原文  {}", truncate(&e.raw, 90)));
        push(format!("    规则  {}", truncate(&e.ruled, 90)));
        push(format!("    落字  {}", truncate(&e.polished, 90)));
    }
    if shown == 0 {
        push("  （没有改动过措辞的句子）".to_string());
    }

    let text = out;
    print!("{text}");

    let report_path = dir.join("report.md");
    let mut md = String::from("# 言出开发日志报告\n\n```text\n");
    md.push_str(&text);
    md.push_str("```\n");
    if std::fs::write(&report_path, md).is_ok() {
        println!("\n报告已写入 {}", report_path.display());
    }

    let csv_path = dir.join("sessions.csv");
    let mut csv = String::from(
        "id,started_local,target,audio_secs,press_to_release_ms,release_to_text_ms,asr_ms,polish_ms,depth,llm,llm_reject,fact_warnings,insert,insert_chars,raw,ruled,polished,aborted\n",
    );
    for e in &events {
        csv.push_str(&format!(
            "{},{},{},{:.2},{},{},{},{},{},{},{},{},{},{},{},{},{},{}\n",
            e.id,
            e.started_local,
            e.target.as_deref().unwrap_or(""),
            e.audio_secs,
            e.press_to_release_ms,
            e.release_to_text_ms,
            e.asr_ms,
            e.polish_ms,
            e.depth,
            e.llm,
            e.llm_reject.as_deref().unwrap_or(""),
            csv_escape(&e.fact_warnings.join(";")),
            e.insert.as_deref().unwrap_or(""),
            e.insert_chars,
            csv_escape(&e.raw),
            csv_escape(&e.ruled),
            csv_escape(&e.polished),
            e.aborted.as_deref().unwrap_or(""),
        ));
    }
    if std::fs::write(&csv_path, csv).is_ok() {
        println!("CSV 已写入 {}", csv_path.display());
    }

    Ok(())
}

// ---------------------------------------------------------------- 测试

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "yanchu-devlog-{tag}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn wav_roundtrip_keeps_samples_and_format() {
        let root = temp_root("wav");
        let samples: Vec<f32> = (0..1600)
            .map(|i| (i as f32 / 1600.0 - 0.5) * 0.8)
            .collect();
        let path = root.join("x.wav");
        write_wav(&path, &samples).unwrap();

        let reader = hound::WavReader::open(&path).unwrap();
        let spec = reader.spec();
        assert_eq!(spec.channels, 1);
        assert_eq!(spec.sample_rate, SAMPLE_RATE);
        assert_eq!(spec.bits_per_sample, 16);
        assert_eq!(reader.len(), samples.len() as u32);

        // 读回来要能再喂给识别层
        let back = crate::audio::read_wav_16k(&path).unwrap();
        assert_eq!(back.len(), samples.len());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn recorder_writes_session_and_event() {
        let root = temp_root("session");
        let rec = Recorder::start_in(&root).expect("采集器建不起来");
        let samples = vec![0.1f32; 32_000];

        rec.note_asr("commit", 0, 16_000, 120, "把日志改成 debug");
        rec.push_segment(0, 16_000, &samples[..16_000], "把日志改成 debug", 120);
        rec.note_asr("final", 16_000, 32_000, 150, "再看一眼");
        rec.push_segment(16_000, 32_000, &samples[16_000..], "再看一眼", 150);

        rec.finish(
            &samples,
            Finish {
                press_to_release_ms: 2_000,
                release_to_text_ms: 700,
                raw: "把日志改成 debug再看一眼".into(),
                tail: "再看一眼".into(),
                corrected: "把日志改成 debug 再看一眼".into(),
                ruled: "把日志改成 debug，再看一眼。".into(),
                polished: "把日志改成 debug，再看一眼。".into(),
                used_llm: false,
                polish_ms: 30,
                llm: Some(LlmTrace {
                    outcome: "skipped".into(),
                    ms: 0,
                    ..Default::default()
                }),
                insert: Some(InsertInfo {
                    status: "inserted".into(),
                    chars: 20,
                    ms: 55,
                    warning: None,
                }),
                ..Default::default()
            },
        );

        let dir = rec.dir().to_path_buf();
        assert!(dir.join("audio.wav").exists());
        assert!(dir.join("seg-01.wav").exists());
        assert!(dir.join("seg-02.wav").exists());

        let json = std::fs::read_to_string(dir.join("session.json")).unwrap();
        let log: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(log["segments"].as_array().unwrap().len(), 2);
        assert_eq!(log["segments"][0]["raw"], "把日志改成 debug");
        assert_eq!(log["env"]["dev_log_dir"], root.display().to_string());
        assert_eq!(log["insert"]["status"], "inserted");

        let events = std::fs::read_to_string(root.join("events.jsonl")).unwrap();
        let event: Event = serde_json::from_str(events.lines().next().unwrap()).unwrap();
        assert_eq!(event.commits, 1);
        assert_eq!(event.asr_ms, 270);
        assert_eq!(event.llm, "skipped");
        assert_eq!(event.insert.as_deref(), Some("inserted"));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn report_runs_on_real_files() {
        let root = temp_root("report");
        let rec = Recorder::start_in(&root).unwrap();
        rec.finish(
            &vec![0.0f32; 16_000],
            Finish {
                press_to_release_ms: 1_500,
                release_to_text_ms: 480,
                raw: "嗯，那个，我想问一下这个接口".into(),
                ruled: "我想问一下这个接口。".into(),
                polished: "我想问一下这个接口。".into(),
                used_llm: true,
                polish_ms: 400,
                llm: Some(LlmTrace {
                    outcome: "used".into(),
                    ms: 380,
                    ..Default::default()
                }),
                ..Default::default()
            },
        );
        report(&root).unwrap();
        assert!(root.join("report.md").exists());
        let csv = std::fs::read_to_string(root.join("sessions.csv")).unwrap();
        assert!(csv.contains("我想问一下这个接口"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn local_time_is_formatted() {
        let now = local_now();
        assert_eq!(now.date.len(), 10);
        assert_eq!(now.iso.len(), 23);
        assert!(now.iso.starts_with(&now.date));
        assert_eq!(now.compact.len(), 19);
    }

    #[test]
    fn truncate_counts_chars_not_bytes() {
        assert_eq!(truncate("中文测试", 2), "中文…");
        assert_eq!(truncate("abc", 5), "abc");
    }
}
