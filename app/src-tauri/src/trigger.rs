//! 触发键与输入模式：配置、校验、占用检测、持久化。
//!
//! 用户可以在控制面板里直接按一组键来定义触发键。这里负责把它规整成
//! 「修饰键在前、普通键在最后」的固定形状 —— 热键钩子靠「最后一个键按下」
//! 判断组合完成，并且只吞掉这个最后的键，修饰键仍然留给目标应用。
//!
//! 配置存 `models/trigger.json`。默认是沿用旧版的右 Alt + 按住说话，
//! 老用户升级后不需要重新设置。

use serde::{Deserialize, Serialize};
use std::path::Path;

use windows::Win32::UI::Input::KeyboardAndMouse::{
    RegisterHotKey, UnregisterHotKey, HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT,
    MOD_SHIFT, MOD_WIN,
};

/// 配置文件（和 state.json / ui.json 放在同一个 models 目录）
pub const FILE: &str = "trigger.json";

pub const VK_ESCAPE: u32 = 0x1B;
pub const VK_RMENU: u32 = 0xA5;

/// 输入模式。和微信输入法一致的两种：
/// * Hold：按住说，松手上屏；
/// * Toggle：按一下开始，再按一下或 Esc 结束。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputMode {
    Hold,
    Toggle,
}

impl Default for InputMode {
    fn default() -> Self {
        Self::Hold
    }
}

impl InputMode {
    pub fn parse(value: &str) -> Result<Self, String> {
        match value.trim().to_ascii_lowercase().as_str() {
            "hold" | "hold_to_talk" => Ok(Self::Hold),
            "toggle" | "click" => Ok(Self::Toggle),
            other => Err(format!("未知的输入模式：{other}")),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Hold => "按住说话",
            Self::Toggle => "按一下开始 / 停止",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TriggerConfig {
    /// 规整后的顺序：修饰键在前，普通键在最后
    pub keys: Vec<u32>,
    #[serde(default)]
    pub mode: InputMode,
    /// 0 = 立即触发；>0 = 按住这么久才开始（避免误触）
    #[serde(default)]
    pub long_press_ms: u32,
}

impl Default for TriggerConfig {
    fn default() -> Self {
        Self {
            keys: vec![VK_RMENU],
            mode: InputMode::Hold,
            long_press_ms: 0,
        }
    }
}

impl TriggerConfig {
    pub fn load(root: &Path) -> Self {
        let path = root.join(FILE);
        let Ok(text) = std::fs::read_to_string(&path) else {
            return Self::default();
        };
        match serde_json::from_str::<TriggerConfig>(&text) {
            Ok(cfg) => {
                let cfg = cfg.normalized();
                match cfg.validate() {
                    Ok(()) => cfg,
                    Err(e) => {
                        log::warn!("{} 不合法（{e}），改用默认触发键", path.display());
                        Self::default()
                    }
                }
            }
            Err(e) => {
                log::warn!("{} 解析失败（{e}），改用默认触发键", path.display());
                Self::default()
            }
        }
    }

    pub fn save(&self, root: &Path) -> anyhow::Result<()> {
        std::fs::create_dir_all(root)?;
        let body = serde_json::to_string_pretty(&self.clone().normalized())?;
        std::fs::write(root.join(FILE), body)?;
        Ok(())
    }

    /// 去掉重复键、把通用修饰键映射成左键、按固定顺序排列。
    pub fn normalized(&self) -> Self {
        let mut mods: Vec<u32> = Vec::new();
        let mut primary: Option<u32> = None;

        for raw in &self.keys {
            let key = canonical_vk(*raw);
            if is_modifier(key) {
                if !mods.contains(&key) {
                    mods.push(key);
                }
            } else if primary.is_none() {
                primary = Some(key);
            }
        }
        mods.sort_by_key(|k| modifier_rank(*k));

        let mut keys = mods;
        if let Some(key) = primary {
            keys.push(key);
        }
        Self {
            keys,
            mode: self.mode,
            long_press_ms: self.long_press_ms.min(2000),
        }
    }

    /// 结构校验。不碰系统注册表；占用检测在 `ensure_available`。
    pub fn validate(&self) -> Result<(), String> {
        let raw = &self.keys;
        if raw.is_empty() {
            return Err("至少设置一个触发键".into());
        }
        if raw.len() > 4 {
            return Err("最多 4 个键（三个修饰键 + 一个普通键）".into());
        }

        let mut seen: Vec<u32> = Vec::new();
        for &key in raw {
            if key == 0 || key > 254 || (1..=6).contains(&key) {
                return Err(format!("不支持的按键（VK 0x{key:02X}）"));
            }
            let key = canonical_vk(key);
            if seen.contains(&key) {
                return Err("同一个键重复了".into());
            }
            seen.push(key);
        }

        if seen.contains(&VK_ESCAPE) {
            return Err("Esc 用于结束录音，不能设为触发键".into());
        }
        if seen.iter().any(|k| matches!(k, 0x5B | 0x5C)) {
            return Err("Win 组合键大多被系统占用，请换一个".into());
        }

        let mods = seen.iter().filter(|k| is_modifier(**k)).count();
        let non_mods = seen.len() - mods;

        if non_mods > 1 {
            return Err("组合键里只能有一个普通键（修饰键之外）".into());
        }
        if non_mods == 0 {
            if mods > 1 {
                return Err("组合键里请再加一个普通键，例如 Ctrl+Shift+Space".into());
            }
            if seen[0] != VK_RMENU {
                return Err(
                    "单个 Ctrl / Shift / Alt 会吞掉正常打字，请再加一个普通键；也可以用 CapsLock 或 F 键"
                        .into(),
                );
            }
        } else {
            let primary = *seen
                .iter()
                .find(|k| !is_modifier(**k))
                .expect("non_mods == 1");
            if mods == 0 && !is_dedicated_single(primary) {
                return Err(
                    "单个字母 / 数字 / 符号会干扰打字，请配合 Ctrl 或 Alt，或换成 F 键、CapsLock"
                        .into(),
                );
            }
            if mods > 0 && !is_dedicated_single(primary) && !has_command_mod(&seen) {
                return Err("普通字母 / 数字键请配合 Ctrl 或 Alt".into());
            }
        }

        if !(self.long_press_ms == 0 || (100..=2000).contains(&self.long_press_ms)) {
            return Err("长按时间要在 100–2000ms 之间（0 表示立即触发）".into());
        }

        let normalized = self.normalized();
        if RESERVED
            .iter()
            .any(|reserved| normalized.keys.as_slice() == *reserved)
        {
            return Err(format!(
                "{} 是系统占用的快捷键，请换一个",
                normalized.display()
            ));
        }
        if RISKY
            .iter()
            .any(|risky| normalized.keys.as_slice() == *risky)
        {
            return Err(format!(
                "{} 是常用编辑快捷键，会与复制 / 粘贴等冲突，请换一个",
                normalized.display()
            ));
        }

        Ok(())
    }

    /// 用 RegisterHotKey 做一次「是否被别的程序占用」的探测。
    ///
    /// 注册成功后立刻注销，不会真的占着这个键。低层键盘钩子本身不受
    /// RegisterHotKey 影响，所以每次保存都探测一次，能发现刚被别的软件抢走的组合。
    pub fn ensure_available(&self) -> Result<(), String> {
        let cfg = self.clone().normalized();
        let Some((&primary, modifiers)) = cfg.keys.split_last() else {
            return Ok(());
        };
        // 单修饰键、CapsLock 这类锁键没法用 RegisterHotKey 表达，跳过探测。
        if is_modifier(primary) || is_lock_key(primary) {
            return Ok(());
        }

        let mut mask = 0u32;
        for key in modifiers {
            mask |= match canonical_vk(*key) {
                0xA0 | 0xA1 => MOD_SHIFT.0,
                0xA2 | 0xA3 => MOD_CONTROL.0,
                0xA4 | 0xA5 => MOD_ALT.0,
                0x5B | 0x5C => MOD_WIN.0,
                _ => 0,
            };
        }

        let id = 0x5943; // "YC"
        unsafe {
            match RegisterHotKey(None, id, HOT_KEY_MODIFIERS(mask | MOD_NOREPEAT.0), primary) {
                Ok(()) => {
                    let _ = UnregisterHotKey(None, id);
                    Ok(())
                }
                Err(e) => {
                    let _ = UnregisterHotKey(None, id);
                    log::warn!("探测触发键 {primary:#x} 失败: {e}");
                    Err("这个快捷键已经被系统或其他程序占用，换一个试试".into())
                }
            }
        }
    }

    pub fn display(&self) -> String {
        self.clone()
            .normalized()
            .keys
            .iter()
            .map(|k| display_key(*k))
            .collect::<Vec<_>>()
            .join("+")
    }

    /// 概览 / 托盘里的一句话说明。
    pub fn trigger_hint(&self) -> String {
        let label = self.display();
        match (self.mode, self.long_press_ms > 0) {
            (InputMode::Hold, true) => format!("长按 {label} 说话，松手落字"),
            (InputMode::Hold, false) => format!("按住 {label} 说话，松手落字"),
            (InputMode::Toggle, true) => format!("长按 {label} 开始，再按一下或 Esc 结束"),
            (InputMode::Toggle, false) => format!("按一下 {label} 开始，再按一下或 Esc 结束"),
        }
    }

    /// 录音中气泡/托盘上的提示。
    pub fn listening_hint(&self) -> String {
        let label = self.display();
        match self.mode {
            InputMode::Hold => "正在听…（松手结束）".into(),
            InputMode::Toggle => format!("正在听…（再按 {label} 或 Esc 结束）"),
        }
    }

    /// 录音太短 / 暂停时提示用户「该怎么按」。
    pub fn start_prompt(&self) -> String {
        let label = self.display();
        match (self.mode, self.long_press_ms > 0) {
            (InputMode::Hold, true) => format!("长按 {label} 说话"),
            (InputMode::Hold, false) => format!("按住 {label} 说话"),
            (InputMode::Toggle, true) => format!("长按 {label} 开始说话"),
            (InputMode::Toggle, false) => format!("按一下 {label} 开始说话"),
        }
    }

    pub fn to_json(&self) -> serde_json::Value {
        let cfg = self.clone().normalized();
        serde_json::json!({
            "keys": cfg.keys,
            "mode": cfg.mode,
            "mode_label": cfg.mode.label(),
            "long_press_ms": cfg.long_press_ms,
            "label": cfg.display(),
            "hint": cfg.trigger_hint(),
        })
    }
}

/// 系统级保留快捷键（已按我们的规范顺序书写）。
const RESERVED: &[&[u32]] = &[
    &[0xA2, 0xA4, 0x2E], // Ctrl+Alt+Delete
    &[0xA2, 0xA4, 0x09], // Ctrl+Alt+Tab
    &[0xA2, 0x20],       // Ctrl+Space（中文输入法切换）
    &[0xA2, 0xA0, 0x1B], // Ctrl+Shift+Esc
    &[0xA2, 0x1B],       // Ctrl+Esc
    &[0xA4, 0x09],       // Alt+Tab
    &[0xA4, 0x73],       // Alt+F4
    &[0xA4, 0x20],       // Alt+Space
    &[0xA4, 0x1B],       // Alt+Esc
    &[0xA4, 0x2C],       // Alt+PrintScreen
];

/// 常用编辑快捷键：不是系统级占用，但绑上去会把复制 / 粘贴吞掉。
const RISKY: &[&[u32]] = &[
    &[0xA2, 0x41], // Ctrl+A
    &[0xA2, 0x43], // Ctrl+C
    &[0xA2, 0x46], // Ctrl+F
    &[0xA2, 0x4E], // Ctrl+N
    &[0xA2, 0x50], // Ctrl+P
    &[0xA2, 0x53], // Ctrl+S
    &[0xA2, 0x54], // Ctrl+T
    &[0xA2, 0x56], // Ctrl+V
    &[0xA2, 0x57], // Ctrl+W
    &[0xA2, 0x58], // Ctrl+X
    &[0xA2, 0x59], // Ctrl+Y
    &[0xA2, 0x5A], // Ctrl+Z
];

fn canonical_vk(key: u32) -> u32 {
    match key {
        0x10 => 0xA0, // 通用 Shift → 左 Shift
        0x11 => 0xA2, // 通用 Ctrl → 左 Ctrl
        0x12 => 0xA4, // 通用 Alt → 左 Alt
        other => other,
    }
}

pub fn is_modifier(key: u32) -> bool {
    matches!(key, 0x10 | 0x11 | 0x12 | 0xA0..=0xA5 | 0x5B | 0x5C)
}

fn modifier_rank(key: u32) -> u8 {
    match canonical_vk(key) {
        0xA2 | 0xA3 => 0, // Ctrl
        0xA0 | 0xA1 => 1, // Shift
        0xA4 | 0xA5 => 2, // Alt
        0x5B | 0x5C => 3, // Win
        _ => 4,
    }
}

fn is_function_key(key: u32) -> bool {
    (0x70..=0x87).contains(&key)
}

fn is_lock_key(key: u32) -> bool {
    matches!(key, 0x14 | 0x90 | 0x91)
}

/// 允许单独作为触发键的「专用键」：不会往输入框里打字。
fn is_dedicated_single(key: u32) -> bool {
    is_function_key(key) || is_lock_key(key) || key == 0x13 // Pause
}

fn has_command_mod(keys: &[u32]) -> bool {
    keys.iter()
        .any(|k| matches!(canonical_vk(*k), 0xA2 | 0xA3 | 0xA4 | 0xA5 | 0x11 | 0x12))
}

fn display_key(key: u32) -> String {
    let key = canonical_vk(key);
    match key {
        0x08 => "退格".into(),
        0x09 => "Tab".into(),
        0x0D => "Enter".into(),
        0x13 => "Pause".into(),
        0x14 => "CapsLock".into(),
        0x1B => "Esc".into(),
        0x20 => "空格".into(),
        0x21 => "PageUp".into(),
        0x22 => "PageDown".into(),
        0x23 => "End".into(),
        0x24 => "Home".into(),
        0x25 => "←".into(),
        0x26 => "↑".into(),
        0x27 => "→".into(),
        0x28 => "↓".into(),
        0x2C => "PrintScreen".into(),
        0x2D => "Insert".into(),
        0x2E => "Delete".into(),
        0x5D => "菜单键".into(),
        0x90 => "NumLock".into(),
        0x91 => "ScrollLock".into(),
        0xA0 => "Shift".into(),
        0xA1 => "右 Shift".into(),
        0xA2 => "Ctrl".into(),
        0xA3 => "右 Ctrl".into(),
        0xA4 => "Alt".into(),
        0xA5 => "右 Alt".into(),
        0x5B => "Win".into(),
        0x5C => "右 Win".into(),
        0x60..=0x69 => format!("小键盘{}", key - 0x60),
        0x6A => "小键盘*".into(),
        0x6B => "小键盘+".into(),
        0x6D => "小键盘-".into(),
        0x6E => "小键盘.".into(),
        0x6F => "小键盘/".into(),
        0x70..=0x87 => format!("F{}", key - 0x70 + 1),
        0x41..=0x5A => char::from_u32(key).unwrap_or('?').to_string(),
        0x30..=0x39 => char::from_u32(key).unwrap_or('?').to_string(),
        0xBA => ";".into(),
        0xBB => "=".into(),
        0xBC => ",".into(),
        0xBD => "-".into(),
        0xBE => ".".into(),
        0xBF => "/".into(),
        0xC0 => "`".into(),
        0xDB => "[".into(),
        0xDC => "\\".into(),
        0xDD => "]".into(),
        0xDE => "'".into(),
        other => format!("VK{other:02X}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(keys: &[u32]) -> TriggerConfig {
        TriggerConfig {
            keys: keys.to_vec(),
            mode: InputMode::Hold,
            long_press_ms: 0,
        }
    }

    #[test]
    fn default_is_right_alt_hold() {
        let cfg = TriggerConfig::default();
        assert_eq!(cfg.keys, vec![VK_RMENU]);
        assert_eq!(cfg.mode, InputMode::Hold);
        assert_eq!(cfg.display(), "右 Alt");
        assert_eq!(cfg.long_press_ms, 0);
    }

    #[test]
    fn normalizes_modifier_order_and_primary_last() {
        let cfg = config(&[0x20, 0xA0, 0xA2]).normalized();
        assert_eq!(cfg.keys, vec![0xA2, 0xA0, 0x20]);
        assert_eq!(cfg.display(), "Ctrl+Shift+空格");
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn rejects_dangerous_single_keys() {
        assert!(config(&[0x41]).validate().is_err()); // A
        assert!(config(&[0x20]).validate().is_err()); // Space
        assert!(config(&[0xA2]).validate().is_err()); // 单 Ctrl
        assert!(config(&[0xA0]).validate().is_err()); // 单 Shift
        assert!(config(&[VK_ESCAPE]).validate().is_err()); // Esc
        assert!(config(&[0x5B, 0x41]).validate().is_err()); // Win+A
    }

    #[test]
    fn allows_function_and_lock_keys_alone() {
        for key in [0x70, 0x78, 0x87, 0x14, 0x90, 0x91, VK_RMENU] {
            assert!(config(&[key]).validate().is_ok(), "VK {key:#x} 应该允许");
        }
    }

    #[test]
    fn rejects_reserved_and_risky_combos() {
        assert!(config(&[0xA4, 0x09]).validate().is_err()); // Alt+Tab
        assert!(config(&[0xA2, 0x20]).validate().is_err()); // Ctrl+Space
        assert!(config(&[0xA2, 0x43]).validate().is_err()); // Ctrl+C
        assert!(config(&[0xA2, 0xA0, 0x20]).validate().is_ok()); // Ctrl+Shift+Space
    }

    #[test]
    fn rejects_two_primaries_and_pure_modifier_combo() {
        assert!(config(&[0xA2, 0x41, 0x42]).validate().is_err());
        assert!(config(&[0xA2, 0xA0]).validate().is_err());
    }

    #[test]
    fn long_press_range() {
        let mut cfg = config(&[0x70]);
        cfg.long_press_ms = 50;
        assert!(cfg.validate().is_err());
        cfg.long_press_ms = 300;
        assert!(cfg.validate().is_ok());
        cfg.long_press_ms = 5000;
        assert!(cfg.validate().is_err());
    }
}
