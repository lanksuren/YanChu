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
}
