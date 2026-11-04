//! 剪贴板：写入要落的字，并在粘贴完成后「有条件地」还原用户原来的剪贴板。
//!
//! 条件恢复是这条链路上最容易写错的地方：
//! 如果在粘贴和还原之间用户自己复制了东西，无条件还原就会把他的内容抹掉。

use arboard::Clipboard;
use std::time::Duration;

/// 粘贴之后等多久再还原剪贴板。
///
/// 这一条是 2026-11-03 09:07 那次「没上屏」的根因：Ctrl+V 只是把按键送进队列，
/// 浏览器（Edge/WebView）读到剪贴板可能晚几十到上百毫秒。原来粘完立刻还原，
/// 目标应用读到的就是**旧的剪贴板内容** —— 等于什么都没粘。
/// 250ms 足够覆盖浏览器，又不至于让用户觉得剪贴板被占着。
const RESTORE_DELAY: Duration = Duration::from_millis(250);

pub fn set_text(text: &str) -> bool {
    match Clipboard::new() {
        Ok(mut c) => c.set_text(text.to_owned()).is_ok(),
        Err(e) => {
            log::warn!("打开剪贴板失败: {e}");
            false
        }
    }
}

pub fn get_text() -> Option<String> {
    Clipboard::new().ok().and_then(|mut c| c.get_text().ok())
}

/// 记住用户原来的剪贴板，粘贴结束后按条件还原。
pub struct ClipboardGuard {
    previous: Option<String>,
}

impl ClipboardGuard {
    pub fn capture() -> Self {
        Self {
            previous: get_text(),
        }
    }

    pub fn set_text(&self, text: &str) -> bool {
        set_text(text)
    }

    /// 只有当剪贴板里还是我们刚才写进去的内容时才还原。
    pub fn restore_if_unchanged(&self, inserted: &str) {
        let Some(prev) = self.previous.as_deref() else {
            // 原来就是空的，没什么可还原
            return;
        };

        match get_text() {
            Some(current) if current == inserted => {
                if !set_text(prev) {
                    log::warn!("还原剪贴板失败");
                }
            }
            Some(_) => {
                // 用户中途复制了别的东西 —— 保持现状，绝不能覆盖
                log::info!("剪贴板已被用户改写，跳过还原");
            }
            None => {}
        }
    }

    /// 延迟还原：粘贴是异步的，必须给目标应用留出读剪贴板的时间。
    /// 放到后台线程，不拖慢「松手 → 落字」。
    pub fn restore_after(self, inserted: &str) {
        let inserted = inserted.to_owned();
        std::thread::spawn(move || {
            std::thread::sleep(RESTORE_DELAY);
            self.restore_if_unchanged(&inserted);
        });
    }
}
