//! 气泡外观：宽度、高度、字号、配色。
//!
//! 这些是用户偏好，不是设计常量 —— 屏幕、视力、使用场景都不一样，
//! 所以放进控制面板让用户自己调，存 `models/ui.json`。
//!
//! 配色（`Theme`）刻意只存「用户改过的那几项」：字段为空表示交给前端按
//! 系统浅色/深色自动配（见 `app/src/theme.js`）。这样默认主题永远跟系统一致，
//! 用户只改一个背景色时，文字与灰字也能自动选到看得清的色阶。

use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BubbleUi {
    pub width: u32,
    pub height: u32,
    pub font_size: f32,
    /// 配色皮肤。字段全空 = 跟随系统浅色 / 深色。
    #[serde(default)]
    pub theme: Theme,
}

/// 气泡与指示器的配色。所有字段都可空：空的项由前端自动配色。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Theme {
    /// 气泡背景（#rgb / #rrggbb）
    pub bubble_bg: Option<String>,
    /// 背景不透明度 0.15–1
    pub bubble_alpha: Option<f32>,
    /// 已定稿正文
    pub bubble_text: Option<String>,
    /// 还没定稿的灰字；留空时由正文色和背景混合
    pub bubble_tail: Option<String>,
    /// 描边；留空时用白色高光
    pub bubble_border: Option<String>,
    /// 投影强度 0–2（1 = 默认）
    pub bubble_shadow: Option<f32>,
    /// 指示器主色
    pub accent: Option<String>,
    /// 完成 / 提示 / 润色三个状态色
    pub ok: Option<String>,
    pub warn: Option<String>,
    pub polish: Option<String>,
}

impl Default for BubbleUi {
    fn default() -> Self {
        // 默认 3 行、10.5px：说话时不想让气泡占掉半个屏幕
        Self {
            width: 440,
            height: 100,
            font_size: 10.5,
            theme: Theme::default(),
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
        let body = serde_json::to_string_pretty(&self.clone().clamped())?;
        std::fs::write(root.join("ui.json"), body)?;
        Ok(())
    }

    /// 防止调出不可能的值（窗口太小看不见、太大挡住半个屏幕；颜色写错写脏）
    pub fn clamped(self) -> Self {
        Self {
            width: self.width.clamp(320, 900),
            height: self.height.clamp(80, 360),
            font_size: (self.font_size.clamp(9.0, 16.0) * 10.0).round() / 10.0,
            theme: self.theme.clamped(),
        }
    }
}

impl Theme {
    pub fn clamped(self) -> Self {
        Self {
            bubble_bg: normalize_hex(self.bubble_bg),
            bubble_text: normalize_hex(self.bubble_text),
            bubble_tail: normalize_hex(self.bubble_tail),
            bubble_border: normalize_hex(self.bubble_border),
            accent: normalize_hex(self.accent),
            ok: normalize_hex(self.ok),
            warn: normalize_hex(self.warn),
            polish: normalize_hex(self.polish),
            bubble_alpha: self.bubble_alpha.map(|v| clamp_round(v, 0.15, 1.0)),
            bubble_shadow: self.bubble_shadow.map(|v| clamp_round(v, 0.0, 2.0)),
        }
    }

    /// 是否还是「跟随系统」的默认主题。
    pub fn is_default(&self) -> bool {
        *self == Theme::default()
    }
}

/// UI 只提供 `#rgb` / `#rrggbb`。统一成小写 6 位；其它值当作没填。
fn normalize_hex(value: Option<String>) -> Option<String> {
    let raw = value?;
    let body = raw.trim().trim_start_matches('#');
    let expanded = match body.len() {
        3 => body.chars().flat_map(|c| [c, c]).collect::<String>(),
        6 => body.to_owned(),
        _ => return None,
    };
    if expanded.chars().all(|c| c.is_ascii_hexdigit()) {
        Some(format!("#{}", expanded.to_ascii_lowercase()))
    } else {
        None
    }
}

fn clamp_round(value: f32, lo: f32, hi: f32) -> f32 {
    ((value.clamp(lo, hi) * 100.0).round()) / 100.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_three_lines() {
        let ui = BubbleUi::default();
        // 10.5px 字号 + 1.5 行高 ≈ 15.75px/行，加内边距和投影留白差不多三行
        assert!((90..=140).contains(&ui.height), "默认高度该在 3 行附近");
        assert!(ui.width < 560, "默认宽度不该太宽");
    }

    #[test]
    fn clamps_absurd_values() {
        let ui = BubbleUi {
            width: 5000,
            height: 10,
            font_size: 99.0,
            theme: Theme::default(),
        }
        .clamped();
        assert_eq!(ui.width, 900);
        assert_eq!(ui.height, 80);
        assert_eq!(ui.font_size, 16.0);
    }

    #[test]
    fn old_ui_json_without_theme_still_loads() {
        // 老版本 ui.json 只有三个尺寸字段，升级后不能因此退回默认尺寸。
        let ui: BubbleUi =
            serde_json::from_str(r#"{"width":520,"height":140,"font_size":12.0}"#).unwrap();
        assert_eq!(ui.width, 520);
        assert_eq!(ui.theme, Theme::default());
    }

    #[test]
    fn theme_is_camel_case_in_json() {
        let theme = Theme {
            bubble_bg: Some("#ffffff".into()),
            bubble_alpha: Some(0.88),
            ..Default::default()
        };
        let json = serde_json::to_value(theme).unwrap();
        assert_eq!(json["bubbleBg"], "#ffffff");
        // f32 → JSON 会带一点浮点尾差，按数值比较
        assert!((json["bubbleAlpha"].as_f64().unwrap() - 0.88).abs() < 1e-6);
        assert!(json.get("bubble_bg").is_none(), "ui.json 用 camelCase");
    }

    #[test]
    fn normalizes_hex_and_drops_invalid_colors() {
        let theme = Theme {
            bubble_bg: Some("#ABC".into()),
            bubble_text: Some("not-a-color".into()),
            bubble_tail: Some("  #112233  ".into()),
            accent: Some("#12345".into()),
            bubble_alpha: Some(9.0),
            bubble_shadow: Some(-3.0),
            ..Default::default()
        }
        .clamped();
        assert_eq!(theme.bubble_bg.as_deref(), Some("#aabbcc"));
        assert_eq!(theme.bubble_text, None);
        assert_eq!(theme.bubble_tail.as_deref(), Some("#112233"));
        assert_eq!(theme.accent, None);
        assert_eq!(theme.bubble_alpha, Some(1.0));
        assert_eq!(theme.bubble_shadow, Some(0.0));
    }

    #[test]
    fn ui_json_roundtrip_keeps_theme() {
        let dir = std::env::temp_dir().join(format!(
            "yanchu-ui-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let ui = BubbleUi {
            width: 520,
            height: 140,
            font_size: 12.0,
            theme: Theme {
                accent: Some("#112233".into()),
                bubble_shadow: Some(0.5),
                ..Default::default()
            },
        };
        ui.save(&dir).unwrap();
        let loaded = BubbleUi::load(&dir);
        assert_eq!(loaded.width, 520);
        assert_eq!(loaded.font_size, 12.0);
        assert_eq!(loaded.theme.accent.as_deref(), Some("#112233"));
        assert_eq!(loaded.theme.bubble_shadow, Some(0.5));
        assert!(loaded.theme.bubble_bg.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
