//! 按键注入。统一走 SendInput，不用已废弃的 keybd_event。

use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP,
    VIRTUAL_KEY, VK_CONTROL, VK_V,
};

fn key_event(vk: VIRTUAL_KEY, up: bool) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: 0,
                dwFlags: KEYBD_EVENT_FLAGS(if up { KEYEVENTF_KEYUP.0 } else { 0 }),
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn send(inputs: &[INPUT]) -> bool {
    if inputs.is_empty() {
        return true;
    }
    let sent = unsafe { SendInput(inputs, std::mem::size_of::<INPUT>() as i32) };
    sent as usize == inputs.len()
}

/// Ctrl+V。按下和抬起之间留一点时间，某些应用（RDP、部分 IDE）对零间隔不敏感。
pub fn send_ctrl_v() -> bool {
    let down = [key_event(VK_CONTROL, false), key_event(VK_V, false)];
    let up = [key_event(VK_V, true), key_event(VK_CONTROL, true)];

    if !send(&down) {
        let _ = send(&up);
        return false;
    }
    std::thread::sleep(std::time::Duration::from_millis(20));
    send(&up)
}
