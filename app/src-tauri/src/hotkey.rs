//! 全局热键：可配置的组合键 / 单键，支持「按住说话」和「按一下开始 / 停止」。
//!
//! 用低层键盘钩子（WH_KEYBOARD_LL）而不是 RegisterHotKey，原因：
//! 1. 松手也是信号，RegisterHotKey 拿不到；
//! 2. 组合键的最后一个普通键要能被吞掉，避免它同时落进目标应用。
//!
//! 设计要点：
//! * 钩子回调里只做「匹配 + 入队」，真正的录音开关跑在独立工作线程；
//! * 组合键只吞最后一个普通键，Ctrl / Shift / Alt 仍然放行，不影响正常打字；
//! * 单独一个右 Alt 仍然是被完全吞掉的，和旧版行为一致；
//! * 触发键可以在控制面板里随时换，不需要重启应用。
//!
//! **钩子回调绝不能阻塞。** 之前把窗口 / 托盘调用放进回调，整个系统的键盘都会卡住。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::{Mutex, OnceLock, RwLock};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GetMessageW, SetWindowsHookExW, UnhookWindowsHookEx, HC_ACTION,
    KBDLLHOOKSTRUCT, MSG, WH_KEYBOARD_LL, WM_KEYDOWN, WM_KEYUP, WM_SYSKEYDOWN, WM_SYSKEYUP,
};

use crate::trigger::{InputMode, TriggerConfig, VK_ESCAPE};

/// 供业务层使用的高层事件。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    /// 开始一轮录音
    Press,
    /// 结束这一轮录音（松手、再按一次、Esc 都归到这里）
    Release,
}

type Callback = Box<dyn Fn(Event) -> bool + Send + 'static>;

/// 钩子线程 → 工作线程。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Signal {
    /// 触发组合已经完整按下（最后一个键按下）
    Down,
    /// 触发组合里的任意键抬起
    Up,
    /// 「按一下」模式录音中按了 Esc
    Stop,
}

/// 钩子回调里维护的匹配状态。只用固定大小数组，不在回调里分配内存。
struct Matcher {
    pressed: [bool; 256],
    /// 这个键的按下被吞掉了，抬起也要吞
    swallowed_up: [bool; 256],
    /// 当前这一组触发键是否完整按下
    combo: bool,
    /// 组合里负责「最后按下」的普通键
    primary: u32,
    esc_swallowed: bool,
}

impl Matcher {
    const fn new() -> Self {
        Self {
            pressed: [false; 256],
            swallowed_up: [false; 256],
            combo: false,
            primary: 0,
            esc_swallowed: false,
        }
    }

    fn reset(&mut self) {
        self.pressed = [false; 256];
        self.swallowed_up = [false; 256];
        self.combo = false;
        self.primary = 0;
        self.esc_swallowed = false;
    }

    /// 返回「是否吞掉这个键」；需要通知工作线程时顺便入队。
    fn handle(
        &mut self,
        vk: u32,
        down: bool,
        cfg: &TriggerConfig,
        active: bool,
        tx: &Sender<Signal>,
    ) -> bool {
        if vk > 255 {
            return false;
        }
        let vk = vk as usize;
        let key = vk as u32;

        // Esc：只在「按一下」模式录音中拦，用来结束录音。
        if key == VK_ESCAPE && !cfg.keys.contains(&VK_ESCAPE) {
            if down {
                if cfg.mode == InputMode::Toggle && active {
                    self.esc_swallowed = true;
                    let _ = tx.send(Signal::Stop);
                    return true;
                }
            } else if self.esc_swallowed {
                self.esc_swallowed = false;
                return true;
            }
            return false;
        }

        if down {
            // 系统自动重复的 keydown：组合已经按下时只吞主键，不再发信号。
            if self.pressed[vk] {
                return self.combo && key == self.primary;
            }
            self.pressed[vk] = true;

            let Some(&primary) = cfg.keys.last() else {
                return false;
            };

            // 只有「最后一个键按下」才算组合完成：这样我们能把最后这个键吞掉，
            // 而修饰键（Ctrl / Shift / Alt）仍然留给目标应用。
            if !self.combo
                && key == primary
                && cfg
                    .keys
                    .iter()
                    .all(|k| *k == primary || self.pressed[*k as usize])
            {
                self.combo = true;
                self.primary = primary;
                self.swallowed_up[vk] = true;
                let _ = tx.send(Signal::Down);
                return true;
            }
            false
        } else {
            self.pressed[vk] = false;
            let mut swallow = false;
            let mut signal = None;

            // 任意一个触发键抬起，这一组就结束了。
            // 主键的抬起仍要靠 swallowed_up 吞掉（哪怕组合先被修饰键取消）。
            if self.combo && cfg.keys.iter().any(|k| *k as usize == vk) {
                self.combo = false;
                signal = Some(Signal::Up);
            }
            if self.swallowed_up[vk] {
                self.swallowed_up[vk] = false;
                swallow = true;
            }
            if let Some(signal) = signal {
                let _ = tx.send(signal);
            }
            swallow
        }
    }
}

static CONFIG: OnceLock<RwLock<TriggerConfig>> = OnceLock::new();
static EVENTS: OnceLock<Sender<Signal>> = OnceLock::new();
static MATCHER: Mutex<Matcher> = Mutex::new(Matcher::new());
/// 控制面板正在录快捷键：此时只放行，不触发录音。
static SUSPENDED: AtomicBool = AtomicBool::new(false);
/// 当前是否真的有一轮录音在进行（决定 Esc 要不要拦）。
static ACTIVE: AtomicBool = AtomicBool::new(false);

pub fn install(config: TriggerConfig, callback: Callback) -> anyhow::Result<()> {
    let _ = CONFIG.set(RwLock::new(config));

    let (tx, rx) = channel::<Signal>();
    let _ = EVENTS.set(tx);

    // 业务线程：按队列顺序处理按下 / 抬起，钩子线程永远不会被业务阻塞。
    std::thread::Builder::new()
        .name("yanchu-hotkey-worker".into())
        .spawn(move || worker(rx, callback))?;

    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<anyhow::Result<()>>();

    std::thread::Builder::new()
        .name("yanchu-hotkey".into())
        .spawn(move || unsafe {
            let hook = match SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook_proc), None, 0) {
                Ok(h) => h,
                Err(e) => {
                    let _ = ready_tx.send(Err(anyhow::anyhow!("安装键盘钩子失败: {e}")));
                    return;
                }
            };
            let _ = ready_tx.send(Ok(()));

            let mut msg = MSG::default();
            // GetMessageW 在 WM_QUIT 时返回 0，出错时返回 -1
            loop {
                let ret = GetMessageW(&mut msg, None, 0, 0);
                if ret.0 == 0 || ret.0 == -1 {
                    break;
                }
            }
            let _ = UnhookWindowsHookEx(hook);
        })?;

    ready_rx.recv()?
}

/// 运行期换键：控制面板保存后调用，不需要重装钩子。
pub fn set_config(config: TriggerConfig) {
    if let Some(lock) = CONFIG.get() {
        if let Ok(mut guard) = lock.write() {
            *guard = config;
        }
    }
    reset_matcher();
}

pub fn config() -> TriggerConfig {
    CONFIG
        .get()
        .and_then(|lock| lock.read().ok().map(|guard| guard.clone()))
        .unwrap_or_default()
}

/// 业务层告诉钩子「现在有没有录音」，用于判断 Esc 是否要拦。
pub fn set_active(active: bool) {
    ACTIVE.store(active, Ordering::Relaxed);
}

/// 录快捷键期间挂起触发：所有键放行，避免在设置窗口里按到旧触发键。
pub fn set_suspended(suspended: bool) {
    SUSPENDED.store(suspended, Ordering::Relaxed);
    reset_matcher();
}

fn reset_matcher() {
    if let Ok(mut matcher) = MATCHER.lock() {
        matcher.reset();
    }
}

unsafe extern "system" fn hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 && lparam.0 != 0 {
        let kb = &*(lparam.0 as *const KBDLLHOOKSTRUCT);

        let event = match wparam.0 as u32 {
            WM_KEYDOWN | WM_SYSKEYDOWN => Some(true),
            WM_KEYUP | WM_SYSKEYUP => Some(false),
            _ => None,
        };

        // 注入事件也照常处理：端到端测试用 keybd_event 合成按键，
        // 而本应用只注入 Ctrl+V，不会命中触发键（编辑类快捷键已在配置校验里排除）。
        if let Some(down) = event {
            if kb.vkCode < 256 && !SUSPENDED.load(Ordering::Relaxed) {
                if let (Some(tx), Some(cfg)) = (EVENTS.get(), CONFIG.get()) {
                    if let Ok(guard) = cfg.read() {
                        if let Ok(mut matcher) = MATCHER.lock() {
                            let swallow = matcher.handle(
                                kb.vkCode,
                                down,
                                &guard,
                                ACTIVE.load(Ordering::Relaxed),
                                tx,
                            );
                            if swallow {
                                return LRESULT(1);
                            }
                        }
                    }
                }
            }
        }
    }
    CallNextHookEx(None, code, wparam, lparam)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Idle,
    /// 已按下，但还没到长按阈值
    Waiting,
    Recording,
}

/// 工作线程：长按计时 + 两种输入模式的状态机。
fn worker(rx: Receiver<Signal>, callback: Callback) {
    let mut phase = Phase::Idle;
    let mut deadline: Option<Instant> = None;

    loop {
        let wait = deadline
            .map(|at| at.saturating_duration_since(Instant::now()))
            .unwrap_or(Duration::from_secs(3600));

        match rx.recv_timeout(wait) {
            Ok(signal) => {
                let cfg = config();
                match signal {
                    Signal::Down => match phase {
                        Phase::Idle => {
                            if cfg.long_press_ms > 0 {
                                phase = Phase::Waiting;
                                deadline = Some(
                                    Instant::now()
                                        + Duration::from_millis(cfg.long_press_ms as u64),
                                );
                                log::debug!("触发键按下，等待长按 {}ms", cfg.long_press_ms);
                            } else if callback(Event::Press) {
                                phase = Phase::Recording;
                                log::info!("触发键按下：开始录音");
                            } else {
                                phase = Phase::Idle;
                            }
                        }
                        Phase::Waiting => {
                            // 长按还没到点又收到 Down（理论上不会）：保持等待
                        }
                        Phase::Recording => {
                            if cfg.mode == InputMode::Toggle {
                                callback(Event::Release);
                                phase = Phase::Idle;
                                log::info!("再按一次触发键：结束录音");
                            }
                        }
                    },
                    Signal::Up => match phase {
                        Phase::Waiting => {
                            // 没按够长按时间就松手：当作误触，不录音
                            phase = Phase::Idle;
                            deadline = None;
                            log::debug!("长按未满就松手，忽略");
                        }
                        Phase::Recording => {
                            if cfg.mode == InputMode::Hold {
                                callback(Event::Release);
                                phase = Phase::Idle;
                                log::info!("触发键松开：结束录音");
                            }
                        }
                        Phase::Idle => {}
                    },
                    Signal::Stop => {
                        if phase == Phase::Recording {
                            callback(Event::Release);
                            phase = Phase::Idle;
                            log::info!("Esc：结束录音");
                        } else if phase == Phase::Waiting {
                            phase = Phase::Idle;
                            deadline = None;
                        }
                    }
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                if phase == Phase::Waiting {
                    let cfg = config();
                    phase = if callback(Event::Press) {
                        Phase::Recording
                    } else {
                        Phase::Idle
                    };
                    if phase == Phase::Recording {
                        log::info!("长按 {}ms 触发：开始录音", cfg.long_press_ms);
                    }
                }
                deadline = None;
            }
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trigger::VK_RMENU;

    fn config(keys: &[u32], mode: InputMode) -> TriggerConfig {
        TriggerConfig {
            keys: keys.to_vec(),
            mode,
            long_press_ms: 0,
        }
    }

    fn run(cfg: &TriggerConfig, events: &[(u32, bool)], active: bool) -> (Vec<bool>, Vec<Signal>) {
        let mut matcher = Matcher::new();
        let (tx, rx) = channel();
        let swallows = events
            .iter()
            .map(|(vk, down)| matcher.handle(*vk, *down, cfg, active, &tx))
            .collect();
        (swallows, rx.try_iter().collect())
    }

    #[test]
    fn single_key_press_release() {
        let cfg = config(&[VK_RMENU], InputMode::Hold);
        let (swallow, signals) = run(&cfg, &[(VK_RMENU, true), (VK_RMENU, false)], false);
        assert_eq!(swallow, vec![true, true]);
        assert_eq!(signals, vec![Signal::Down, Signal::Up]);
    }

    #[test]
    fn combo_swallows_only_the_primary_key() {
        let cfg = config(&[0xA2, 0xA0, 0x20], InputMode::Hold);
        let (swallow, signals) = run(
            &cfg,
            &[
                (0xA0, true),
                (0xA2, true),
                (0x20, true),
                (0x20, false),
                (0xA2, false),
                (0xA0, false),
            ],
            false,
        );
        assert_eq!(swallow, vec![false, false, true, true, false, false]);
        assert_eq!(signals, vec![Signal::Down, Signal::Up]);
    }

    #[test]
    fn auto_repeat_does_not_retrigger() {
        let cfg = config(&[VK_RMENU], InputMode::Hold);
        let (swallow, signals) = run(
            &cfg,
            &[
                (VK_RMENU, true),
                (VK_RMENU, true),
                (VK_RMENU, true),
                (VK_RMENU, false),
            ],
            false,
        );
        assert_eq!(swallow, vec![true, true, true, true]);
        assert_eq!(signals, vec![Signal::Down, Signal::Up]);
    }

    #[test]
    fn esc_stops_only_toggle_and_only_when_active() {
        let toggle = config(&[VK_RMENU], InputMode::Toggle);
        let (swallow, signals) = run(&toggle, &[(VK_ESCAPE, true), (VK_ESCAPE, false)], true);
        assert_eq!(swallow, vec![true, true]);
        assert_eq!(signals, vec![Signal::Stop]);

        let (swallow, signals) = run(&toggle, &[(VK_ESCAPE, true), (VK_ESCAPE, false)], false);
        assert_eq!(swallow, vec![false, false]);
        assert!(signals.is_empty());

        let hold = config(&[VK_RMENU], InputMode::Hold);
        let (swallow, signals) = run(&hold, &[(VK_ESCAPE, true), (VK_ESCAPE, false)], true);
        assert_eq!(swallow, vec![false, false]);
        assert!(signals.is_empty());
    }

    #[test]
    fn does_not_trigger_when_primary_is_pressed_before_modifiers() {
        let cfg = config(&[0xA2, 0x20], InputMode::Hold);
        let (swallow, signals) = run(
            &cfg,
            &[(0x20, true), (0xA2, true), (0x20, false), (0xA2, false)],
            false,
        );
        assert_eq!(swallow, vec![false, false, false, false]);
        assert!(signals.is_empty());
    }
}
