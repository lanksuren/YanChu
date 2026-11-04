//! 流式定稿状态机：把「按停顿硬切 + 立刻补句号」换成「带重叠区的滑动窗口」。
//!
//! 原来是：检测到 ≥500ms 静音就切一刀，切出来的那段立刻补「。」并锁死。
//! 结果句号的数量等于停顿的数量 —— 一句话里停顿三次就变成三句：
//!
//! ```text
//! 我想说一下。这个接口。返回空。
//! ```
//!
//! 现在的分工（对应产品定义里 20% / 60% / 20% 的窗口）：
//!
//! ```text
//!        ┌──── 20% ────┬──────── 60% ────────┬──── 20% ────┐
//! 窗口 i  │  与 i-1 共用  │      本窗口独占       │  与 i+1 共用  │
//!        └─────────────┴─────────────────────┴─────────────┘
//! ```
//!
//! * **冻结区**（`frozen`）：滑过去的 60%，永远不再变，屏幕上已经定型的字靠它稳定；
//! * **草稿区**（`soft`）：最后 20% + 下一窗口的前 20%，断点还没定、标点可以再判一次；
//! * 尾巴（gray）不在这里，它还在识别。
//!
//! 关键改动是**断点判定推迟**：一段文字说完了，先放一个临时逗号；
//! 等右文（下一段的开头，或者尾巴的前几个字）出现，才决定这里该是「，」还是「。」。
//! 判据见 [`decide_break`]。

use std::time::Instant;

use serde::Serialize;

/// 草稿区上限（按字）。超过就把前面一段冻死。
pub const SOFT_MAX: usize = 28;
/// 冻结时永远留在草稿区里的字数 —— 这就是「与下一个窗口的重叠区」。
pub const SOFT_KEEP: usize = 12;
/// 这么长的静音才算「人真的停下来了」。实测值 ≈ 真实停顿 - 500ms，(1.4s 真实停顿才算句号)
pub const LONG_PAUSE_MS: u64 = 1200;
/// 左文短于这个字数，就不算一个分句：不加标点，直接和后面接上。
///
/// 这是「逗号太多」的主开关：口语里两三个字一停很常见（「我们可以 / 测试一下」），
/// 每个停顿都给逗号，读起来就是一堆碎片。
pub const MIN_CLAUSE: usize = 8;
/// 上一个标点之后至少要隔这么多字，才允许再插一个逗号。
pub const MIN_GAP: usize = 16;
/// 一句里攒到这么多字还没标点，就必须给一个逗号，否则读起来喘不过气。
pub const MAX_RUN: usize = 32;

/// 这些是「话头」，后面跟逗号是自然的（「好的，我们可以…」）。
/// 只认整段就等于它，不能是「我们可以」这种半个从句。
const DISCOURSE_MARKERS: &[&str] = &[
    "好的", "好", "行", "对", "是的", "嗯", "没问题", "谢谢", "OK", "ok", "那么", "这样",
];

/// 右文以这些词开头 → 说明是在接着上一句说，用逗号。
const CONTINUATIONS: &[&str] = &[
    "然后", "就是", "所以", "但是", "但", "而且", "并且", "还有", "不过", "因为", "如果",
    "另外", "比如", "例如", "接着", "同时", "也就是", "于是", "因此", "这样", "那么", "这个",
    "那个", "第一", "第二", "第三", "首先", "其次", "最后", "再说",
];

/// 右文以这些字开头 → 后面这个词组挂在前半句上，断开会散架。
const MID_SENTENCE_STARTS: &[&str] = &[
    "在", "把", "从", "给", "向", "对", "和", "与", "跟", "的", "地", "得", "了", "着", "过",
    "就", "也", "都", "还", "又", "很", "太", "更",
];

/// 左边以这些字结尾 → 话明显没说完（「放到」「因为我」），用逗号接下去。
const CONNECTORS: &[char] = &[
    '的', '地', '得', '着', '过', '把', '被', '和', '与', '跟', '或', '而', '从', '给', '让',
    '使', '就', '也', '都', '还', '又', '很', '太', '更', '最',
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Break {
    /// 不加标点（左文已经有终结标点）
    None,
    Comma,
    Period,
}

impl Break {
    pub fn as_char(self) -> Option<char> {
        match self {
            Self::None => None,
            Self::Comma => Some('，'),
            Self::Period => Some('。'),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Comma => "comma",
            Self::Period => "period",
        }
    }
}

/// 一次断点判定，写进开发日志 —— 调判据时要能一条条看回来。
#[derive(Debug, Clone, Serialize)]
pub struct BreakRecord {
    pub pause_ms: u64,
    /// 左文末尾（最多 12 字）
    pub left: String,
    /// 右文开头（最多 12 字）
    pub right: String,
    /// comma / period / none
    pub decision: String,
}

/// 流式定稿的状态机。
#[derive(Default)]
pub struct Streamer {
    /// 冻结区：永不再变
    frozen: String,
    /// 草稿区：断点未定，可以重判标点
    soft: String,
    /// 识别原文（只追加，不加工），落盘和对比用
    raw: String,
    /// 等待判定的断点：soft 末尾那个临时逗号是几点放的
    pending: Option<Instant>,
    /// 最后一段原文带的终结标点（「吗？」这类不能在定稿时丢掉）
    last_terminal: Option<char>,
    pub breaks: Vec<BreakRecord>,
}

impl Streamer {
    pub fn new() -> Self {
        Self::default()
    }

    /// 气泡里显示的实色文字：冻结 + 草稿（草稿尾部可能带一个临时逗号）。
    pub fn display(&self) -> String {
        format!("{}{}", self.frozen, self.soft)
    }

    /// 识别原文（未润色、未补标点）。
    pub fn raw(&self) -> &str {
        &self.raw
    }

    /// 冻结区（调试与测试用：这段永远不变）。
    pub fn frozen(&self) -> &str {
        &self.frozen
    }

    pub fn frozen_len(&self) -> usize {
        self.frozen.chars().count()
    }

    pub fn soft_len(&self) -> usize {
        self.soft.chars().count()
    }

    /// 追加一个定稿片段：`seg_raw` 是识别原文，`seg_polished` 是小润色后的样子。
    ///
    /// 追加之后先放一个临时逗号、把断点挂起，等右文出现再决定。
    pub fn push_segment(&mut self, seg_raw: &str, seg_polished: &str) {
        let seg_raw = seg_raw.trim();
        if seg_raw.is_empty() {
            return;
        }

        // 原文照抄（含它自带的标点），供落盘与对比
        self.raw.push_str(seg_raw);
        if let Some(c) = seg_raw.chars().last() {
            if matches!(c, '？' | '！' | '?' | '!') {
                self.last_terminal = Some(c);
            }
        }

        // 上一段的断点如果还没判，用这一段的开头当右文补判
        if self.pending.is_some() {
            let pause = self.pending.map(|t| t.elapsed().as_millis() as u64).unwrap_or(0);
            let right = seg_polished.to_string();
            self.resolve(pause, &right);
        }

        let body = strip_trailing_punct(seg_polished);
        if body.is_empty() {
            return;
        }
        self.soft.push_str(body);

        // 断点挂起：先不写标点，等右文到了再决定加「，」「。」还是什么都不加。
        // 不预先塞一个逗号，是因为它会在等右文的几百毫秒里挂在行尾（看着像错别字）。
        self.pending = Some(Instant::now());

        self.freeze_if_needed();
    }

    /// 尾巴出现了内容 → 右文有了，把挂起的断点判掉。
    ///
    /// `pause_ms` 由调用方从**音频**里量出来（静音多长），比看时钟准：
    /// 走时钟会把 tick 的 450ms 延迟也算进去。
    pub fn see_tail(&mut self, tail_polished: &str, pause_ms: u64) {
        let Some(_at) = self.pending else {
            return;
        };
        let right = tail_polished.trim();
        if right.chars().count() < 2 {
            return;
        }
        self.resolve(pause_ms, right);
    }

    /// 定稿：去掉临时逗号，补回最后一段的终结标点。
    pub fn finish(&mut self) -> String {
        self.pending = None;
        while self.soft.ends_with('，') {
            self.soft.pop();
        }
        if let Some(c) = self.last_terminal {
            if matches!(c, '？' | '！' | '?') {
                self.soft.push(if c == '?' { '？' } else { c });
            }
        }
        self.display()
    }

    /// 判定并写入这个断点的标点。
    fn resolve(&mut self, pause_ms: u64, right: &str) {
        if self.pending.take().is_none() {
            return;
        }
        let left = self.soft.clone();
        let decision = decide_break(&left, right, pause_ms);
        if let Some(c) = decision.as_char() {
            self.soft.push(c);
        }
        self.breaks.push(BreakRecord {
            pause_ms,
            left: tail_chars(&left, 12),
            right: head_chars(right, 12),
            decision: decision.label().to_string(),
        });

        // 断点定了，顺手看看要不要冻结前面一段
        self.freeze_if_needed();
    }

    /// 草稿区太长就把前面一段冻死，永远保留最后 [`SOFT_KEEP`] 个字可改。
    fn freeze_if_needed(&mut self) {
        if self.pending.is_some() {
            return; // 断点还没定，别急着冻
        }
        let len = self.soft.chars().count();
        if len <= SOFT_MAX {
            return;
        }
        let target = len - SOFT_KEEP;
        let cut = best_cut(&self.soft, target);
        let frozen_part: String = self.soft.chars().take(cut).collect();
        let rest: String = self.soft.chars().skip(cut).collect();
        self.frozen.push_str(&frozen_part);
        self.soft = rest;
        log::debug!("流式冻结 {} 字（剩 {} 字可改）", cut, self.soft.chars().count());
    }
}

/// 这个断点该加什么标点。
///
/// 默认是**不加**：口语里三五个字一停太常见（「我们可以 / 测试一下」），
/// 每个停顿都给逗号，读起来就是一堆碎片。只有拿到「这是分句边界」的证据才插标点。
///
/// 判据顺序（先命中先用）：
/// 1. 左文为空 → 不加；
/// 2. 静音够长、左文也够长 → 句号（人真的停下来了，而且这句说完了）；
/// 3. 攒太长了（≥26 字还没标点）→ 逗号，否则读着喘不过气；
/// 4. 右文以承接词开头 → 逗号（「然后」「所以」就是接话的信号）；
/// 5. 左文是「好的/行/对」这种话头 → 逗号；
/// 6. 左文短于 8 字 → **不加**（这是逗号太多的主因）；
/// 7. 离上一个标点不到 12 字 → 不加；
/// 8. 左文以虚词结尾 → 逗号；
/// 9. 其余 → 逗号。
pub fn decide_break(left: &str, right: &str, pause_ms: u64) -> Break {
    let left = left.trim_end_matches(['，', '。', '、', '；', '：', ',', ';', ' ']);
    if left.is_empty() {
        return Break::None;
    }

    let left_len = left.chars().count();
    // 当前这一小段（上一个标点之后，逗号也算）已经有多长
    let since = left
        .rsplit(['。', '，', '、', '；', '：', '！', '？', '.', ',', '!', '?', ';', ':'])
        .next()
        .unwrap_or(left);
    let run_len = since.chars().count();

    // 2. 真的停下来了，而且这句话已经说完整
    if pause_ms >= LONG_PAUSE_MS && left_len >= MIN_CLAUSE {
        return Break::Period;
    }

    // 3. 右文是承接词
    for word in CONTINUATIONS {
        if right.starts_with(word) {
            return Break::Comma;
        }
    }

    // 4. 话头
    if DISCOURSE_MARKERS.contains(&left) {
        return Break::Comma;
    }

    // 5. 右文以介词/助词开头：后面这个词组是挂在前面的，断开会散架
    //    （「的模式 / 在每一句话里面」）
    if MID_SENTENCE_STARTS.iter().any(|w| right.starts_with(w)) {
        return Break::None;
    }

    // 6. 一口气攒太长，必须给个逗号
    if run_len >= MAX_RUN {
        return Break::Comma;
    }

    // 7. 太短，算不上一个分句
    if left_len < MIN_CLAUSE {
        return Break::None;
    }

    // 8. 离上一个标点太近，别再插
    if run_len < MIN_GAP {
        return Break::None;
    }

    // 9. 以虚词结尾，说明还要接着说
    if let Some(last) = left.chars().last() {
        if CONNECTORS.contains(&last) {
            return Break::Comma;
        }
    }

    // 10. 够长、也不是话头 —— 正常断句
    Break::Comma
}

/// 冻结时的切点：优先落在标点后面，找不到就在目标位置硬切。
fn best_cut(text: &str, target: usize) -> usize {
    let chars: Vec<char> = text.chars().collect();
    let lo = target.saturating_sub(8);
    let mut best = None;
    for i in lo..=target.min(chars.len()) {
        if i == 0 || i >= chars.len() {
            continue;
        }
        if matches!(chars[i - 1], '，' | '。' | '！' | '？' | '、' | '；' | '：') {
            best = Some(i);
        }
    }
    best.unwrap_or(target.min(chars.len()))
}

/// 去掉末尾标点（分段拼接时，标点由断点判据重新决定）。
fn strip_trailing_punct(s: &str) -> &str {
    s.trim_end_matches(['。', '，', '、', '；', '：', '！', '？', '.', ',', '!', '?', ';', ':', ' '])
}

fn tail_chars(s: &str, n: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    let start = chars.len().saturating_sub(n);
    chars[start..].iter().collect()
}

fn head_chars(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(text: &str) -> (String, String) {
        (text.to_string(), strip_trailing_punct(text).to_string())
    }

    /// 一句话里停顿三次，不能变成三个句号，也不该每个停顿都给逗号
    #[test]
    fn short_pauses_do_not_add_punctuation_everywhere() {
        let mut s = Streamer::new();
        let (raw, pol) = seg("我想说一下。");
        s.push_segment(&raw, &pol);
        s.see_tail("这个接口返回空", 300);

        let (raw, pol) = seg("这个接口返回空。");
        s.push_segment(&raw, &pol);
        s.see_tail("我怀疑是缓存", 300);

        let (raw, pol) = seg("我怀疑是缓存。");
        s.push_segment(&raw, &pol);
        let text = s.finish();

        assert!(text.matches('。').count() <= 1, "got {text}");
        assert!(text.matches('，').count() <= 1, "逗号不该每个停顿一个：{text}");
    }

    /// 真的停了很久，该断句
    #[test]
    fn long_pause_makes_period() {
        assert_eq!(
            decide_break("第一件事已经做完了", "第二件", LONG_PAUSE_MS),
            Break::Period
        );
        // 同样的停顿，但左文太短，不能断成一句
        assert_eq!(decide_break("我们可以", "测试一下", LONG_PAUSE_MS), Break::None);
    }

    /// 右文是承接词 → 逗号
    #[test]
    fn continuation_words_keep_comma() {
        assert_eq!(decide_break("先把迁移跑一遍", "然后重启服务", 200), Break::Comma);
        assert_eq!(decide_break("接口返回空", "所以我去查了日志", 200), Break::Comma);
    }

    /// 短片段不给标点：这是「逗号太多」的主因
    #[test]
    fn short_fragments_merge() {
        assert_eq!(decide_break("我们可以", "测试一下这个", 200), Break::None);
        assert_eq!(decide_break("我现在", "在每一句话里面", 200), Break::None);
        assert_eq!(decide_break("更换的模式", "我现在", 200), Break::None);
        assert_eq!(decide_break("好的", "我们可以", 200), Break::Comma); // 话头例外
    }

    /// 离上一个标点太近也不给
    #[test]
    fn keeps_a_gap_between_commas() {
        let left = "好的，我们可以测试一下";
        assert_eq!(decide_break(left, "更换的模式", 200), Break::None);
    }

    /// 一逗到底也不行：攒太长了必须给逗号
    #[test]
    fn long_run_gets_comma() {
        let left = "第一先把数据库迁移跑完第二重启服务第三看看健康检查有没有问题";
        assert_eq!(decide_break(left, "第四件事", 200), Break::Comma);
    }

    /// 拿用户真实的测试案例回放：逗号密度必须明显下来
    #[test]
    fn user_sample_stays_readable() {
        let segs = [
            "好的，我们可以。",
            "测试一下这个。",
            "更换的模式。",
            "我现在。",
            "在每一句话里面。",
            "停顿的都很多啊。",
            "是停顿的很多。",
            "然后我看到现在的状态和模式呢。",
            "好像在每一个断点之后都会有一个逗号。",
            "这是我是不是很理解。",
            "也是不太支持的。",
        ];
        let mut s = Streamer::new();
        for (i, text) in segs.iter().enumerate() {
            let (raw, pol) = seg(text);
            s.push_segment(&raw, &pol);
            // 真实链路上，下一段的开头会先作为尾巴出现，右文判定靠它
            let next = segs.get(i + 1).copied().unwrap_or("下一句的开头");
            let head: String = strip_trailing_punct(next).chars().take(8).collect();
            s.see_tail(&head, 600);
        }
        let text = s.finish();
        let commas = text.matches('，').count();
        println!("回放结果（逗号 {commas} 个）：{text}");
        assert!(
            commas <= 6,
            "11 个片段最多插 6 个逗号，实际 {commas} 个：{text}"
        );
    }

    /// 冻结之后，冻结区的字一个都不能变
    #[test]
    fn frozen_prefix_never_changes() {
        let mut s = Streamer::new();
        for i in 0..6 {
            let raw = format!("第{i}段内容大概是十个字左右。");
            let pol = strip_trailing_punct(&raw).to_string();
            s.push_segment(&raw, &pol);
            // 模拟右文到达
            s.see_tail("然后继续往下说别的事情", 300);
        }
        let before = s.frozen().to_string();
        assert!(!before.is_empty(), "攒了这么多字应该已经冻结过");
        s.see_tail("完全不同的右文", 300);
        assert_eq!(before, s.frozen(), "冻结区不许变");
        assert!(s.soft_len() <= SOFT_MAX + 1, "草稿区不该无限长");
    }

    /// 最后一句是问句，定稿不能丢问号
    #[test]
    fn keeps_question_mark() {
        let mut s = Streamer::new();
        let (raw, pol) = seg("这个接口为什么返回空？");
        s.push_segment(&raw, &pol);
        let text = s.finish();
        assert!(text.ends_with('？'), "got {text}");
    }

    /// 定稿末尾不留临时逗号
    #[test]
    fn finish_drops_provisional_comma() {
        let mut s = Streamer::new();
        let (raw, pol) = seg("先把这段逻辑抽出来。");
        s.push_segment(&raw, &pol);
        let text = s.finish();
        assert!(!text.ends_with('，'), "got {text}");
    }

    /// 每段都带自己的标点（ASR 常见），拼起来不能出现「？。」
    #[test]
    fn segments_do_not_double_punctuate() {
        let mut s = Streamer::new();
        let (raw, pol) = seg("第一批数据。");
        s.push_segment(&raw, &pol);
        s.see_tail("第二批数据", 300);
        let (raw, pol) = seg("第二批数据。");
        s.push_segment(&raw, &pol);
        let text = s.finish();
        assert!(!text.contains("。。"), "got {text}");
        assert!(!text.contains("。，"), "got {text}");
    }
}
