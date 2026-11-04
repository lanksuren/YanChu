//! 文字处理管线：识别出来的原始文本 → 用户看到的文字。
//!
//! 顺序是有讲究的：
//! 1. 热词纠正先做 —— 它需要在「英文单词还带着空格」的原始形态上做匹配，
//!    一旦润色层动过间距，`time out → timeout` 这类判断就难做了；
//! 2. 规则润色后做 —— 它负责标点、中英间距、口头语这些表面文章；
//! 3. 大润色（LLM）最后做 —— 只在不超时的前提下生效，超时就退回第 2 步的结果。

use std::time::Instant;

use crate::glossary::Glossary;
use crate::llm::{self, Depth, LlmAttempt, LlmConfig};
use crate::rules::Rules;

pub struct Pipeline {
    glossary: Glossary,
    rules: Rules,
    llm: Option<LlmConfig>,
}

/// 只跑前两层的中间结果（气泡里的「每句小润色」用这个）。
pub struct RuleStages {
    pub corrected: String,
    pub ruled: String,
    pub fixes: Vec<(String, String)>,
}

/// 三段结果，控制面板要拿来做前后对比。
/// `llm` 里还留着「为什么没用上大润色」，开发模式下会整段落盘。
pub struct Stages {
    pub corrected: String,
    pub ruled: String,
    pub final_text: String,
    pub fixes: Vec<(String, String)>,
    pub used_llm: bool,
    pub llm: LlmAttempt,
    /// 这次走的是哪一档
    pub depth: Depth,
    /// 热词 + 规则层的耗时（不含大润色）
    pub ms_rule: u64,
    /// 整条管线耗时
    pub ms_total: u64,
}

impl Pipeline {
    pub fn load() -> Self {
        let root = crate::asr::model_root();
        Self {
            glossary: Glossary::load(&root.join("glossary.txt")),
            rules: Rules::load(&root.join("rules.txt")),
            llm: LlmConfig::load(&root),
        }
    }

    /// 只加载词表，不读 llm.json —— 气泡里每句都要用，不能顺带把大润色拖进来。
    pub fn load_rules() -> Self {
        let root = crate::asr::model_root();
        Self {
            glossary: Glossary::load(&root.join("glossary.txt")),
            rules: Rules::load(&root.join("rules.txt")),
            llm: None,
        }
    }

    pub fn with_default_glossary() -> Self {
        Self {
            glossary: Glossary::default(),
            rules: Rules::default(),
            llm: None,
        }
    }

    pub fn llm_enabled(&self) -> bool {
        self.llm.as_ref().map(|c| c.is_configured()).unwrap_or(false)
    }

    /// 控制面板做「只走规则」的对照实验时用。
    pub fn without_llm(mut self) -> Self {
        self.llm = None;
        self
    }

    /// 热词 + 规则层。句子一闭合就跑这一层，结果直接进气泡。
    pub fn rule_only(&self, raw: &str) -> RuleStages {
        let (corrected, fixes) = self.glossary.correct(raw);
        // 用户替换表：热词表管「听错的词」，这里管「固定要改的写法」
        let replaced = self.rules.apply(&corrected);
        let ruled = crate::polish::rule_polish(&replaced);
        RuleStages {
            corrected,
            ruled,
            fixes,
        }
    }

    /// 跑完整条管线（标准档），保留每一步的中间结果。
    pub fn run(&self, raw: &str) -> Stages {
        self.run_with(raw, Depth::Standard)
    }

    /// 跑完整条管线，指定档位。
    ///
    /// 本地模型（Ollama）只有一套提示词：深度档只在云端 API 上开放，
    /// 这里会把档位强制降回标准 —— 免得 UI 上切了深度、实际还在给 2B 喂长提示。
    pub fn run_with(&self, raw: &str, depth: Depth) -> Stages {
        let started = Instant::now();

        let RuleStages {
            corrected,
            ruled,
            fixes,
        } = self.rule_only(raw);
        let ms_rule = started.elapsed().as_millis() as u64;

        let depth = match &self.llm {
            Some(cfg) if cfg.is_local() => Depth::Standard,
            _ => depth,
        };

        // 交给模型的文本：默认用「规则层之后」的版本 —— 口头语已经被小润色删掉，
        // 输入更短，2B 也不用再分心处理「嗯/呃」。
        // 设 YANCHU_LLM_INPUT=corrected 可以回到「未删语气词」的版本做对照。
        let prefer_corrected = matches!(
            std::env::var("YANCHU_LLM_INPUT").ok().as_deref(),
            Some("corrected")
        );
        let llm_input: &str = if prefer_corrected { &corrected } else { &ruled };

        let attempt = match &self.llm {
            // 短句 + 规则层已经干净 → 不打扰模型。依据见 `settled_enough` 的注释：
            // 这类文本在实测里模型改了等于没改，却要等 400–600ms。
            Some(cfg) if cfg.is_local() && settled_enough(llm_input) => {
                log::info!("大润色跳过：短句且规则层已干净（{llm_input}）");
                llm::LlmAttempt::skipped(depth)
            }
            Some(cfg) => llm::polish(llm_input, cfg, depth),
            None => LlmAttempt::skipped(depth),
        };

        // 用不上就退回规则层结果 —— 这里不报错、不提示，用户不该感知到
        let (raw_final, used_llm) = match attempt.text() {
            Some(t) => (t.to_string(), true),
            None => (ruled.clone(), false),
        };

        // 兜底排版：原文明显在列举（第一步 / 一个是 / 如果×3）时排成 1. 2. 3.。
        // 2B 判断不了「该不该分点」，但标记在文本里是稳定的，规则切更可靠。
        let final_text = crate::polish::format_enumerations(&raw_final);

        Stages {
            corrected,
            ruled,
            final_text,
            fixes,
            used_llm,
            llm: attempt,
            depth,
            ms_rule,
            ms_total: started.elapsed().as_millis() as u64,
        }
    }

    /// 返回（最终文本，改动列表）。改动列表用来统计误伤。
    pub fn cleanup(&self, raw: &str) -> (String, Vec<(String, String)>) {
        let stages = self.run(raw);
        if stages.used_llm {
            log::info!("大润色生效: {} 字", stages.final_text.chars().count());
        } else if let Some(reason) = &stages.llm.reason {
            log::info!("大润色回退（{reason}）");
        }
        (stages.final_text, stages.fixes)
    }
}

/// 短句 + 规则层已经干净 → 不值得等一次大润色。
///
/// 依据（2026-11-04 实测，本机 qwen3.5:2b，`--polish-test` 与 devlog 对拍）：
///
/// * 27 字的真实会话连续 7 次，模型输出与规则层**逐字相同**，每次却要等 500–600ms；
/// * 8 条多样短句里，模型真正改对的只有三类：
///   补问号（「…是为什么？」）、删句首的「这个/那个」口头语、一口气说话时补连接词前的逗号 ——
///   而这三类恰好是规则层现在做不到、只有模型能做的；
/// * 反过来说，**只要这三类症状一个都不出现**，短句交给模型就是纯等待。
///
/// 所以判据不是「字数少于 N」，而是「规则层收拾完了没有」：
/// 有症状就交给模型，没症状就省掉这 400ms。长文本一律交给模型（分段、整理有价值）。
fn settled_enough(text: &str) -> bool {
    if !short_skip_enabled() {
        return false;
    }
    if text.chars().count() >= SETTLED_MAX_CHARS {
        return false;
    }
    !has_question_without_mark(text)
        && !starts_with_demonstrative(text)
        && !has_run_on_conjunction(text)
}

/// 超过这个字数就不算「短句」，一律交给大润色。
const SETTLED_MAX_CHARS: usize = 30;

/// 关掉「短句跳过」：`YANCHU_LLM_SETTLED=0`（做 A/B 用）。
fn short_skip_enabled() -> bool {
    !matches!(
        std::env::var("YANCHU_LLM_SETTLED").ok().as_deref(),
        Some("0") | Some("off") | Some("false")
    )
}

/// 句尾是疑问词却没有问号：「…是为什么。」这种，模型会补成「？」。
fn has_question_without_mark(text: &str) -> bool {
    const TAILS: &[&str] = &[
        "为什么", "怎么样", "怎么办", "是不是", "有没有", "多少", "几点", "多久", "吗", "呢",
    ];
    let body = text.trim_end_matches(['。', '，', '、', ' ', '\n']);
    if body.ends_with('？') || body.ends_with('?') {
        return false;
    }
    TAILS.iter().any(|w| body.ends_with(w))
}

/// 句首残留的「这个/那个」口头语：「那个他说明天上线」。
///
/// 只认后面紧跟人称代词（或再一个指示词）的写法；
/// 「这个 bug」「这个方案」是指示代词，必须保留，不能当成待收拾的症状。
fn starts_with_demonstrative(text: &str) -> bool {
    let chars: Vec<char> = text.trim_start().chars().collect();
    chars.len() >= 3
        && (chars[0] == '这' || chars[0] == '那')
        && chars[1] == '个'
        && matches!(chars[2], '我' | '你' | '咱' | '他' | '她' | '这' | '那')
}

/// 一口气说完、连接词前没有标点：「跑完然后重启」。
///
/// 只收录「逗号是惯例」的几个；「因为」故意不在里面 ——
/// 「返回空是因为缓存没命中」不加逗号才是对的。
fn has_run_on_conjunction(text: &str) -> bool {
    const CONJ: &[&str] = &["然后", "但是", "所以", "不过", "而且", "另外"];
    const PUNCT: &[char] = &['，', '。', '？', '！', '、', '；', '：', ',', '?', '!', ';', ':'];

    let chars: Vec<char> = text.chars().collect();
    for word in CONJ {
        let w: Vec<char> = word.chars().collect();
        if chars.len() <= w.len() {
            continue;
        }
        for i in 1..=chars.len() - w.len() {
            if chars[i..i + w.len()] == w[..] && !PUNCT.contains(&chars[i - 1]) {
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn end_to_end_cleanup() {
        let pipeline = Pipeline::with_default_glossary();
        let (out, _) = pipeline.cleanup("嗯，把这段逻辑抽成一个 Helber 函数放到 Uils 里");
        assert!(out.contains("helper"), "got {out}");
        assert!(out.contains("utils"), "got {out}");
        assert!(!out.starts_with("嗯"), "口头语没删掉: {out}");
        assert!(out.ends_with('。'), "没补句号: {out}");
    }

    #[test]
    fn keeps_term_spacing() {
        let pipeline = Pipeline::with_default_glossary();
        let (out, _) = pipeline.cleanup("记得检查 healthth check");
        assert!(out.contains("health check"), "got {out}");
    }

    #[test]
    fn stages_expose_intermediate_results() {
        let pipeline = Pipeline::with_default_glossary();
        let s = pipeline.run("嗯，这个 Helber 函数");
        assert!(s.raw_free_of_filler(), "规则层应删掉句首语气词: {}", s.ruled);
        assert!(s.final_text.contains("helper"));
        assert!(!s.used_llm, "没有配置 LLM 时不应标记为使用过");
    }

    #[test]
    fn stages_keep_llm_verdict_for_diagnostics() {
        let pipeline = Pipeline::with_default_glossary().without_llm();
        let s = pipeline.run("嗯，这个函数的入参要校验一下");
        assert_eq!(s.llm.outcome(), "skipped", "没配置大润色应记成 skipped");
        assert!(s.ms_rule <= s.ms_total, "分段耗时不应当超过总耗时");
    }

    impl Stages {
        fn raw_free_of_filler(&self) -> bool {
            !self.ruled.starts_with('嗯')
        }
    }

    /// 2026-11-04 的 8 条实测短句：模型改了 5 条、改了等于没改 3 条。
    ///
    /// 判据的要求是：模型改对的那 5 条必须被判成「还没收拾干净」，
    /// 只有模型纯属白跑的那 3 条才允许跳过 —— 这样跳过就换不来质量损失。
    #[test]
    fn settled_gate_matches_measured_value() {
        let needs_model = [
            "帮我查一下这个接口返回值为空是为什么。", // 缺问号
            "这个我们先跑一遍迁移然后重启。",         // 句首这个 + 连接词无标点
            "我觉得这个方案可以但是需要再确认一下。", // 连接词无标点
            "先把迁移跑完然后重启服务。",             // 连接词无标点
            "那个他说明天上线。",                     // 句首那个 + 人称
        ];
        for t in needs_model {
            assert!(!settled_enough(t), "应该交给模型: {t}");
        }

        let skip_ok = [
            "这个接口返回空是因为缓存没有命中。",
            "整一下这个需求。",
            "这个 bug 只在 production 出现。", // 「这个」修饰名词，不是口头语
        ];
        for t in skip_ok {
            assert!(settled_enough(t), "应该跳过模型: {t}");
        }
    }

    #[test]
    fn settled_gate_keeps_long_text_on_model() {
        let long =
            "我们这次上线要做的事情很多，先把数据库迁移跑完，再重启服务，最后把日志里的报错整理一下发给我。";
        assert!(!settled_enough(long), "长文本一律交给模型");
    }

    #[test]
    fn settled_gate_can_be_disabled() {
        std::env::set_var("YANCHU_LLM_SETTLED", "0");
        let decided = settled_enough("整一下这个需求。");
        std::env::remove_var("YANCHU_LLM_SETTLED");
        assert!(!decided, "关掉开关后不该跳过");
    }
}
