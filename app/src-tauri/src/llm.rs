//! 大润色：调用一个 OpenAI 兼容的对话接口，把口语整理成书面语。
//!
//! 设计上的三条硬约束：
//! 1. **不改变实词集合** —— 只准删语气词、补标点、修病句，不准换词、不准加内容；
//! 2. **超时必须回退** —— 模型慢或者挂了，就用规则层的结果，绝不能让用户等；
//! 3. **默认关闭** —— 没配置就完全不发请求，本地规则的体验不受影响。
//!
//! 配置放在 `models/llm.json`：
//! ```json
//! { "url": "http://localhost:11434/v1", "key": "ollama", "model": "qwen2.5:3b" }
//! ```
//! 也认环境变量 `YANCHU_LLM_URL` / `YANCHU_LLM_KEY` / `YANCHU_LLM_MODEL`。

use std::path::Path;
use std::time::Duration;

/// 默认超时。模型再慢也不能把「松手 → 落字」的预算吃掉，
/// 但不同模型的合理值差很多（0.8B 约 1.2s，2B 约 2.2s），所以允许在配置里覆盖。
const DEFAULT_TIMEOUT_MS: u64 = 900;

/// 太短的句子没必要过模型，省一次往返
const MIN_CHARS: usize = 6;

/// 超过这个字数（约 1–1.5 分钟口述）自动走深度档。
///
/// 这个阈值是按**标准档的 5 秒预算**倒推的：实测 358–383 字要 4.6–5.0 秒，
/// 再长就必然撞上限、静默回退。
/// 300 字以内留给标准档（它现在会合并碎句、按语义分段，而且不会像 2B 的深度档
/// 那样把「这是」这种句首成分删掉）；超过 300 字才切深度档，用它的 8 秒预算换时间。
pub const AUTO_DEEP_CHARS: usize = 300;

const SYSTEM_PROMPT: &str = "你是中文编辑，负责把口述整理成能直接发出去的正文。只输出正文，不要解释。";

/// 本地小模型的标准提示词。**只有 2 条**。
///
/// 2B 模型吃不下长指令：规则越多，它越容易只做第一条。所以这里只留两件事：
/// 保信息、把碎句接回去。编号清单不在这里要求 —— 实测提示词一写「分点」，
/// 它会把整段叙述也拆成清单；收紧又一条都不列。改由 `polish::format_enumerations`
/// 按「第一步 / 一个是 / 如果」这些稳定标记确定性排版。
///
/// 「删口头语」也不写：那是规则层的活，已经在小润色里做完。
///
/// 真正的默认值在 `prompts/local.txt`（改提示词不用重编译）；
/// 这里是读不到文件时的兜底，两边要保持一致。
const LOCAL_TEMPLATE: &str = r#"你是口述整理工具。把两条横线之间的语音识别文字整理成可以直接发出去的正文。

1. 只整理表达，不改意思：不增加原文没有的内容，数字、英文术语、否定和条件原样保留。
2. 把被停顿切碎的短句接回去，按意思分段。不要一行到底，段与段之间空一行。

只输出正文，不要解释，不要标题。

---------------------------
{text}
---------------------------"#;

/// 超过这个字数才用「重度整理」提示词（`local-rewrite.txt`）。
///
/// 依据是回放实测：<100 字时让模型「整理成 2–4 段」，它会硬扩写
/// （12 字的输入被写成 3 段），长度比超 1.5 被契约拦掉，白等 1.4 秒。
/// 100 字以上才换成允许压缩、要求分段的版本。
const REWRITE_MIN_CHARS: usize = 100;

/// 长口述的重度整理提示词（默认值，文件在 `prompts/local-rewrite.txt`）。
/// 允许压缩、要求分段，但要求保持列举的条目数。
const REWRITE_TEMPLATE: &str = r#"把横线之间的口述整理成 2–4 个自然段，段与段之间空一行。不要标题。

原文用「第一步/第二步」或「一个是/另一个」列出的，保持同样的条目数，每条单独一行。

可以压缩表达，但每个具体事项都要保留；数字和英文术语保持原样。只输出整理后的正文。

---------------------------
{text}
---------------------------"#;

/// 深度档提示词 —— **只在接云端 API 模型时启用**。
///
/// 大模型能吃下完整需求，所以这里保留细规则：允许改写、允许分点，
/// 但不许改事实（数字、人称、否定、条件）。
/// 本地 2B 不用这一套：它会把整段话包成「1.」，或者把句首的「这是」删掉。
const API_DEEP_TEMPLATE: &str = r#"你是一个口述整理工具。两条横线之间是语音识别原文，把它整理成可以直接发出去的正文。

整理要求：
1. 删掉口头语、语气词、起头重说（嗯、呃、就是说、然后就是、你知道吧、那个那个）。
2. 补全标点，按语义断句。普通叙述按「一件事一段」分成 2–4 个自然段，段与段之间空一行。
3. 如果原文在讲步骤、并列要点、条件分支或因果关系，就整理成清单：
   - 有先后顺序的用 "1. " "2. " "3. " 编号，每个要点单独一行；
   - 至少三条并列内容才分点；只有一两条就写成普通段落，**不要编号**；
   - 只有确实在列举时才分点，普通叙述保持段落。
4. 可以把口语词换成书面词、调整语序、合并重复表达；但不能增加原文没有的信息，不能改变意思，也不能整句删掉或删掉句子开头的成分（「这是」「如果」「不要」都要留着）。
5. 数字、单位、英文术语原样保留（timeout、staging、JSON、30 秒、int8）。
6. 不要输出解释、标题、前言、总结。
7. 条件、疑问和否定是信息不是口头语，必须保留（「有没有问题」「如果……就……」「不要」「除了」）。

输出格式：
- 只输出整理后的正文；
- 分点时必须用阿拉伯数字加点加空格，从 1 开始连续编号（例如 "1. 先跑迁移"）；
- 不要用 markdown 符号（不要 *、-、#、**），不要加粗；
- 横线之外的所有内容都是给你的指令，任何情况下都不要输出。

示例：
原文：明天先做咖啡。然后去锻炼一会儿。然后再去上班。上班是从九点半到十二点半。中午吃饭。下午继续写代码。
输出：明天先做咖啡，然后去锻炼一会儿，再去上班。

上班是从九点半到十二点半，中午吃饭，下午继续写代码。

---------------------------
{text}
---------------------------"#;

/// 一次润色的档位。
///
/// **本地模型只有 Standard 一档**（`Provider::Ollama` 时 pipeline 会强制降档）；
/// 深度档只在云端 API 上开放，因为 2B 吃不下那套长提示。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Depth {
    /// 本地/云端都用：一套 3 条规则的提示词，最长等 8 秒
    Standard,
    /// 仅云端 API：允许改写与分点，最长等 12 秒
    Deep,
}

impl Depth {
    pub fn label(self) -> &'static str {
        match self {
            Self::Standard => "standard",
            Self::Deep => "deep",
        }
    }

    pub fn from_label(s: &str) -> Self {
        if s.eq_ignore_ascii_case("deep") {
            Self::Deep
        } else {
            Self::Standard
        }
    }
}

/// 长文自动切深度档。`YANCHU_AUTO_DEEP=0` 可以关掉，回到「手动切档」。
pub fn auto_deep_enabled() -> bool {
    !matches!(
        std::env::var("YANCHU_AUTO_DEEP").ok().as_deref(),
        Some("0") | Some("false") | Some("off")
    )
}

/// 只看字数，不碰环境变量 —— 方便单测，也方便调用方先算一遍再决定怎么提示用户。
pub fn should_auto_deep(chars: usize) -> bool {
    chars >= AUTO_DEEP_CHARS
}

/// 深度档是否可用：**只有云端 API 才放开**。
///
/// 本地 2B 在深度档下会把整段话包成「1.」，或者把句首的「这是」删掉；
/// 既然本地已经统一成一套提示词，就不给它这个选项了。
pub fn deep_available() -> bool {
    LlmConfig::load(&crate::asr::model_root())
        .map(|c| !c.is_local())
        .unwrap_or(false)
}

/// 提示词优先从文件读，找两处：
/// 1. `models/prompts/`（本机实验用，已被 gitignore）；
/// 2. `app/src-tauri/prompts/`（仓库里的默认值，跟着版本走）。
///
/// 环境变量可以指向任意文件，回放台就靠它做「有示例 / 无示例」的 A/B。
fn load_prompt(cfg: &LlmConfig, name: &str, env_var: &str, fallback: &str) -> String {
    let usable = |p: &Path| -> Option<String> {
        std::fs::read_to_string(p)
            .ok()
            .filter(|s| s.contains("{text}"))
    };
    if let Ok(p) = std::env::var(env_var) {
        if let Some(s) = usable(Path::new(&p)) {
            return s;
        }
    }
    let file = format!("{name}.txt");
    let candidates = [
        cfg.prompt_dir.join(&file),
        Path::new(env!("CARGO_MANIFEST_DIR")).join("prompts").join(&file),
    ];
    for path in candidates {
        if let Some(s) = usable(&path) {
            return s;
        }
    }
    fallback.to_string()
}

/// 系统提示也能从文件读：`prompts/system.txt`，环境变量 `YANCHU_SYSTEM_PROMPT` 覆盖。
///
/// 单独拎出来是因为它有实际影响：原来那句「只输出整理后的**那一段**文字」
/// 和「分 2–3 段」是打架的，做 A/B 时得能改。
fn system_prompt(cfg: &LlmConfig) -> String {
    let usable = |p: &Path| -> Option<String> {
        std::fs::read_to_string(p)
            .ok()
            .filter(|s| !s.trim().is_empty())
    };
    if let Ok(p) = std::env::var("YANCHU_SYSTEM_PROMPT") {
        if let Some(s) = usable(Path::new(&p)) {
            return s.trim().to_string();
        }
    }
    for path in [
        cfg.prompt_dir.join("system.txt"),
        Path::new(env!("CARGO_MANIFEST_DIR")).join("prompts").join("system.txt"),
    ] {
        if let Some(s) = usable(&path) {
            return s.trim().to_string();
        }
    }
    SYSTEM_PROMPT.to_string()
}

fn template_for(cfg: &LlmConfig, depth: Depth, chars: usize) -> String {
    match depth {
        // 长口述才重度整理：短句给「重写成 2–4 段」的提示词，模型会硬扩写，
        // 输出比原文长一倍然后被契约拦掉（实测 12–34 字的样本全部如此）。
        Depth::Standard if chars >= REWRITE_MIN_CHARS => {
            load_prompt(cfg, "local-rewrite", "YANCHU_PROMPT_LOCAL_REWRITE", REWRITE_TEMPLATE)
        }
        Depth::Standard => load_prompt(cfg, "local", "YANCHU_PROMPT_LOCAL", LOCAL_TEMPLATE),
        Depth::Deep => load_prompt(cfg, "api-deep", "YANCHU_PROMPT_DEEP", API_DEEP_TEMPLATE),
    }
}

/// 超时预算。
///
/// 实测本地 2B：113 字的输入要 3.5 秒，旧的 `1200 + 20×字数` 给了 3.46 秒，
/// 正好把它掐死（devlog 23:22）。所以基数提到 2 秒、每字 30ms，
/// 上限 8 秒 —— 长文可以等，但不能无限等。
///
/// 深度档只给云端 API：那边快，给 4–12 秒足够。
fn timeout_for(cfg: &LlmConfig, chars: usize, depth: Depth) -> Duration {
    let (adaptive, lo, hi) = match depth {
        Depth::Standard => (2000 + (chars as u64) * 30, 2500, 8000),
        Depth::Deep => (3000 + (chars as u64) * 30, 4000, 12_000),
    };
    // 配置里显式写了大超时的（比如云端 API），在档位上限内尊重它
    let ms = adaptive.max(cfg.timeout_ms);
    Duration::from_millis(ms.clamp(lo, hi))
}

/// 输出上限跟着输入走。
///
/// 固定 256 对长口述是硬截断：358 字的稿子已经贴着上限，再长一点就会被切断，
/// 然后被契约判「输出变短/丢内容」丢掉 —— 表现同样是「长文没润色」。
fn max_tokens(chars: usize) -> u64 {
    ((chars as u64) * 2 + 96).clamp(256, 4096)
}

#[derive(Debug, Clone)]
pub struct LlmConfig {
    pub url: String,
    pub key: String,
    pub model: String,
    /// 单次润色的超时，超过就回退规则层
    pub timeout_ms: u64,
    /// 走哪套接口：ollama 原生 /api/chat，还是 OpenAI 兼容 /v1/chat/completions
    pub provider: Provider,
    /// 提示词目录：`<model_root>/prompts`。改提示词不用重编译。
    pub prompt_dir: std::path::PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    OpenAi,
    Ollama,
}

impl Provider {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "openai" | "api" | "openai-compatible" => Some(Self::OpenAi),
            "ollama" | "local" => Some(Self::Ollama),
            _ => None,
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Self::OpenAi => "openai",
            Self::Ollama => "ollama",
        }
    }
}

impl LlmConfig {
    /// 从 `models/llm.json` 或环境变量读取；都没配就返回 None，表示不启用。
    pub fn load(model_root: &Path) -> Option<Self> {
        let from_env = |name: &str| std::env::var(name).ok().filter(|v| !v.trim().is_empty());

        let file = std::fs::read_to_string(model_root.join("llm.json"))
            .ok()
            .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok());
        let from_file = |name: &str| {
            file.as_ref()
                .and_then(|v| v[name].as_str())
                .map(str::to_string)
        };

        let url = from_env("YANCHU_LLM_URL").or_else(|| from_file("url"))?;
        let key = from_env("YANCHU_LLM_KEY")
            .or_else(|| from_file("key"))
            .unwrap_or_default();
        let model = from_env("YANCHU_LLM_MODEL")
            .or_else(|| from_file("model"))
            .unwrap_or_else(|| "qwen3.5:2b".to_string());

        let timeout_ms = from_env("YANCHU_LLM_TIMEOUT_MS")
            .and_then(|v| v.parse().ok())
            .or_else(|| file.as_ref().and_then(|v| v["timeout_ms"].as_u64()))
            .unwrap_or(DEFAULT_TIMEOUT_MS);

        // 配置里显式写 provider 时优先；老配置没有这个字段，按地址推断。
        let provider_hint =
            from_env("YANCHU_LLM_PROVIDER").or_else(|| from_file("provider"));
        let provider = provider_hint
            .as_deref()
            .and_then(Provider::parse)
            .unwrap_or_else(|| {
                if url.contains("11434") {
                    Provider::Ollama
                } else {
                    Provider::OpenAi
                }
            });

        Some(Self {
            url,
            key,
            model,
            timeout_ms,
            provider,
            prompt_dir: model_root.join("prompts"),
        })
    }

    /// 本地跑的小模型（Ollama）：只有一个档位，深度档不开放。
    pub fn is_local(&self) -> bool {
        self.provider == Provider::Ollama
    }

    pub fn is_configured(&self) -> bool {
        !self.url.trim().is_empty()
    }

    /// 控制面板的「测试连接」：发一个极小请求，确认地址 / key / 模型能通。
    /// 不跑润色提示词，避免把测试流量花在长文本上。
    pub fn ping(&self) -> anyhow::Result<(String, u64)> {
        anyhow::ensure!(self.is_configured(), "没有配置接口地址");
        let started = std::time::Instant::now();
        let base = self.url.trim_end_matches('/');
        let (endpoint, body) = match self.provider {
            Provider::Ollama => {
                let root = base.strip_suffix("/v1").unwrap_or(base);
                (
                    format!("{root}/api/chat"),
                    serde_json::json!({
                        "model": self.model,
                        "stream": false,
                        "think": false,
                        "keep_alive": "8h",
                        "options": { "temperature": 0, "num_predict": 8 },
                        "messages": [{ "role": "user", "content": "只回复 OK" }]
                    }),
                )
            }
            Provider::OpenAi => (
                format!("{base}/chat/completions"),
                serde_json::json!({
                    "model": self.model,
                    "temperature": 0,
                    "max_tokens": 8,
                    "stream": false,
                    "messages": [{ "role": "user", "content": "只回复 OK" }]
                }),
            ),
        };

        let mut request = ureq::post(&endpoint)
            .timeout(Duration::from_secs(15))
            .set("Content-Type", "application/json");
        if !self.key.is_empty() {
            request = request.set("Authorization", &format!("Bearer {}", self.key));
        }
        let response = request.send_json(body)?;
        let json: serde_json::Value = response.into_json()?;
        let content = match self.provider {
            Provider::Ollama => json["message"]["content"].as_str(),
            Provider::OpenAi => json["choices"][0]["message"]["content"].as_str(),
        };
        let reply = content.unwrap_or("").trim().to_string();
        anyhow::ensure!(!reply.is_empty(), "响应里没有 content 字段");
        Ok((reply, started.elapsed().as_millis() as u64))
    }
}

/// 一次大润色的结果。
///
/// 之所以不直接返回 `Option<String>`：排查「为什么没润色」时，
/// 光知道「没生效」不够 —— 是超时、契约不过、还是压根没配，
/// 三种原因对应三种完全不同的修法。开发模式要把这个写进日志。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// 没配置，或者文本太短不值得发请求
    Skipped,
    /// 模型输出通过契约校验，可以采用
    Used,
    /// 发了请求但没用上：超时、网络错、输出为空、契约不过
    Failed,
}

#[derive(Debug, Clone)]
pub struct LlmAttempt {
    pub outcome: Outcome,
    pub depth: Depth,
    /// 可以采用时才非空
    pub text: Option<String>,
    /// 模型原始输出（剥掉引号/前缀之后、校验之前）；失败时也可能有
    pub raw: String,
    /// 失败原因，写进日志给人看
    pub reason: Option<String>,
    pub ms: u64,
    /// 事实项告警（数字/人称/否定变化）：**只记录，不拦截**。
    /// 拦截条件还没定，先攒真实数据（见 docs/问题清单.md YC-001）。
    pub warnings: Vec<String>,
}

impl LlmAttempt {
    pub(crate) fn skipped(depth: Depth) -> Self {
        Self {
            outcome: Outcome::Skipped,
            depth,
            text: None,
            raw: String::new(),
            reason: None,
            ms: 0,
            warnings: Vec::new(),
        }
    }

    fn failed(depth: Depth, reason: impl Into<String>, raw: impl Into<String>, ms: u64) -> Self {
        Self {
            outcome: Outcome::Failed,
            depth,
            text: None,
            raw: raw.into(),
            reason: Some(reason.into()),
            ms,
            warnings: Vec::new(),
        }
    }

    fn used(depth: Depth, text: String, raw: String, ms: u64, warnings: Vec<String>) -> Self {
        Self {
            outcome: Outcome::Used,
            depth,
            text: Some(text),
            raw,
            reason: None,
            ms,
            warnings,
        }
    }

    pub fn text(&self) -> Option<&str> {
        self.text.as_deref()
    }

    pub fn ms(&self) -> u64 {
        self.ms
    }

    /// 写进开发日志的一句话结论：used / skipped / failed
    pub fn outcome(&self) -> &'static str {
        match self.outcome {
            Outcome::Skipped => "skipped",
            Outcome::Used => "used",
            Outcome::Failed => "failed",
        }
    }
}

/// 跑一次大润色。调用方在拿到 `Used` 之外的任何结果时都应回退到规则层。
pub fn polish(text: &str, cfg: &LlmConfig, depth: Depth) -> LlmAttempt {
    let trimmed = text.trim();
    if !cfg.is_configured() || trimmed.chars().count() < MIN_CHARS {
        return LlmAttempt::skipped(depth);
    }

    let started = std::time::Instant::now();
    let elapsed = || started.elapsed().as_millis() as u64;

    let base = cfg.url.trim_end_matches('/');
    let timeout = timeout_for(cfg, trimmed.chars().count(), depth);
    let num_predict = max_tokens(trimmed.chars().count());
    let template = template_for(cfg, depth, trimmed.chars().count());
    let user_message = template.replace("{text}", trimmed);
    let system = system_prompt(cfg);

    let (endpoint, body) = match cfg.provider {
        // Ollama 原生接口：可以显式关掉思考，返回体是 message.content
        Provider::Ollama => {
            // 配置里习惯写 http://127.0.0.1:11434/v1，原生接口要的是根路径
            let root = base.strip_suffix("/v1").unwrap_or(base);
            (
                format!("{root}/api/chat"),
            serde_json::json!({
                "model": cfg.model,
                "stream": false,
                "think": false,
                // 保活：模型卸载后重新加载要 2–3 秒，第一句话必然超时。
                // 让 Ollama 一直把模型留在显存里。
                "keep_alive": "8h",
                "options": { "temperature": 0.1, "num_predict": num_predict },
                "messages": [
                    { "role": "system", "content": system },
                    { "role": "user", "content": user_message }
                ]
            }),
            )
        }
        Provider::OpenAi => (
            format!("{base}/chat/completions"),
            serde_json::json!({
                "model": cfg.model,
                "temperature": 0.1,
                "max_tokens": num_predict,
                "stream": false,
                "messages": [
                    { "role": "system", "content": system },
                    { "role": "user", "content": user_message }
                ]
            }),
        ),
    };

    let mut request = ureq::post(&endpoint)
        .timeout(timeout)
        .set("Content-Type", "application/json");

    if !cfg.key.is_empty() {
        request = request.set("Authorization", &format!("Bearer {}", cfg.key));
    }

    let response = match request.send_json(body) {
        Ok(r) => r,
        Err(e) => {
            let ms = elapsed();
            log::warn!("大润色请求失败（{}，{}ms）: {e}", depth.label(), ms);
            return LlmAttempt::failed(depth, format!("请求失败: {e}"), "", ms);
        }
    };

    let json: serde_json::Value = match response.into_json() {
        Ok(v) => v,
        Err(e) => {
            let ms = elapsed();
            log::warn!("大润色响应不是 JSON: {e}");
            return LlmAttempt::failed(depth, format!("响应解析失败: {e}"), "", ms);
        }
    };

    let content = match cfg.provider {
        Provider::Ollama => json["message"]["content"].as_str(),
        Provider::OpenAi => json["choices"][0]["message"]["content"].as_str(),
    };

    let content = match content {
        Some(c) => c.trim(),
        None => {
            let ms = elapsed();
            log::warn!("大润色响应里没有 content: {json}");
            return LlmAttempt::failed(depth, "响应缺少 content 字段", "", ms);
        }
    };

    if content.is_empty() {
        return LlmAttempt::failed(depth, "模型返回空字符串", "", elapsed());
    }

    // 模型偶尔会自作主张加引号或「整理后的文本：」这类前缀，剥掉
    let cleaned = content
        .trim_matches(|c| c == '"' || c == '“' || c == '”' || c == '「' || c == '」')
        .trim();

    let prefix_stripped = cleaned
        .strip_prefix("整理后：")
        .or_else(|| cleaned.strip_prefix("整理后的文本："))
        .or_else(|| cleaned.strip_prefix("润色后："))
        .unwrap_or(cleaned)
        .trim();

    if prefix_stripped.is_empty() {
        return LlmAttempt::failed(depth, "剥前缀之后是空字符串", cleaned, elapsed());
    }

    log::info!("LLM 原始输出: {prefix_stripped:?}");

    // 收口：把模型的输出再过一遍规则层。
    // 深度档先把 markdown 记号规整成「1. 2. 3.」，再统一删残留语气词、修标点。
    // 模型答应的事不一定做，这一步是确定性的兜底。
    let body = finalize_body(depth, prefix_stripped);

    if body.is_empty() {
        return LlmAttempt::failed(depth, "规整分点之后是空文本", prefix_stripped, elapsed());
    }

    // 契约校验：模型说了不算，程序说了算。
    // 小模型（实测 0.5B）会自己加解释、把 production 翻成「生产环境」，
    // 这些都必须挡掉，回退到规则层结果。
    if let Some(reason) = contract_violation_for(depth, trimmed, &body) {
        log::warn!(
            "大润色输出违反契约（{}，{}档），已丢弃: {body}",
            reason,
            depth.label()
        );
        return LlmAttempt::failed(depth, reason, body, elapsed());
    }

    // 事实项告警：只记录、不拦截（见 docs/问题清单.md YC-001）
    let warnings = fact_warnings(trimmed, &body);
    if !warnings.is_empty() {
        log::warn!("事实项告警（{}）：{:?}", depth.label(), warnings);
    }

    LlmAttempt::used(depth, body.clone(), body, elapsed(), warnings)
}

/// 模型输出的确定性收口。
///
/// 两件事：把 markdown 记号统一成「1. 2. 3.」，再用规则层兜一遍标点/残留语气词。
/// 本地标准档现在也会产出编号清单，所以这一步不分档位。
fn finalize_body(_depth: Depth, text: &str) -> String {
    crate::polish::rule_polish(&normalize_list_markers(text))
}

/// 判断润色结果是否破坏契约（用于测试和 `--dev-report` 复检）。
pub fn violates_contract(input: &str, output: &str) -> bool {
    contract_violation_for(Depth::Standard, input, output).is_some()
}

pub fn contract_violation(input: &str, output: &str) -> Option<&'static str> {
    contract_violation_for(Depth::Standard, input, output)
}

/// 契约的具体条款。返回第一条不满足的原因，方便日志里写清楚「为什么被丢」。
///
/// 契约的核心不是「一个字都不能变」，而是**不许改事实、不许丢术语**：
/// * 英文术语必须原样出现（`production` 不能被翻成「生产环境」）；
/// * 不许凭空加词（模型爱加「可能是因为……」「以下是整理后的文本」）；
/// * 长度不能暴涨（补标点会略长，但不该翻倍）。
/// 措辞重组是允许的 —— 否则大润色相对规则层毫无增量。
///
/// 两档只放宽「尺度」，不放宽「事实」：深度档可以改写、可以变长（分点会多出编号和换行），
/// 但仍然不许丢术语、不许加前言、不许把一句扩成五句。
pub fn contract_violation_for(
    depth: Depth,
    input: &str,
    output: &str,
) -> Option<&'static str> {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return Some("输出为空");
    }

    // 1. 不允许前言/解释性开头
    if is_preamble(trimmed) {
        return Some("以解释性前言开头");
    }

    // 2. 英文术语必须原样保留，但**识别错的可以修**：
    //    tging → staging、jason → JSON 是加分项，不该判成「丢了术语」。
    //    所以用编辑距离 ≤2 把「原文里的词」和「输出里的词」认成同一个。
    let words = |s: &str| -> std::collections::HashSet<String> {
        s.split(|c: char| !c.is_ascii_alphanumeric())
            // 纯数字不算「英文术语」——数字归事实项校验管，混在一起会把
            // 「30 秒改成 90 秒」误报成「丢了英文术语」
            .filter(|w| w.len() > 1 && !w.chars().all(|c| c.is_ascii_digit()))
            .map(|w| w.to_lowercase())
            .collect()
    };
    let in_words = words(input);
    let out_words = words(output);
    let close = |a: &str, b: &str| -> bool {
        if a.chars().count() < 3 || b.chars().count() < 3 {
            return a == b;
        }
        edit_distance(a, b) <= 2
    };

    if in_words
        .iter()
        .any(|w| !out_words.contains(w) && !out_words.iter().any(|o| close(w, o)))
    {
        return Some("丢了英文术语"); // 丢了，而且不像笔误
    }
    // 新增术语：只要它是原文某个词的「近似修正」就不算新增
    let novel_words = out_words
        .iter()
        .filter(|w| !in_words.contains(*w) && !in_words.iter().any(|i| close(i, w)))
        .count();
    if novel_words > 1 {
        return Some("新增了英文术语，疑似编内容");
    }

    // 3. 长度比：允许摘要式整理（2026-11-03 产品决定）—— 压缩到一半左右可以接受，
    //    但不能只剩几句话。上限仍然拦「加解释」。
    let (min_ratio, max_ratio) = match depth {
        Depth::Standard => (0.45_f32, 1.5_f32),
        Depth::Deep => (0.45, 2.0),
    };
    let ratio = output.chars().count() as f32 / input.chars().count().max(1) as f32;
    if ratio > max_ratio {
        return Some("输出明显变长，疑似加了解释");
    }
    if ratio < min_ratio {
        return Some("输出明显变短，疑似丢内容");
    }

    // 4. 新增汉字比例：允许重写，但大半都是原文没有的字，多半是在编
    let novelty_limit = match depth {
        Depth::Standard => 0.55,
        Depth::Deep => 0.60,
    };
    let in_chars: std::collections::HashSet<char> = input.chars().filter(|c| is_cjk(*c)).collect();
    let out_cjk: Vec<char> = output.chars().filter(|c| is_cjk(*c)).collect();
    if !out_cjk.is_empty() {
        let novel = out_cjk.iter().filter(|c| !in_chars.contains(c)).count();
        if novel as f32 / out_cjk.len() as f32 > novelty_limit {
            return Some("新增汉字比例过高，疑似重写内容");
        }
    }

    // 5. 短句不许改事实（YC-001）：
    //    数字、否定不能变；「我」不能变成「你」。
    //    长文允许压缩改写，删掉「我们」可能是整理；短句没有这个必要。
    if depth == Depth::Standard && input.chars().count() < REWRITE_MIN_CHARS {
        let a = strip_list_markers(input);
        let b = strip_list_markers(output);
        if numbers(&a) != numbers(&b) {
            return Some("短句被改了数字");
        }
        if count_chars(&a, NEGATIONS) != count_chars(&b, NEGATIONS) {
            return Some("短句被改了否定");
        }
        let me_in = count_chars(&a, &['我', '咱']);
        let me_out = count_chars(&b, &['我', '咱']);
        let you_in = count_chars(&a, &['你', '您']);
        let you_out = count_chars(&b, &['你', '您']);
        // 语气框架里的「我」不算主语：「我建议你…」→「建议你…」是压缩，不是换人
        let opinion = OPINION_FRAMES
            .iter()
            .map(|f| a.matches(f).count())
            .sum::<usize>();
        let me_effective = me_in.saturating_sub(opinion);
        // 「我需要你…」→「你需要…」：主语被换掉了。
        if me_effective > me_out && (you_out > you_in || (me_out == 0 && you_out > 0)) {
            return Some("把「我」改成了「你」");
        }

        // 「我需要你确认」→「我需要确认」：把请求对象删了，谁来确认就变了。
        // 只认这几个动词：省掉「请你/提醒你」这类客气话是正常的，不算。
        const REQUEST_OBJECT: &[&str] = &["需要你", "要你", "让你", "要求你"];
        if REQUEST_OBJECT.iter().any(|p| a.contains(p) && !b.contains(p)) {
            return Some("丢了请求对象「你」");
        }

        // 「我把配置改好了」→「他把配置改好了」：句首主语从第一人称变第三人称。
        // 只看句首 —— 句中的「我和他」被压成「跟他」是正常压缩，不算换人。
        let starts_with_opinion = OPINION_FRAMES.iter().any(|f| a.starts_with(f));
        let head = |s: &str| s.chars().find(|c| is_cjk(*c) || c.is_ascii_alphanumeric());
        if !starts_with_opinion {
            if let (Some(from), Some(to)) = (head(&a), head(&b)) {
                if matches!(from, '我' | '咱') && matches!(to, '你' | '您' | '他' | '她') {
                    return Some("句子的主语被换掉了");
                }
            }
        }
    }

    // 6. 分段/分点数量要有上限：一段话拆成二十条基本是在编
    if depth == Depth::Deep && trimmed.lines().count() > 15 {
        return Some("分点过多，疑似展开过度");
    }
    // 标准档允许分段（长口述按「一件事一段」），但也不能无限切
    let paragraphs = trimmed
        .split("\n\n")
        .filter(|p| !p.trim().is_empty())
        .count();
    let max_paragraphs = match depth {
        Depth::Standard => 8,
        Depth::Deep => 12,
    };
    if paragraphs > max_paragraphs {
        return Some("分段过多，疑似展开过度");
    }

    None
}

/// 前言判定的标记。**必须是完整短语**，不能是「好的」「这是」这种正常起句 ——
/// 早期版本拿这两个词当前缀黑名单，结果把「好的，我们可以…」「这是气泡的…」
/// 这种老老实实的正文整段丢掉了（devlog 2026-11-02 两段标准档历史）。
const PREAMBLE_MARKERS: &[&str] = &[
    "以下是",
    "整理后",
    "润色后",
    "整理如下",
    "润色如下",
    "输出如下",
    "我明白了",
    "明白了，",
    "为您整理",
    "这是整理",
    "这是润色",
    "这是为您",
    "整理好的",
    "润色好的",
];

/// 剥掉开头的礼貌词再判前言：「好的，以下是…」要认出来，
/// 但「好的，我们可以…」不能被误判。
fn strip_polite_lead(s: &str) -> &str {
    for p in ["好的，", "好的。", "好的,", "好的 ", "嗯，", "嗯。"] {
        if let Some(rest) = s.strip_prefix(p) {
            return rest.trim_start();
        }
    }
    s
}

/// 这段输出是不是模型在「报幕」而不是在写正文。
///
/// 判据刻意收得很紧：只有完整短语才算，而且「以下是 / 整理后」这类
/// 还要求后面很快出现冒号或「文本 / 内容 / 结果」——否则正文里
/// 「整理后的文件我放在共享盘了」也会被误伤。
fn is_preamble(output: &str) -> bool {
    let rest = strip_polite_lead(output.trim());
    for marker in PREAMBLE_MARKERS {
        let Some(after) = rest.strip_prefix(marker) else {
            continue;
        };
        let strong = matches!(
            *marker,
            "我明白了" | "明白了，" | "为您整理" | "这是整理" | "这是润色" | "这是为您"
        );
        if strong {
            return true;
        }
        let tail: String = after.chars().take(12).collect();
        // 只认「报幕词」，不认裸冒号：「以下是我们要做的三件事：」是正文，不是前言
        if tail.contains("文本")
            || tail.contains("内容")
            || tail.contains("结果")
            || tail.contains("如下")
            || tail.contains("整理")
            || tail.contains("润色")
        {
            return true;
        }
    }
    false
}

/// 把模型爱用的 markdown 记号统一成「1. 2. 3.」。
///
/// 提示词里已经要求不要用 `*`/`-`/`#`，但小模型不一定听；
/// 这一步是确定性的收口，保证落进输入框的是规整编号，而不是一堆星号。
///
/// 只有一条的「清单」不算清单：模型有时会把一整段话塞进「1. 」里，
/// 落进输入框就变成孤零零的「1. …」。这种情况直接去掉编号，当正文处理。
pub fn normalize_list_markers(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 16);
    let mut n = 0usize;
    let single_item = text
        .lines()
        .filter(|l| split_marker(l.trim()).0 && !split_marker(l.trim()).1.trim().is_empty())
        .count()
        <= 1;

    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            out.push('\n');
            continue;
        }
        let (is_item, rest) = split_marker(trimmed);
        let rest = strip_bold(rest);
        if is_item && !rest.trim().is_empty() && !single_item {
            n += 1;
            out.push_str(&format!("{n}. {}", rest.trim()));
        } else {
            out.push_str(rest.trim());
        }
        out.push('\n');
    }

    // 连续空行压成一个
    let mut collapsed = String::with_capacity(out.len());
    let mut blank = 0;
    for line in out.lines() {
        if line.trim().is_empty() {
            blank += 1;
            if blank > 1 {
                continue;
            }
        } else {
            blank = 0;
        }
        collapsed.push_str(line.trim_end());
        collapsed.push('\n');
    }
    collapsed.trim().to_string()
}

/// 拆掉行首的列表记号，返回（是否是列表项, 去掉记号后的内容）。
fn split_marker(line: &str) -> (bool, &str) {
    // markdown 标题：去掉 #，但不当成编号项
    if let Some(rest) = line.strip_prefix('#') {
        return (false, rest.trim_start_matches('#').trim_start());
    }
    for p in ["- ", "* ", "• ", "· ", "+ ", "－ "] {
        if let Some(rest) = line.strip_prefix(p) {
            return (true, rest.trim_start());
        }
    }

    // 1. / 1、 / 1) —— 只认 1–2 位数字，避免把「2026 年」当成编号
    let digits: String = line.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() || digits.len() > 2 {
        return (false, line);
    }
    let rest = &line[digits.len()..];
    if let Some(r) = rest.strip_prefix('.') {
        // 「1.5 倍」不是编号：点号后面必须跟空白或结束
        if r.is_empty() || r.starts_with(' ') || r.starts_with('\t') {
            return (true, r.trim_start());
        }
    }
    for p in ["、", ")", "．"] {
        if let Some(r) = rest.strip_prefix(p) {
            if !r.trim().is_empty() {
                return (true, r.trim_start());
            }
        }
    }
    (false, line)
}

/// 去掉 markdown 粗体记号，保留文字本身。
fn strip_bold(s: &str) -> String {
    s.replace("**", "").replace("__", "")
}

/// 事实项告警：数字 / 人称 / 否定有没有被改掉。
///
/// **只记录、不拦截**。拦截的门槛要先用真实语料量出来，
/// 现在直接拦会把「整一下→处理一下」这种正当改写一起误杀（见 docs/问题清单.md YC-001）。
pub fn fact_warnings(input: &str, output: &str) -> Vec<String> {
    let mut warnings = Vec::new();
    let a = strip_list_markers(input);
    let b = strip_list_markers(output);

    if numbers(&a) != numbers(&b) {
        warnings.push("数字变化".to_string());
    }
    if count_chars(&a, PERSONAL) != count_chars(&b, PERSONAL) {
        warnings.push("人称变化".to_string());
    }
    if count_chars(&a, NEGATIONS) != count_chars(&b, NEGATIONS) {
        warnings.push("否定词变化".to_string());
    }

    warnings
}

const PERSONAL: &[char] = &['我', '你', '您', '他', '她', '咱'];

/// 语气框架：这些「我」只是在表达态度/建议，删掉不改变「谁做这件事」。
///
/// 实测误报：原文「**我建议**你把日志级别调到 debug」，模型压成「建议你…」，
/// 被「把『我』改成了『你』」误拦。这类开头的「我」要从主语计数里扣掉。
const OPINION_FRAMES: &[&str] = &[
    "我觉得", "我认为", "我感觉", "我建议", "我看", "我想", "我猜", "我估计",
    "我担心", "我发现", "我记得",
];
const NEGATIONS: &[char] = &['不', '没', '别', '勿', '未'];

fn count_chars(s: &str, set: &[char]) -> usize {
    s.chars().filter(|c| set.contains(c)).count()
}

/// 抽出所有数字（阿拉伯和中文都认），按出现顺序。
///
/// `30 秒` 和 `三十秒` 必须等价 —— 否则模型把阿拉伯数字写成中文就会被误判成
/// 「短句被改了数字」。顺序保留：`从30改成60` 和 `从60改成30` 是两回事。
fn numbers(s: &str) -> Vec<String> {
    let chars: Vec<char> = s.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;

    while i < chars.len() {
        if chars[i].is_ascii_digit() {
            let start = i;
            while i < chars.len() && chars[i].is_ascii_digit() {
                i += 1;
            }
            let text: String = chars[start..i].iter().collect();
            if let Ok(n) = text.parse::<u64>() {
                out.push(n.to_string());
            }
            continue;
        }
        if is_cn_numeral(chars[i]) {
            // 「第一」「第二」是序数，不是数量
            if i > 0 && chars[i - 1] == '第' {
                i += 1;
                continue;
            }
            let start = i;
            while i < chars.len() && is_cn_numeral(chars[i]) {
                i += 1;
            }
            // 只有后面跟着单位才算数字：「一个方案」「一起」「有一点」「十分满意」都不算
            let next2: String = chars[i..].iter().take(2).collect();
            let unit = next2.starts_with("分钟")
                || next2.starts_with("小时")
                || next2.starts_with("毫秒")
                || chars.get(i).map(|c| is_number_unit(*c)).unwrap_or(false);
            if unit {
                let text: String = chars[start..i].iter().collect();
                if let Some(n) = parse_cn_number(&text) {
                    out.push(n.to_string());
                }
            }
            continue;
        }
        i += 1;
    }
    out
}

fn is_cn_numeral(c: char) -> bool {
    matches!(
        c,
        '零' | '〇' | '一' | '二' | '两' | '三' | '四' | '五' | '六' | '七' | '八' | '九'
            | '十' | '百' | '千'
    )
}

/// 数字后面跟这些才算「数量」。
///
/// 刻意不放「点」「分」「号」：
/// 「有一点问题」「十分满意」「三号」里的数字是虚的，算进去只会误报。
/// 「分钟 / 小时」两字单位在调用处单独认。
fn is_number_unit(c: char) -> bool {
    "秒次倍元米克页行篇周年月天条".contains(c)
}

fn cn_digit(c: char) -> Option<u64> {
    Some(match c {
        '零' | '〇' => 0,
        '一' => 1,
        '二' | '两' => 2,
        '三' => 3,
        '四' => 4,
        '五' => 5,
        '六' => 6,
        '七' => 7,
        '八' => 8,
        '九' => 9,
        _ => return None,
    })
}

/// 中文数字转整数：三十 → 30，一百零五 → 105，三百五 → 350（口语省略）。
fn parse_cn_number(s: &str) -> Option<u64> {
    let mut section = 0u64;
    let mut num: Option<u64> = None;
    let mut last_unit = 0u64;
    let mut zero_after_unit = false;
    let mut seen = false;

    for c in s.chars() {
        match c {
            '十' | '百' | '千' => {
                let unit = match c {
                    '十' => 10,
                    '百' => 100,
                    _ => 1000,
                };
                section += num.unwrap_or(1) * unit;
                last_unit = unit;
                num = None;
                zero_after_unit = false;
                seen = true;
            }
            _ => {
                let d = cn_digit(c)?;
                if d == 0 {
                    zero_after_unit = true;
                }
                num = Some(d);
                seen = true;
            }
        }
    }

    let tail = num.unwrap_or(0);
    // 口语省略：三百五 = 350、一百二 = 120；但「一百零五」是 105
    if last_unit >= 100 && tail > 0 && !zero_after_unit {
        section += tail * (last_unit / 10);
    } else {
        section += tail;
    }
    if !seen {
        return None;
    }
    Some(section)
}

/// 去掉行首编号，避免「1. 2. 3.」被当成数字变化
fn strip_list_markers(s: &str) -> String {
    s.lines()
        .map(|l| split_marker(l.trim()).1)
        .collect::<Vec<_>>()
        .join("\n")
}

fn is_cjk(c: char) -> bool {
    matches!(c as u32, 0x4E00..=0x9FFF | 0x3400..=0x4DBF)
}

/// 两个英文词差几个字母。用来把 `tging → staging` 这种识别修正认成同一个词。
fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];
    for i in 1..=a.len() {
        cur[0] = i;
        for j in 1..=b.len() {
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

/// 让 Ollama 继续保活：运行期间每 3 小时续一次 8h，避免长时间闲置后模型被卸掉。
pub fn keepalive(cfg: &LlmConfig) {
    if !cfg.is_local() {
        return;
    }
    let base = cfg.url.trim_end_matches('/');
    let root = base.strip_suffix("/v1").unwrap_or(base);
    let body = serde_json::json!({
        "model": cfg.model,
        "stream": false,
        "think": false,
        "keep_alive": "8h",
        "options": { "num_predict": 1 },
        "messages": [{ "role": "user", "content": "hi" }]
    });
    match ureq::post(&format!("{root}/api/chat"))
        .timeout(Duration::from_secs(30))
        .send_json(body)
    {
        Ok(_) => log::info!("LLM 保活完成"),
        Err(e) => log::warn!("LLM 保活失败: {e}"),
    }
}

/// 退出时卸载本地 Ollama 模型：言出不在了，就别再占着 2–3GB 显存。
/// 只对本地 Ollama 生效；云端 API 没有“卸载”这回事。
pub fn unload(cfg: &LlmConfig) {
    if !cfg.is_local() {
        return;
    }
    let base = cfg.url.trim_end_matches('/');
    let root = base.strip_suffix("/v1").unwrap_or(base);
    let body = serde_json::json!({
        "model": cfg.model,
        "keep_alive": 0
    });
    match ureq::post(&format!("{root}/api/generate"))
        .timeout(Duration::from_secs(2))
        .send_json(body)
    {
        Ok(_) => log::info!("已卸载本地润色模型: {}", cfg.model),
        Err(e) => log::warn!("卸载润色模型失败: {e}"),
    }
}

/// 预热：发一个极小请求，把模型加载进显存，省掉第一句话的 2–3 秒冷加载。
pub fn warmup(cfg: &LlmConfig) {
    if !cfg.is_configured() || !cfg.is_local() {
        return;
    }
    let base = cfg.url.trim_end_matches('/');
    let root = base.strip_suffix("/v1").unwrap_or(base);
    let body = serde_json::json!({
        "model": cfg.model,
        "stream": false,
        "think": false,
        "keep_alive": "8h",
        "options": { "num_predict": 1 },
        "messages": [{ "role": "user", "content": "hi" }]
    });
    let started = std::time::Instant::now();
    match ureq::post(&format!("{root}/api/chat"))
        .timeout(Duration::from_secs(30))
        .send_json(body)
    {
        Ok(_) => log::info!("LLM 预热完成，用时 {:?}", started.elapsed()),
        Err(e) => log::warn!("LLM 预热失败: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_cfg(url: &str, timeout_ms: u64) -> LlmConfig {
        LlmConfig {
            url: url.into(),
            key: String::new(),
            model: "x".into(),
            timeout_ms,
            provider: Provider::OpenAi,
            prompt_dir: std::env::temp_dir().join("yanchu-test-prompts"),
        }
    }

    #[test]
    fn not_configured_returns_none() {
        let cfg = test_cfg("", 900);
        let r = polish("这是一句足够长的话，应该被跳过", &cfg, Depth::Standard);
        assert_eq!(r.outcome(), "skipped");
        assert!(r.text().is_none());
    }

    #[test]
    fn short_text_skipped() {
        let cfg = test_cfg("http://127.0.0.1:1/v1", 900);
        let r = polish("好", &cfg, Depth::Standard);
        assert_eq!(r.outcome(), "skipped");
        assert!(r.text().is_none());
    }

    #[test]
    fn unreachable_endpoint_falls_back() {
        // 端口 1 不会有服务在听：必须安静地失败，让调用方用规则层结果
        let cfg = test_cfg("http://127.0.0.1:1/v1", 900);
        let r = polish("这是一句足够长的中文，用来测试回退", &cfg, Depth::Standard);
        assert_eq!(r.outcome(), "failed");
        assert!(r.text().is_none());
    }

    #[test]
    fn attempt_reports_elapsed_ms() {
        let cfg = test_cfg("http://127.0.0.1:1/v1", 300);
        let r = polish("这是一句足够长的中文，用来测试耗时", &cfg, Depth::Standard);
        // 失败也要有耗时，否则分析报告里分不清「秒回」和「超时」
        assert!(r.ms() > 0, "耗时应该被记录，实际 {}", r.ms());
    }

    #[test]
    fn deep_timeout_is_more_patient_than_standard() {
        let cfg = test_cfg("http://127.0.0.1:1/v1", 900);
        let text = "一".repeat(60);
        let standard = timeout_for(&cfg, text.chars().count(), Depth::Standard);
        let deep = timeout_for(&cfg, text.chars().count(), Depth::Deep);
        assert!(
            deep > standard,
            "深度档必须比标准档更能等：{deep:?} vs {standard:?}"
        );
        assert!(deep >= Duration::from_millis(4000), "深度档下限 4s");
    }

    #[test]
    fn standard_timeout_caps_at_eight_seconds() {
        // 本地单档：长文可以等，但也得有上限
        let cfg = test_cfg("http://127.0.0.1:1/v1", 1200);
        let long = "一".repeat(1000);
        assert_eq!(
            timeout_for(&cfg, long.chars().count(), Depth::Standard),
            Duration::from_millis(8000)
        );
    }

    #[test]
    fn deep_timeout_caps_at_twelve_seconds() {
        let cfg = test_cfg("http://127.0.0.1:1/v1", 1200);
        let long = "一".repeat(5000);
        assert_eq!(
            timeout_for(&cfg, long.chars().count(), Depth::Deep),
            Duration::from_millis(12_000)
        );
    }

    #[test]
    fn short_text_still_gets_a_tight_budget() {
        let cfg = test_cfg("http://127.0.0.1:1/v1", 900);
        let d = timeout_for(&cfg, 10, Depth::Standard);
        assert_eq!(d, Duration::from_millis(2500), "短句不该被拖到秒级");
    }

    #[test]
    fn output_limit_follows_input_length() {
        // 固定 256 会把 358 字以上的输出截断，然后被契约当成「丢内容」
        assert_eq!(max_tokens(20), 256);
        assert!(max_tokens(358) > 600);
        assert_eq!(max_tokens(100_000), 4096);
    }

    #[test]
    fn long_text_auto_switches_to_deep() {
        assert!(!should_auto_deep(AUTO_DEEP_CHARS - 1));
        assert!(should_auto_deep(AUTO_DEEP_CHARS));
        assert!(should_auto_deep(1000));
    }

    #[test]
    fn finalize_strips_leftover_fillers() {
        // 提示词说了删语气词，模型漏删时由规则层兜底
        let out = finalize_body(Depth::Standard, "嗯，好的，我们可以去测试一下。");
        assert!(!out.contains('嗯'), "残留语气词没删干净: {out}");
        assert!(out.starts_with("好的"), "正常内容不能被动: {out}");
    }

    #[test]
    fn finalize_keeps_paragraph_breaks() {
        let out = finalize_body(Depth::Standard, "第一件事做咖啡。\n\n第二件事去上班。");
        assert!(out.contains("\n\n"), "段落空行被吃掉了: {out:?}");
    }

    // ---- 契约校验：这些都是实测里小模型真实犯过的错 ----

    #[test]
    fn contract_violation_explains_why() {
        // 报告里要能说清「为什么被丢」，不能只有一个 bool
        let input = "这个 bug 在 tging 环境复现不了，只在 production 出现。";
        let output = "这个 bug 在测试环境（tging）中重现，但在生产环境中无法复现。";
        assert_eq!(
            contract_violation(input, output),
            Some("丢了英文术语"),
            "production 被翻译成中文应该被识别为丢术语"
        );
    }

    #[test]
    fn rejects_translated_term() {
        // 0.5B 真的把 production 翻成了「生产环境」
        let input = "这个 bug 在 tging 环境复现不了，只在 production 出现。";
        let output = "这个 bug 在测试环境（tging）中重现，但在生产环境中无法复现。";
        assert!(violates_contract(input, output));
    }

    #[test]
    fn accepts_asr_typo_fix_in_english_term() {
        // ASR 把 staging 认成 stging：模型改回来是加分项，不能判「丢术语」
        let input = "这个 bug 只在 stging 复现，只在 production 出现。";
        let output = "这个 bug 只在 staging 复现，只在 production 出现。";
        assert_eq!(contract_violation_for(Depth::Standard, input, output), None);
    }

    #[test]
    fn fuzzy_match_does_not_excuse_a_different_word() {
        // 近似判定只认「差一两个字母」，换个词照样拦
        assert!(edit_distance("stging", "staging") <= 2);
        assert!(edit_distance("production", "prod") > 2);
    }

    #[test]
    fn rejects_added_explanation() {
        let input = "啊，那个，我想问一下这个接口为什么返回空。";
        let output = "那个接口返回空，可能是因为数据没有正确解析或者服务器出现了问题。";
        assert!(violates_contract(input, output));
    }

    #[test]
    fn rejects_preamble() {
        let input = "把这段逻辑抽成一个 helper 函数放到 utils 里。";
        let output = "好的，我明白了。请将逻辑放在这里，我会把它抽成一个 helper 函数放到 utils 里。";
        assert!(violates_contract(input, output));
    }

    // ---- 前言判定：正常起句不能被误杀（devlog 2026-11-02 的两段历史） ----

    #[test]
    fn legit_opening_hao_de_is_not_preamble() {
        // 50 秒那段历史的开头就是这个，模型输出被整段丢掉
        let input = "好的，我们可以，测试一下这个勾画的模式。";
        assert_eq!(
            contract_violation_for(Depth::Standard, input, input),
            None
        );
    }

    #[test]
    fn legit_opening_zhe_shi_is_not_preamble() {
        // 94 秒那段历史的开头「这是气泡的…」，模型加了「一个」也不该判前言
        let input = "这是气泡的，停顿性的话，我觉得可以多讲两句。";
        let output = "这是一个气泡的停顿性测试，我觉得可以多讲两句。";
        assert_eq!(
            contract_violation_for(Depth::Standard, input, output),
            None
        );
    }

    #[test]
    fn rejects_real_preamble_with_colon() {
        let input = "把这段逻辑抽成一个 helper 函数放到 utils 里。";
        let output = "好的，以下是整理后的文本：\n把逻辑抽成 helper 函数放到 utils 里。";
        assert_eq!(
            contract_violation_for(Depth::Standard, input, output),
            Some("以解释性前言开头")
        );
    }

    #[test]
    fn rejects_short_preamble() {
        let input = "把这段逻辑抽成一个 helper 函数放到 utils 里。";
        let output = "以下是整理后的文本：把逻辑抽成 helper 函数放到 utils 里。";
        assert_eq!(
            contract_violation_for(Depth::Standard, input, output),
            Some("以解释性前言开头")
        );
    }

    #[test]
    fn keeps_legit_sentence_starting_with_zheng_li_hou() {
        // 「整理后的文件我放在共享盘了」是正文，不是报幕
        let input = "整理后的文件我放在共享盘了，你看一下。";
        assert_eq!(contract_violation_for(Depth::Standard, input, input), None);
    }

    #[test]
    fn keeps_legit_sentence_starting_with_yi_xia_shi() {
        // 口述里真的会有人说「以下是……」，裸冒号不能当成前言
        let input = "以下是我们要做的三件事，先做咖啡，然后开会。";
        assert_eq!(contract_violation_for(Depth::Standard, input, input), None);
    }

    #[test]
    fn accepts_summary_but_rejects_gutting() {
        // 允许摘要式整理：100 字压到 55 字可以接受
        let input = "一".repeat(100);
        assert_eq!(
            contract_violation_for(Depth::Standard, &input, &"一".repeat(55)),
            None
        );
        // 只剩三成，基本是把内容删光了
        assert_eq!(
            contract_violation_for(Depth::Standard, &input, &"一".repeat(30)),
            Some("输出明显变短，疑似丢内容")
        );
    }

    #[test]
    fn tolerates_normal_filler_deletion() {
        // 删口头语 + 合并碎句掉个 10–20% 是正常的，不能拦
        let input = "嗯，那个，我需要你把 timeout 从三十秒改成六十秒。";
        let output = "我需要你把 timeout 从三十秒改成六十秒。";
        assert_eq!(contract_violation_for(Depth::Standard, input, output), None);
    }

    // ---- 短句不许改事实（YC-001） ----

    #[test]
    fn rejects_person_swap_in_short_sentence() {
        let input = "我需要你把 timeout 从 30 秒改成 60 秒。";
        let output = "你需要把 timeout 从 30 秒改成 60 秒。";
        assert_eq!(
            contract_violation_for(Depth::Standard, input, output),
            Some("把「我」改成了「你」")
        );
    }

    #[test]
    fn allows_dropping_opinion_frame() {
        // 实测误报：我建议你… → 建议你…，被当成换主语
        let input = "我建议你把日志级别调到 debug。";
        let output = "建议你把日志级别调到 debug。";
        assert_eq!(contract_violation_for(Depth::Standard, input, output), None);
    }

    #[test]
    fn rejects_dropping_request_object_you() {
        // 真人测试里抓到的漏报：我需要你确认 → 我需要确认，谁确认变了
        let input = "我需要你确认一下生产环境的状态。";
        let output = "我需要确认一下生产环境的状态。";
        assert_eq!(
            contract_violation_for(Depth::Standard, input, output),
            Some("丢了请求对象「你」")
        );
    }

    #[test]
    fn allows_dropping_you_in_short_sentence() {
        // 「你帮我…」→「帮我…」：说话人没变，只是省了个称呼
        let input = "你帮我看一下这个接口的返回值。";
        let output = "帮我看一下这个接口的返回值。";
        assert_eq!(contract_violation_for(Depth::Standard, input, output), None);
    }

    #[test]
    fn rejects_number_change_in_short_sentence() {
        let input = "把 timeout 从 30 秒改成 60 秒。";
        let output = "把 timeout 从 30 秒改成 90 秒。";
        assert_eq!(
            contract_violation_for(Depth::Standard, input, output),
            Some("短句被改了数字")
        );
    }

    #[test]
    fn numbers_treat_chinese_and_arabic_alike() {
        assert_eq!(
            numbers("把 timeout 从 30 秒改成 60 秒"),
            numbers("把 timeout 从三十秒改成六十秒")
        );
        assert_eq!(numbers("从三十秒改成六十秒"), vec!["30", "60"]);
    }

    #[test]
    fn numbers_ignore_articles_and_ordinals() {
        assert!(numbers("有一个方案").is_empty());
        assert!(numbers("第一步先做").is_empty());
        assert!(numbers("有一点问题").is_empty());
        assert!(numbers("十分满意").is_empty());
        assert!(numbers("三号那天").is_empty());
        assert_eq!(numbers("三次"), vec!["3"]);
        assert_eq!(numbers("五分钟"), vec!["5"]);
        assert_eq!(numbers("三小时"), vec!["3"]);
        assert_eq!(numbers("二百毫秒"), vec!["200"]);
        assert_eq!(numbers("三条"), vec!["3"]);
    }

    #[test]
    fn accepts_chinese_numeral_rewrite() {
        // 模型把阿拉伯数字写成中文，不能判成「改了数字」
        let input = "把 timeout 从 30 秒改成 60 秒。";
        let output = "把 timeout 从三十秒改成六十秒。";
        assert_eq!(contract_violation_for(Depth::Standard, input, output), None);
    }

    #[test]
    fn rejects_subject_swap_to_third_person() {
        // YC-001 的另一半：我→他/她
        let input = "我把这个配置改好了。";
        let output = "他把这个配置改好了。";
        assert_eq!(
            contract_violation_for(Depth::Standard, input, output),
            Some("句子的主语被换掉了")
        );
    }

    #[test]
    fn allows_compressing_i_and_he() {
        // 「我和他确认过了」→「跟他确认过了」是压缩，不是换主语
        let input = "我和他确认过了，这个没问题。";
        let output = "跟他确认过了，这个没问题。";
        assert_eq!(contract_violation_for(Depth::Standard, input, output), None);
    }

    #[test]
    fn allows_dropping_opinion_frame_before_you() {
        let input = "我觉得你应该先备份。";
        let output = "你应该先备份。";
        assert_eq!(contract_violation_for(Depth::Standard, input, output), None);
    }

    // ---- 标准档现在允许合并碎句、分段 ----

    #[test]
    fn merges_chopped_sentences() {
        // 语音识别按停顿切碎，大润色按语义接回去 —— 实词没变，不算违约
        let input = "好的，我们可以。测试一下这个。勾画的模式。我现在在每一句话里面。";
        let output = "好的，我们可以测试一下这个勾画的模式。我现在在每一句话里面。";
        assert_eq!(contract_violation_for(Depth::Standard, input, output), None);
    }

    #[test]
    fn allows_paragraphs_in_standard() {
        let input = "明天先做咖啡。然后去锻炼一会儿。再去上班。上班从九点半到十二点半。";
        let output = "明天先做咖啡，然后去锻炼一会儿，再去上班。\n\n上班从九点半到十二点半。";
        assert_eq!(contract_violation_for(Depth::Standard, input, output), None);
    }

    #[test]
    fn rejects_too_many_paragraphs() {
        let input = "一二三四".repeat(10);
        let output = (0..10).map(|_| "一二三四").collect::<Vec<_>>().join("\n\n");
        assert_eq!(
            contract_violation_for(Depth::Standard, &input, &output),
            Some("分段过多，疑似展开过度")
        );
    }

    #[test]
    fn accepts_pure_filler_removal() {
        let input = "嗯，那个，我需要你把 timeout 从三十秒改成六十秒。";
        let output = "我需要你把 timeout 从三十秒改成六十秒。";
        assert!(!violates_contract(input, output));
    }

    #[test]
    fn accepts_punctuation_fix() {
        let input = "这个接口返回值为空是为什么";
        let output = "这个接口返回值为空，是为什么？";
        assert!(!violates_contract(input, output));
    }

    // ---- 深度档：分点必须规整 ----

    #[test]
    fn normalizes_markdown_bullets_to_numbers() {
        let raw = "先做三件事：\n- 跑一遍迁移\n* 重启服务\n• 看日志";
        assert_eq!(
            normalize_list_markers(raw),
            "先做三件事：\n1. 跑一遍迁移\n2. 重启服务\n3. 看日志"
        );
    }

    #[test]
    fn renumbers_existing_list() {
        assert_eq!(
            normalize_list_markers("3. 第三\n7. 第七"),
            "1. 第三\n2. 第七"
        );
    }

    #[test]
    fn single_item_list_is_not_numbered() {
        // 模型有时把整段话塞进「1. 」，落字不该出现孤零零的编号
        assert_eq!(
            normalize_list_markers("1. 明天先做咖啡，然后去上班。"),
            "明天先做咖啡，然后去上班。"
        );
    }

    #[test]
    fn does_not_mistake_decimals_for_list_markers() {
        // 「1.5 倍」不是编号，不能被改成「1. 5 倍」
        assert_eq!(normalize_list_markers("延迟是 1.5 倍"), "延迟是 1.5 倍");
    }

    #[test]
    fn strips_markdown_bold_and_headings() {
        assert_eq!(
            normalize_list_markers("## 结论\n**不要把日志写进主线程**"),
            "结论\n不要把日志写进主线程"
        );
    }

    #[test]
    fn deep_contract_allows_lists_but_still_rejects_lost_terms() {
        let input = "先把 migration 跑一遍，然后重启 service，最后看 log。";
        let ok = "1. 跑一遍 migration\n2. 重启 service\n3. 查看 log";
        assert_eq!(contract_violation_for(Depth::Deep, input, ok), None);

        // 分点可以，把术语翻成中文不行
        let bad = "1. 跑一遍迁移\n2. 重启服务\n3. 查看日志";
        assert!(contract_violation_for(Depth::Deep, input, bad).is_some());
    }

    #[test]
    fn deep_tolerates_more_expansion_than_standard() {
        // 同一段输出（1.9 倍长、全是原文里出现过的字）：
        // 标准档判「变长」，深度档放行 —— 两档的差别就在这些尺度上
        let input = "一二三四五六七八九十";
        let output = "一二三四五六七八九十一二三四五六七八九";
        assert!(contract_violation_for(Depth::Standard, input, output).is_some());
        assert_eq!(contract_violation_for(Depth::Deep, input, output), None);
    }

    // ---- 事实项告警：只记录、不拦截 ----

    #[test]
    fn flags_person_change() {
        // 实测抓到的真事：我需要 → 你需要
        let input = "我需要你把 timeout 从 30 秒改成 60 秒。";
        let output = "你需要把 timeout 从 30 秒改成 60 秒。";
        assert_eq!(fact_warnings(input, output), vec!["人称变化"]);
    }

    #[test]
    fn flags_number_change() {
        let input = "把 timeout 从 30 秒改成 60 秒。";
        let output = "把 timeout 从 30 秒改成 90 秒。";
        assert!(fact_warnings(input, output).contains(&"数字变化".to_string()));
    }

    #[test]
    fn flags_negation_change() {
        let input = "这个接口不需要鉴权。";
        let output = "这个接口需要鉴权。";
        assert!(fact_warnings(input, output).contains(&"否定词变化".to_string()));
    }

    #[test]
    fn list_numbers_are_not_fact_warnings() {
        // 「1. 2. 3.」是排版，不是数字事实
        let input = "先跑迁移然后重启服务";
        let output = "1. 跑迁移\n2. 重启服务";
        let got = fact_warnings(input, output);
        assert!(got.is_empty(), "{got:?}");
    }

    #[test]
    fn clean_rewrite_has_no_warnings() {
        let input = "嗯，那个，把日志级别从 info 改成 debug 吧。";
        let output = "把日志级别从 info 改成 debug。";
        assert!(fact_warnings(input, output).is_empty());
    }
}
