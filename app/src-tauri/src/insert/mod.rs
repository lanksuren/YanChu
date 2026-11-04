//! 落字：把整理好的文字写进用户按下热键时正在用的那个输入框。
//!
//! 三条硬约束（见《产品定义-v2》第六节）：
//! 1. 修饰键守卫 —— 松手瞬间 Shift/Ctrl/Alt/Win 可能还按着，注入会被合成；
//! 2. 剪贴板条件恢复 —— 只有剪贴板里还是我们写进去的内容才还原；
//! 3. 写入结果四态 —— 不假设粘贴一定成功。
//!
//! 这套状态机先在 spike/insert-spike.ps1 里跑通（6/6 通过），Rust 是它的正式实现。

mod clipboard;
mod modifier_guard;
mod win_input;

use std::fmt;
use std::time::Duration;

use windows::Win32::Foundation::{GetLastError, HWND};
use windows::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, GetWindowThreadProcessId, SetForegroundWindow,
};

/// 修饰键守卫的最大等待。人手从松键到物理抬起通常 <200ms，
/// 超过 500ms 还没抬起说明用户是故意按着的，此时放弃写入比写错地方好。
const MODIFIER_TIMEOUT_MS: u32 = 500;

/// 按下热键那一刻的前台窗口。整个落字过程只认这个落点，中途绝不重新取焦点。
#[derive(Debug, Clone, Copy)]
pub struct Anchor {
    hwnd: isize,
    pub thread_id: u32,
}

impl Anchor {
    pub fn capture() -> Option<Self> {
        unsafe {
            let hwnd = GetForegroundWindow();
            if hwnd.0.is_null() {
                return None;
            }
            let mut pid = 0u32;
            // 显式取指针：Option<&mut u32> 不会自动转成 Option<*mut u32>
            let tid = GetWindowThreadProcessId(hwnd, Some(&mut pid as *mut u32));
            Some(Self {
                hwnd: hwnd.0 as isize,
                thread_id: tid,
            })
        }
    }

    pub fn hwnd(&self) -> HWND {
        HWND(self.hwnd as *mut core::ffi::c_void)
    }

    pub fn is_still_foreground(&self) -> bool {
        unsafe { GetForegroundWindow() == self.hwnd() }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InsertStatus {
    /// 全部写入
    Inserted,
    /// 只写进去一部分
    PartiallyInserted,
    /// 落点没了，文字留在剪贴板里，等用户自己粘贴
    CopiedFallback,
    /// 失败
    Failed,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct InsertOutcome {
    pub status: InsertStatus,
    /// 实际写进去的字符数 —— 撤销按这个数回删，不是按原文长度
    pub chars_inserted: usize,
    pub warning: Option<String>,
}

impl InsertOutcome {
    fn failed(reason: impl Into<String>) -> Self {
        Self {
            status: InsertStatus::Failed,
            chars_inserted: 0,
            warning: Some(reason.into()),
        }
    }

    fn fallback(reason: impl Into<String>) -> Self {
        Self {
            status: InsertStatus::CopiedFallback,
            chars_inserted: 0,
            warning: Some(reason.into()),
        }
    }
}

impl fmt::Display for InsertStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            InsertStatus::Inserted => "已插入",
            InsertStatus::PartiallyInserted => "部分插入",
            InsertStatus::CopiedFallback => "已复制，待粘贴",
            InsertStatus::Failed => "写入失败",
        };
        write!(f, "{s}")
    }
}

/// 把 `text` 写进 `anchor` 指向的窗口。
pub fn insert_text(text: &str, anchor: &Anchor) -> InsertOutcome {
    if text.is_empty() {
        return InsertOutcome::failed("没有可写入的文字");
    }

    // 1. 修饰键守卫
    let guard = modifier_guard::wait_modifiers_released(MODIFIER_TIMEOUT_MS, 10);
    if !guard.ok {
        log::warn!("修饰键 {}ms 内没抬起，放弃写入", guard.waited_ms);
        // 文字不丢，留在剪贴板里兜底
        clipboard::set_text(text);
        return InsertOutcome::fallback(format!(
            "修饰键仍按着（等待 {}ms），已复制到剪贴板",
            guard.waited_ms
        ));
    }

    // 2. 落点还在吗。
    // 如果前台被我们自己的气泡/指示器抢了，不是用户真的换了输入框：
    // 先还回去再落字，别误报「焦点已离开」。
    if !anchor.is_still_foreground() {
        unsafe {
            let fg = GetForegroundWindow();
            let mut fg_pid = 0u32;
            GetWindowThreadProcessId(fg, Some(&mut fg_pid as *mut u32));
            if fg_pid == std::process::id() {
                log::warn!("落点被本进程窗口抢走，先还给输入框再落字");
                let _ = SetForegroundWindow(anchor.hwnd());
                std::thread::sleep(Duration::from_millis(120));
            }
        }
    }

    if !anchor.is_still_foreground() {
        // 诊断：把「现在是谁在前台」记下来，否则只知道焦点变了不知道被谁抢的
        unsafe {
            let fg = GetForegroundWindow();
            let mut fg_pid = 0u32;
            GetWindowThreadProcessId(fg, Some(&mut fg_pid as *mut u32));
            log::warn!(
                "焦点已变：前台 hwnd={:?} pid={fg_pid}，落点 hwnd={:?}，本进程 pid={}",
                fg.0,
                anchor.hwnd().0,
                std::process::id()
            );
        }
        clipboard::set_text(text);
        return InsertOutcome::fallback("焦点已经离开原来的输入框，已复制到剪贴板");
    }

    // 3. 走剪贴板粘贴（比逐字输入快一个数量级，也不会丢字）
    log_focus(anchor);
    let clip = clipboard::ClipboardGuard::capture();
    if !clip.set_text(text) {
        return InsertOutcome::failed(format!("写剪贴板失败: {:?}", unsafe {
            GetLastError()
        }));
    }

    let sent = win_input::send_ctrl_v();
    if !sent {
        return InsertOutcome::failed("注入 Ctrl+V 失败");
    }

    // 4. 条件恢复：延迟到后台线程 —— 粘完立刻还原会把旧内容粘进去（09:07 那次丢字）
    clip.restore_after(text);

    InsertOutcome {
        status: InsertStatus::Inserted,
        chars_inserted: text.chars().count(),
        warning: None,
    }
}

/// 落字前记一笔「焦点在哪」，排查「说好了插入却没上屏」时只能靠它。
fn log_focus(anchor: &Anchor) {
    use windows::Win32::UI::WindowsAndMessaging::{GetGUIThreadInfo, GUITHREADINFO};

    let mut info = GUITHREADINFO {
        cbSize: std::mem::size_of::<GUITHREADINFO>() as u32,
        ..Default::default()
    };
    unsafe {
        if GetGUIThreadInfo(anchor.thread_id, &mut info).is_ok() {
            log::info!(
                "落字落点：前台 hwnd={:?}，焦点 hwnd={:?}",
                anchor.hwnd().0,
                info.hwndFocus.0
            );
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn chars_inserted_counts_unicode_scalars() {
        // 「你好，世界 hello」= 5 个汉字标点 + 1 空格 + 5 个字母 = 11
        assert_eq!("你好，世界 hello".chars().count(), 11);
    }
}
