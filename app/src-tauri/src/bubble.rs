//! 两个窗口：
//!
//! * `indicator` —— 小圆环，实时跟着鼠标走，用动画表示「在录音 / 有没有声音 /
//!   声音多大 / 正在润色」。它必须跟手，所以用一个 30Hz 的线程挪窗口位置。
//! * `bubble` —— 屏幕下方的浅色气泡，显示实时识别出来的文字。
//!
//! 两个窗口都必须「不抢焦点 + 鼠标穿透」，否则用户点一下就跑到后台去了，
//! 而且落字的焦点校验会失败。

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, PhysicalPosition, PhysicalSize};
use windows::Win32::Foundation::{HWND, POINT};
use windows::Win32::Graphics::Gdi::ClientToScreen;

use windows::Win32::UI::WindowsAndMessaging::{
    GetCursorPos, GetForegroundWindow, GetGUIThreadInfo, GetSystemMetrics, GetWindowLongPtrW,
    GetWindowThreadProcessId, SetForegroundWindow, SetWindowLongPtrW, ShowWindow, GUITHREADINFO,
    GWL_EXSTYLE, SM_CXSCREEN, SM_CYSCREEN, SW_SHOWNOACTIVATE, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
};

use crate::insert::Anchor;
use crate::tray;

/// 指示器是否正在跟随鼠标
static FOLLOWING: AtomicBool = AtomicBool::new(false);

/// 每一轮「显示」的序号。旧的 hide_later 醒来时如果序号变了，
/// 说明期间又开了新一轮，不能把新一轮的窗口关掉。
static SHOW_GEN: AtomicU64 = AtomicU64::new(0);

/// 窗口当前是否可见。用来只在「从隐藏到显示」时发一次进场事件。
static VISIBLE: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Serialize)]
struct BubbleUpdate {
    kind: &'static str,
    /// 已经说完、不再变化的句子
    committed: String,
    /// 正在说、还可能变的尾巴
    tail: String,
    /// 定稿时的整段文本
    text: String,
    /// 状态行（「深度整理中，可以等一两秒…」之类）。空串就用前端默认文案。
    status: String,
}

#[derive(Clone, Serialize)]
struct IndicatorUpdate {
    state: &'static str,
    level: f32,
}

pub fn init(app: &AppHandle) -> anyhow::Result<()> {
    for label in ["bubble", "indicator"] {
        let Some(win) = app.get_webview_window(label) else {
            continue;
        };
        win.set_ignore_cursor_events(true)?;

        // WebView2 自己有一层默认底色（白），窗口 transparent 只影响窗口本身，
        // 挡不住这层底色 —— 圆角以外就会露出一个灰白的方块。
        // 必须把 webview 的背景色显式设成全透明（alpha = 0）。
        let _ = win.set_background_color(Some(tauri::window::Color(0, 0, 0, 0)));

        let ok = apply_no_activate(&win);
        log::info!("{label} 窗口就绪（不抢焦点={ok}）");
    }

    // 鼠标跟随线程：只在指示器可见时跑
    let follow_app = app.clone();
    std::thread::Builder::new()
        .name("yanchu-follow".into())
        .spawn(move || loop {
            std::thread::sleep(Duration::from_millis(33));
            if !FOLLOWING.load(Ordering::Relaxed) {
                continue;
            }
            let Some(win) = follow_app.get_webview_window("indicator") else {
                continue;
            };
            if let Some(pt) = cursor_point() {
                // 指示器放在光标右下方一点，别挡住用户正在看的地方
                let _ = win.set_position(PhysicalPosition::new(pt.x + 18, pt.y + 20));
            }
        })?;

    Ok(())
}

fn apply_no_activate(win: &tauri::WebviewWindow) -> bool {
    unsafe {
        let Ok(h) = win.hwnd() else {
            return false;
        };
        let hwnd = HWND(h.0 as *mut core::ffi::c_void);
        let no_activate = WS_EX_NOACTIVATE.0 as isize;
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        let patched = ex | no_activate | (WS_EX_TOOLWINDOW.0 as isize);
        if patched != ex {
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, patched);
        }
        (GetWindowLongPtrW(hwnd, GWL_EXSTYLE) & no_activate) != 0
    }
}

/// 显示但不激活：SW_SHOWNOACTIVATE。WebView2 在冷启动时 `show()` 可能抢一次前台，
/// 用这个可以避免把用户输入框的焦点顶掉。
fn show_quiet(win: &tauri::WebviewWindow) {
    unsafe {
        if let Ok(h) = win.hwnd() {
            let hwnd = HWND(h.0 as *mut core::ffi::c_void);
            if ShowWindow(hwnd, SW_SHOWNOACTIVATE).as_bool() {
                return;
            }
        }
    }
    let _ = win.show();
}

/// 显示之后盯一小会儿：只要前台还是本进程的窗口，就还给 anchor。
/// 冷启动时 WebView2 可能晚 100–300ms 才激活，所以不能只查一次。
fn guard_focus(anchor: Anchor) {
    std::thread::spawn(move || {
        for _ in 0..6 {
            std::thread::sleep(Duration::from_millis(70));
            unsafe {
                let fg = GetForegroundWindow();
                let mut pid = 0u32;
                GetWindowThreadProcessId(fg, Some(&mut pid as *mut u32));
                if pid != std::process::id() {
                    return;
                }
                let _ = SetForegroundWindow(anchor.hwnd());
            }
        }
    });
}

/// 一次录音开始：显示指示器（跟随鼠标）和底部气泡。
pub fn start_session(app: &AppHandle, anchor: &Anchor) {
    SHOW_GEN.fetch_add(1, Ordering::Relaxed);
    if let Some(win) = app.get_webview_window("indicator") {
        apply_no_activate(&win);
        if let Some(pt) = cursor_point() {
            let _ = win.set_position(PhysicalPosition::new(pt.x + 18, pt.y + 20));
        }
        show_quiet(&win);
        FOLLOWING.store(true, Ordering::Relaxed);
    }

    if let Some(win) = app.get_webview_window("bubble") {
        apply_no_activate(&win);
        place_bottom_center(&win);
        show_quiet(&win);
    }

    let listening = crate::hotkey::config().listening_hint();
    emit_bubble(app, "listening", &listening);
    emit_indicator(app, "listening", 0.0);
    if !VISIBLE.swap(true, Ordering::Relaxed) {
        let _ = app.emit("shown", ());
    }
    tray::set_phase(app, "listening");
    // 冷启动时 WebView2 可能稍后才激活；盯 400ms，前台一旦是本进程就还给输入框
    guard_focus(*anchor);
}

/// 录音中的电平与状态。`state`: listening / speaking / silent / polishing
pub fn set_level(app: &AppHandle, state: &'static str, level: f32) {
    emit_indicator(app, state, level);
}

pub fn partial(app: &AppHandle, text: &str) {
    emit_bubble(app, "partial", text);
}

/// 落字完成后的指示器确认：绿勾停 650ms，然后只收指示器。
/// 气泡在落字前已经收掉了，这里不再碰它——二次 hide/再次播退场会让
/// 透明窗口在 DWM 里闪一下白框。
pub fn finish_indicator(app: &AppHandle) {
    emit_indicator(app, "done", 0.0);
    // 托盘：整理中（紫）→ 已落字（绿）
    tray::set_phase(app, "done");
    let app = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(650));
        FOLLOWING.store(false, Ordering::Relaxed);
        if let Some(win) = app.get_webview_window("indicator") {
            let _ = win.hide();
        }
        // 绿勾停留一会儿再回待机，别让托盘一直卡在「正在整理」
        std::thread::sleep(Duration::from_millis(900));
        tray::set_phase(
            &app,
            if tray::dictation_enabled() {
                "idle"
            } else {
                "paused"
            },
        );
    });
}

/// 落字前先把气泡收掉：退场动画走完 → 隐藏窗口 → 调用方才把文字写进输入框。
/// 阻塞 140ms；调用方在后台线程（on_release），不占 UI。
pub fn fade_out_before_insert(app: &AppHandle) {
    if !VISIBLE.load(Ordering::Relaxed) {
        return;
    }
    let _ = app.emit("hiding", ());
    std::thread::sleep(Duration::from_millis(140));
    if let Some(win) = app.get_webview_window("bubble") {
        let _ = win.hide();
    }
    VISIBLE.store(false, Ordering::Relaxed);
    FOLLOWING.store(false, Ordering::Relaxed);
}

pub fn notice(app: &AppHandle, kind: &'static str, text: &str, hold: Duration) {
    SHOW_GEN.fetch_add(1, Ordering::Relaxed);
    if let Some(win) = app.get_webview_window("indicator") {
        show_quiet(&win);
        FOLLOWING.store(true, Ordering::Relaxed);
    }
    if let Some(win) = app.get_webview_window("bubble") {
        place_bottom_center(&win);
        show_quiet(&win);
    }
    emit_indicator(app, "notice", 0.0);
    emit_bubble(app, kind, text);
    if !VISIBLE.swap(true, Ordering::Relaxed) {
        let _ = app.emit("shown", ());
    }
    // 提示类消息结束这一轮：托盘不要停在紫色「正在整理」
    tray::set_phase(
        app,
        if tray::dictation_enabled() {
            "idle"
        } else {
            "paused"
        },
    );
    hide_later(app, hold);
}

/// 停一会儿再消失，让用户看清落进去的是什么。
pub fn hide_later(app: &AppHandle, delay: Duration) {
    let app = app.clone();
    let gen = SHOW_GEN.load(Ordering::Relaxed);
    std::thread::spawn(move || {
        std::thread::sleep(delay);
        // 期间又开了一轮（新的录音 / 新的提示），这一轮就别关了
        if SHOW_GEN.load(Ordering::Relaxed) != gen {
            return;
        }
        // 先让前端播 120ms 退场，再真正隐藏窗口
        let _ = app.emit("hiding", ());
        std::thread::sleep(Duration::from_millis(140));
        if SHOW_GEN.load(Ordering::Relaxed) != gen {
            return;
        }
        hide_all(&app);
    });
}

pub fn hide_all(app: &AppHandle) {
    FOLLOWING.store(false, Ordering::Relaxed);
    VISIBLE.store(false, Ordering::Relaxed);
    for label in ["bubble", "indicator"] {
        if let Some(win) = app.get_webview_window(label) {
            let _ = win.hide();
        }
    }
    // 回到待机；听写被暂停时保持暂停图标
    tray::set_phase(
        app,
        if tray::dictation_enabled() {
            "idle"
        } else {
            "paused"
        },
    );
}

fn emit_bubble(app: &AppHandle, kind: &'static str, text: &str) {
    let _ = app.emit(
        "bubble",
        BubbleUpdate {
            kind,
            committed: if kind == "done" || kind == "error" {
                text.to_owned()
            } else {
                String::new()
            },
            tail: if kind == "done" || kind == "error" {
                String::new()
            } else {
                text.to_owned()
            },
            text: text.to_owned(),
            status: String::new(),
        },
    );
}

/// 润色中：文字先留着，加一个呼吸的点，让用户知道还在处理。
pub fn polishing(app: &AppHandle, text: &str) {
    tray::set_phase(app, "polishing");
    let _ = app.emit(
        "bubble",
        BubbleUpdate {
            kind: "polishing",
            committed: text.to_owned(),
            tail: String::new(),
            text: text.to_owned(),
            status: String::new(),
        },
    );
}

/// 深度档润色中：这一档要等，必须明确告诉用户在做什么、大概多久。
///
/// `hint` 由调用方给：手动切档是「一两秒」，长文自动切档是「3–5 秒」——
/// 同样是转圈，等 5 秒和等 1 秒的体感完全不同，文案不能共用。
pub fn deep_polishing(app: &AppHandle, text: &str, hint: &str) {
    tray::set_phase(app, "polishing");
    let _ = app.emit(
        "bubble",
        BubbleUpdate {
            kind: "deep",
            committed: text.to_owned(),
            tail: String::new(),
            text: text.to_owned(),
            status: hint.to_owned(),
        },
    );
}

/// 实时更新：前半段已经定稿，尾巴还在变。
pub fn partial_parts(app: &AppHandle, committed: &str, tail: &str) {
    let _ = app.emit(
        "bubble",
        BubbleUpdate {
            kind: "partial",
            committed: committed.to_owned(),
            tail: tail.to_owned(),
            text: format!("{committed}{tail}"),
            status: String::new(),
        },
    );
}

fn emit_indicator(app: &AppHandle, state: &'static str, level: f32) {
    let _ = app.emit("indicator", IndicatorUpdate { state, level });
}

fn cursor_point() -> Option<POINT> {
    let mut pt = POINT::default();
    unsafe {
        if GetCursorPos(&mut pt).is_ok() {
            return Some(pt);
        }
    }
    None
}

/// 气泡贴在屏幕下方居中，离任务栏留一点距离。
fn place_bottom_center(win: &tauri::WebviewWindow) {
    let size = win
        .outer_size()
        .unwrap_or_else(|_| PhysicalSize::new(620, 96));

    let (screen_w, screen_h) = unsafe {
        (
            GetSystemMetrics(SM_CXSCREEN),
            GetSystemMetrics(SM_CYSCREEN),
        )
    };

    let x = (screen_w - size.width as i32) / 2;
    let y = screen_h - size.height as i32 - 96;
    let _ = win.set_position(PhysicalPosition::new(x.max(0), y.max(0)));
}

/// 光标位置优先取「插入符」，这样气泡能贴在正在打字的地方（提示类消息用）。
#[allow(dead_code)]
fn caret_point(thread_id: u32) -> Option<POINT> {
    unsafe {
        let mut info = GUITHREADINFO {
            cbSize: std::mem::size_of::<GUITHREADINFO>() as u32,
            ..Default::default()
        };

        if GetGUIThreadInfo(thread_id, &mut info).is_ok() && !info.hwndCaret.0.is_null() {
            let mut pt = POINT {
                x: info.rcCaret.left,
                y: info.rcCaret.bottom,
            };
            if ClientToScreen(info.hwndCaret, &mut pt).as_bool() {
                return Some(pt);
            }
        }
    }
    None
}
