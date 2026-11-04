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
use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
use windows::Win32::UI::WindowsAndMessaging::{
    BringWindowToTop, GetForegroundWindow, GetWindowThreadProcessId, SetForegroundWindow,
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

/// 把前台还给 `target`，成功返回 true。
///
/// **只调 SetForegroundWindow 是不够的**：Windows 有前台锁，后台进程改前台会被静默拒绝
/// （返回值是 false，而且前台压根没变）。标准解法是把当前前台线程和本线程
/// AttachThreadInput 到一起，临时共享输入队列，再 SetForegroundWindow ——
/// `tools/e2e-test.ps1` 里也是这么做的。
///
/// 这一步的意义：气泡/指示器偶尔会把前台抢过去，用户其实一直待在输入框里，
/// 以前抢不回来就报「焦点已经离开原来的输入框」，等于把字丢到剪贴板（2026-11-04 反馈）。
pub fn force_foreground(target: HWND) -> bool {
    unsafe {
        if GetForegroundWindow() == target {
            return true;
        }

        let fg = GetForegroundWindow();
        let mut fg_pid = 0u32;
        let fg_tid = GetWindowThreadProcessId(fg, Some(&mut fg_pid as *mut u32));
        let cur_tid = GetCurrentThreadId();

        let attached = fg_tid != 0 && fg_tid != cur_tid && AttachThreadInput(cur_tid, fg_tid, true).as_bool();
        let _ = BringWindowToTop(target);
        let raised = SetForegroundWindow(target).as_bool();
        if attached {
            let _ = AttachThreadInput(cur_tid, fg_tid, false);
        }

        raised && GetForegroundWindow() == target
    }
}

/// 前台是谁的进程（诊断用）
fn foreground_pid() -> u32 {
    unsafe {
        let fg = GetForegroundWindow();
        let mut pid = 0u32;
        GetWindowThreadProcessId(fg, Some(&mut pid as *mut u32));
        pid
    }
}

/// 试着把前台抢回 anchor，最多试 `tries` 次，间隔 60ms。
///
/// 为什么要重试：抢回前台之后目标窗口还要几十毫秒才把焦点落到输入框里，
/// 只查一次会把「正在回来的路上」误判成「回不来」。
pub fn reclaim_foreground(anchor: &Anchor, tries: u32) -> bool {
    for _ in 0..tries {
        if anchor.is_still_foreground() {
            return true;
        }
        if force_foreground(anchor.hwnd()) && anchor.is_still_foreground() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(60));
    }
    anchor.is_still_foreground()
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
    // 前台被抢走有两种情况，处理不一样：
    //   * 被我们自己的气泡/指示器抢了 —— 用户根本没动，抢回来接着落字；
    //   * 别的进程抢了 —— 也可能是输入法候选框之类的临时窗口，同样先抢一次试试。
    // 以前只调一次 SetForegroundWindow，被前台锁拒绝后就报「焦点已离开」，
    // 用户明明还在输入框里却被告知失去了焦点（2026-11-04 反馈）。
    if !anchor.is_still_foreground() {
        let ours = foreground_pid() == std::process::id();
        if ours {
            log::warn!("落点被本进程窗口抢走，用 AttachThreadInput 抢回前台");
        } else {
            log::warn!("落点不是前台，先尝试抢回（前台 pid={}）", foreground_pid());
        }
        if reclaim_foreground(anchor, 5) {
            log::info!("前台已抢回落点，继续落字");
        }
    }

    if !anchor.is_still_foreground() {
        // 诊断：把「现在是谁在前台」记下来，否则只知道焦点变了不知道被谁抢的
        let fg_pid = foreground_pid();
        unsafe {
            log::warn!(
                "焦点已变：前台 hwnd={:?} pid={fg_pid}，落点 hwnd={:?}，本进程 pid={}",
                GetForegroundWindow().0,
                anchor.hwnd().0,
                std::process::id()
            );
        }
        clipboard::set_text(text);
        let why = if fg_pid == std::process::id() {
            "气泡挡住了落点，抢不回前台，已复制到剪贴板"
        } else {
            "焦点已经离开原来的输入框，已复制到剪贴板"
        };
        return InsertOutcome::fallback(why);
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
