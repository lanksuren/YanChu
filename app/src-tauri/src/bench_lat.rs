//! 延迟拆分测量：`yanchu.exe --bench-lat <wav>`。
//!
//! 为什么要有这个：`--bench` 只给「一段音频识别花了多少毫秒」，
//! 而 Qwen3-ASR 0.6B 的一次解码 = **encoder（随音频长度线性）+ decoder（随 token 数线性）**，
//! 两者要优化手段完全不同：
//!
//! * encoder 占大头 → 只能缩短音频（尾巴上限）、或在 GPU 上跑；
//! * decoder 占大头 → 只能减少解码次数（tick 节奏）、或把 KV cache 留在设备上。
//!
//! 分离办法：同一段长度下，分别解「真人说话」和「等长静音」。
//! 静音那次的 encoder 成本一样，decoder 只出 1 个 token（EOS），
//! 于是：
//!
//! ```text
//! encoder(秒) ≈ 静音耗时(秒) - 1 个 token 的成本
//! decoder     ≈ 说话耗时(秒) - 静音耗时(秒)，再除以字数
//! ```
//!
//! 注意静音是数字零，比真实静音干净，VAD 特征会略有不同；但只用来估斜率，够用。

use std::path::Path;
use std::time::Instant;

const RATE: usize = 16_000;
/// 测这些长度（秒）。12s 是当前 `MAX_TAIL_SAMPLES`，1s 是 P50 短句。
const DURATIONS: &[usize] = &[1, 2, 4, 6, 8, 12, 16];
/// 每档跑几遍取中位数
const RUNS: usize = 3;

pub fn run(path: &Path) -> anyhow::Result<()> {
    let cfg = crate::asr::AsrConfig::load(&crate::asr::model_root());
    println!(
        "识别模型: {}（{}）threads={}",
        cfg.label(),
        cfg.resolved_dir().display(),
        cfg.thread_count()
    );

    let load_started = Instant::now();
    let mut transcriber = crate::asr::Transcriber::load(&cfg)?;
    println!("模型加载: {}ms", load_started.elapsed().as_millis());

    let samples = crate::audio::read_wav_16k(path)?;
    let total_secs = samples.len() as f32 / RATE as f32;
    println!("音频: {}（{total_secs:.1}s）", path.display());

    // 预热：第一次 decode 会把 ORT session 的懒初始化、内存分配都算进去，
    // 和真实使用（常驻进程）不符，先跑掉。
    let warm = &samples[..samples.len().min(RATE)];
    let warm_started = Instant::now();
    let _ = transcriber.transcribe(warm)?;
    println!("预热 1s: {}ms（不计入结果）\n", warm_started.elapsed().as_millis());

    println!(
        "{:>6} {:>10} {:>12} {:>12} {:>8} {:>10}",
        "秒", "说话ms", "静音ms", "差值ms", "字数", "ms/字"
    );

    let mut speech_points: Vec<(f64, f64)> = Vec::new();
    let mut silence_points: Vec<(f64, f64)> = Vec::new();

    for &secs in DURATIONS {
        let need = secs * RATE;
        if samples.len() < need {
            continue;
        }
        let speech = &samples[..need];
        let silence = vec![0.0f32; need];

        let speech_ms = median_ms(RUNS, || transcriber.transcribe(speech).map(|s| s.len()))?;
        let silence_ms = median_ms(RUNS, || transcriber.transcribe(&silence).map(|s| s.len()))?;

        let text = transcriber.transcribe(speech)?;
        let chars = text.chars().count();

        let diff = speech_ms as f64 - silence_ms as f64;
        let per_char = if chars > 0 {
            format!("{:.1}", diff / chars as f64)
        } else {
            "-".to_string()
        };

        println!(
            "{secs:>6} {speech_ms:>10} {silence_ms:>12} {diff:>12.0} {chars:>8} {per_char:>10}"
        );

        speech_points.push((secs as f64, speech_ms as f64));
        silence_points.push((secs as f64, silence_ms as f64));
    }

    let (a_s, b_s) = fit(&speech_points);
    let (a_e, b_e) = fit(&silence_points);

    println!();
    println!("线性拟合 耗时(ms) = 截距 + 斜率 × 音频秒数");
    println!("  说话: {a_s:.0}ms + {b_s:.0}ms/秒");
    println!("  静音: {a_e:.0}ms + {b_e:.0}ms/秒   ← 斜率≈encoder 成本（每秒音频）");
    println!();
    println!(
        "判读：encoder 斜率 {b_e:.0}ms/秒；说话总斜率 {b_s:.0}ms/秒。\n\
         若 b_e 占 b_s 的大头（>60%），瓶颈在 encoder → 缩短音频/上 GPU；\n\
         否则瓶颈在 decoder → 减解码次数/GPU 上留 KV cache。"
    );

    Ok(())
}

/// 跑 n 遍，返回中位数（毫秒）。闭包返回 (耗时, 结果长度)，只用来避免编译器优化掉调用。
fn median_ms<F>(n: usize, mut f: F) -> anyhow::Result<u64>
where
    F: FnMut() -> anyhow::Result<usize>,
{
    let mut times = Vec::with_capacity(n);
    for _ in 0..n {
        let started = Instant::now();
        let _ = f()?;
        times.push(started.elapsed().as_millis() as u64);
    }
    times.sort_unstable();
    Ok(times[times.len() / 2])
}

/// 最小二乘：y = a + b·x
fn fit(points: &[(f64, f64)]) -> (f64, f64) {
    if points.len() < 2 {
        return (0.0, 0.0);
    }
    let n = points.len() as f64;
    let sum_x: f64 = points.iter().map(|p| p.0).sum();
    let sum_y: f64 = points.iter().map(|p| p.1).sum();
    let sum_xx: f64 = points.iter().map(|p| p.0 * p.0).sum();
    let sum_xy: f64 = points.iter().map(|p| p.0 * p.1).sum();
    let denom = n * sum_xx - sum_x * sum_x;
    if denom.abs() < 1e-9 {
        return (sum_y / n, 0.0);
    }
    let b = (n * sum_xy - sum_x * sum_y) / denom;
    let a = (sum_y - b * sum_x) / n;
    (a, b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_recovers_line() {
        let points = [(1.0, 110.0), (2.0, 210.0), (3.0, 310.0)];
        let (a, b) = fit(&points);
        assert!((a - 10.0).abs() < 1e-6, "a={a}");
        assert!((b - 100.0).abs() < 1e-6, "b={b}");
    }

    #[test]
    fn fit_needs_two_points() {
        assert_eq!(fit(&[(1.0, 5.0)]), (0.0, 0.0));
    }
}
