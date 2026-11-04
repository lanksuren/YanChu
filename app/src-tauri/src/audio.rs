//! 麦克风采集。
//!
//! 关键设计：**采集流一直开着**，按下热键只是「开始记录」。
//!
//! 之前的实现是按下才 build_input_stream + play，实测要 100–300ms，
//! 用户在按键的瞬间就开始说话了，于是开头几个字必然丢掉 —— 这正是
//! 实测反馈里「按下去前几个字不会被转录」的原因。
//!
//! 现在改成：
//! * 启动时就把流打开，回调持续往一个 300ms 的环形缓冲里写（预录）；
//! * 按下时把预录内容倒进录音缓冲，所以按键前那 300ms 也在；
//! * 同时算出平滑后的电平，供录音指示器做「有没有声音、声音多大」的动画。
//!
//! 隐私边界不变：音频只在本机内存里，不落盘；没在录音时环形缓冲每写一格就丢一格。

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::SampleFormat;

const TARGET_RATE: u32 = 16_000;
/// 预录时长：覆盖「按下热键到真正发声」之间的空隙
const PREROLL_SECS: f32 = 0.35;

pub struct AudioEngine {
    shared: Arc<Shared>,
    _thread: std::thread::JoinHandle<()>,
}

struct Shared {
    /// 是否在录音
    armed: AtomicBool,
    /// 测试模式：忽略真实麦克风，只接受 inject 注入的音频
    mock: AtomicBool,
    /// 录音缓冲（设备原始采样率、单声道）
    buffer: Mutex<Vec<f32>>,
    /// 常驻预录环形缓冲
    preroll: Mutex<VecDeque<f32>>,
    /// 平滑后的电平，f32 的位模式
    level: AtomicU32,
    /// 设备采样率，用于最后重采样
    src_rate: AtomicU32,
}

impl Default for Shared {
    fn default() -> Self {
        Self {
            armed: AtomicBool::new(false),
            mock: AtomicBool::new(false),
            buffer: Mutex::new(Vec::new()),
            preroll: Mutex::new(VecDeque::new()),
            level: AtomicU32::new(0f32.to_bits()),
            src_rate: AtomicU32::new(TARGET_RATE),
        }
    }
}

impl AudioEngine {
    /// 打开设备并持续采集。返回时流已经在跑。
    pub fn start() -> anyhow::Result<Self> {
        let shared = Arc::new(Shared::default());
        let (ready_tx, ready_rx) = mpsc::channel::<anyhow::Result<()>>();

        let sink = shared.clone();
        let thread = std::thread::Builder::new()
            .name("yanchu-audio".into())
            .spawn(move || capture_loop(sink, ready_tx))?;

        match ready_rx.recv_timeout(std::time::Duration::from_secs(5)) {
            Ok(Ok(())) => Ok(Self {
                shared,
                _thread: thread,
            }),
            Ok(Err(e)) => Err(e),
            Err(_) => Err(anyhow::anyhow!("麦克风启动超时")),
        }
    }

    /// 开始记录：把预录的 300ms 倒进录音缓冲，这样按键前的声音也留得住。
    pub fn arm(&self) {
        let preroll: Vec<f32> = self
            .shared
            .preroll
            .lock()
            .map(|p| p.iter().copied().collect())
            .unwrap_or_default();

        if let Ok(mut buffer) = self.shared.buffer.lock() {
            buffer.clear();
            buffer.extend_from_slice(&preroll);
        }
        self.shared.armed.store(true, Ordering::SeqCst);
    }

    pub fn is_armed(&self) -> bool {
        self.shared.armed.load(Ordering::SeqCst)
    }

    /// 当前录音缓冲的快照，供实时识别用。
    pub fn snapshot(&self) -> Vec<f32> {
        self.shared
            .buffer
            .lock()
            .map(|b| b.clone())
            .unwrap_or_default()
    }

    /// 当前录音缓冲的快照（16kHz，可直接喂模型），供实时识别用。
    pub fn snapshot_16k(&self) -> Vec<f32> {
        let raw = self.snapshot();
        let rate = self.shared.src_rate.load(Ordering::Relaxed);
        resample_linear(&raw, rate, TARGET_RATE)
    }

    /// 把一段 16kHz 音频注入录音缓冲。
    ///
    /// 只给测试用：端到端脚本用它按真实节奏「播放」一段 wav，
    /// 这样实时识别、电平、气泡更新的路径都能被自动验证，而不是只测最终落字。
    pub fn inject(&self, samples_16k: &[f32]) {
        let rate = self.shared.src_rate.load(Ordering::Relaxed);
        let at_device_rate = resample_linear(samples_16k, TARGET_RATE, rate);
        self.shared.update_level(&at_device_rate);
        if self.shared.armed.load(Ordering::Relaxed) {
            if let Ok(mut buf) = self.shared.buffer.lock() {
                buf.extend_from_slice(&at_device_rate);
            }
        }
    }

    /// 测试模式：打开后真实麦克风的数据一律丢弃，只认 inject。
    /// 否则 wav 注入和真实采集会同时写进同一个缓冲，音频长度翻倍。
    pub fn set_mock(&self, on: bool) {
        self.shared.mock.store(on, Ordering::SeqCst);
    }

    /// 当前电平（0.0–1.0 量级），供指示器做动画。
    pub fn level(&self) -> f32 {
        f32::from_bits(self.shared.level.load(Ordering::Relaxed))
    }

    /// 停止记录并返回 16kHz 单声道样本。
    pub fn stop(&self) -> Vec<f32> {
        self.shared.armed.store(false, Ordering::SeqCst);
        let raw = self
            .shared
            .buffer
            .lock()
            .map(|mut b| std::mem::take(&mut *b))
            .unwrap_or_default();
        let rate = self.shared.src_rate.load(Ordering::Relaxed);
        resample_linear(&raw, rate, TARGET_RATE)
    }
}

fn capture_loop(shared: Arc<Shared>, ready_tx: Sender<anyhow::Result<()>>) {
    let host = cpal::default_host();
    let Some(device) = host.default_input_device() else {
        let _ = ready_tx.send(Err(anyhow::anyhow!("没有可用的录音设备")));
        return;
    };

    let supported = match device.default_input_config() {
        Ok(c) => c,
        Err(e) => {
            let _ = ready_tx.send(Err(anyhow::anyhow!("读不到录音设备配置: {e}")));
            return;
        }
    };

    let src_rate = supported.sample_rate().0;
    let channels = supported.channels() as usize;
    let sample_format = supported.sample_format();
    let config = supported.config();
    shared.src_rate.store(src_rate, Ordering::Relaxed);

    log::info!(
        "录音设备: {}（{} Hz, {} 声道, {:?}）",
        device.name().unwrap_or_else(|_| "<未知设备>".into()),
        src_rate,
        channels,
        sample_format
    );

    let max_preroll = (src_rate as f32 * PREROLL_SECS) as usize;
    let err_fn = |e| log::error!("音频流出错: {e}");

    let stream = match sample_format {
        SampleFormat::F32 => {
            let sink = shared.clone();
            device.build_input_stream(
                &config,
                move |data: &[f32], _: &_| sink.push_interleaved(data, channels, max_preroll, |s| s),
                err_fn,
                None,
            )
        }
        SampleFormat::I16 => {
            let sink = shared.clone();
            device.build_input_stream(
                &config,
                move |data: &[i16], _: &_| {
                    sink.push_interleaved(data, channels, max_preroll, |s| s as f32 / i16::MAX as f32)
                },
                err_fn,
                None,
            )
        }
        SampleFormat::U16 => {
            let sink = shared.clone();
            device.build_input_stream(
                &config,
                move |data: &[u16], _: &_| {
                    sink.push_interleaved(data, channels, max_preroll, |s| {
                        (s as f32 - 32768.0) / 32768.0
                    })
                },
                err_fn,
                None,
            )
        }
        other => {
            let _ = ready_tx.send(Err(anyhow::anyhow!("不支持的采样格式: {other:?}")));
            return;
        }
    };

    let stream = match stream {
        Ok(s) => s,
        Err(e) => {
            let _ = ready_tx.send(Err(anyhow::anyhow!("打开录音流失败: {e}")));
            return;
        }
    };

    if let Err(e) = stream.play() {
        let _ = ready_tx.send(Err(anyhow::anyhow!("启动录音流失败: {e}")));
        return;
    }

    let _ = ready_tx.send(Ok(()));
    log::info!("采集流已常开（预录 {PREROLL_SECS}s）");

    // 流必须活在这个线程的栈上，park 住即可；
    // 进程退出时随线程一起结束。
    loop {
        std::thread::park();
    }
}

impl Shared {
    fn push_interleaved<T, F>(&self, data: &[T], channels: usize, max_preroll: usize, convert: F)
    where
        F: Fn(T) -> f32,
        T: Copy,
    {
        if data.is_empty() || channels == 0 {
            return;
        }

        // 测试模式下真实麦克风的数据一律丢弃
        if self.mock.load(Ordering::Relaxed) {
            return;
        }

        // 降成单声道
        let mut mono: Vec<f32> = Vec::with_capacity(data.len() / channels);
        for frame in data.chunks(channels) {
            let sum: f32 = frame.iter().map(|s| convert(*s)).sum();
            mono.push(sum / channels as f32);
        }

        self.update_level(&mono);

        // 预录环形缓冲一直更新
        if let Ok(mut pre) = self.preroll.lock() {
            for &s in &mono {
                if pre.len() >= max_preroll {
                    pre.pop_front();
                }
                pre.push_back(s);
            }
        }

        // 录音中才写进正式缓冲
        if self.armed.load(Ordering::Relaxed) {
            if let Ok(mut buf) = self.buffer.lock() {
                buf.extend_from_slice(&mono);
            }
        }
    }

    /// 平滑电平：直接用电平的瞬时值会让指示器抖得看不清
    fn update_level(&self, mono: &[f32]) {
        if mono.is_empty() {
            return;
        }
        let rms = (mono.iter().map(|s| s * s).sum::<f32>() / mono.len() as f32).sqrt();
        let prev = f32::from_bits(self.level.load(Ordering::Relaxed));
        let smoothed = if rms > prev { rms } else { prev * 0.78 + rms * 0.22 };
        self.level.store(smoothed.to_bits(), Ordering::Relaxed);
    }
}

/// 读 wav → 16kHz 单声道 f32。验收工具和「wav 冒充麦克风」共用这一份。
pub(crate) fn read_wav_16k(path: &std::path::Path) -> anyhow::Result<Vec<f32>> {
    let mut reader = hound::WavReader::open(path)?;
    let spec = reader.spec();

    let interleaved: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Int => {
            let max = (1i64 << (spec.bits_per_sample.saturating_sub(1))) as f32;
            reader
                .samples::<i32>()
                .map(|s| s.map(|v| v as f32 / max))
                .collect::<Result<Vec<_>, _>>()?
        }
        hound::SampleFormat::Float => reader.samples::<f32>().collect::<Result<Vec<_>, _>>()?,
    };

    let channels = spec.channels.max(1) as usize;
    let mono: Vec<f32> = if channels == 1 {
        interleaved
    } else {
        interleaved
            .chunks(channels)
            .map(|frame| frame.iter().sum::<f32>() / channels as f32)
            .collect()
    };

    Ok(resample_linear(&mono, spec.sample_rate, TARGET_RATE))
}

/// 线性插值重采样。原型够用，音质要求更高的阶段换 sherpa 自带的 LinearResampler。
pub(crate) fn resample_linear(input: &[f32], from: u32, to: u32) -> Vec<f32> {
    if from == to || input.is_empty() {
        return input.to_vec();
    }
    let ratio = from as f64 / to as f64;
    let out_len = ((input.len() as f64) / ratio).floor() as usize;
    let mut out = Vec::with_capacity(out_len);
    for i in 0..out_len {
        let pos = i as f64 * ratio;
        let idx = pos.floor() as usize;
        let frac = (pos - idx as f64) as f32;
        let a = input.get(idx).copied().unwrap_or(0.0);
        let b = input.get(idx + 1).copied().unwrap_or(a);
        out.push(a + (b - a) * frac);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resample_halves_length() {
        let input = vec![0.0f32; 48_000];
        let out = resample_linear(&input, 48_000, 16_000);
        assert_eq!(out.len(), 16_000);
    }

    #[test]
    fn resample_keeps_rate_when_equal() {
        let input = vec![0.5f32; 100];
        assert_eq!(resample_linear(&input, 16_000, 16_000).len(), 100);
    }
}
