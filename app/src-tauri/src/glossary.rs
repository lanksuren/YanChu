//! 热词纠正层：把小模型听错的术语拉回用户词表。
//!
//! 验收数据说明这块是刚需：SenseVoice 对短的英文技术词偏弱，
//! `helper → Helber`、`utils → Uils`、`node → Notode`，而这些词恰好是目标用户的高频词。
//!
//! 设计上刻意保守 —— 误替换比不替换更糟：
//! * 只在编辑距离足够小时才替换，短的词阈值更严；
//! * 候选歧义（两个词表项一样近）时放弃；
//! * 替换结果一律用词表里的规范写法。
//!
//! 同时支持两种条目：
//! * `helper`        普通术语，按英文单词做模糊匹配
//! * `入餐=入参`      显式映射，做整串替换（中文同音字这类没法靠编辑距离推）
//!
//! 第三层是**拼音同音**：中文术语被识别成同音字时，编辑距离可能大到 3、4
//! （`负载均衡 → 负在均衡` 距离 2，`幂等性 → 密等性` 距离 1），靠拼字纠不回来，
//! 但读音是一样的。所以对词表里的**中文术语**再算一遍拼音，命中就整词替换。
//! 只做 ≥3 个汉字的词：两个字的同音词太多（实现/时限、公式/公示），误伤比救回多。

use std::path::Path;

/// 默认词表：面向中英混说的开发者场景，不含任何只服务测试语料的词。
const DEFAULT_TERMS: &[&str] = &[
    "API", "JSON", "CSV", "SQL", "HTTP", "HTTPS", "URL", "URI", "UUID", "token", "cookie",
    "cache", "queue", "thread", "async", "await", "lock", "mutex", "index", "schema",
    "migration", "rollback", "deploy", "release", "staging", "production", "debug", "timeout",
    "retry", "buffer", "stream", "socket", "proxy", "gateway", "service", "handler",
    "middleware", "controller", "router", "helper", "utils", "config", "env", "log", "metrics",
    "optional", "required", "request", "response", "header", "payload", "endpoint", "parameter",
    "argument", "return", "value", "array", "object", "string", "boolean", "number", "float",
    "error", "warning", "exception", "panic", "assert", "health", "health check", "benchmark",
    "performance", "latency", "throughput", "memory", "leak", "thread pool", "callback",
    "promise", "closure", "iterator", "generator", "decorator", "instance", "interface",
    "database", "table", "query", "index", "transaction", "shard", "replica", "backup",
    "server", "client", "host", "port", "domain", "certificate", "session", "auth", "login",
    "permission", "role", "version", "install", "upgrade", "script", "command", "terminal",
    "shell", "patch", "diff", "conflict", "workflow", "pipeline", "artifact", "registry",
    "docker", "docker compose", "kubernetes", "redis", "mysql", "postgres", "nginx", "kafka", "git",
    "rebase", "merge", "branch", "commit", "push", "pull", "issue", "bug", "feature",
    "refactor", "test", "pytest", "unittest", "mock", "lambda", "function", "node", "npm",
    "pnpm", "rust", "go", "python", "java", "typescript", "javascript", "react", "vue", "CI",
    "CD", "PR", "MR", "health check", "load balancer", "rate limit", "int8", "fp16", "onnx",
    // 中文技术术语：只参与拼音同音纠错（≥3 字），不参与英文的编辑距离匹配。
    // 内置这几个是因为目标用户天天说，而它们被识别成同音字的概率不低
    // （负载均衡 → 负在均衡、幂等性 → 密等性）。
    "负载均衡", "健康检查", "幂等性", "灰度发布", "消息队列", "分布式锁", "熔断器", "限流器",
];

#[derive(Debug, Clone)]
enum Piece {
    Word(String),
    Other(String),
}

/// 词表里的中文术语，预先算好拼音，纠正时只做字符串比较。
#[derive(Debug, Clone)]
struct CjkTerm {
    term: String,
    /// 无声调拼音，例如「负载均衡」→ fuzaijunheng
    pinyin: String,
    len: usize,
}

#[derive(Debug, Clone)]
pub struct Glossary {
    /// 规范写法，例如 "health check"
    terms: Vec<String>,
    /// 小写、去空格后的形态，用于匹配
    normalized: Vec<String>,
    /// 显式的整串映射：「入餐」→「入参」
    literal: Vec<(String, String)>,
    /// 中文术语的拼音索引（≥3 个汉字）
    cjk: Vec<CjkTerm>,
}

impl Default for Glossary {
    fn default() -> Self {
        Self::from_terms(DEFAULT_TERMS.iter().map(|s| s.to_string()).collect())
    }
}

impl Glossary {
    /// 内置词表的文本形态，供控制面板首次打开时展示和编辑。
    pub fn default_text() -> String {
        let mut out = String::from("# 每行一个词；需要显式纠正时写「会被误读成=正确写法」\n");
        for term in DEFAULT_TERMS {
            out.push_str(term);
            out.push('\n');
        }
        out
    }

    pub fn from_terms(terms: Vec<String>) -> Self {
        let mut glossary = Self {
            terms: Vec::new(),
            normalized: Vec::new(),
            literal: Vec::new(),
            cjk: Vec::new(),
        };
        for term in terms {
            glossary.push(term);
        }
        glossary
    }

    /// 加载词表：内置默认 + 文件里追加。文件不存在就用默认。
    pub fn load(path: &Path) -> Self {
        let mut glossary = Self::default();
        if let Ok(content) = std::fs::read_to_string(path) {
            for line in content.lines() {
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }
                glossary.push(line.to_string());
            }
            log::info!("热词表: {} 条（含 {}）", glossary.terms.len(), path.display());
        }
        glossary
    }

    fn push(&mut self, raw: String) {
        if let Some((from, to)) = raw.split_once('=') {
            let from = from.trim().to_string();
            let to = to.trim().to_string();
            if !from.is_empty() && !to.is_empty() {
                self.literal.push((from, to));
            }
            return;
        }

        let term = raw.trim();
        if term.is_empty() {
            return;
        }
        if let Some(ct) = cjk_term(term) {
            self.cjk.push(ct);
        }
        self.terms.push(term.to_string());
        self.normalized.push(normalize_term(term));
    }

    pub fn len(&self) -> usize {
        self.terms.len()
    }

    /// 纠正一段文本，返回纠正后的文本和所有改动（用于统计误伤）。
    pub fn correct(&self, text: &str) -> (String, Vec<(String, String)>) {
        let mut fixes = Vec::new();
        let mut out = self.correct_literal(text, &mut fixes);
        out = self.correct_homophones(&out, &mut fixes);
        out = self.correct_words(&out, &mut fixes);
        (out, fixes)
    }

    /// 中文术语的同音纠错：读音一样、写法不同的整词替换。
    ///
    /// 只在词表里出现过的术语上动手，所以误伤面被词表本身限住了 ——
    /// 用户没加的词，这里一个都不会改。
    fn correct_homophones(&self, text: &str, fixes: &mut Vec<(String, String)>) -> String {
        // 关掉开关：`YANCHU_PINYIN=0`
        if self.cjk.is_empty() || !pinyin_enabled() {
            return text.to_string();
        }

        let chars: Vec<char> = text.chars().collect();
        // 每个汉字的无声调拼音，一次算好；非汉字是 None
        let py: Vec<Option<String>> = chars.iter().map(|c| char_pinyin(*c)).collect();

        let mut out = String::with_capacity(text.len() + 8);
        let mut i = 0;
        'outer: while i < chars.len() {
            for term in &self.cjk {
                if i + term.len > chars.len() {
                    continue;
                }
                let window = &chars[i..i + term.len];
                if !window.iter().all(|c| is_cjk(*c)) {
                    continue;
                }
                // 已经写对了就别动（大小写/全半角由别的层管）
                let candidate: String = window.iter().collect();
                if candidate == term.term {
                    continue;
                }

                let mut joined = String::with_capacity(term.pinyin.len());
                let mut complete = true;
                for k in i..i + term.len {
                    match &py[k] {
                        Some(p) => joined.push_str(p),
                        None => {
                            complete = false;
                            break;
                        }
                    }
                }
                if !complete || joined != term.pinyin {
                    continue;
                }

                out.push_str(&term.term);
                fixes.push((candidate, term.term.clone()));
                i += term.len;
                continue 'outer;
            }
            out.push(chars[i]);
            i += 1;
        }
        out
    }

    fn correct_literal(&self, text: &str, fixes: &mut Vec<(String, String)>) -> String {
        let mut out = text.to_string();
        for (from, to) in &self.literal {
            if out.contains(from.as_str()) {
                out = out.replace(from.as_str(), to.as_str());
                fixes.push((from.clone(), to.clone()));
            }
        }
        out
    }

    fn correct_words(&self, text: &str, fixes: &mut Vec<(String, String)>) -> String {
        let pieces = split(text);
        let mut out = String::with_capacity(text.len() + 16);
        let mut i = 0;

        while i < pieces.len() {
            match &pieces[i] {
                Piece::Other(s) => {
                    out.push_str(s);
                    i += 1;
                }
                Piece::Word(word) => {
                    // 先试「两个词拼起来」：模型常把 docker compose 读成 dockercompose
                    if let Some((joined_fix, consumed)) = self.try_join(&pieces, i) {
                        let original = format!(
                            "{} {}",
                            word,
                            self.word_at(&pieces, i + 2).unwrap_or_default()
                        );
                        // 拼完和原文一模一样就不算一次纠正，否则统计里会出现空替换
                        if joined_fix != original {
                            fixes.push((original, joined_fix.clone()));
                        }
                        out.push_str(&joined_fix);
                        i += consumed;
                        continue;
                    }

                    // 再试单词模糊匹配
                    if let Some(canonical) = self.best_match(word) {
                        if canonical != word.as_str() {
                            fixes.push((word.clone(), canonical.to_string()));
                            out.push_str(canonical);
                            i += 1;
                            continue;
                        }
                    }

                    out.push_str(word);
                    i += 1;
                }
            }
        }
        out
    }

    fn word_at<'a>(&self, pieces: &'a [Piece], idx: usize) -> Option<&'a str> {
        match pieces.get(idx) {
            Some(Piece::Word(w)) => Some(w.as_str()),
            _ => None,
        }
    }

    /// 两个相邻英文词拼起来是否恰好命中词表（允许 1 个字符的误差）。
    fn try_join(&self, pieces: &[Piece], i: usize) -> Option<(String, usize)> {
        let Piece::Word(first) = &pieces[i] else {
            return None;
        };

        // 形态 A：中间有个空格 -> ["P", " ", "test"]
        // 形态 B：直接贴着（本身就是一整块）-> 不算 join
        let (second, consumed) = match pieces.get(i + 1) {
            Some(Piece::Other(sep)) if sep.as_str() == " " => match pieces.get(i + 2) {
                Some(Piece::Word(w)) => (w.clone(), 3),
                _ => return None,
            },
            _ => return None,
        };

        let joined = format!("{first}{second}");
        let key = normalize_term(&joined);

        // 拼起来之后必须明显更长，否则「a b」这种会被乱拼
        if key.len() < 5 {
            return None;
        }

        let mut best: Option<(usize, usize)> = None;
        for (idx, norm) in self.normalized.iter().enumerate() {
            let d = edit_distance(key.as_bytes(), norm.as_bytes());
            // 拼接本身就是一次大胆猜测，容错只给 1
            if d <= 1 {
                match best {
                    Some((bd, _)) if bd <= d => {}
                    _ => best = Some((d, idx)),
                }
            }
        }

        best.map(|(_, idx)| (self.terms[idx].clone(), consumed))
    }

    fn best_match<'a>(&'a self, token: &str) -> Option<&'a str> {
        let key = normalize_term(token);
        if key.is_empty() {
            return None;
        }

        // 已经是词表里的词（可能大小写不同）就直接用规范写法
        if let Some(idx) = self.normalized.iter().position(|n| *n == key) {
            return Some(self.terms[idx].as_str());
        }

        let limit = distance_limit(key.len());

        // 收集所有在阈值内的候选
        let mut candidates: Vec<(usize, usize)> = self
            .normalized
            .iter()
            .enumerate()
            .map(|(idx, norm)| (edit_distance(key.as_bytes(), norm.as_bytes()), idx))
            .filter(|(d, _)| *d <= limit)
            .collect();

        if candidates.is_empty() {
            return None;
        }

        let best_distance = candidates.iter().map(|(d, _)| *d).min().expect("非空");
        candidates.retain(|(d, _)| *d == best_distance);

        // 并列时用首字母消歧：`dbug` 到 `debug` 和 `bug` 都是 1，
        // 但 dbug 的首字母是 d，只有 debug 对得上。
        if candidates.len() > 1 {
            let first = key.chars().next();
            candidates.retain(|(_, idx)| norm_first(&self.normalized[*idx]) == first);
        }

        // 还是并列就放弃 —— 猜错比不改更糟
        if candidates.len() != 1 {
            return None;
        }

        let idx = candidates[0].1;
        // 太短的词换个字母就变意思，要求首字母一致
        if key.len() <= 3 && norm_first(&self.normalized[idx]) != key.chars().next() {
            return None;
        }

        Some(self.terms[idx].as_str())
    }
}

fn norm_first(s: &str) -> Option<char> {
    s.chars().next()
}

fn is_cjk(c: char) -> bool {
    matches!(c as u32, 0x4E00..=0x9FFF | 0x3400..=0x4DBF)
}

/// 拼音同音纠错只对「整词都是汉字、3–6 个字」的术语生效。
///
/// 为什么卡 3 个字：两个字的同音词太密（实现/时限、公式/公示、参数/餐数），
/// 一个词表项就能把正常句子改错；三个字以上同音碰撞的概率低得多。
fn cjk_term(term: &str) -> Option<CjkTerm> {
    let chars: Vec<char> = term.chars().collect();
    if !(3..=6).contains(&chars.len()) || !chars.iter().all(|c| is_cjk(*c)) {
        return None;
    }
    let mut pinyin = String::with_capacity(chars.len() * 6);
    for c in &chars {
        pinyin.push_str(&char_pinyin(*c)?);
    }
    Some(CjkTerm {
        term: term.to_string(),
        pinyin,
        len: chars.len(),
    })
}

/// 单个汉字的无声调拼音；非汉字返回 None。
fn char_pinyin(c: char) -> Option<String> {
    use pinyin::ToPinyin;
    let s = c.to_string();
    s.as_str()
        .to_pinyin()
        .next()
        .flatten()
        .map(|p| p.plain().to_string())
}

fn pinyin_enabled() -> bool {
    match std::env::var("YANCHU_PINYIN") {
        Ok(v) => !matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "0" | "off" | "false" | "no"
        ),
        Err(_) => true,
    }
}

/// 匹配用的归一化：小写 + 去掉空格和连字符
fn normalize_term(term: &str) -> String {
    term.chars()
        .filter(|c| !c.is_whitespace() && *c != '-' && *c != '_')
        .flat_map(|c| c.to_lowercase())
        .collect()
}

/// 编辑距离阈值随长度收紧 —— 短词容错小。
///
/// 这组数是被误伤数据压出来的：第一版对 4 字词放宽到 2，
/// 结果 `info → int8`（距离 2）被误改；对 2 字词放宽到 1，
/// 结果 `Pt → PR` 被误改。误替换对用户的伤害比不替换大得多，
/// 所以短词一律只接受完全一致。
fn distance_limit(len: usize) -> usize {
    match len {
        0..=3 => 0,
        4..=5 => 1,
        6..=9 => 2,
        _ => 3,
    }
}

fn split(text: &str) -> Vec<Piece> {
    let mut pieces = Vec::new();
    let mut buf = String::new();
    let mut buf_is_word = false;

    for c in text.chars() {
        let is_word = c.is_ascii_alphanumeric();
        if is_word != buf_is_word && !buf.is_empty() {
            pieces.push(if buf_is_word {
                Piece::Word(std::mem::take(&mut buf))
            } else {
                Piece::Other(std::mem::take(&mut buf))
            });
        }
        buf_is_word = is_word;
        buf.push(c);
    }
    if !buf.is_empty() {
        pieces.push(if buf_is_word {
            Piece::Word(buf)
        } else {
            Piece::Other(buf)
        });
    }
    pieces
}

fn edit_distance(a: &[u8], b: &[u8]) -> usize {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn g() -> Glossary {
        Glossary::default()
    }

    #[test]
    fn fixes_near_miss_english() {
        let (out, _) = g().correct("把这段逻辑抽成一个 Helber 函数");
        assert!(out.contains("helper"), "got {out}");
    }

    #[test]
    fn fixes_utils() {
        let (out, _) = g().correct("放到 Uils 里");
        assert!(out.contains("utils"), "got {out}");
    }

    #[test]
    fn fixes_joined_words() {
        // 模型把 docker compose 读成 dockercompose 或者分开写
        let (out, _) = g().correct("生成一个 dockercompose 文件");
        assert!(out.contains("docker"), "got {out}");
    }

    #[test]
    fn fixes_split_words() {
        // 反过来：time out 应该合回 timeout
        let (out, _) = g().correct("把 time out 改成 60 秒");
        assert!(out.contains("timeout"), "got {out}");
    }

    #[test]
    fn leaves_correct_words_alone() {
        let (out, fixes) = g().correct("把 JSON 格式化一下");
        assert_eq!(out, "把 JSON 格式化一下");
        assert!(fixes.is_empty());
    }

    #[test]
    fn does_not_touch_chinese() {
        let (out, _) = g().correct("这个接口的返回值为空");
        assert_eq!(out, "这个接口的返回值为空");
    }

    #[test]
    fn literal_mapping_works() {
        let glossary = Glossary::from_terms(vec!["入餐=入参".to_string()]);
        let (out, _) = glossary.correct("这个函数的入餐类型改一下");
        assert!(out.contains("入参"), "got {out}");
    }

    #[test]
    fn ambiguous_short_token_is_left_alone() {
        // "an" 离 api / env 都只有 1，歧义时不能乱改
        let (out, _) = g().correct("an apple");
        assert_eq!(out, "an apple");
    }

    #[test]
    fn single_letter_is_not_rewritten() {
        let (out, _) = g().correct("用 R 重写");
        // R 太短，编辑距离到 rust 是 3，超出阈值
        assert_eq!(out, "用 R 重写");
    }

    #[test]
    fn preserves_canonical_case() {
        let (out, _) = g().correct("这个 api 有点慢");
        assert!(out.contains("API"), "got {out}");
    }

    // ---- 误伤回归：这两个都是实测抓到的误替换 ----

    #[test]
    fn does_not_rewrite_info_to_int8() {
        // info 到 int8 编辑距离是 2，4 字词只允许 1，不该动
        let (out, fixes) = g().correct("把日志级别从 info 改成 debug");
        assert!(out.contains("info"), "got {out}");
        assert!(fixes.is_empty(), "不该有替换: {fixes:?}");
    }

    #[test]
    fn does_not_rewrite_two_letter_token() {
        // Pt 到 PR 距离 1，但 2 字词必须完全一致才认
        let (out, fixes) = g().correct("用 Pt 跑一下单元测试");
        assert!(out.contains("Pt"), "got {out}");
        assert!(fixes.is_empty(), "不该有替换: {fixes:?}");
    }

    #[test]
    fn still_fixes_five_letter_typos() {
        // 收紧阈值之后，该救的还得救
        let (out, _) = g().correct("改成 opional 类型");
        assert!(out.contains("optional"), "got {out}");
    }

    // ---- 拼音同音层 ----

    #[test]
    fn fixes_homophone_cjk_term() {
        // 「负载均衡」被识别成「负在均衡」：拼字距离 1 但读音完全一样
        let glossary = Glossary::from_terms(vec!["负载均衡".to_string()]);
        let (out, fixes) = glossary.correct("这个负在均衡要加一个健康检查");
        assert!(out.contains("负载均衡"), "got {out}");
        assert_eq!(fixes, vec![("负在均衡".to_string(), "负载均衡".to_string())]);
    }

    #[test]
    fn homophone_skips_two_char_terms() {
        // 两个字的同音词太多，宁可不改
        let glossary = Glossary::from_terms(vec!["入参".to_string()]);
        let (out, fixes) = glossary.correct("这个函数的入餐要校验");
        assert!(out.contains("入餐"), "got {out}");
        assert!(fixes.is_empty());
    }

    #[test]
    fn homophone_leaves_correct_word_alone() {
        let glossary = Glossary::from_terms(vec!["负载均衡".to_string()]);
        let (out, fixes) = glossary.correct("这个负载均衡要加一个健康检查");
        assert_eq!(out, "这个负载均衡要加一个健康检查");
        assert!(fixes.is_empty());
    }

    #[test]
    fn homophone_keeps_english_terms_working() {
        // 加了中文术语之后，英文那条路不能被影响
        let glossary = Glossary::from_terms(vec!["负载均衡".to_string(), "helper".to_string()]);
        let (out, _) = glossary.correct("抽成一个 Helber 函数");
        assert!(out.contains("helper"), "got {out}");
    }

    #[test]
    fn pinyin_table_is_available() {
        // 这条测试同时验证依赖是否真的可用（离线环境会在这里就炸）
        assert_eq!(char_pinyin('负').as_deref(), Some("fu"));
        assert_eq!(char_pinyin('a'), None);
    }

    #[test]
    fn default_glossary_has_cjk_terms_for_homophones() {
        // 内置词表必须带几个中文术语，否则拼音层默认是死的
        let g = g();
        assert!(!g.cjk.is_empty(), "内置词表没有中文术语，拼音层不会生效");
        let (out, fixes) = g.correct("这个负在均衡要加一个健康检查");
        assert!(out.contains("负载均衡"), "got {out}");
        assert_eq!(fixes.len(), 1, "{fixes:?}");
    }

    #[test]
    fn homophone_does_not_touch_ordinary_sentence() {
        // 普通句子不能被拼音层改坏
        let g = g();
        let text = "这个接口的返回值为空，数据库连接也是断的";
        let (out, fixes) = g.correct(text);
        assert_eq!(out, text);
        assert!(fixes.is_empty(), "{fixes:?}");
    }
}
