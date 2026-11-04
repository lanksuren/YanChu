//! 修饰键守卫。
//!
//! 用户按着 Ctrl 松手时，我们注入的 Ctrl+V 会变成 Ctrl+Ctrl+V，
//! 目标应用收到的是「粘贴」但修饰键状态是脏的，某些应用会直接吞掉按键。
//! 所以注入前必须等到四个修饰键全部抬起。

use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT,
};

const MODIFIERS: [i32; 5] = [
    VK_SHIFT.0 as i32,
    VK_CONTROL.0 as i32,
    VK_MENU.0 as i32,
    VK_LWIN.0 as i32,
    VK_RWIN.0 as i32,
];

/// 最高位为 1 表示当前按着
fn any_modifier_held() -> bool {
    MODIFIERS
        .iter()
        .any(|vk| unsafe { (GetAsyncKeyState(*vk) as u16 & 0x8000) != 0 })
}

pub struct GuardResult {
    pub ok: bool,
    pub waited_ms: u64,
}

pub fn wait_modifiers_released(timeout_ms: u32, poll_ms: u64) -> GuardResult {
    let started = std::time::Instant::now();

    while started.elapsed().as_millis() < timeout_ms as u128 {
        if !any_modifier_held() {
            return GuardResult {
                ok: true,
                waited_ms: started.elapsed().as_millis() as u64,
            };
        }
        std::thread::sleep(std::time::Duration::from_millis(poll_ms));
    }

    GuardResult {
        ok: !any_modifier_held(),
        waited_ms: started.elapsed().as_millis() as u64,
    }
}
