//! 小润色：规则层，跑在模型前面，负责那些「便宜且确定」的修正。
//!
//! 这一层只管措辞，不动语序 —— 语序归大润色。两层的契约是
//! 「实字集合必须完全一致」，规则层尤其不能把词删没了。
//!
//! 语气词是最容易做错的地方：「这个」在「这个接口」里是正常的指示代词，
//! 在「这个，我觉得」里才是口头语。所以删除必须带上下文条件，
//! 只在「被标点单独框住」「句首」「重复出现」这几种情况下动手。

/// 句首口头语。只在后面还有实际内容时才删，避免把一整句「嗯」删成空。
const LEADING_FILLERS: &[&str] = &[
    "嗯", "呃", "额", "啊", "哦", "唉", "那个", "那么", "就是说", "然后就是", "其实", "反正",
];

/// 被标点单独框住的语气词。只有这种形态才敢无条件删。
const ISOLATED_FILLERS: &[&str] = &[
    "嗯", "呃", "额", "啊", "哦", "唉", "那个", "这个", "就是说", "然后", "对吧", "你知道吧",
    "怎么说", "就是说啊",
];

/// 允许折叠的重复词。只列这些，不能通用折叠 ——
/// 「看看」「试试」是正常说法，折叠了就错。
const COLLAPSIBLE_REPEATS: &[&str] = &[
    "嗯", "呃", "额", "啊", "就是", "然后", "那个", "这个", "我", "你", "他", "她", "它",
];

/// 纯语气词：这些字不会出现在任何词里，可以无条件删。
/// （「额」不行 —— 「金额」「额度」都是正常词）
const PURE_INTERJECTIONS: &[&str] = &["嗯", "呃"];

/// 整词口头禅：可以出现在句子中间，删掉不影响语义。
/// 但要注意「也就是说」里的「就是说」不能删，所以带前字守卫。
const PHRASE_FILLERS: &[&str] = &[
    "就是说", "然后就是", "你知道吧", "怎么说呢", "怎么说", "对吧",
];

pub fn rule_polish(raw: &str) -> String {
    polish_inner(raw, true)
}

/// 流式中间结果用：规则全跑，但**不补终点标点**。
///
/// 句子还没说完就补句号，正是「停顿一多满屏句号」的来源。
/// 终点标点只在松手、整段定稿时补一次。
pub fn rule_polish_partial(raw: &str) -> String {
    polish_inner(raw, false)
}

fn polish_inner(raw: &str, terminal: bool) -> String {
    let mut text = raw.trim().to_string();
    if text.is_empty() {
        return text;
    }

    text = strip_leading_filler(&text);
    text = collapse_repeated_words(&text);
    text = strip_isolated_fillers(&text);
    text = strip_pure_interjections(&text);
    text = strip_phrase_fillers(&text);
    text = strip_demonstrative_fillers(&text);
    text = tidy_punctuation(&text);
    text = collapse_repeated_punctuation(&text);
    text = repair_mid_sentence_period(&text);
    text = soften_internal_periods(&text);
    text = space_cjk_and_latin(&text);
    text = normalize_spaces(&text);
    if terminal {
        text = ensure_terminal_punctuation(&text);
    }

    text
}

/// 「抽成一个。helper 函数」→「抽成一个 helper 函数」
//
// ASR 会在中英切换的停顿处插一个句号。判据刻意收得很紧：
/// **句号前面是汉字、后面紧跟小写英文字母或数字**才认。
/// 大写开头的英文更可能是真的新句子（「这样做不对。Rust 里…」），不动。
fn repair_mid_sentence_period(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len() + 4);

    for (i, &c) in chars.iter().enumerate() {
        let mid = c == '。'
            && i > 0
            && is_cjk(chars[i - 1])
            && matches!(chars.get(i + 1), Some(n) if n.is_ascii_lowercase() || n.is_ascii_digit());
        if mid {
            out.push(' ');
        } else {
            out.push(c);
        }
    }
    out
}

/// 后面接这些词时，前面的句号多半是 ASR 按停顿硬加的，降级成逗号。
///
/// 这些词本身就把分句连起来（「，如果……」「，然后……」），
/// 前面用句号读起来是断的。真正的句子边界不会以这些词开头。
const CLAUSE_STARTERS: &[&str] = &[
    "如果", "然后", "所以", "但是", "不过", "因为", "而且", "另外", "同时", "还有", "比如",
    "例如", "于是", "因此", "否则", "要不", "虽然", "即使", "除非", "一旦",
];

fn soften_internal_periods(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());

    for (i, &c) in chars.iter().enumerate() {
        if c == '。' {
            let rest: String = chars[i + 1..].iter().take(4).collect();
            let rest = rest.trim_start();
            if CLAUSE_STARTERS.iter().any(|w| rest.starts_with(w)) {
                out.push('，');
                continue;
            }
        }
        out.push(c);
    }
    out
}

/// 「我觉得嗯这个事情」→「我觉得这个事情」
///
/// 只处理那些不可能组成词的字，所以敢在句中出现就删。
fn strip_pure_interjections(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        if PURE_INTERJECTIONS.contains(&ch.to_string().as_str()) {
            continue;
        }
        out.push(ch);
    }

    // 整句只有语气词时（用户只说了个「嗯」），不要删成空
    let meaningful = out
        .chars()
        .any(|c| c.is_alphanumeric() || is_cjk(c) || matches!(c, ' '));
    if meaningful {
        out
    } else {
        text.to_string()
    }
}



/// 「然后那个我们把…」里的「就是说 / 然后就是 / 你知道吧」这类整词口头禅。
///
/// 前后有保护：「也就是说」不能删成「也」。
fn strip_phrase_fillers(text: &str) -> String {
    let mut out = text.to_string();
    for filler in PHRASE_FILLERS {
        let mut result = String::with_capacity(out.len());
        let chars: Vec<char> = out.chars().collect();
        let target: Vec<char> = filler.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            let matches = i + target.len() <= chars.len()
                && chars[i..i + target.len()] == target[..];
            // 前面是「也」时不动（也就是、也就是说）
            let guarded = i > 0 && matches!(chars[i - 1], '也' | '就' | '这');
            if matches && !guarded {
                i += target.len();
            } else {
                result.push(chars[i]);
                i += 1;
            }
        }
        out = result;
    }
    out
}

/// 删掉语气词之后可能留下「，，」「，。」这种，整理一下。
fn tidy_punctuation(text: &str) -> String {
    let mut out = text.to_string();
    for (from, to) in [
        ("，，", "，"),
        ("，。", "。"),
        ("，、", "、"),
        ("，！", "！"),
        ("，？", "？"),
        ("， ", "，"),
        (" ，", "，"),
        // 静音尾巴常被识别成一个「。」，接在问号/叹号后面就是「？。」这种怪东西
        ("？。", "？"),
        ("！。", "！"),
        // 断点判据有时会在问号后面再补一个逗号（「怎样的？，那么…」）
        ("？，", "？"),
        ("！，", "！"),
        ("。？", "？"),
        ("。！", "！"),
        ("。，", "。"),
    ] {
        while out.contains(from) {
            out = out.replace(from, to);
        }
    }
    out
}

/// 「，嗯，」→「，」；「。那个，」→「。」
fn strip_isolated_fillers(text: &str) -> String {
    let mut out = text.to_string();
    for filler in ISOLATED_FILLERS {
        let patterns = [
            format!("，{filler}，"),
            format!("，{filler}。"),
            format!("。{filler}，"),
            format!("、{filler}，"),
            format!("，{filler}、"),
        ];
        let replacements = ["，", "。", "。", "，", "，"];
        for (p, r) in patterns.iter().zip(replacements.iter()) {
            out = out.replace(p.as_str(), r);
        }
    }
    out
}

/// 「然后然后」→「然后」，「我我」→「我」
fn collapse_repeated_words(text: &str) -> String {
    let mut out = text.to_string();
    for word in COLLAPSIBLE_REPEATS {
        let doubled = format!("{word}{word}");
        while out.contains(doubled.as_str()) {
            out = out.replace(doubled.as_str(), word);
        }
    }
    out
}

fn strip_leading_filler(text: &str) -> String {
    let mut out = text;
    loop {
        let mut matched = false;
        for filler in LEADING_FILLERS {
            if let Some(rest) = out.strip_prefix(filler) {
                // 「那个/这个」后面跟名词时是指示代词（那个产品经理），不能删
                if matches!(*filler, "那个" | "这个") {
                    if let Some(next) = rest.chars().next() {
                        let pronoun = matches!(next, '我' | '你' | '咱');
                        let punct = matches!(next, '，' | ',' | '、' | ' ' | '。' | '！' | '？');
                        if !pronoun && !punct {
                            continue;
                        }
                    }
                }

                // 后面还有内容才敢删
                let rest = rest.trim_start_matches(['，', ',', '、', ' ']);
                if !rest.is_empty() {
                    out = rest;
                    matched = true;
                    break;
                }
            }
        }
        if !matched {
            break;
        }
    }
    out.to_string()
}

fn collapse_repeated_punctuation(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last_punct: Option<char> = None;

    for ch in text.chars() {
        let is_punct = matches!(
            ch,
            '。' | '，' | '、' | '？' | '！' | '；' | '：' | ',' | '?' | '!'
        );
        if is_punct && Some(ch) == last_punct {
            continue;
        }
        last_punct = if is_punct { Some(ch) } else { None };
        out.push(ch);
    }
    out
}

/// 「用 Rust 写」→「用 Rust 写」，但「用Rust写」→「用 Rust 写」。
fn space_cjk_and_latin(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len() + 8);

    for (i, &ch) in chars.iter().enumerate() {
        out.push(ch);
        let Some(&next) = chars.get(i + 1) else {
            continue;
        };
        let gap =
            (is_cjk(ch) && is_latin_or_digit(next)) || (is_latin_or_digit(ch) && is_cjk(next));
        if gap && next != ' ' && ch != ' ' {
            out.push(' ');
        }
    }
    out
}



fn is_latin_or_digit(c: char) -> bool {
    c.is_ascii_alphanumeric()
}

fn normalize_spaces(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut prev_space = false;
    for ch in text.chars() {
        if ch == ' ' {
            if !prev_space {
                out.push(ch);
            }
            prev_space = true;
        } else {
            prev_space = false;
            out.push(ch);
        }
    }
    out.trim().to_string()
}

fn ensure_terminal_punctuation(text: &str) -> String {
    let Some(last) = text.chars().last() else {
        return text.to_string();
    };
    if matches!(last, '。' | '？' | '！' | '?' | '!' | '，' | ',' | '、' | '.') {
        return text.to_string();
    }
    format!("{text}。")
}

/// 「那个/这个」只有在后面紧跟「我/你/咱」时才是口头语
/// （「那个我们就这么办」）。后面跟名词的（那个接口、这个方案）是指示代词，必须保留。
fn strip_demonstrative_fillers(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        let is_demo = chars[i] == '那' || chars[i] == '这';
        let has_ge = i + 1 < chars.len() && chars[i + 1] == '个';
        let next = chars.get(i + 2).copied();
        let filler = is_demo
            && has_ge
            && matches!(next, Some('我') | Some('你') | Some('咱'));
        if filler {
            i += 2; // 丢掉「那个」「这个」
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

fn is_cjk(c: char) -> bool {
    matches!(c as u32, 0x4E00..=0x9FFF | 0x3400..=0x4DBF)
}

/// 原文明显在列举时，把它排成编号清单。
///
/// 为什么不让模型做：2B 对「什么时候该分点」几乎没有判断力 ——
/// 提示词里写「讲步骤就分点」，它会把整段叙述也拆成清单；
/// 收紧成「只有明确列举才分点」，它又一条都不列（两版实测都是 10 段里
/// 只有 0 段出了多段文本）。
///
/// 但「第一步 / 一个是 / 如果」这些标记在文本里是稳定的，用规则切最可靠：
/// 至少两个强标记（第N步、首先、其次）或三个弱标记（一个是、还有、另外、如果）
/// 才认定为列举，否则原样返回。
pub fn format_enumerations(text: &str) -> String {
    // 模型已经自己排好编号的，不要再动
    if text.trim_start().starts_with("1. ") || text.contains("\n1. ") {
        return text.to_string();
    }

    let chars: Vec<char> = text.chars().collect();
    let mut steps = Vec::new();
    let mut pairs = Vec::new();
    let mut conds = Vec::new();

    let mut i = 0;
    while i < chars.len() {
        if let Some((kind, len)) = marker_at(&chars, i) {
            let prev_ok = i == 0
                || matches!(
                    chars[i - 1],
                    '。' | '！' | '？' | '：' | '，' | '；' | '、' | '\n'
                );
            if prev_ok {
                match kind {
                    EnumKind::Step => steps.push(i),
                    EnumKind::Pair => pairs.push(i),
                    EnumKind::Cond => conds.push(i),
                }
                i += len;
                continue;
            }
        }
        i += 1;
    }

    let marks = if steps.len() >= 2 {
        steps
    } else if pairs.len() >= 3 {
        pairs
    } else if conds.len() >= 3 {
        conds
    } else {
        return text.to_string();
    };

    let intro: String = chars[..marks[0]].iter().collect();
    let intro = intro
        .trim()
        .trim_end_matches(['，', '：', '、', '；', ' '])
        .trim();

    let mut out = String::new();
    if !intro.is_empty() {
        out.push_str(intro);
        out.push_str("\n\n");
    }

    let mut n = 0usize;
    for (k, &start) in marks.iter().enumerate() {
        let end = marks.get(k + 1).copied().unwrap_or(chars.len());
        let raw: String = chars[start..end].iter().collect();
        let Some((_, len)) = marker_at(&chars, start) else {
            continue;
        };
        // 条件标记（如果）是语义，不能删；步骤/并列的序号只是排版，删掉换成 1. 2. 3.
        let keep_marker = matches!(marker_kind_at(&chars, start), Some(EnumKind::Cond));
        let body: String = if keep_marker {
            raw
        } else {
            raw.chars().skip(len).collect()
        };
        let body = body
            .trim_start_matches(['，', '、', '：', ' ', '\n'])
            .trim();
        // 「另一个**是**…」「还有**就是**…」：编号已经代替了连接词，把它去掉
        // （只剥行首的「是/就」，不碰正文里的）
        let body = body
            .trim_start_matches(['是', '就'])
            .trim_start_matches(['，', '、', '：', ' '])
            .trim()
            .trim_end_matches(['，', '、', '；'])
            .trim();
        if body.is_empty() {
            continue;
        }
        let body = if body.ends_with('。') || body.ends_with('？') || body.ends_with('！') {
            body.to_string()
        } else {
            format!("{body}。")
        };
        n += 1;
        out.push_str(&format!("{n}. {body}\n"));
    }

    if n < 2 {
        return text.to_string();
    }
    out.trim_end().to_string()
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum EnumKind {
    /// 第N步 / 第N点 / 首先 / 其次 / 再次
    Step,
    /// 一个是 / 另一个 / 还有 / 另外
    Pair,
    /// 如果（连续三次以上）
    Cond,
}

fn marker_kind_at(chars: &[char], i: usize) -> Option<EnumKind> {
    marker_at(chars, i).map(|(kind, _)| kind)
}

/// 从位置 `i` 开始是不是一个列举标记，返回（类型，字符数）。
fn marker_at(chars: &[char], i: usize) -> Option<(EnumKind, usize)> {
    let rest: String = chars[i..].iter().take(3).collect();
    let mut it = rest.chars();
    let c0 = it.next()?;

    // 第N步 / 第N点 / 第N条
    if c0 == '第' {
        if let (Some(c1), Some(c2)) = (it.next(), it.next()) {
            if "一二三四五六七八九十".contains(c1) && matches!(c2, '步' | '点' | '条') {
                return Some((EnumKind::Step, 3));
            }
        }
    }
    if rest.starts_with("首先") || rest.starts_with("其次") || rest.starts_with("再次") {
        return Some((EnumKind::Step, 2));
    }
    if rest.starts_with("一个是") || rest.starts_with("另一个") {
        return Some((EnumKind::Pair, 3));
    }
    if rest.starts_with("还有") || rest.starts_with("另外") {
        return Some((EnumKind::Pair, 2));
    }
    if rest.starts_with("如果") {
        return Some((EnumKind::Cond, 2));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drops_leading_filler() {
        assert_eq!(rule_polish("嗯，帮我看下这个"), "帮我看下这个。");
    }

    #[test]
    fn keeps_pure_filler() {
        // 整句只有「嗯」时不能删成空
        assert_eq!(rule_polish("嗯"), "嗯。");
    }

    #[test]
    fn spaces_cjk_and_latin() {
        assert_eq!(rule_polish("用Rust写一个demo"), "用 Rust 写一个 demo。");
    }

    #[test]
    fn does_not_double_space() {
        assert_eq!(rule_polish("用 Rust 写"), "用 Rust 写。");
    }

    #[test]
    fn collapses_double_punctuation() {
        assert_eq!(rule_polish("真的吗？？"), "真的吗？");
    }

    #[test]
    fn keeps_existing_terminal() {
        assert_eq!(rule_polish("好的。"), "好的。");
    }

    #[test]
    fn empty_stays_empty() {
        assert_eq!(rule_polish("   "), "");
    }

    // ---- 语气词 ----

    #[test]
    fn drops_isolated_filler_between_commas() {
        assert_eq!(rule_polish("我觉得，嗯，这个方案可以"), "我觉得，这个方案可以。");
    }

    #[test]
    fn drops_isolated_after_period() {
        assert_eq!(rule_polish("先这样。那个，我们继续"), "先这样。我们继续。");
    }

    #[test]
    fn collapses_repeated_words() {
        assert_eq!(rule_polish("然后然后我们就走了"), "然后我们就走了。");
        assert_eq!(rule_polish("我我我觉得可以"), "我觉得可以。");
    }

    #[test]
    fn keeps_demonstrative_zhege() {
        // 「这个」在这里是指示代词，不是口头语，不能删
        assert_eq!(rule_polish("这个接口有问题"), "这个接口有问题。");
        assert_eq!(rule_polish("那个产品经理说下周上线"), "那个产品经理说下周上线。");
    }

    #[test]
    fn keeps_legit_reduplication() {
        // 「看看」「试试」是正常说法
        assert_eq!(rule_polish("你先看看这个"), "你先看看这个。");
    }

    #[test]
    fn drops_several_leading_fillers() {
        assert_eq!(rule_polish("啊，那个，我想问一下"), "我想问一下。");
    }

    // ---- 句中的语气词（之前完全没处理，用户反馈「没有润色」的主要原因）----

    #[test]
    fn drops_mid_sentence_interjections() {
        assert_eq!(rule_polish("我觉得嗯这个事情还需要想想"), "我觉得这个事情还需要想想。");
        // 「那个 + 我们」是口头语，规则层直接删
        assert_eq!(rule_polish("然后那个我们把日志改成 debug"), "然后我们把日志改成 debug。");
    }

    #[test]
    fn drops_phrase_fillers_anywhere() {
        assert_eq!(rule_polish("就是说我们这个方案要改"), "我们这个方案要改。");
        assert_eq!(rule_polish("这个你知道吧得重新跑"), "这个得重新跑。");
    }

    #[test]
    fn keeps_ye_jiu_shi_shuo() {
        // 「也就是说」里的「就是说」不能删
        assert_eq!(rule_polish("也就是说这个方案要改"), "也就是说这个方案要改。");
    }

    #[test]
    fn does_not_break_words_with_e() {
        // 「额」出现在「金额」里，不能当语气词删
        assert_eq!(rule_polish("这个金额有点高"), "这个金额有点高。");
    }

    #[test]
    fn tidies_punctuation_after_removal() {
        assert_eq!(rule_polish("我觉得，嗯，可以"), "我觉得，可以。");
    }

    // ---- 句中断句（YC-006）----

    #[test]
    fn repairs_period_before_lowercase_english() {
        // 识别在停顿处插了句号，后面是小写英文 → 一定是句中断句
        assert_eq!(
            rule_polish("把这段逻辑抽成一个。helper 函数"),
            "把这段逻辑抽成一个 helper 函数。"
        );
        assert_eq!(
            rule_polish("我需要你把。timeout 从 30 秒改成 60 秒"),
            "我需要你把 timeout 从 30 秒改成 60 秒。"
        );
    }

    #[test]
    fn keeps_period_before_uppercase_english() {
        // 大写开头更像真的新句子，别乱动
        assert_eq!(
            rule_polish("这样做不对。Rust 里应该用宏"),
            "这样做不对。Rust 里应该用宏。"
        );
    }

    #[test]
    fn collapses_period_after_question_mark() {
        // 静音尾巴被识别成「。」，接在问号后面就成了「？。」
        assert_eq!(rule_polish("为什么返回空？。"), "为什么返回空？");
        assert_eq!(rule_polish("太好了！。"), "太好了！");
    }

    #[test]
    fn softens_period_before_clause_starter() {
        // ASR 按停顿硬加的句号，后面接的是承接词 → 应该是逗号
        assert_eq!(
            rule_polish("第三步看一下健康检查有没有问题。如果没问题我们就发布"),
            "第三步看一下健康检查有没有问题，如果没问题我们就发布。"
        );
        assert_eq!(
            rule_polish("先把迁移跑完。然后重启服务"),
            "先把迁移跑完，然后重启服务。"
        );
        // 真正的句子边界不动
        assert_eq!(
            rule_polish("第一件事做完了。第二件事开始"),
            "第一件事做完了。第二件事开始。"
        );
    }

    #[test]
    fn keeps_period_between_chinese() {
        assert_eq!(rule_polish("先做这个。再做那个"), "先做这个。再做那个。");
    }

    // ---- 列举排版：标记稳定，规则切 ----

    #[test]
    fn formats_step_enumeration() {
        let t = "明天我要做三件事：第一步先把脚本跑一遍。第二步，重启服务，然后看日志。第三步，同步配置。";
        let out = format_enumerations(t);
        assert!(out.contains("1. 先把脚本跑一遍"), "{out}");
        assert!(out.contains("2. 重启服务"), "{out}");
        assert!(out.contains("3. 同步配置"), "{out}");
    }

    #[test]
    fn formats_pair_enumeration() {
        let t = "改动主要有几个方面，一个是把宽度做成可调。另一个是加了字号设置。还有就是把渲染抽出来。另外，顺手修了个问题。";
        let out = format_enumerations(t);
        assert!(out.contains("1. 把宽度做成可调"), "{out}");
        assert!(out.contains("3. 把渲染抽出来"), "{out}");
        assert!(out.contains("4. 顺手修了个问题"), "{out}");
    }

    #[test]
    fn formats_condition_enumeration() {
        let t = "如果返回 401，就先检查 token。如果返回 500，那基本是服务端的问题。如果一直超时，先看本地网络。";
        let out = format_enumerations(t);
        assert!(out.contains("1. 如果返回 401"), "{out}");
        assert!(out.contains("3. 如果一直超时"), "{out}");
    }

    #[test]
    fn leaves_prose_alone() {
        // 只有「先…然后…」没有明确列举标记的，不能硬拆成清单
        let t = "这是气泡的，停顿性的话我觉得可以多讲两句，先做咖啡，然后再去上班。";
        assert_eq!(format_enumerations(t), t);
    }

    #[test]
    fn leaves_single_marker_alone() {
        let t = "另外就是时间，本地模型跑三百多个字要四五秒。";
        assert_eq!(format_enumerations(t), t);
    }

    #[test]
    fn does_not_touch_existing_numbering() {
        let t = "1. 先跑迁移\n2. 重启服务";
        assert_eq!(format_enumerations(t), t);
    }
}
