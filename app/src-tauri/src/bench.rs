//! 验收工具：`yanchu.exe --bench <目录>` 离线跑一批 wav，输出 CER、术语保留率和延迟分布。
//!
//! 这个模式不走麦克风也不开窗口，纯粹量识别层 —— 对应产品定义第十节「首个 spike 的验收标准」。
//! 每台机器都能复现，比「说一遍感觉还行」可靠。

use std::path::{Path, PathBuf};
use std::time::Instant;

const TARGET_RATE: u32 = 16_000;

pub fn run(dir: &Path) -> anyhow::Result<()> {
    anyhow::ensure!(dir.is_dir(), "不是目录: {}", dir.display());

    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().map(|x| x == "wav").unwrap_or(false))
        .collect();
    files.sort();

    anyhow::ensure!(!files.is_empty(), "目录里没有 wav: {}", dir.display());

    // 识别模型由 models/asr.json 决定；命令行可用 YANCHU_ASR_ENGINE 覆盖。
    let asr_cfg = crate::asr::AsrConfig::load(&crate::asr::model_root());
    println!(
        "识别模型: {}（{}）",
        asr_cfg.label(),
        asr_cfg.resolved_dir().display()
    );
    let mut transcriber = crate::asr::Transcriber::load(&asr_cfg)?;

    let mut latencies: Vec<u64> = Vec::with_capacity(files.len());
    let mut cers: Vec<f64> = Vec::with_capacity(files.len());
    let mut cers_raw: Vec<f64> = Vec::with_capacity(files.len());
    let mut term_hits = 0usize;
    let mut term_total = 0usize;
    let mut fix_count = 0usize;
    let mut fix_wrong = 0usize;

    // 只跑词表 + 规则层：对比 ASR 时不能让大润色把差异抹平
    let pipeline = crate::pipeline::Pipeline::load_rules();

    for wav in &files {
        let samples = crate::audio::read_wav_16k(wav)?;
        let audio_secs = samples.len() as f32 / TARGET_RATE as f32;

        let started = Instant::now();
        let segments = transcriber.transcribe_segments(&samples)?;
        let text = segments.join("");
        let ms = started.elapsed().as_millis() as u64;

        let reference = std::fs::read_to_string(wav.with_extension("txt"))
            .unwrap_or_default()
            .trim()
            .to_string();

        // 用户实际看到的是走完润色管线的文本，CER 以它为准
        let (final_text, fixes) = pipeline.cleanup(&text);

        // 误替换判定：改成的那个词如果根本没出现在参考文本里，就是误伤
        let ref_lower = reference.to_lowercase();
        for (_, to) in &fixes {
            fix_count += 1;
            let needle = to.to_lowercase();
            if !ref_lower.contains(needle.as_str()) {
                fix_wrong += 1;
            }
        }

        let cer_raw = cer_of(&reference, &text);
        let cer = cer_of(&reference, &final_text);
        let (hit, total) = term_preservation(&reference, &final_text);
        term_hits += hit;
        term_total += total;

        latencies.push(ms);
        cers.push(cer);
        cers_raw.push(cer_raw);

        println!(
            "{}",
            serde_json::json!({
                "file": wav.file_name().and_then(|s| s.to_str()).unwrap_or("?"),
                "audio_secs": (audio_secs * 10.0).round() / 10.0,
                "ms": ms,
                "rtf": (ms as f32 / 1000.0 / audio_secs.max(0.01) * 100.0).round() / 100.0,
                "cer_raw": (cer_raw * 1000.0).round() / 1000.0,
                "cer": (cer * 1000.0).round() / 1000.0,
                "fixes": fixes.iter().map(|(a, b)| format!("{a}→{b}")).collect::<Vec<_>>(),
                "terms_kept": hit,
                "terms_total": total,
                "ref": reference,
                "hyp_raw": text,
                "hyp": final_text,
            })
        );
    }

    let n = cers.len();
    let mean_cer = cers.iter().sum::<f64>() / n as f64;
    let mean_cer_raw = cers_raw.iter().sum::<f64>() / n as f64;
    let p95_ms = percentile(&mut latencies, 95.0);
    let p50_ms = percentile(&mut latencies, 50.0);

    println!();
    println!("=== 汇总（{} 段） ===", n);
    println!(
        "识别模型          : {}（{}）",
        asr_cfg.label(),
        asr_cfg.resolved_dir().display()
    );
    println!(
        "字错率 CER        : {:.1}%（原始 {:.1}%）",
        mean_cer * 100.0,
        mean_cer_raw * 100.0
    );
    println!(
        "英文术语保留率    : {:.0}% ({}/{})",
        if term_total == 0 {
            100.0
        } else {
            term_hits as f64 / term_total as f64 * 100.0
        },
        term_hits,
        term_total
    );
    println!("单段识别 P50/P95  : {}ms / {}ms", p50_ms, p95_ms);
    println!(
        "热词纠正          : {} 次，误替换 {} 次（{:.0}%）",
        fix_count,
        fix_wrong,
        if fix_count == 0 {
            0.0
        } else {
            fix_wrong as f64 / fix_count as f64 * 100.0
        }
    );

    Ok(())
}

/// 比对前的归一化。三件事：
/// 1. 去 BOM —— 参考文本是带 BOM 的 UTF-8，不剥掉会平白多一个「错字」；
/// 2. 去标点和空白 —— 标点差异不算错字；
/// 3. 数字统一成阿拉伯数字 —— 模型开了 ITN，「二十四小时」会写成「24小时」，
///    两种写法都对，不能算错。这两个口径不修，CER 会明显虚高。
fn normalize(text: &str) -> Vec<char> {
    let cleaned: Vec<char> = text
        .chars()
        .filter(|c| {
            !c.is_whitespace()
                && *c != '\u{feff}'
                && !matches!(
                    c,
                    '。' | '，'
                        | '、'
                        | '？'
                        | '！'
                        | '；'
                        | '：'
                        | '…'
                        | '—'
                        | '.'
                        | '?'
                        | '!'
                        | ';'
                        | ':'
                )
        })
        .collect();

    canonicalize_numbers(&cleaned)
}

fn is_cn_numeral(c: char) -> bool {
    matches!(c, '零' | '〇' | '一' | '二' | '三' | '四' | '五' | '六' | '七' | '八' | '九' | '十' | '百' | '千' | '万')
}

/// 把中文数字串换成阿拉伯数字（支持 0–9999），阿拉伯数字原样保留。
fn canonicalize_numbers(input: &[char]) -> Vec<char> {
    let mut out = Vec::with_capacity(input.len());
    let mut i = 0;

    while i < input.len() {
        if is_cn_numeral(input[i]) {
            let start = i;
            while i < input.len() && is_cn_numeral(input[i]) {
                i += 1;
            }
            let run: String = input[start..i].iter().collect();
            match parse_cn_number(&run) {
                Some(n) => out.extend(n.to_string().chars()),
                None => out.extend(run.chars()),
            }
        } else if input[i].is_ascii_digit() {
            out.push(input[i]);
            i += 1;
        } else {
            out.push(input[i]);
            i += 1;
        }
    }
    out
}

fn parse_cn_number(s: &str) -> Option<u64> {
    let mut total = 0u64;
    let mut section = 0u64;
    let mut number = 0u64;

    for c in s.chars() {
        match c {
            '零' | '〇' => {}
            // 不能用 '一'..='九'：汉字码位不按数值排序（二 = U+4E8C 排在 九 = U+4E5D 之后），
            // 那样会漏掉二四五六八。只能一个个列出来。
            '一' | '二' | '三' | '四' | '五' | '六' | '七' | '八' | '九' => {
                number = match c {
                    '一' => 1,
                    '二' => 2,
                    '三' => 3,
                    '四' => 4,
                    '五' => 5,
                    '六' => 6,
                    '七' => 7,
                    '八' => 8,
                    '九' => 9,
                    _ => unreachable!(),
                }
            }
            '十' => {
                if number == 0 {
                    number = 1;
                }
                section += number * 10;
                number = 0;
            }
            '百' => {
                section += number * 100;
                number = 0;
            }
            '千' => {
                section += number * 1000;
                number = 0;
            }
            '万' => {
                total += (section + number) * 10_000;
                section = 0;
                number = 0;
            }
            _ => return None,
        }
    }
    Some(total + section + number)
}

fn cer_of(reference: &str, hypothesis: &str) -> f64 {
    let r = normalize(reference);
    if r.is_empty() {
        return 0.0;
    }
    let h = normalize(hypothesis);
    edit_distance(&r, &h) as f64 / r.len() as f64
}

fn edit_distance(a: &[char], b: &[char]) -> usize {
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];

    for (i, ca) in a.iter().enumerate() {
        cur[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            cur[j + 1] = (prev[j + 1] + 1).min(cur[j] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

/// 参考文本里的英文术语，有多少原样出现在识别结果里
fn term_preservation(reference: &str, hypothesis: &str) -> (usize, usize) {
    let terms: Vec<String> = reference
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| w.len() > 1 && w.chars().any(|c| c.is_ascii_alphabetic()))
        .map(|w| w.to_lowercase())
        .collect();

    let hyp_lower = hypothesis.to_lowercase();
    let hits = terms.iter().filter(|t| hyp_lower.contains(t.as_str())).count();
    (hits, terms.len())
}

fn percentile(values: &mut [u64], p: f64) -> u64 {
    if values.is_empty() {
        return 0;
    }
    values.sort_unstable();
    let rank = (p / 100.0 * (values.len() - 1) as f64).round() as usize;
    values[rank.min(values.len() - 1)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cer_is_zero_for_identical() {
        assert_eq!(cer_of("你好世界", "你好世界"), 0.0);
    }

    #[test]
    fn ignores_punctuation_and_space() {
        assert_eq!(cer_of("你好，世界。", "你好世界"), 0.0);
    }

    #[test]
    fn counts_substitution() {
        // 「今天天气不错」6 个字，「不错」→「不好」差 1 个字 = 1/6
        let cer = cer_of("今天天气不错", "今天天气不好");
        assert!((cer - 1.0 / 6.0).abs() < 1e-9, "got {cer}");
    }

    #[test]
    fn detects_english_terms() {
        let (hit, total) = term_preservation("用 Rust 写一个 demo", "用 Rust 写一个 demo");
        assert_eq!((hit, total), (2, 2));

        let (hit, total) = term_preservation("用 Rust 写一个 demo", "用拉斯特点写一个呆萌");
        assert_eq!((hit, total), (0, 2));
    }

    #[test]
    fn percentile_picks_upper_rank() {
        let mut v = vec![10, 20, 30, 40, 50];
        assert_eq!(percentile(&mut v, 50.0), 30);
        assert_eq!(percentile(&mut v, 95.0), 50);
    }

    #[test]
    fn ignores_utf8_bom() {
        // 参考文本是 Set-Content -Encoding UTF8 写出来的，开头带 BOM
        assert_eq!(cer_of("\u{feff}帮我查一下", "帮我查一下"), 0.0);
    }

    #[test]
    fn treats_chinese_and_arabic_numerals_alike() {
        assert_eq!(cer_of("缓存过期时间设成二十四小时", "缓存过期时间设成24小时"), 0.0);
        assert_eq!(cer_of("从三十秒改成六十秒", "从30秒改成60秒"), 0.0);
        assert_eq!(cer_of("最大连接数调到五十", "最大连接数调到50"), 0.0);
    }

    #[test]
    fn still_counts_wrong_numbers() {
        // 数字写错还是要算错
        assert!(cer_of("改成六十秒", "改成30秒") > 0.0);
    }

    #[test]
    fn parses_chinese_numbers() {
        assert_eq!(parse_cn_number("二十四"), Some(24));
        assert_eq!(parse_cn_number("三十"), Some(30));
        assert_eq!(parse_cn_number("五十"), Some(50));
        assert_eq!(parse_cn_number("三"), Some(3));
        assert_eq!(parse_cn_number("一百零八"), Some(108));
    }
}

/// `--record-wav <路径> [秒数] [前置毫秒]`：用真实麦克风录一段，写成 16kHz 单声道 wav。
///
/// 只负责录，不负责节奏和句子 —— 那些交给 `tools/record-corpus.ps1`。
/// 之所以单独做一个模式：`bench/RESULTS.md` 里的所有数字都来自 TTS 合成语料，
/// 真人声音这一环一直没验过（`--mic-check` 在本机还测出全零）。
///
/// 输出里带 RMS 和峰值，是为了让「设备打开了但收到数字静音」这种情况当场暴露，
/// 而不是录完 20 句才发现全是零。
pub fn record_wav(path: &Path, secs: f32, lead_ms: u64) -> anyhow::Result<()> {
    anyhow::ensure!(secs > 0.5 && secs < 120.0, "录音时长不合理: {secs}s");

    let engine = crate::audio::AudioEngine::start()?;
    // 刚打开的流前几帧可能是空的，先让它跑一会儿
    std::thread::sleep(std::time::Duration::from_millis(lead_ms));
    engine.arm();
    std::thread::sleep(std::time::Duration::from_secs_f32(secs));
    let samples = engine.stop();

    let n = samples.len();
    anyhow::ensure!(n > 0, "没采到任何采样");

    let peak = samples.iter().fold(0f32, |m, s| m.max(s.abs()));
    let rms = (samples.iter().map(|s| s * s).sum::<f32>() / n as f32).sqrt();

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    crate::devlog::write_wav(path, &samples)?;

    println!(
        "录音完成: {}  （{:.1}s，{} 采样，RMS {:.4}，峰值 {:.4}）",
        path.display(),
        n as f32 / TARGET_RATE as f32,
        n,
        rms,
        peak
    );

    // 这两个阈值不是为了严格判定，是为了让「麦克风其实是哑的」立刻可见
    if peak < 0.001 {
        println!("警告: 峰值接近 0，麦克风可能被硬件静音，或系统噪声门把它压成了数字静音");
    } else if rms < 0.002 {
        println!("警告: 电平极低，离麦克风太远或增益太小");
    }
    Ok(())
}
