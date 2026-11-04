//! 气泡外观：宽度、高度、字号。
//!
//! 这三个数是用户偏好，不是设计常量 —— 屏幕、视力、使用场景都不一样，
//! 所以放进控制面板让用户自己调，存 `models/ui.json`。

use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct BubbleUi {
    pub width: u32,
    pub height: u32,
    pub font_size: f32,
}

impl Default for BubbleUi {
    fn default() -> Self {
        // 默认 3 行、11px：说话时不想让气泡占掉半个屏幕
        Self {
            width: 440,
            height: 100,
            font_size: 10.5,
        }
    }
}

impl BubbleUi {
    pub fn load(root: &Path) -> Self {
        std::fs::read_to_string(root.join("ui.json"))
            .ok()
            .and_then(|t| serde_json::from_str::<BubbleUi>(&t).ok())
            .map(Self::clamped)
            .unwrap_or_default()
    }

    pub fn save(&self, root: &Path) -> anyhow::Result<()> {
        std::fs::create_dir_all(root)?;
        let body = serde_json::to_string_pretty(&self.clamped())?;
        std::fs::write(root.join("ui.json"), body)?;
        Ok(())
    }

    /// 防止调出不可能的值（窗口太小看不见、太大挡住半个屏幕）
    pub fn clamped(self) -> Self {
        Self {
            width: self.width.clamp(320, 900),
            height: self.height.clamp(80, 360),
            font_size: (self.font_size.clamp(9.0, 16.0) * 10.0).round() / 10.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_three_lines() {
        let ui = BubbleUi::default();
        // 11px 字号 + 1.5 行高 ≈ 16.5px/行，加内边距和投影留白差不多三行
        assert!((90..=140).contains(&ui.height), "默认高度该在 3 行附近");
        assert!(ui.width < 560, "默认宽度不该太宽");
    }

    #[test]
    fn clamps_absurd_values() {
        let ui = BubbleUi {
            width: 5000,
            height: 10,
            font_size: 99.0,
        }
        .clamped();
        assert_eq!(ui.width, 900);
        assert_eq!(ui.height, 80);
        assert_eq!(ui.font_size, 16.0);
    }
}
