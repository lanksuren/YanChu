//! 替换规则层：用户可编辑的 `旧=新` 表。
//!
//! 跑在热词表之后、规则润色之前。分工和 CapsWriter 的 `hot.txt` / `hot-rule.txt` 一样：
//! * 热词表（`glossary.txt`）管「听错的词」——靠编辑距离和拼音，模糊匹配；
//! * 这里管「固定要改的写法」——不猜，命中就换，比如
//!   `入餐=入参`、`负在均衡=负载均衡`、`第X步=第X步`。
//!
//! 只做等号替换，不做正则：2B 时代先把最常用的一层跑通，
//! 正则留给真正需要的时候再加（CapsWriter 的 hot-rule.txt 也是这两类）。

use std::path::Path;

#[derive(Debug, Clone, Default)]
pub struct Rules {
    /// (旧, 新)，按文件顺序依次替换
    pairs: Vec<(String, String)>,
}

impl Rules {
    pub fn load(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(text) => Self::parse(&text),
            Err(_) => Self::default(),
        }
    }

    pub fn parse(text: &str) -> Self {
        let mut pairs = Vec::new();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((from, to)) = line.split_once('=') else {
                continue;
            };
            let from = from.trim();
            let to = to.trim();
            if from.is_empty() {
                continue;
            }
            pairs.push((from.to_string(), to.to_string()));
        }
        Self { pairs }
    }

    /// 按顺序逐条替换。一条规则只扫一遍，避免 `a=b`、`b=a` 互相打架时死循环。
    pub fn apply(&self, text: &str) -> String {
        let mut out = text.to_string();
        for (from, to) in &self.pairs {
            if out.contains(from.as_str()) {
                out = out.replace(from.as_str(), to);
            }
        }
        out
    }

    pub fn len(&self) -> usize {
        self.pairs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.pairs.is_empty()
    }
}

/// 控制面板里的默认内容：两句注释 + 一个示例，用户照着写。
pub fn default_text() -> String {
    "# 每行一条：旧=新。# 开头是注释。\n\
     # 跑在热词表之后、标点整理之前，命中就替换，不做模糊匹配。\n\
     # 例：入餐=入参\n"
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_in_order() {
        let r = Rules::parse("入餐=入参\n负在均衡=负载均衡");
        assert_eq!(r.apply("入餐传错了，负在均衡也有问题"), "入参传错了，负载均衡也有问题");
    }

    #[test]
    fn ignores_comments_and_blank_lines() {
        let r = Rules::parse("# 注释\n\n  helper = 辅助函数 ");
        assert_eq!(r.len(), 1);
        assert_eq!(r.apply("用 helper 写"), "用 辅助函数 写");
    }

    #[test]
    fn missing_equals_line_is_skipped() {
        let r = Rules::parse("这一行没有等号\nA=B");
        assert_eq!(r.len(), 1);
    }

    #[test]
    fn empty_value_deletes() {
        let r = Rules::parse("嗯= ");
        assert_eq!(r.apply("嗯这个可以"), "这个可以");
    }
}
