//! 托盘图标与菜单，以及三个全局开关的唯一真源。
//!
//! 菜单项：语音听写开关、润色开关、深度整理、打开控制面板、复制最近一次、开机启动、重启、退出。
//!
//! 三个开关是真实生效的全局状态：
//! * 听写开关关掉之后，触发键不再触发录音（钩子照样装着，但给一次提示）；
//! * 润色开关控制落字前是否跑润色管线；
//! * 深度整理只在配置了云端 API 时可用（本地模型只有一套提示词）。
//!
//! 状态只在这里改：改完统一「更新托盘勾选 → 持久化 → 广播 state-changed」，
//! 控制面板和托盘都从这一份状态读，避免两处显示不一致。
//! 触发键的显示文案从 `hotkey::config()` 现读，换键后立刻同步。


use std::sync::atomic::{AtomicBool, Ordering};

use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder, Wry};

/// 语音听写总开关
pub static DICTATION_ENABLED: AtomicBool = AtomicBool::new(true);
/// 润色总开关
pub static POLISH_ENABLED: AtomicBool = AtomicBool::new(true);
/// 深度整理档：允许改写与分点，松手后多等 1–2 秒
pub static DEEP_ENABLED: AtomicBool = AtomicBool::new(false);

/// 托盘菜单里的常驻项。保存下来，面板改状态时才能同步勾选/文案。
struct TrayMenu {
    dictation: CheckMenuItem<Wry>,
    polish: CheckMenuItem<Wry>,
    deep: CheckMenuItem<Wry>,
    status: MenuItem<Wry>,
    copy_last: MenuItem<Wry>,
    autostart: CheckMenuItem<Wry>,
}

pub fn dictation_enabled() -> bool {
    DICTATION_ENABLED.load(Ordering::Relaxed)
}

pub fn polish_enabled() -> bool {
    POLISH_ENABLED.load(Ordering::Relaxed)
}

pub fn deep_enabled() -> bool {
    DEEP_ENABLED.load(Ordering::Relaxed)
}

pub fn set_dictation(on: bool) {
    DICTATION_ENABLED.store(on, Ordering::Relaxed);
}

pub fn set_polish(on: bool) {
    POLISH_ENABLED.store(on, Ordering::Relaxed);
}

pub fn set_deep(on: bool) {
    DEEP_ENABLED.store(on, Ordering::Relaxed);
}

/// 当前状态（面板和事件都用这一份）。
pub fn state_json() -> serde_json::Value {
    serde_json::json!({
        "dictation": dictation_enabled(),
        "polish": polish_enabled(),
        "deep": deep_enabled(),
        "deep_available": crate::llm::deep_available(),
    })
}

fn state_path() -> std::path::PathBuf {
    crate::asr::model_root().join("state.json")
}

/// 启动时恢复上次的开关。读不到就用默认值，不报错。
pub fn load_state() {
    let path = state_path();
    let Ok(text) = std::fs::read_to_string(&path) else {
        return;
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
        log::warn!("state.json 解析失败，用默认开关: {}", path.display());
        return;
    };
    if let Some(b) = v.get("dictation").and_then(|x| x.as_bool()) {
        set_dictation(b);
    }
    if let Some(b) = v.get("polish").and_then(|x| x.as_bool()) {
        set_polish(b);
    }
    if let Some(b) = v.get("deep").and_then(|x| x.as_bool()) {
        set_deep(b);
    }
    log::info!(
        "恢复开关：听写={} 润色={} 深度={}",
        dictation_enabled(),
        polish_enabled(),
        deep_enabled()
    );
}

fn persist_state() {
    let path = state_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let v = serde_json::json!({
        "dictation": dictation_enabled(),
        "polish": polish_enabled(),
        "deep": deep_enabled(),
    });
    if let Err(e) = std::fs::write(&path, v.to_string()) {
        log::warn!("写 state.json 失败: {e}");
    }
}

/// 把当前状态同步到托盘勾选、磁盘、以及所有窗口。
fn publish(app: &AppHandle) {
    if let Some(menu) = app.try_state::<TrayMenu>() {
        let _ = menu.dictation.set_checked(dictation_enabled());
        let _ = menu.polish.set_checked(polish_enabled());
        let _ = menu.deep.set_checked(deep_enabled() && crate::llm::deep_available());
    }
    refresh_menu(app);
    persist_state();
    let _ = app.emit("state-changed", state_json());
    log::info!(
        "状态已同步：听写={} 润色={} 深度={}",
        dictation_enabled(),
        polish_enabled(),
        deep_enabled()
    );
}

/// 当前状态行 / tooltip 的文案（触发键换了之后也跟着变）。
fn status_text() -> String {
    let label = crate::hotkey::config().display();
    if dictation_enabled() {
        format!("● 正在听 · {label}")
    } else {
        format!("⏸ 已暂停（{label} 不触发）")
    }
}

fn tooltip_text(phase: &str) -> String {
    let cfg = crate::hotkey::config();
    match phase {
        "loading" => "言出 · 正在加载模型…".to_string(),
        "listening" => format!("言出 · {}", cfg.listening_hint()),
        "polishing" => "言出 · 正在整理…".to_string(),
        "done" => "言出 · 已落字".to_string(),
        "paused" => format!("言出 · 已暂停（{} 不触发）", cfg.display()),
        _ => format!("言出 —— {}（单击打开控制面板）", cfg.trigger_hint()),
    }
}

/// 刷新「状态行 / 复制 / tooltip」这些依赖运行时状态的内容。
pub fn refresh_menu(app: &AppHandle) {
    let Some(menu) = app.try_state::<TrayMenu>() else {
        return;
    };
    let has_text = app
        .try_state::<crate::Runtime>()
        .and_then(|rt| rt.last_text.try_lock().ok().map(|g| g.is_some()))
        .unwrap_or(false);
    let status = status_text();
    let _ = menu.status.set_text(&status);
    let _ = menu.copy_last.set_enabled(has_text);

    // 润色配置可能在控制面板里从本地切到云端（或反过来）：
    // 深度档的可用性和文案要跟着变，不能等到重启。
    let deep_ok = crate::llm::deep_available();
    let _ = menu.deep.set_enabled(deep_ok);
    let _ = menu.deep.set_text(if deep_ok {
        "深度整理（多等 1–2 秒）"
    } else {
        "深度整理（需要配置 API 模型）"
    });
    let _ = menu.deep.set_checked(deep_enabled() && deep_ok);

    if let Some(tray) = app.tray_by_id("yanchu") {
        let tip = tooltip_text(&current_phase());
        let _ = tray.set_tooltip(Some(tip.as_str()));
    }
    // 开机启动的勾选只在建菜单和用户切换时更新：
    // 每次落字都跑一次 reg query 会白闪一个控制台（即使加了 NO_WINDOW 也没必要）。
}

pub fn apply_dictation(app: &AppHandle, on: bool) {
    set_dictation(on);
    publish(app);
    set_phase(app, if on { "idle" } else { "paused" });
    log::info!("语音听写: {}", if on { "开" } else { "关" });
}

pub fn apply_polish(app: &AppHandle, on: bool) {
    set_polish(on);
    publish(app);
    log::info!("润色: {}", if on { "开" } else { "关" });
}

pub fn apply_deep(app: &AppHandle, on: bool) {
    // 没有云端 API 时深度档不生效，别让用户切到一个假开关
    if on && !crate::llm::deep_available() {
        log::info!("深度整理不可用（未配置云端 API），忽略这次切换");
        publish(app);
        return;
    }
    set_deep(on);
    publish(app);
    log::info!("润色档位: {}", if on { "深度" } else { "标准" });
}

pub fn build(app: &AppHandle) -> tauri::Result<()> {
    let dictation = CheckMenuItem::with_id(
        app,
        "dictation",
        "语音听写",
        true,
        dictation_enabled(),
        None::<&str>,
    )?;
    let polish = CheckMenuItem::with_id(
        app,
        "polish",
        "润色",
        true,
        polish_enabled(),
        None::<&str>,
    )?;
    // 深度档只对云端 API 开放：本地模型只有一套提示词。
    // 本地跑的时候这一项直接置灰，免得用户切了却没效果。
    let deep_ok = crate::llm::deep_available();
    let deep = CheckMenuItem::with_id(
        app,
        "deep",
        if deep_ok {
            "深度整理（多等 1–2 秒）"
        } else {
            "深度整理（需要配置 API 模型）"
        },
        deep_ok,
        deep_enabled() && deep_ok,
        None::<&str>,
    )?;
    let initial_status = status_text();
    let status = MenuItem::with_id(app, "status", initial_status.as_str(), false, None::<&str>)?;
    let panel = MenuItem::with_id(app, "panel", "控制面板…", true, None::<&str>)?;
    let copy_last = MenuItem::with_id(app, "copy_last", "复制最近一次落字", false, None::<&str>)?;
    let autostart = CheckMenuItem::with_id(
        app,
        "autostart",
        "开机启动",
        true,
        autostart_enabled(),
        None::<&str>,
    )?;
    let restart = MenuItem::with_id(app, "restart", "重启", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;

    let menu = Menu::with_items(
        app,
        &[
            &status,
            &PredefinedMenuItem::separator(app)?,
            &dictation,
            &polish,
            &deep,
            &PredefinedMenuItem::separator(app)?,
            &panel,
            &copy_last,
            &PredefinedMenuItem::separator(app)?,
            &autostart,
            &restart,
            &quit,
        ],
    )?;

    app.manage(TrayMenu {
        dictation: dictation.clone(),
        polish: polish.clone(),
        deep: deep.clone(),
        status: status.clone(),
        copy_last: copy_last.clone(),
        autostart: autostart.clone(),
    });

    let mut builder = TrayIconBuilder::with_id("yanchu")
        .tooltip(tooltip_text("idle"))
        .menu(&menu)
        .show_menu_on_left_click(false)
        // 左键单击 = 打开控制面板。Windows 用户的第一直觉就是点图标，
        // 只把功能藏在右键菜单里等于没入口。
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                open_panel(tray.app_handle());
            }
        })
        .on_menu_event(move |app, event| match event.id().as_ref() {
            "dictation" => apply_dictation(app, !dictation_enabled()),
            "polish" => apply_polish(app, !polish_enabled()),
            "deep" => apply_deep(app, !deep_enabled()),
            "panel" => {
                open_panel(app);
            }
            "copy_last" => {
                let text = app
                    .try_state::<crate::Runtime>()
                    .and_then(|rt| rt.last_text.try_lock().ok().and_then(|g| g.clone()));
                match text {
                    Some(t) => {
                        match arboard::Clipboard::new() {
                            Ok(mut cb) => {
                                let _ = cb.set_text(t);
                                log::info!("已复制最近一次落字");
                            }
                            Err(e) => log::warn!("复制失败: {e}"),
                        }
                    }
                    None => log::info!("没有可复制的落字"),
                }
            }
            "autostart" => {
                let next = !autostart_enabled();
                set_autostart(next);
                if let Some(menu) = app.try_state::<TrayMenu>() {
                    let _ = menu.autostart.set_checked(next);
                }
                log::info!("开机启动: {}", if next { "开" } else { "关" });
            }
            "restart" => {
                // 用 Tauri 的重启：会带着同样的参数重新拉起进程
                log::info!("重启言出");
                app.restart();
            }
            "quit" => {
                log::info!("退出言出（会卸载本地润色模型）");
                crate::shutdown(app);
            }
            _ => {}
        });

    if let Some(icon) = app.default_window_icon().cloned() {
        builder = builder.icon(icon);
    }

    builder.build(app)?;
    set_phase(
        app,
        if crate::WARMUPS_LEFT.load(Ordering::SeqCst) > 0 {
            "loading"
        } else if dictation_enabled() {
            "idle"
        } else {
            "paused"
        },
    );
    log::info!("托盘图标就绪");
    Ok(())
}

/// 托盘图标的四个状态：待机 / 监听 / 整理 / 暂停。
/// 图标是编译时嵌进来的（icons/tray-*.png），tooltip 同步说明当前状态。
///
/// 这个函数会碰到托盘（主线程 API），所以**永远异步执行**：
/// 它可能被热键工作线程调用，绝不能在那里等主线程。
pub fn set_phase(app: &AppHandle, phase: &str) {
    let app = app.clone();
    let phase = phase.to_string();
    std::thread::spawn(move || {
        let _guard = PHASE_LOCK.lock().unwrap();
        apply_phase(&app, &phase);
    });
}

static PHASE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
static CURRENT_PHASE: std::sync::Mutex<String> = std::sync::Mutex::new(String::new());
static LOADING_ALT: AtomicBool = AtomicBool::new(false);

/// 模型预热时闪烁两帧，让「正在加载」不只是静态颜色。
pub fn pulse_loading(app: &AppHandle) {
    if current_phase() != "loading" {
        return;
    }
    let alt = LOADING_ALT.fetch_xor(true, Ordering::SeqCst);
    let bytes: &[u8] = if alt {
        include_bytes!("../icons/tray-loading2.png")
    } else {
        include_bytes!("../icons/tray-loading.png")
    };
    let Some(tray) = app.tray_by_id("yanchu") else {
        return;
    };
    if let Ok(img) = tauri::image::Image::from_bytes(bytes) {
        let _ = tray.set_icon(Some(img));
    }
}

pub fn current_phase() -> String {
    CURRENT_PHASE.lock().map(|s| s.clone()).unwrap_or_default()
}

fn apply_phase(app: &AppHandle, phase: &str) {
    if let Ok(mut cur) = CURRENT_PHASE.lock() {
        *cur = phase.to_string();
    }
    let Some(tray) = app.tray_by_id("yanchu") else {
        return;
    };
    let bytes: &[u8] = match phase {
        "loading" => &include_bytes!("../icons/tray-loading.png")[..],
        "listening" => &include_bytes!("../icons/tray-listen.png")[..],
        "polishing" => &include_bytes!("../icons/tray-polish.png")[..],
        "done" => &include_bytes!("../icons/tray-done.png")[..],
        "paused" => &include_bytes!("../icons/tray-paused.png")[..],
        _ => &include_bytes!("../icons/tray-idle.png")[..],
    };
    if let Ok(img) = tauri::image::Image::from_bytes(bytes) {
        let _ = tray.set_icon(Some(img));
    }
    let tip = tooltip_text(phase);
    let _ = tray.set_tooltip(Some(tip.as_str()));
    log::info!("托盘相位: {phase}");
}

pub fn open_panel(app: &AppHandle) {
    if let Some(win) = app.get_webview_window("settings") {
        let _ = win.unminimize();
        let _ = win.show();
        let _ = win.set_focus();
        open_tab_from_env(&win);
        return;
    }

    // 懒创建：启动时不再预建 760×620 的面板窗口，避免它在托盘出现前闪一下。
    // 第一次从托盘打开时才建；建好后只 hide，不销毁。
    let builder = WebviewWindowBuilder::new(app, "settings", WebviewUrl::App("settings.html".into()))
        .title("言出 · 控制面板")
        .inner_size(760.0, 620.0)
        .resizable(true)
        .center()
        // 等页面第一帧画完再显示，避免先看到一个白框
        .on_page_load(|w, _| {
            let _ = w.show();
            let _ = w.set_focus();
            open_tab_from_env(&w);
        })
        .visible(false);

    match builder.build() {
        Ok(win) => {
            // 页面可能已经加载完（事件错过），兜底再显示一次
            let fallback = win.clone();
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(700));
                let _ = fallback.show();
                let _ = fallback.set_focus();
                open_tab_from_env(&fallback);
            });
        }
        Err(e) => log::error!("创建控制面板失败: {e}"),
    }
}

/// 调试 / 截图用：`YANCHU_PANEL_TAB=models` 打开面板时直接切到对应页签。
/// 只接受字母数字，避免把环境变量拼进 JS。
fn open_tab_from_env(win: &tauri::WebviewWindow) {
    let Ok(tab) = std::env::var("YANCHU_PANEL_TAB") else {
        return;
    };
    if tab.is_empty() || !tab.chars().all(|c| c.is_ascii_alphanumeric()) {
        return;
    }
    log::info!("面板页签切换：{tab}");
    match win.eval(&format!(
        "document.querySelector('.tab[data-tab=\"{tab}\"]')?.click()"
    )) {
        Ok(()) => log::info!("面板页签切换请求已发出：{tab}"),
        Err(e) => log::warn!("面板页签切换失败：{e}"),
    }
}

// ---------------- 开机启动（HKCU Run，不需要管理员） ----------------

const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
/// 不创建控制台窗口。reg.exe 是控制台程序，GUI 进程直接 spawn 会闪一个黑框。
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

fn reg_command() -> std::process::Command {
    use std::os::windows::process::CommandExt;
    let mut cmd = std::process::Command::new("reg");
    cmd.creation_flags(CREATE_NO_WINDOW);
    cmd
}

pub fn autostart_enabled() -> bool {
    reg_command()
        .args(["query", RUN_KEY, "/v", "YanChu"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn autostart_command() -> String {
    let exe = std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    format!("\"{exe}\" --minimized")
}

fn set_autostart(on: bool) {
    let mut cmd = reg_command();
    if on {
        cmd.args([
            "add",
            RUN_KEY,
            "/v",
            "YanChu",
            "/t",
            "REG_SZ",
            "/d",
            &autostart_command(),
            "/f",
        ]);
    } else {
        cmd.args(["delete", RUN_KEY, "/v", "YanChu", "/f"]);
    }
    match cmd.output() {
        Ok(o) if o.status.success() => {
            log::info!("开机启动已{}", if on { "开启" } else { "关闭" })
        }
        Ok(o) => log::warn!("设置开机启动失败: {}", String::from_utf8_lossy(&o.stderr)),
        Err(e) => log::warn!("调用 reg 失败: {e}"),
    }
}
