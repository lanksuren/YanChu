//! 言出 —— 按住说话，落成整理好的文字。
//!
//! 一条链路：热键按下 → 记下落点并开始记录（麦克风常开，带 300ms 预录）
//!   → 说话过程中实时识别、气泡持续更新 → 松手 → 定稿 → 润色 → 写入原输入框。
//!
//! 两个窗口：跟随鼠标的录音指示器 + 屏幕下方的气泡。

mod asr;
mod audio;
mod bench;
mod bench_lat;
mod bubble;
mod devlog;
mod glossary;
mod history;
mod hotkey;
mod insert;
mod llm;
mod model_profiles;
mod pipeline;
mod polish;
mod rules;
mod stream;
mod tray;
mod trigger;
mod ui;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use tauri::{AppHandle, Emitter, Manager};

/// 一次「按住」的生命周期。
#[derive(Default)]
struct Session {
    anchor: Option<insert::Anchor>,
    pressed: bool,
    /// 按下时刻，用来算「按住多久」；开发模式要写进日志
    pressed_at: Option<std::time::Instant>,
    /// 定稿状态机：冻结区 + 草稿区（见 stream.rs）
    stream: stream::Streamer,
    /// 音频侧已经切到哪里（16k 采样点）；文字侧可能还在草稿区里改标点
    committed_end: usize,
    /// 会话代数：换一轮就 +1，旧的电平 / 识别线程看到代数变了必须退出，
    /// 不能把上一轮的识别结果写进新一轮。
    generation: u64,
}

#[derive(Default)]
struct Runtime {
    session: Mutex<Session>,
    transcriber: Mutex<Option<asr::Transcriber>>,
    /// 最近一次落字的最终文本，供托盘的「复制最近一次」用
    last_text: Mutex<Option<String>>,
    /// 常开采集流
    engine: Mutex<Option<audio::AudioEngine>>,
    /// 润色历史（内存里，最多 50 条）
    history: Mutex<history::History>,
    /// 开发模式采集器：每次按住一个，松手时落盘
    devlog: Mutex<Option<devlog::Recorder>>,
    /// 气泡用的「词表 + 规则层」管线。
    /// 每句定稿都要跑一次，不能每次重读词表（还会刷日志）；
    /// 词表保存后置空，下次现读。
    rules: Mutex<Option<pipeline::Pipeline>>,
}

/// 暂停状态下重复按触发键时，最多每 1.5 秒提示一次，免得刷屏。
static PAUSED_NOTICE_AT: Mutex<Option<std::time::Instant>> = Mutex::new(None);

/// 全局热键是否注册成功（启动时 install 成功才会置 true）。
static HOTKEY_OK: AtomicBool = AtomicBool::new(false);

/// 启动预热还剩几件事（识别模型 / 润色模型）。到 0 才把托盘从「加载中」切回待机。
static WARMUPS_LEFT: AtomicUsize = AtomicUsize::new(0);
/// 启动后是否已经发生过一次真实按键会话。发生过就不再执行「把前台还给启动前窗口」，
/// 否则会把用户正在打字的输入框顶掉（表现为「焦点已离开」）。
static SESSION_SEEN: AtomicBool = AtomicBool::new(false);

fn warmup_done(app: &AppHandle) {
    let left = WARMUPS_LEFT.load(Ordering::SeqCst);
    if left == 0 {
        return;
    }
    if WARMUPS_LEFT.fetch_sub(1, Ordering::SeqCst) <= 1 && tray::current_phase() == "loading" {
        tray::set_phase(
            app,
            if tray::dictation_enabled() {
                "idle"
            } else {
                "paused"
            },
        );
    }
}

/// 实时识别的节奏。太快会让 CPU 一直跑满，太慢用户会觉得卡住。
const PARTIAL_INTERVAL: Duration = Duration::from_millis(450);/// 少于这个长度不值得跑一次识别
const PARTIAL_MIN_SAMPLES: usize = 12_800; // 0.8s @16k

/// 一直不停顿时，最多攒这么长的音频就必须切一刀（12s）。
/// 不切的话尾巴越长、每次重识别越慢，最后气泡会彻底跟不上说话。
const MAX_TAIL_SAMPLES: usize = 192_000;
/// 强制切段时优先落在这么长的位置之前（10s）
const FORCED_COMMIT_SAMPLES: usize = 160_000;

pub fn run() {
    let args: Vec<String> = std::env::args().collect();

    // --dev-log 是 YANCHU_DEV_LOG=1 的命令行等价物，方便在快捷方式/脚本里临时打开
    if args.iter().any(|a| a == "--dev-log") {
        std::env::set_var(devlog::ENV_FLAG, "1");
    }

    devlog::init_logging();

    // 维护用：崩溃/被强杀后，手动把本地 Ollama 模型卸掉，释放显存。
    if args.iter().any(|a| a == "--unload-llm") {
        match llm::LlmConfig::load(&asr::model_root()) {
            Some(cfg) => {
                llm::unload(&cfg);
                println!("已请求卸载本地润色模型: {}", cfg.model);
            }
            None => println!("未配置大润色模型，无需卸载"),
        }
        return;
    }

    // 开发日志报告：不开采集也能读已有记录，所以放在「已开启」提示之前
    if let Some(idx) = args.iter().position(|a| a == "--dev-report") {
        let dir = args
            .get(idx + 1)
            .map(std::path::PathBuf::from)
            .unwrap_or_else(devlog::default_dir);
        if let Err(e) = devlog::report(&dir) {
            eprintln!("开发日志报告生成失败: {e}");
            std::process::exit(1);
        }
        return;
    }

    if devlog::enabled() {
        log::warn!(
            "开发模式采集已开启：每次说话的音频与文本会写入 {}（产品版默认关闭）",
            devlog::default_dir().display()
        );
    }

    // 恢复上次的开关，再让环境变量覆盖（开发期开关优先）
    tray::load_state();

    // 启动即切深度档（托盘里也能切）：`YANCHU_DEEP=1`
    if matches!(
        std::env::var("YANCHU_DEEP")
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
            .as_str(),
        "1" | "on" | "true"
    ) {
        tray::set_deep(true);
    }

    // 建窗口之前先记住当前前台窗口：Tauri 创建（哪怕是隐藏的）窗口时，
    // Windows 会把新窗口设成激活，用户正在打的字就被抢走了。
    let foreground_before_start = foreground_window();

    if let Some(idx) = args.iter().position(|a| a == "--bench") {
        let dir = args
            .get(idx + 1)
            .cloned()
            .unwrap_or_else(|| "bench/corpus".to_string());
        if let Err(e) = bench::run(std::path::Path::new(&dir)) {
            eprintln!("验收失败: {e}");
            std::process::exit(1);
        }
        return;
    }

    // 延迟拆分：encoder 成本 vs decoder 成本，见 bench_lat.rs 的注释
    if let Some(idx) = args.iter().position(|a| a == "--bench-lat") {
        let path = args
            .get(idx + 1)
            .cloned()
            .unwrap_or_else(|| "bench/corpus/03.wav".to_string());
        if let Err(e) = bench_lat::run(std::path::Path::new(&path)) {
            eprintln!("延迟测量失败: {e}");
            std::process::exit(1);
        }
        return;
    }

    if args.iter().any(|a| a == "--models-json") {
        println!(
            "{}",
            serde_json::to_string_pretty(&models_json()).unwrap_or_default()
        );
        return;
    }

    if args.iter().any(|a| a == "--llm-ping") {
        let root = asr::model_root();
        let Some(cfg) = llm::LlmConfig::load(&root) else {
            eprintln!("✗ 没有配置润色模型（models/llm.json）");
            std::process::exit(1);
        };
        println!("{} / {} @ {}", cfg.provider.key(), cfg.model, cfg.url);
        match cfg.ping() {
            Ok((reply, ms)) => println!("✓ {ms}ms，回复：{reply}"),
            Err(e) => {
                eprintln!("✗ {e}");
                std::process::exit(1);
            }
        }
        return;
    }

    if let Some(idx) = args.iter().position(|a| a == "--set-asr") {
        let engine = args.get(idx + 1).cloned().unwrap_or_default();
        let cfg = asr::AsrConfig {
            engine,
            model_dir: None,
            threads: None,
        }
        .normalized();
        if !cfg.available() {
            eprintln!(
                "✗ {} 模型文件不完整: {}",
                cfg.label(),
                cfg.resolved_dir().display()
            );
            std::process::exit(1);
        }
        let started = std::time::Instant::now();
        match asr::Transcriber::load(&cfg) {
            Ok(model) => {
                drop(model);
                if let Err(e) = cfg.save(&asr::model_root()) {
                    eprintln!("✗ 保存识别模型配置失败: {e}");
                    std::process::exit(1);
                }
                println!(
                    "✓ 识别模型已切换为 {}，加载 {:?}",
                    cfg.label(),
                    started.elapsed()
                );
            }
            Err(e) => {
                eprintln!("✗ {e}");
                std::process::exit(1);
            }
        }
        return;
    }

    if let Some(idx) = args.iter().position(|a| a == "--polish-test") {
        let text = args.get(idx + 1).cloned().unwrap_or_default();
        // 和真实落字保持一致：长文自动切深度档（--standard 可以强制标准档做对照）
        let force_standard = args.iter().any(|a| a == "--standard");
        let explicit_deep = args.iter().any(|a| a == "--deep");
        let auto_deep = !force_standard
            && !explicit_deep
            && llm::auto_deep_enabled()
            && llm::should_auto_deep(text.chars().count());
        let depth = if force_standard {
            llm::Depth::Standard
        } else if explicit_deep || auto_deep {
            llm::Depth::Deep
        } else {
            llm::Depth::Standard
        };
        let pipeline = pipeline::Pipeline::load();
        let stages = pipeline.run_with(&text, depth);
        println!("输入    : {text}");
        println!("规则层  : {}", stages.ruled);
        println!("最终    : {}", stages.final_text);
        println!(
            "大润色  : {}（{}档{}，{}ms，{}）",
            stages.used_llm,
            stages.depth.label(),
            if auto_deep { "，长文自动" } else { "" },
            stages.llm.ms(),
            match stages.llm.reason.clone() {
                Some(reason) => reason,
                None if stages.used_llm => "已采用".to_string(),
                None => "未配置或文本太短".to_string(),
            }
        );
        if !stages.llm.warnings.is_empty() {
            println!("事实项  : {:?}（只记录，不拦截）", stages.llm.warnings);
        }
        println!(
            "耗时    : 规则 {}ms / 合计 {}ms",
            stages.ms_rule, stages.ms_total
        );
        return;
    }
    if let Some(idx) = args.iter().position(|a| a == "--replay") {
        let corpus = args.get(idx + 1).cloned().unwrap_or_default();
        let out = args
            .iter()
            .position(|a| a == "--out")
            .and_then(|i| args.get(i + 1))
            .cloned();
        let deep = args.iter().any(|a| a == "--deep");
        if let Err(e) = run_replay(
            std::path::Path::new(&corpus),
            out.as_deref().map(std::path::Path::new),
            deep,
        ) {
            eprintln!("回放失败: {e}");
            std::process::exit(1);
        }
        return;
    }

    if let Some(idx) = args.iter().position(|a| a == "--stream-test") {
        let path = args
            .get(idx + 1)
            .cloned()
            .unwrap_or_else(|| "bench/corpus/01.wav".to_string());
        if let Err(e) = run_stream_test(std::path::Path::new(&path)) {
            eprintln!("流式自检失败: {e}");
            std::process::exit(1);
        }
        return;
    }
    if let Some(idx) = args.iter().position(|a| a == "--mic-check") {
        let secs: u64 = args.get(idx + 1).and_then(|s| s.parse().ok()).unwrap_or(3);
        run_mic_check(secs);
        return;
    }

    if let Some(idx) = args.iter().position(|a| a == "--mem-check") {
        let secs: u64 = args.get(idx + 1).and_then(|s| s.parse().ok()).unwrap_or(20);
        run_mem_check(secs);
        return;
    }

    tauri::Builder::default()
        // 第二次启动只唤出已有实例的面板，避免两套键盘钩子/两个托盘
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            tray::open_panel(app);
        }))
        .plugin(tauri_plugin_notification::init())
        .invoke_handler(tauri::generate_handler![
            set_dictation,
            set_polish,
            set_deep,
            get_state,
            get_diagnostics,
            retry_audio,
            get_flags,
            get_logs,
            clear_logs,
            quit_app,
            get_ui,
            save_ui,
            get_trigger,
            check_trigger,
            save_trigger,
            set_capture_mode,
            get_models,
            switch_asr,
            save_polish_profiles,
            test_polish_profile,
            list_ollama_models,
            get_history,
            clear_history,
            get_glossary,
            save_glossary,
            get_rules,
            save_rules,
            run_polish,
            get_llm_status
        ])
        // 控制面板的关闭 = 收进托盘：只 hide，不销毁。
        // 销毁后 get_webview_window("settings") 返回 None，托盘的「控制面板…」就再也开不出来。
        .on_window_event(|window, event| {
            if window.label() != "settings" {
                return;
            }
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                // 面板里可能还停在「录快捷键」状态：关掉面板必须恢复触发，
                // 否则用户会以为快捷键坏了。
                crate::hotkey::set_suspended(false);
                let _ = window.hide();
                log::info!("控制面板已收进托盘");
            }
        })
        .setup(move |app| {
            app.manage(Runtime::default());

            let handle = app.handle().clone();
            bubble::init(&handle)?;
            // 气泡外观按用户上次调好的来（窗口尺寸 + 字号）
            apply_bubble_ui(&handle, ui::BubbleUi::load(&asr::model_root()));

            // 采集流一开始就打开：按下去才开麦会丢掉开头 100–300ms
            match audio::AudioEngine::start() {
                Ok(engine) => {
                    let rt = handle.state::<Runtime>();
                    *rt.engine.lock().unwrap() = Some(engine);
                }
                Err(e) => {
                    log::error!("麦克风打不开: {e}（录音功能不可用）");
                    notify(
                        &handle,
                        "麦克风打不开",
                        "语音输入暂时不可用。插好麦克风后到控制面板点「重试」。",
                    );
                }
            }

            let trigger_cfg = trigger::TriggerConfig::load(&asr::model_root());
            log::info!(
                "触发键：{}（{}，长按 {}ms）",
                trigger_cfg.display(),
                trigger_cfg.mode.label(),
                trigger_cfg.long_press_ms
            );
            let hotkey_handle = app.handle().clone();
            hotkey::install(
                trigger_cfg,
                Box::new(move |ev| match ev {
                    hotkey::Event::Press => on_press(&hotkey_handle),
                    hotkey::Event::Release => {
                        on_release(&hotkey_handle);
                        true
                    }
                }),
            )?;
            HOTKEY_OK.store(true, Ordering::Relaxed);

            log::info!("言出已启动 READY：{}", hotkey::config().trigger_hint());

            // 隐藏窗口不该占着前台：把焦点还给启动前那个窗口（YC-003）
            restore_foreground(foreground_before_start);
            std::thread::spawn(move || {
                // WebView2 初始化完可能再抢一次，隔一会儿补一刀。
                // 但用户已经开始说话/刚落完字时绝不能补——那会把输入框顶掉，
                // 表现为「焦点已离开，已复制到剪贴板」。
                std::thread::sleep(Duration::from_millis(1500));
                if SESSION_SEEN.load(Ordering::Relaxed) {
                    log::info!("启动后已有按键会话，跳过延迟前台恢复");
                    return;
                }
                restore_foreground(foreground_before_start);
            });

            // 模型预热期间托盘显示琥珀色「正在加载模型…」，两个预热都结束再回待机。
            let llm_configured = llm::LlmConfig::load(&asr::model_root()).is_some();
            WARMUPS_LEFT.store(if llm_configured { 2 } else { 1 }, Ordering::SeqCst);
            tray::build(&app.handle().clone())?;

            // 预热期间托盘图标两帧轮换，像在闪「加载中」
            let pulse = app.handle().clone();
            std::thread::spawn(move || {
                while WARMUPS_LEFT.load(Ordering::SeqCst) > 0 {
                    std::thread::sleep(Duration::from_millis(420));
                    tray::pulse_loading(&pulse);
                }
            });

            // 兜底：预热线程卡住/异常时，15 秒后也要结束「加载中」
            let watchdog = app.handle().clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_secs(15));
                if WARMUPS_LEFT.swap(0, Ordering::SeqCst) > 0
                    && tray::current_phase() == "loading"
                {
                    log::warn!("模型预热超时，托盘结束加载态");
                    tray::set_phase(&watchdog, "idle");
                }
            });

            // 调试/截图用：设了 YANCHU_OPEN_PANEL 就自动打开控制面板
            if std::env::var("YANCHU_OPEN_PANEL").is_ok() {
                tray::open_panel(app.handle());
            }

            // 调试 / 验收：YANCHU_THEME=dark|light 强制三个窗口的主题，
            // 不用改系统设置也能验证深色样式。
            if let Ok(v) = std::env::var("YANCHU_THEME") {
                let theme = match v.trim().to_ascii_lowercase().as_str() {
                    "dark" => Some(tauri::Theme::Dark),
                    "light" => Some(tauri::Theme::Light),
                    _ => None,
                };
                if let Some(theme) = theme {
                    for label in ["settings", "bubble", "indicator"] {
                        if let Some(win) = app.get_webview_window(label) {
                            let _ = win.set_theme(Some(theme));
                        }
                    }
                    log::info!("强制窗口主题: {v}");
                }
            }

            // 开发/支持用：YANCHU_NOTIFY_TEST=1 在启动 3 秒后弹一条系统通知，
            // 用来确认通知链路是通的（不参与产品逻辑）。
            if std::env::var("YANCHU_NOTIFY_TEST").is_ok() {
                let h = app.handle().clone();
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_secs(3));
                    notify(&h, "言出测试通知", "如果你看到这条，系统通知是通的。");
                });
            }

            // 第一次运行：把面板打开一次，告诉用户它装好了（之后不再打扰）
            let first_run = asr::model_root().join(".first-run-done");
            if !first_run.exists() && std::env::var("YANCHU_OPEN_PANEL").is_err() {
                tray::open_panel(app.handle());
                let _ = std::fs::write(&first_run, b"1");
                log::info!("首次运行：已打开控制面板");
            }

            // 预热识别模型，避免第一句话多等约 800ms
            let asr_cfg = asr::AsrConfig::load(&asr::model_root());
            log::info!(
                "识别模型：{}（{}）",
                asr_cfg.label(),
                asr_cfg.resolved_dir().display()
            );
            let warm = app.handle().clone();
            std::thread::spawn(move || {
                let rt = warm.state::<Runtime>();
                let mut slot = rt.transcriber.lock().unwrap();
                if slot.is_none() {
                    let started = std::time::Instant::now();
                    match asr::Transcriber::load(&asr_cfg) {
                        Ok(t) => {
                            *slot = Some(t);
                            log::info!("ASR_READY 模型预热完成，用时 {:?}", started.elapsed());
                        }
                        Err(e) => log::warn!("模型预热失败（首句会现加载）: {e}"),
                    }
                }
                drop(slot);
                warmup_done(&warm);
            });

            // 预热润色模型：冷加载要 2–3 秒，不预热的话第一句话必然超时
            let warm_llm = app.handle().clone();
            std::thread::spawn(move || {
                if let Some(cfg) = llm::LlmConfig::load(&asr::model_root()) {
                    llm::warmup(&cfg);
                    warmup_done(&warm_llm);
                }
            });

            // 运行期间常驻：每 3 小时续一次 keep_alive（8h），避免长闲置被 Ollama 卸掉。
            // 退出由 shutdown() 主动卸载。
            if let Some(cfg) = llm::LlmConfig::load(&asr::model_root()) {
                if cfg.is_local() {
                    std::thread::spawn(move || loop {
                        std::thread::sleep(Duration::from_secs(3 * 60 * 60));
                        llm::keepalive(&cfg);
                    });
                }
            }

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("言出启动失败");
}

/// 退出：先把本地润色模型从 Ollama 卸掉，再结束进程。
/// 运行期间不卸载——模型常驻是产品行为。
pub(crate) fn shutdown(app: &AppHandle) {
    if let Some(cfg) = llm::LlmConfig::load(&asr::model_root()) {
        llm::unload(&cfg);
    }
    app.exit(0);
}

/// 暂停状态下按触发键：给一次 1.1 秒的提示，1.5 秒内不重复。
fn paused_notice(app: &AppHandle) {
    let now = std::time::Instant::now();
    {
        let mut last = PAUSED_NOTICE_AT.lock().unwrap();
        if let Some(t) = *last {
            if now.duration_since(t) < Duration::from_millis(1500) {
                return;
            }
        }
        *last = Some(now);
    }
    bubble::notice(
        app,
        "error",
        "听写已暂停 · 托盘里可恢复",
        Duration::from_millis(2200),
    );
    log::info!("听写已暂停：按键提示已发出");
}

/// 系统通知：气泡会消失，阻断级失败要让用户之后还能看到。
fn notify(app: &AppHandle, title: &str, body: &str) {
    use tauri_plugin_notification::NotificationExt;
    if let Err(e) = app
        .notification()
        .builder()
        .title(title)
        .body(body)
        .show()
    {
        log::warn!("系统通知发送失败: {e}");
    }
}

/// 钩子回调里只做极轻的事 —— 立刻返回，否则整个系统的键盘都会被拖住。
///
/// 返回 true 表示这一轮录音真的开始了；钩子侧据此决定要不要进入 Recording。
fn on_press(app: &AppHandle) -> bool {
    // 托盘里关掉听写之后，热键照样会被钩到。不能「什么都不做」：
    // 用户分不清「暂停」和「坏了」，所以在别的线程给一次轻提示。
    if !tray::dictation_enabled() {
        log::info!("触发键：听写已暂停，准备给提示");
        let app = app.clone();
        std::thread::spawn(move || paused_notice(&app));
        return false;
    }

    let rt = app.state::<Runtime>();
    SESSION_SEEN.store(true, Ordering::Relaxed);

    // 先抓落点：抓不到就不算开始，避免 session.pressed 卡在 true。
    let Some(anchor) = insert::Anchor::capture() else {
        log::warn!("没抓到前台窗口，这次不录");
        return false;
    };

    let generation = {
        let mut session = rt.session.lock().unwrap();
        if session.pressed {
            return true; // 已经有一轮在录（理论上不会走到这里）
        }
        session.generation = session.generation.wrapping_add(1);
        session.pressed = true;
        session.pressed_at = Some(std::time::Instant::now());
        session.anchor = Some(anchor);
        session.stream = stream::Streamer::new();
        session.committed_end = 0;
        session.generation
    };
    hotkey::set_active(true);

    // 开发模式：这一次按住的整段音频、每句切片、全链路文本都会落盘。
    // 建目录要碰磁盘，所以放到独立线程上 —— 键盘钩子回调里不能做这种活。
    if devlog::enabled() {
        let target = devlog::capture_target();
        let devlog_app = app.clone();
        std::thread::spawn(move || {
            let recorder = devlog::Recorder::start_with(target);
            if let Some(rec) = recorder.as_ref() {
                log::info!(
                    "DEVLOG 采集开始 {}（目标 {}）→ {}",
                    rec.id(),
                    rec.target_summary(),
                    rec.dir().display()
                );
            }
            let rt = devlog_app.state::<Runtime>();
            let still_same = rt
                .session
                .lock()
                .map(|s| s.pressed && s.generation == generation)
                .unwrap_or(false);
            if still_same {
                *rt.devlog.lock().unwrap() = recorder;
            }
        });
    }

    // wav 冒充模式（端到端测试）：不开麦，把 wav 按真实节奏喂进采集缓冲，
    // 这样实时识别、电平、气泡更新的路径都能被自动验证。
    let mock = std::env::var("YANCHU_WAV_INPUT").ok();
    if let Some(engine) = rt.engine.lock().unwrap().as_ref() {
        engine.set_mock(mock.is_some());
    }

    if let Some(path) = mock {
        log::info!("wav 冒充麦克风: {path}");
        let feeder_app = app.clone();
        std::thread::spawn(move || feed_wav(feeder_app, PathBuf::from(path), generation));
    }

    if let Some(engine) = rt.engine.lock().unwrap().as_ref() {
        engine.arm();
    }

    bubble::start_session(app, &anchor);

    // 电平推送：让指示器能「跟着声音呼吸」
    let level_app = app.clone();
    std::thread::spawn(move || level_loop(level_app, generation));

    // 实时识别：说话过程中气泡持续更新
    let stream_app = app.clone();
    std::thread::spawn(move || streaming_loop(stream_app, generation));
    true
}

fn on_release(app: &AppHandle) {
    let rt = app.state::<Runtime>();
    hotkey::set_active(false);
    let (anchor, press_to_release_ms, streamer, committed_end) = {
        let mut session = rt.session.lock().unwrap();
        if !session.pressed {
            return;
        }
        session.pressed = false;
        let held = session
            .pressed_at
            .take()
            .map(|t| t.elapsed().as_millis() as u64)
            .unwrap_or(0);
        (
            session.anchor.take(),
            held,
            // 把这一轮的状态机整个取出来，别留给下一轮覆盖：
            // 用户完全可能刚落完字就马上开始下一次。
            std::mem::take(&mut session.stream),
            session.committed_end,
        )
    };

    let Some(anchor) = anchor else {
        return;
    };

    bubble::set_level(app, "polishing", 0.0);
    let display = streamer.display();
    if !display.is_empty() {
        bubble::polishing(app, &display);
    }

    let samples = rt
        .engine
        .lock()
        .unwrap()
        .as_ref()
        .map(|e| e.stop())
        .unwrap_or_default();

    let app = app.clone();
    std::thread::spawn(move || {
        finish(
            &app,
            samples,
            anchor,
            press_to_release_ms,
            streamer,
            committed_end,
        )
    });
}

/// 开发模式下取当前采集器，做点事。没开采集时开销只有一次锁。
fn with_recorder(app: &AppHandle, f: impl FnOnce(&devlog::Recorder)) {
    if !devlog::enabled() {
        return;
    }
    let rt = app.state::<Runtime>();
    let guard = rt.devlog.lock().unwrap();
    if let Some(rec) = guard.as_ref() {
        f(rec);
    }
}

/// 记一次识别调用：耗时、采样范围、结果。实时预览和定稿分别标 kind。
fn note_asr(app: &AppHandle, kind: &str, from: usize, to: usize, ms: u64, text: &str) {
    with_recorder(app, |rec| rec.note_asr(kind, from, to, ms, text));
}

/// 气泡用的小润色：词表 + 规则层，不碰大润色。
///
/// 管线缓存在 Runtime 里，避免每句都重读词表（重读还会把「热词表 N 条」刷进日志）。
fn polish_sentence(app: &AppHandle, raw: &str) -> (String, Vec<(String, String)>) {
    let rt = app.state::<Runtime>();
    let mut slot = rt.rules.lock().unwrap();
    if slot.is_none() {
        *slot = Some(pipeline::Pipeline::load_rules());
    }
    let stages = slot.as_ref().expect("刚放进去的").rule_only(raw);
    (stages.ruled, stages.fixes)
}

/// 把 wav 按 100ms 一片、实时节奏喂进采集缓冲，模拟真人说话。
fn feed_wav(app: AppHandle, path: PathBuf, generation: u64) {
    let rt = app.state::<Runtime>();
    let samples = match audio::read_wav_16k(&path) {
        Ok(s) => s,
        Err(e) => {
            log::error!("读 wav 失败: {e}");
            return;
        }
    };

    const CHUNK: usize = 1_600; // 100ms @16k
    for chunk in samples.chunks(CHUNK) {
        let current = rt
            .session
            .lock()
            .map(|s| s.pressed && s.generation == generation)
            .unwrap_or(false);
        if !current {
            return;
        }
        if let Some(engine) = rt.engine.lock().unwrap().as_ref() {
            engine.inject(chunk);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// 60ms 一次把电平推给指示器窗口。
fn level_loop(app: AppHandle, generation: u64) {
    let rt = app.state::<Runtime>();
    loop {
        std::thread::sleep(Duration::from_millis(60));

        let current = rt
            .session
            .lock()
            .map(|s| s.pressed && s.generation == generation)
            .unwrap_or(false);
        if !current {
            break;
        }

        let engine = rt.engine.lock().unwrap();
        let level = engine.as_ref().map(|e| e.level()).unwrap_or(0.0);
        drop(engine);

        // 阈值取 0.01：正常说话 0.03–0.2，环境噪声通常 <0.005
        let state = if level > 0.01 { "speaking" } else { "silent" };
        bubble::set_level(&app, state, level);
    }
}

/// 是否该把前面这段定稿了。
///
/// 判据是「说了至少 0.5 秒，然后出现 300ms 以上的静音」——
/// 那基本就是一句话说完了。定稿之后这段文字不再重算，
/// 屏幕上已说完的句子就不会跟着后面的识别一直变。
fn find_commit_point(samples: &[f32], from: usize) -> Option<usize> {
    const FRAME: usize = 1_600; // 100ms @16k
    const SILENCE_RMS: f32 = 0.008;
    // 500ms。真人句间停顿通常 300–800ms，词间空隙 50–200ms；
    // 定太短会把句子从中间切断，屏幕上就会出现「帮我生成一个。」这种碎片。
    const NEED_SILENT_FRAMES: usize = 5;
    const MIN_SPEECH: usize = 24_000; // 1.5s：太短就切会把句子切成碎片

    let mut i = from;
    let mut silence_run = 0usize;
    let mut silence_start = 0usize;

    while i + FRAME <= samples.len() {
        let frame = &samples[i..i + FRAME];
        let rms = (frame.iter().map(|s| s * s).sum::<f32>() / FRAME as f32).sqrt();

        if rms < SILENCE_RMS {
            if silence_run == 0 {
                silence_start = i;
            }
            silence_run += 1;

            // 每段静音只在满 300ms 的那一刻判定一次。
            // 不这样的话，一段 5 秒的静音会被反复判定，
            // 在中间的任意位置「凑」出一个假切点。
            if silence_run == NEED_SILENT_FRAMES && silence_start >= from + MIN_SPEECH {
                // 返回静音的「末尾」而不是开头：否则下一拍会从静音里重新开始，
                // 把同一段停顿再切一次，切出一个只有标点的空片段。
                return Some(i + FRAME);
            }
        } else {
            silence_run = 0;
        }

        i += FRAME;
    }
    None
}

/// 当前这一轮还在进行，且没有换成下一轮。
fn session_is_current(rt: &Runtime, generation: u64) -> bool {
    rt.session
        .lock()
        .map(|s| s.pressed && s.generation == generation)
        .unwrap_or(false)
}

/// 说话过程中每隔 450ms 跑一次识别。
///
/// 策略：**已经说完的句子定稿后不再重算**，只有尾巴在变。
/// 否则后面每说一句，前面所有字都会跟着重排，屏幕上整段都在跳。
fn streaming_loop(app: AppHandle, generation: u64) {
    let rt = app.state::<Runtime>();

    loop {
        std::thread::sleep(PARTIAL_INTERVAL);

        if !session_is_current(&rt, generation) {
            break;
        }

        let samples = {
            let engine = rt.engine.lock().unwrap();
            match engine.as_ref() {
                Some(e) => e.snapshot_16k(),
                None => continue,
            }
        };

        if samples.len() < PARTIAL_MIN_SAMPLES {
            continue;
        }

        let committed_end = {
            let session = rt.session.lock().unwrap();
            if !session.pressed || session.generation != generation {
                break;
            }
            session.committed_end
        };

        // 1. 碰到句间停顿就把前面这段定稿。
        //    如果一直不停顿，尾巴会无限长大：每 450ms 都要重识别整段，
        //    越说越慢，最后气泡完全跟不上。所以超过 12 秒就强制找一个低能量点切一刀。
        let point = find_commit_point(&samples, committed_end).or_else(|| {
            if samples.len() > committed_end + MAX_TAIL_SAMPLES {
                Some(forced_commit_point(&samples, committed_end))
            } else {
                None
            }
        });

        if let Some(point) = point {
            log::info!("可定稿点: {committed_end}..{point}");
            let started = std::time::Instant::now();
            let result = transcribe(&app, trim_trailing_silence(&samples[committed_end..point]));
            let asr_ms = started.elapsed().as_millis() as u64;
            let joined = match &result {
                Ok(v) => v.join(""),
                Err(e) => format!("<识别失败: {e}>"),
            };
            if !session_is_current(&rt, generation) {
                break;
            }
            note_asr(&app, "commit", committed_end, point, asr_ms, &joined);

            if let Ok(text) = result {
                let text = text.join("").trim().to_string();
                // 只有标点的片段（纯静音被识别成「。」）不算一句话
                if has_content(&text) {
                    // 句子一闭合就过「热词 + 规则」：气泡里看到的就是润色后的样子，
                    // 而不是一堆「嗯、那个」等落字时才变干净。
                    let (polished, fixes) = if tray::polish_enabled() {
                        polish_sentence(&app, &text)
                    } else {
                        (text.clone(), Vec::new())
                    };
                    if !fixes.is_empty() {
                        log::info!("气泡润色 {:?}", fixes);
                    }

                    let mut session = rt.session.lock().unwrap();
                    if !session.pressed || session.generation != generation {
                        break;
                    }
                    // 交给状态机：不补句号，只放一个临时逗号，等右文再判
                    session.stream.push_segment(&text, &polished);
                    session.committed_end = point;
                    let display = session.stream.display();
                    drop(session);

                    if polished == text {
                        log::info!("定稿: {text}");
                    } else {
                        log::info!("定稿: {text} → {polished}");
                    }

                    // 这一句的音频切片单独存一份，后面可以脱离整段单独重跑
                    with_recorder(&app, |rec| {
                        rec.push_segment(committed_end, point, &samples[committed_end..point], &text, asr_ms)
                    });

                    // 冻结可能刚发生，立刻刷一帧
                    bubble::partial_parts(&app, &display, "");
                } else {
                    log::debug!("跳过纯标点片段: {text:?}");
                }
            }
        }

        // 2. 尾巴继续试识别
        let tail_from = {
            let session = rt.session.lock().unwrap();
            if !session.pressed || session.generation != generation {
                break;
            }
            session.committed_end
        };

        let tail = if samples.len() > tail_from + 3_200 {
            let started = std::time::Instant::now();
            let result = transcribe(&app, &samples[tail_from..]);
            let asr_ms = started.elapsed().as_millis() as u64;
            let joined = match &result {
                Ok(v) => v.join(""),
                Err(e) => format!("<识别失败: {e}>"),
            };
            if !session_is_current(&rt, generation) {
                break;
            }
            note_asr(&app, "partial", tail_from, samples.len(), asr_ms, &joined);
            result.map(|v| v.join("")).unwrap_or_default()
        } else {
            String::new()
        };

        let tail = tail.trim();
        // 尾巴只剩标点时（句间停顿会被识别成一个「。」）不往气泡里放，
        // 否则屏幕上会短暂出现「……问题。。」这种双句号
        let tail = if has_content(tail) { tail } else { "" };

        // 灰字也过一遍规则层：去掉「嗯/呃」这类，用户不用等落字才看见干净文本
        let tail = if tail.is_empty() || !tray::polish_enabled() {
            tail.to_string()
        } else {
            polish::rule_polish_partial(tail)
        };

        // 右文到了：把挂起的断点判掉（这一步可能把临时逗号换成句号）
        let display = {
            let mut session = rt.session.lock().unwrap();
            if !session.pressed || session.generation != generation {
                break;
            }
            if !tail.is_empty() {
                // 停顿长度从音频里量：断点时静音已经满 500ms，剩下的继续数
                let pause_ms = 500 + silence_after(&samples, tail_from);
                session.stream.see_tail(&tail, pause_ms);
            }
            session.stream.display()
        };

        if !display.is_empty() || !tail.is_empty() {
            bubble::partial_parts(&app, &display, &tail);
        }
    }
}

fn finish(
    app: &AppHandle,
    samples: Vec<f32>,
    anchor: insert::Anchor,
    press_to_release_ms: u64,
    mut streamer: stream::Streamer,
    committed_end: usize,
) {
    let started = std::time::Instant::now();

    // 采集器在这里取走：这次按住到此结束，后面无论从哪条路返回都要落盘
    let recorder = app
        .state::<Runtime>()
        .devlog
        .lock()
        .ok()
        .and_then(|mut slot| slot.take());

    // 提前退出（录音太短 / 识别失败 / 没听清）也要留证据，
    // 否则「这一句为什么没落字」永远查不出来
    let bail = |reason: &str| {
        if let Some(rec) = &recorder {
            rec.finish(
                &samples,
                devlog::Finish {
                    press_to_release_ms,
                    release_to_text_ms: started.elapsed().as_millis() as u64,
                    aborted: Some(reason.to_string()),
                    ..Default::default()
                },
            );
        }
    };

    // 采集器已经取出来了，这里不能再走 Runtime 里的那一份
    let note = |kind: &str, from: usize, to: usize, ms: u64, text: &str| {
        if let Some(rec) = &recorder {
            rec.note_asr(kind, from, to, ms, text);
        }
    };

    // 少于 150ms 的音频不可能是有效说话，直接丢弃，省一次模型推理
    if samples.len() < 2_400 {
        log::info!("录音太短（{} 采样），忽略", samples.len());
        bubble::notice(
            app,
            "error",
            &hotkey::config().start_prompt(),
            Duration::from_millis(1200),
        );
        bail("录音太短");
        return;
    }

    log::info!(
        "收到 {} 个采样（{:.1}s），开始定稿",
        samples.len(),
        samples.len() as f32 / 16_000.0
    );

    let mut tail = String::new();
    let mut asr_error: Option<String> = None;

    let from = if samples.len() > committed_end + 3_200 {
        Some(committed_end)
    } else if streamer.display().is_empty() {
        Some(0)
    } else {
        None
    };

    if let Some(from) = from {
        let t = std::time::Instant::now();
        let result = transcribe(app, trim_trailing_silence(&samples[from..]));
        let asr_ms = t.elapsed().as_millis() as u64;
        match result {
            Ok(v) => {
                tail = v.join("");
                note("final", from, samples.len(), asr_ms, &tail);
            }
            Err(e) => {
                note(
                    "final",
                    from,
                    samples.len(),
                    asr_ms,
                    &format!("<识别失败: {e}>"),
                );
                asr_error = Some(e.to_string());
            }
        }
    }

    if let Some(e) = &asr_error {
        log::error!("识别失败: {e}");
        bubble::notice(
            app,
            "error",
            &format!("识别失败：{e}"),
            Duration::from_secs(2),
        );
        bail(&format!("识别失败: {e}"));
        return;
    }

    // 尾巴可能只是静音被识别成的一个「。」——那不算内容，
    // 留着会让「气泡最后一帧」和「原文」都多一个孤零零的句号
    if !has_content(tail.trim()) {
        tail.clear();
    }

    // 最后一段也要进状态机：它会先把上一个挂起的断点判掉（这次有右文了）
    if !tail.is_empty() {
        let tail_text = tail.trim().to_string();
        let polished_tail = if tray::polish_enabled() {
            polish_sentence(app, &tail_text).0
        } else {
            tail_text.clone()
        };
        streamer.push_segment(&tail_text, &polished_tail);
    }

    // 大润色的输入是「气泡最后一帧」：冻结区 + 草稿区（终点标点在这里补齐）
    let text = streamer.finish();
    // 识别原文单独留一份，开发日志与三段对比要用
    let raw_text = streamer.raw().to_string();
    let breaks = streamer.breaks.clone();

    // 纯标点不算一句话：静音段常被识别成一个「。」，
    // 放过去就会往输入框里落一个孤零零的句号
    if !has_content(&text) {
        bubble::notice(app, "error", "没听清", Duration::from_millis(1200));
        bail("没听清");
        return;
    }

    // 档位：托盘里手动开的深度档优先；没开时，长文自动切深度档。
    // 但深度档**只对云端 API 开放** —— 本地 2B 只有一套提示词（见 llm.rs）。
    let text_chars = text.chars().count();
    let deep_ok = llm::deep_available();
    let forced_deep = deep_ok && tray::deep_enabled();
    let auto_deep =
        deep_ok && !forced_deep && llm::auto_deep_enabled() && llm::should_auto_deep(text_chars);
    let depth = if forced_deep || auto_deep {
        llm::Depth::Deep
    } else {
        llm::Depth::Standard
    };
    if auto_deep {
        log::info!(
            "长文自动切深度档（{text_chars} 字 ≥ {} 字），预计多等 3–6 秒",
            llm::AUTO_DEEP_CHARS
        );
    }

    // 深度档要等，先让用户看见「在处理」—— 文案按预计时长分开写：
    // 手动切档是一两秒，长文自动切档要 5 秒上下，体感完全不是一回事
    if depth == llm::Depth::Deep && tray::polish_enabled() {
        let hint = if auto_deep {
            "长文整理中，大约 5–8 秒…"
        } else {
            "深度整理中，可以等一两秒…"
        };
        bubble::deep_polishing(app, &text, hint);
    }

    let polish_started = std::time::Instant::now();
    let stages = if tray::polish_enabled() {
        Some(pipeline::Pipeline::load().run_with(&text, depth))
    } else {
        None
    };
    let polish_ms = polish_started.elapsed().as_millis() as u64;

    let polished = stages
        .as_ref()
        .map(|s| s.final_text.clone())
        .unwrap_or_else(|| text.clone());
    let ruled = stages
        .as_ref()
        .map(|s| s.ruled.clone())
        .unwrap_or_else(|| text.clone());
    let corrected = stages
        .as_ref()
        .map(|s| s.corrected.clone())
        .unwrap_or_else(|| text.clone());
    let used_llm = stages.as_ref().map(|s| s.used_llm).unwrap_or(false);
    let fixes = stages.as_ref().map(|s| s.fixes.clone()).unwrap_or_default();

    // 大润色为什么没用上，是排查「感觉没润色」的第一现场
    match stages.as_ref().map(|s| &s.llm) {
        Some(attempt) if attempt.outcome() == "failed" => {
            log::warn!(
                "大润色回退（{}档）：{}",
                depth.label(),
                attempt.reason.clone().unwrap_or_default()
            );
            // 深度档用户等了几秒，回退必须让他在气泡消失后还能看到
            if depth == llm::Depth::Deep {
                notify(app, "深度整理没完成", "已经用标准结果落字，可以稍后再试。");
            }
        }
        Some(attempt) if attempt.outcome() == "used" => {
            log::info!("大润色生效（{}档）：{}ms", depth.label(), attempt.ms())
        }
        _ => {}
    }

    if !fixes.is_empty() {
        log::info!("热词纠正 {:?}", fixes);
    }

    // 先把气泡收掉，再把文字落进输入框：
    // 视觉顺序变成「润色结束 → 气泡退场 → 字出现在输入框」，不会出现气泡还挡着字的情况。
    bubble::fade_out_before_insert(app);

    let insert_started = std::time::Instant::now();
    let outcome = insert::insert_text(&polished, &anchor);
    let insert_ms = insert_started.elapsed().as_millis() as u64;
    let release_to_text_ms = started.elapsed().as_millis() as u64;

    // 记一笔历史，控制面板里能看「识别 → 小润色 → 最终」的逐字差异
    {
        let rt = app.state::<Runtime>();
        rt.history.lock().unwrap().push(
            raw_text.clone(),
            corrected.clone(),
            ruled.clone(),
            polished.clone(),
            used_llm,
            depth.label().to_string(),
            release_to_text_ms,
        );
    }

    log::info!(
        "落字完成: {:?}，写入 {} 字，用时 {:?}",
        outcome.status,
        outcome.chars_inserted,
        started.elapsed()
    );

    // 开发模式：这一次按住的完整记录（含被拒的模型输出与失败原因）
    if let Some(rec) = &recorder {
        rec.finish(
            &samples,
            devlog::Finish {
                press_to_release_ms,
                release_to_text_ms,
                raw: raw_text.clone(),
                tail: tail.trim().to_string(),
                display: text.clone(),
                corrected,
                ruled,
                polished: polished.clone(),
                used_llm,
                polish_ms,
                fixes,
                depth: depth.label().to_string(),
                breaks,
                llm: stages.as_ref().map(|s| devlog::LlmTrace::from(&s.llm)),
                insert: Some(devlog::InsertInfo {
                    status: insert_key(outcome.status).to_string(),
                    chars: outcome.chars_inserted,
                    ms: insert_ms,
                    warning: outcome.warning.clone(),
                }),
                ..Default::default()
            },
        );
    }

    match outcome.status {
        insert::InsertStatus::Inserted | insert::InsertStatus::PartiallyInserted => {
            if outcome.chars_inserted > 0 {
                let rt = app.state::<Runtime>();
                *rt.last_text.lock().unwrap() = Some(polished.clone());
                // 托盘菜单里「复制最近一次」的可用状态跟着刷新
                tray::refresh_menu(app);
            }
            bubble::finish_indicator(app);
        }
        insert::InsertStatus::CopiedFallback => {
            let msg = outcome
                .warning
                .unwrap_or_else(|| "已复制到剪贴板".to_string());
            bubble::notice(app, "error", &msg, Duration::from_secs(2));
            notify(app, "没能自动粘贴", &msg);
        }
        insert::InsertStatus::Failed => {
            let msg = outcome.warning.unwrap_or_else(|| "写入失败".to_string());
            bubble::notice(app, "error", &msg, Duration::from_secs(2));
            notify(app, "落字失败", &msg);
        }
    }
}

/// 落字状态的机器可读名字（`InsertStatus` 的 Display 是给用户看的中文）
fn insert_key(status: insert::InsertStatus) -> &'static str {
    match status {
        insert::InsertStatus::Inserted => "inserted",
        insert::InsertStatus::PartiallyInserted => "partially_inserted",
        insert::InsertStatus::CopiedFallback => "copied_fallback",
        insert::InsertStatus::Failed => "failed",
    }
}

fn is_cjk(c: char) -> bool {
    matches!(c as u32, 0x4E00..=0x9FFF | 0x3400..=0x4DBF)
}

/// 说话一直不停顿时的强制切点：在目标位置附近找一帧最安静的，找不到就硬切。
fn forced_commit_point(samples: &[f32], from: usize) -> usize {
    const FRAME: usize = 1_600;
    let target = from + FORCED_COMMIT_SAMPLES;
    let lo = target.saturating_sub(8_000); // 往回找 0.5s
    let hi = (target + 8_000).min(samples.len().saturating_sub(FRAME));

    let mut best = target.min(hi);
    let mut best_rms = f32::MAX;
    let mut i = lo;
    while i <= hi {
        if i + FRAME > samples.len() {
            break;
        }
        let frame = &samples[i..i + FRAME];
        let rms = (frame.iter().map(|s| s * s).sum::<f32>() / FRAME as f32).sqrt();
        if rms < best_rms {
            best_rms = rms;
            best = i + FRAME;
        }
        i += FRAME;
    }
    best
}

/// 一段静音有多长（毫秒）。从 `from` 开始数连续的低电平帧。
///
/// 用来量「这个停顿到底多长」—— 比看时钟准，时钟会把 450ms 的 tick 也加进去。
fn silence_after(samples: &[f32], from: usize) -> u64 {
    const FRAME: usize = 1_600; // 100ms
    const SILENCE_RMS: f32 = 0.008;

    let mut ms = 0;
    let mut i = from;
    while i + FRAME <= samples.len() {
        let frame = &samples[i..i + FRAME];
        let rms = (frame.iter().map(|s| s * s).sum::<f32>() / FRAME as f32).sqrt();
        if rms >= SILENCE_RMS {
            break;
        }
        ms += 100;
        i += FRAME;
    }
    ms
}

/// 把尾巴上的静音切掉再送识别。
///
/// 不切的话，最后那 0.5–2 秒静音会被模型「听」出字来（实测会冒出「没」「我」这类
/// 单字），落进输入框就是凭空多一个字。
fn trim_trailing_silence(samples: &[f32]) -> &[f32] {
    const FRAME: usize = 1_600; // 100ms
    const SILENCE_RMS: f32 = 0.008;

    let mut end = samples.len();
    while end > FRAME {
        let frame = &samples[end - FRAME..end];
        let rms = (frame.iter().map(|s| s * s).sum::<f32>() / FRAME as f32).sqrt();
        if rms >= SILENCE_RMS {
            break;
        }
        end -= FRAME;
    }
    &samples[..end]
}

/// 有没有实际内容。只有标点的片段（静音常被识别成一个「。」）不算。
fn has_content(s: &str) -> bool {
    s.chars().any(|c| c.is_alphanumeric() || is_cjk(c))
}

/// 当前前台窗口的句柄（0 表示没有）。
fn foreground_window() -> Option<isize> {
    unsafe {
        let h = windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow();
        if h.0.is_null() {
            None
        } else {
            Some(h.0 as isize)
        }
    }
}

/// 如果前台已经被本进程的窗口占了（多半是刚建好的隐藏控制面板），就还给启动前的窗口。
///
/// 这不是洁癖：开机自启时抢走焦点，用户正在打的字会直接进到别的窗口。
fn restore_foreground(prev: Option<isize>) {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{
        GetForegroundWindow, GetWindowThreadProcessId, SetForegroundWindow,
    };

    let Some(prev) = prev else {
        return;
    };
    unsafe {
        let fg = GetForegroundWindow();
        if fg.0.is_null() || fg.0 as isize == prev {
            return;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(fg, Some(&mut pid as *mut u32));
        if pid != std::process::id() {
            return; // 前台不是我们的窗口，别多管闲事
        }
        let prev_hwnd = HWND(prev as *mut core::ffi::c_void);
        if SetForegroundWindow(prev_hwnd).as_bool() {
            log::info!("启动时把前台还给原窗口（隐藏窗口不该抢焦点）");
        }
    }
}

fn transcribe(app: &AppHandle, samples: &[f32]) -> anyhow::Result<Vec<String>> {
    let rt = app.state::<Runtime>();
    let mut slot = rt.transcriber.lock().unwrap();

    if slot.is_none() {
        let cfg = asr::AsrConfig::load(&asr::model_root());
        *slot = Some(asr::Transcriber::load(&cfg)?);
    }
    let recognizer = slot.as_mut().expect("刚放进去的");
    recognizer.transcribe_segments(samples)
}

/// 控制面板用的命令。三个开关都走 tray 的统一入口：
/// 更新托盘勾选 → 持久化 → 广播 state-changed。
#[tauri::command]
fn set_dictation(app: AppHandle, on: bool) {
    tray::apply_dictation(&app, on);
}

#[tauri::command]
fn set_polish(app: AppHandle, on: bool) {
    tray::apply_polish(&app, on);
}

#[tauri::command]
fn set_deep(app: AppHandle, on: bool) {
    tray::apply_deep(&app, on);
}

#[tauri::command]
fn get_state() -> serde_json::Value {
    log::info!(
        "控制面板读取状态：听写={} 润色={} 深度={}",
        tray::dictation_enabled(),
        tray::polish_enabled(),
        tray::deep_enabled()
    );
    let asr_cfg = asr::AsrConfig::load(&asr::model_root());
    serde_json::json!({
        "dictation": tray::dictation_enabled(),
        "polish": tray::polish_enabled(),
        "deep": tray::deep_enabled(),
        "deep_available": llm::deep_available(),
        "engine": if cfg!(feature = "asr") { asr_cfg.engine.clone() } else { "placeholder".to_string() },
        "engine_label": if cfg!(feature = "asr") { asr_cfg.label() } else { "占位识别器" },
    })
}

/// 控制面板「设置 → 运行状态」用：现在到底能不能用。
#[tauri::command]
fn get_diagnostics(app: AppHandle) -> serde_json::Value {
    let rt = app.state::<Runtime>();
    let mic = rt.engine.lock().map(|g| g.is_some()).unwrap_or(false);
    // 识别时锁会被占住，try_lock 失败说明正在跑，不算没就绪
    let asr = match rt.transcriber.try_lock() {
        Ok(g) => {
            if g.is_some() {
                "ready"
            } else {
                "loading"
            }
        }
        Err(_) => "busy",
    };
    let asr_cfg = asr::AsrConfig::load(&asr::model_root());
    let trigger = hotkey::config();
    let trigger_label = trigger.display();
    serde_json::json!({
        "mic": mic,
        "asr": asr,
        "asr_engine": asr_cfg.engine,
        "asr_label": asr_cfg.label(),
        "asr_dir": asr_cfg.resolved_dir().display().to_string(),
        "asr_available": asr_cfg.available(),
        "placeholder": !cfg!(feature = "asr"),
        "hotkey": HOTKEY_OK.load(Ordering::Relaxed),
        "hotkey_label": trigger_label,
        "hotkey_mode": trigger.mode.label(),
        "hotkey_hint": trigger.trigger_hint(),
        "hotkey_long_press_ms": trigger.long_press_ms,
        "deep": llm::deep_available(),
        "llm": llm::LlmConfig::load(&asr::model_root()).is_some(),
    })
}

/// 麦克风打不开时让用户点一次重试，不用重启应用。
#[tauri::command]
fn retry_audio(app: AppHandle) -> bool {
    let rt = app.state::<Runtime>();
    match audio::AudioEngine::start() {
        Ok(engine) => {
            *rt.engine.lock().unwrap() = Some(engine);
            log::info!("麦克风重连成功");
            true
        }
        Err(e) => {
            log::error!("麦克风重连失败: {e}");
            false
        }
    }
}

/// 控制面板：退出（会卸载本地润色模型）。
#[tauri::command]
fn quit_app(app: AppHandle) {
    shutdown(&app);
}

/// 控制面板「开发者模式」：最近的应用日志（内存里最多 400 行）。
#[tauri::command]
fn get_logs(limit: Option<usize>) -> Vec<String> {
    devlog::recent_logs(limit.unwrap_or(300))
}

#[tauri::command]
fn clear_logs() {
    devlog::clear_logs();
}

/// 手动降级开关：YANCHU_REDUCED_MOTION=1 时前端关掉位移与循环动画。
#[tauri::command]
fn get_flags() -> serde_json::Value {
    let rm = std::env::var("YANCHU_REDUCED_MOTION")
        .map(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "on" | "true"))
        .unwrap_or(false);
    serde_json::json!({ "reduced_motion": rm })
}

/// 控制面板：读气泡外观参数
#[tauri::command]
fn get_ui() -> serde_json::Value {    let ui = ui::BubbleUi::load(&asr::model_root());
    serde_json::json!({
        "width": ui.width,
        "height": ui.height,
        "font_size": ui.font_size,
    })
}

/// 控制面板：改气泡外观。存盘 + 立刻把窗口改成新尺寸。
#[tauri::command]
fn save_ui(
    app: AppHandle,
    width: u32,
    height: u32,
    font_size: f32,
) -> Result<serde_json::Value, String> {
    let ui = ui::BubbleUi {
        width,
        height,
        font_size,
    }
    .clamped();
    ui.save(&asr::model_root()).map_err(|e| e.to_string())?;
    apply_bubble_ui(&app, ui);
    log::info!(
        "气泡外观：{}x{}，字号 {}",
        ui.width,
        ui.height,
        ui.font_size
    );
    Ok(serde_json::json!({
        "width": ui.width,
        "height": ui.height,
        "font_size": ui.font_size,
    }))
}

/// 把外观参数应用到气泡窗口（尺寸）和页面（字号）。
fn apply_bubble_ui(app: &AppHandle, ui: ui::BubbleUi) {
    use tauri::{LogicalSize, Size};

    if let Some(win) = app.get_webview_window("bubble") {
        let _ = win.set_size(Size::Logical(LogicalSize::new(
            ui.width as f64,
            ui.height as f64,
        )));
        // 字号走 CSS 变量：页面还没加载完时 eval 会失败，所以两边都发一次
        let _ = win.eval(&format!(
            "document.documentElement.style.setProperty('--bubble-font','{}px')",
            ui.font_size
        ));
    }
    let _ = app.emit(
        "ui",
        serde_json::json!({ "font_size": ui.font_size }),
    );
}

/// 控制面板：读当前触发键与输入模式。
#[tauri::command]
fn get_trigger() -> serde_json::Value {
    hotkey::config().to_json()
}

/// 控制面板：录完快捷键后先检查一遍，不落盘。
/// 结构错误（单个字母、Esc、Win）和「已被别的程序占用」都在这里拦住。
#[tauri::command]
fn check_trigger(keys: Vec<u32>) -> serde_json::Value {
    let cfg = trigger::TriggerConfig {
        keys,
        mode: trigger::InputMode::Hold,
        long_press_ms: 0,
    };
    match cfg.validate().and_then(|_| cfg.ensure_available()) {
        Ok(()) => {
            let cfg = cfg.normalized();
            serde_json::json!({
                "ok": true,
                "message": "",
                "keys": cfg.keys,
                "label": cfg.display(),
            })
        }
        Err(e) => {
            let cfg = cfg.normalized();
            serde_json::json!({
                "ok": false,
                "message": e,
                "keys": cfg.keys,
                "label": cfg.display(),
            })
        }
    }
}

/// 控制面板：保存触发键与输入模式，立刻生效并持久化。
#[tauri::command]
fn save_trigger(
    app: AppHandle,
    keys: Vec<u32>,
    mode: String,
    long_press_ms: u32,
) -> Result<serde_json::Value, String> {
    let mode = trigger::InputMode::parse(&mode)?;
    let cfg = trigger::TriggerConfig {
        keys,
        mode,
        long_press_ms,
    };
    cfg.validate()?;
    cfg.ensure_available()?;
    let cfg = cfg.normalized();

    // 换键之前先把正在录的这一轮按正常流程结束掉：
    // 旧键的按下/抬起状态不能带到新键上。
    stop_session_if_recording(&app);

    cfg.save(&asr::model_root())
        .map_err(|e| format!("保存失败：{e}"))?;
    hotkey::set_config(cfg.clone());
    tray::refresh_menu(&app);
    let _ = app.emit("trigger-changed", cfg.to_json());
    log::info!(
        "触发键已更新：{}（{}，长按 {}ms）",
        cfg.display(),
        cfg.mode.label(),
        cfg.long_press_ms
    );
    Ok(cfg.to_json())
}

/// 面板进入 / 退出「录快捷键」状态。录入期间热键必须挂起，
/// 否则用户按到旧触发键会真的开始录音，还可能把字落进设置窗口。
#[tauri::command]
fn set_capture_mode(app: AppHandle, on: bool) {
    if on {
        stop_session_if_recording(&app);
    }
    hotkey::set_suspended(on);
    log::info!("快捷键录入：{}", if on { "挂起触发" } else { "恢复触发" });
}

/// 控制面板「模型」页用：当前识别模型、可切换的本地识别模型、全部润色配置。
fn models_json() -> serde_json::Value {
    let root = asr::model_root();
    let asr_cfg = asr::AsrConfig::load(&root);
    let asr_models = asr::available_models(&asr_cfg);
    let engine = asr_cfg.engine.clone();
    let label = asr_cfg.label();
    let dir = asr_cfg.resolved_dir().display().to_string();
    let threads = asr_cfg.thread_count();
    let available = asr_cfg.available();
    let polish = model_profiles::PolishSettings::load(&root);
    serde_json::json!({
        "asr": {
            "engine": engine,
            "label": label,
            "dir": dir,
            "threads": threads,
            "available": available,
            "models": asr_models,
        },
        "polish": polish,
    })
}

#[tauri::command]
fn get_models() -> serde_json::Value {
    models_json()
}

/// 切换本地识别模型：重新加载后替换运行时识别器，并持久化到 asr.json。
#[tauri::command]
fn switch_asr(
    app: AppHandle,
    engine: String,
    model_dir: Option<String>,
    threads: Option<i32>,
) -> Result<serde_json::Value, String> {
    let root = asr::model_root();
    let cfg = asr::AsrConfig {
        engine,
        model_dir,
        threads,
    }
    .normalized();

    if !cfg.available() {
        return Err(format!(
            "{} 的模型文件不完整：{}",
            cfg.label(),
            cfg.resolved_dir().display()
        ));
    }

    // 换模型前先把正在录的这一轮结束掉；加载完成后才替换，失败时旧模型继续可用。
    stop_session_if_recording(&app);
    let started = std::time::Instant::now();
    let transcriber = asr::Transcriber::load(&cfg)
        .map_err(|e| format!("加载 {} 失败：{e}", cfg.label()))?;
    cfg.save(&root)
        .map_err(|e| format!("保存识别模型配置失败：{e}"))?;

    {
        let rt = app.state::<Runtime>();
        let mut slot = rt
            .transcriber
            .lock()
            .map_err(|_| "识别器被占用，稍后再试".to_string())?;
        *slot = Some(transcriber);
    }

    log::info!(
        "识别模型已切换：{}（{}），加载 {:?}",
        cfg.label(),
        cfg.resolved_dir().display(),
        started.elapsed()
    );
    let payload = models_json();
    let _ = app.emit("models-changed", payload.clone());
    let _ = app.emit("state-changed", tray::state_json());
    Ok(payload)
}

/// 保存整套润色配置，并把当前启用项写进 llm.json（立即生效）。
#[tauri::command]
fn save_polish_profiles(
    app: AppHandle,
    profiles: Vec<model_profiles::PolishProfile>,
    active: String,
) -> Result<serde_json::Value, String> {
    let root = asr::model_root();
    let settings = model_profiles::PolishSettings { active, profiles }.normalized();
    settings.validate()?;
    settings
        .save(&root)
        .map_err(|e| format!("保存润色配置失败：{e}"))?;
    settings
        .apply_active(&root)
        .map_err(|e| format!("应用润色配置失败：{e}"))?;

    // 本地模型只有标准档；切到本地后把「深度整理」关掉，切回云端再放开。
    if settings
        .active_profile()
        .map(|p| p.is_local())
        .unwrap_or(true)
    {
        tray::apply_deep(&app, false);
    } else {
        tray::refresh_menu(&app);
        let _ = app.emit("state-changed", tray::state_json());
    }

    // 本地 Ollama 冷加载要 2–3 秒，后台预热；云端 warmup 会直接返回。
    if let Some(cfg) = llm::LlmConfig::load(&root) {
        std::thread::spawn(move || llm::warmup(&cfg));
    }

    log::info!(
        "润色配置已更新：{} 套，启用 {}",
        settings.profiles.len(),
        if settings.active.is_empty() {
            "（无）"
        } else {
            settings.active.as_str()
        }
    );
    let payload = models_json();
    let _ = app.emit("models-changed", payload.clone());
    Ok(payload)
}

#[tauri::command]
fn test_polish_profile(
    profile: model_profiles::PolishProfile,
) -> Result<serde_json::Value, String> {
    let profile = profile.normalized(0);
    profile.validate()?;
    let cfg = profile.to_llm_config(&asr::model_root());
    match cfg.ping() {
        Ok((reply, ms)) => Ok(serde_json::json!({
            "ok": true,
            "ms": ms,
            "reply": reply,
        })),
        Err(e) => Err(format!("连接失败：{e}")),
    }
}

/// 查本机 Ollama 已安装的模型，供润色配置里的模型名输入提示。
#[tauri::command]
fn list_ollama_models(url: String) -> Result<Vec<String>, String> {
    model_profiles::ollama_models(&url).map_err(|e| e.to_string())
}

fn stop_session_if_recording(app: &AppHandle) {
    let recording = app
        .try_state::<Runtime>()
        .and_then(|rt| rt.session.lock().ok().map(|s| s.pressed))
        .unwrap_or(false);
    if recording {
        log::info!("换触发键：先结束正在录的这一轮");
        on_release(app);
    }
}

/// 控制面板：润色历史（某一天）。一次返回「日期列表 + 当天的记录」，
/// 省得前端为切日期再跑一趟。
#[tauri::command]
fn get_history(app: AppHandle, date: Option<String>) -> serde_json::Value {
    let rt = app.state::<Runtime>();
    let h = rt.history.lock().unwrap();
    let days = h.days();
    let entries = h.list(date.as_deref());
    let current = date
        .filter(|d| !d.trim().is_empty())
        .unwrap_or_else(|| crate::devlog::local_now().date);
    serde_json::json!({ "date": current, "days": days, "entries": entries })
}

#[tauri::command]
fn clear_history(app: AppHandle, date: Option<String>) {
    let rt = app.state::<Runtime>();
    rt.history.lock().unwrap().clear(date.as_deref());
}

/// 控制面板：读当前热词表
#[tauri::command]
fn get_glossary() -> String {
    let path = asr::model_root().join("glossary.txt");
    // 文件还不存在时展示内置词表，用户可以直接在上面增删再保存
    std::fs::read_to_string(&path).unwrap_or_else(|_| glossary::Glossary::default_text())
}

/// 控制面板：写热词表。气泡里的规则层管线有缓存，保存后要让它下次现读。
#[tauri::command]
fn save_glossary(app: AppHandle, text: String) -> Result<usize, String> {
    let path = asr::model_root().join("glossary.txt");
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    std::fs::write(&path, &text).map_err(|e| e.to_string())?;

    // 缓存失效：下一次句子闭合时重新加载词表
    let rt = app.state::<Runtime>();
    *rt.rules.lock().unwrap() = None;

    let count = text
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.trim().starts_with('#'))
        .count();
    log::info!("热词表已更新：{count} 条");
    Ok(count)
}

/// 控制面板：读替换规则表（`models/rules.txt`）
#[tauri::command]
fn get_rules() -> String {
    let path = asr::model_root().join("rules.txt");
    std::fs::read_to_string(&path).unwrap_or_else(|_| rules::default_text())
}

/// 控制面板：写替换规则表。和热词表一样，保存后让规则层管线下次重读。
#[tauri::command]
fn save_rules(app: AppHandle, text: String) -> Result<usize, String> {
    let path = asr::model_root().join("rules.txt");
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    std::fs::write(&path, &text).map_err(|e| e.to_string())?;

    let rt = app.state::<Runtime>();
    *rt.rules.lock().unwrap() = None;

    let count = rules::Rules::parse(&text).len();
    log::info!("替换规则已更新：{count} 条");
    Ok(count)
}

/// 控制面板：对一段文本跑润色，分别给出规则层和最终结果，用于模型对比。
#[tauri::command]
fn run_polish(text: String, use_llm: bool, depth: Option<String>) -> serde_json::Value {
    let started = std::time::Instant::now();
    let depth = depth
        .as_deref()
        .map(llm::Depth::from_label)
        .unwrap_or(llm::Depth::Standard);
    let pipeline = pipeline::Pipeline::load();
    let pipeline = if use_llm {
        pipeline
    } else {
        pipeline.without_llm()
    };
    let stages = pipeline.run_with(&text, depth);
    let llm_outcome = stages.llm.outcome();
    let llm_ms = stages.llm.ms();
    let llm_note = match stages.llm.reason.clone() {
        Some(reason) => reason,
        None if stages.used_llm => "已采用".to_string(),
        None => "未配置或文本太短".to_string(),
    };
    serde_json::json!({
        "ruled": stages.ruled,
        "final": stages.final_text,
        "used_llm": stages.used_llm,
        "depth": stages.depth.label(),
        "llm_outcome": llm_outcome,
        "llm_ms": llm_ms,
        "llm_note": llm_note,
        "warnings": stages.llm.warnings,
        "fixes": stages.fixes.iter().map(|(a, b)| format!("{a} → {b}")).collect::<Vec<_>>(),
        "ms": stages.ms_total,
        "ms_rule": stages.ms_rule,
        "wall_ms": started.elapsed().as_millis() as u64,
    })
}

/// 控制面板：LLM 配置状态
#[tauri::command]
fn get_llm_status() -> serde_json::Value {
    let root = asr::model_root();
    let cfg = llm::LlmConfig::load(&root);
    let settings = model_profiles::PolishSettings::load(&root);
    let active = settings.active_profile();
    let configured = cfg.as_ref().map(|c| c.is_configured()).unwrap_or(false);
    serde_json::json!({
        "configured": configured,
        "url": cfg.as_ref().map(|c| c.url.clone()).unwrap_or_default(),
        "model": cfg.as_ref().map(|c| c.model.clone()).unwrap_or_default(),
        "provider": cfg.as_ref().map(|c| c.provider.key()).unwrap_or(""),
        "timeout_ms": cfg.as_ref().map(|c| c.timeout_ms).unwrap_or(0),
        "profile_name": active.map(|p| p.name.clone()).unwrap_or_default(),
        "path": root.join("llm.json").display().to_string(),
    })
}

/// 回放：拿一批历史口述原文跑润色管线，出一张可读的对照表。
///
/// 语料是 JSONL，一行一条：`{"id":"...","raw":"..."}`。
/// 一次加载管线跑完全部样本，比在 PowerShell 里一条条调 CLI 稳，也快。
fn run_replay(
    corpus: &std::path::Path,
    out: Option<&std::path::Path>,
    force_deep: bool,
) -> anyhow::Result<()> {
    use std::fmt::Write as _;

    let text = std::fs::read_to_string(corpus).map_err(|e| {
        anyhow::anyhow!("读不到语料 {}：{e}", corpus.display())
    })?;
    let pipeline = pipeline::Pipeline::load();

    struct Row {
        id: String,
        chars: usize,
        depth: String,
        used: bool,
        ms: u64,
        note: String,
        raw: String,
        ruled: String,
        final_text: String,
        periods_before: usize,
        periods_after: usize,
        paras: usize,
        fillers: usize,
        /// 输入里的汉字有多少比例还出现在成品里（摘要程度的粗略指标）
        coverage: f32,
    }

    let mut rows: Vec<Row> = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let v: serde_json::Value = serde_json::from_str(line)
            .map_err(|e| anyhow::anyhow!("第 {} 行不是合法 JSON：{e}", i + 1))?;
        let id = v["id"].as_str().unwrap_or("?").to_string();
        let raw = v["raw"].as_str().unwrap_or("").trim().to_string();
        if raw.is_empty() {
            continue;
        }

        let depth = if force_deep {
            llm::Depth::Deep
        } else {
            llm::Depth::Standard
        };
        let s = pipeline.run_with(&raw, depth);
        let note = match (&s.llm.reason, s.used_llm) {
            (Some(reason), _) => reason.clone(),
            (None, true) if !s.llm.warnings.is_empty() => format!("告警 {:?}", s.llm.warnings),
            (None, true) => String::new(),
            (None, false) => "未走模型".to_string(),
        };

        rows.push(Row {
            id,
            chars: raw.chars().count(),
            depth: s.depth.label().to_string(),
            used: s.used_llm,
            ms: s.llm.ms(),
            note,
            periods_before: count_char(&s.ruled, '。'),
            periods_after: count_char(&s.final_text, '。'),
            paras: paragraph_count(&s.final_text),
            fillers: count_fillers(&s.final_text),
            coverage: coverage_ratio(&s.ruled, &s.final_text),
            raw,
            ruled: s.ruled.clone(),
            final_text: s.final_text.clone(),
        });
    }

    let n = rows.len();
    let used = rows.iter().filter(|r| r.used).count();
    let mut times: Vec<u64> = rows.iter().filter(|r| r.used).map(|r| r.ms).collect();
    times.sort_unstable();
    let percentile = |q: f64| -> u64 {
        if times.is_empty() {
            return 0;
        }
        let idx = ((times.len() as f64 - 1.0) * q).round() as usize;
        times[idx]
    };
    let period_drop: i64 = rows
        .iter()
        .map(|r| r.periods_before as i64 - r.periods_after as i64)
        .sum();
    let with_paras = rows.iter().filter(|r| r.paras > 1).count();
    let fillers: usize = rows.iter().map(|r| r.fillers).sum();
    let avg_coverage = if n == 0 {
        0.0
    } else {
        rows.iter().map(|r| r.coverage).sum::<f32>() / n as f32
    };

    let mut md = String::new();
    let _ = writeln!(md, "# 回放报告：{}", corpus.display());
    let _ = writeln!(md, "\n- 样本 {}，采用 {}（{}%）", n, used, if n == 0 { 0 } else { used * 100 / n });
    let _ = writeln!(md, "- 模型耗时 P50 {}ms / P95 {}ms", percentile(0.5), percentile(0.95));
    let _ = writeln!(md, "- 句号变化：合计减少 {period_drop} 个（合并碎句）");
    let _ = writeln!(md, "- 出了多段的样本：{with_paras} / {n}");
    let _ = writeln!(md, "- 内容覆盖（输入汉字仍出现在成品中的比例）：平均 {avg_coverage:.0}%");
    let _ = writeln!(md, "- 成品残留语气词：{fillers} 处");
    let _ = writeln!(md, "\n| # | id | 字数 | 档位 | 采用 | ms | 句号 | 段 | 覆盖 | 语气词 | 备注 |");
    let _ = writeln!(md, "|---|---|---|---|---|---|---|---|---|---|---|");
    for (i, r) in rows.iter().enumerate() {
        let _ = writeln!(
            md,
            "| {} | {} | {} | {} | {} | {} | {}→{} | {} | {:.0}% | {} | {} |",
            i + 1,
            r.id,
            r.chars,
            r.depth,
            if r.used { "是" } else { "否" },
            r.ms,
            r.periods_before,
            r.periods_after,
            r.paras,
            r.coverage,
            r.fillers,
            r.note.replace('|', "/")
        );
    }
    let _ = writeln!(md, "\n## 明细");
    for r in rows.iter() {
        let _ = writeln!(md, "\n### {}（{} 字）", r.id, r.chars);
        let _ = writeln!(md, "- 原文：{}", r.raw);
        let _ = writeln!(md, "- 规则层：{}", r.ruled);
        let _ = writeln!(md, "- 成品：{}", r.final_text.replace('\n', " ⏎ "));
    }

    match out {
        Some(path) => {
            std::fs::write(path, &md)?;
            println!("回放完成：{}（{n} 段，采用 {used}）", path.display());
        }
        None => print!("{md}"),
    }
    Ok(())
}

fn count_char(s: &str, c: char) -> usize {
    s.chars().filter(|x| *x == c).count()
}

fn paragraph_count(s: &str) -> usize {
    s.split("\n\n").filter(|p| !p.trim().is_empty()).count()
}

fn count_fillers(s: &str) -> usize {
    s.chars()
        .filter(|c| matches!(c, '嗯' | '呃' | '啊' | '哦' | '唉'))
        .count()
}

fn is_cjk_char(c: char) -> bool {
    matches!(c as u32, 0x4E00..=0x9FFF | 0x3400..=0x4DBF)
}

/// 输入里出现过的汉字，有多少比例还出现在成品里。
///
/// 允许摘要之后，这个数比长度比更能说明「丢了多少」：
/// 正常的合并/删口头语通常还在 85% 以上，压成概述会掉到 70% 以下。
fn coverage_ratio(input: &str, output: &str) -> f32 {
    let seen: std::collections::HashSet<char> = output.chars().filter(|c| is_cjk_char(*c)).collect();
    let all: std::collections::HashSet<char> = input.chars().filter(|c| is_cjk_char(*c)).collect();
    if all.is_empty() {
        return 100.0;
    }
    let kept = all.iter().filter(|c| seen.contains(c)).count();
    kept as f32 / all.len() as f32 * 100.0
}

/// 流式自检：拿一段 wav 走一遍「识别 → 断点判定 → 定稿」，不开窗口、不碰焦点。
///
/// 这是调标点判据的主力工具：真实 ASR + 真实规则，结果直接打出来，
/// 不用对着麦克风反复说同一句。
fn run_stream_test(path: &std::path::Path) -> anyhow::Result<()> {
    let samples = audio::read_wav_16k(path)?;
    println!(
        "输入: {}（{:.1}s）",
        path.display(),
        samples.len() as f32 / 16_000.0
    );

    let asr_cfg = asr::AsrConfig::load(&asr::model_root());
    println!(
        "识别模型: {}（{}）",
        asr_cfg.label(),
        asr_cfg.resolved_dir().display()
    );
    let mut transcriber = asr::Transcriber::load(&asr_cfg)?;
    let rules = pipeline::Pipeline::load_rules();

    let step = 7_200; // 450ms @16k，和真实实时识别同频
    let mut streamer = stream::Streamer::new();
    let mut pos = 0usize;
    let mut end = step;

    while end <= samples.len() {
        let point = find_commit_point(&samples[..end], pos).or_else(|| {
            if end > pos + MAX_TAIL_SAMPLES {
                Some(forced_commit_point(&samples[..end], pos))
            } else {
                None
            }
        });
        if let Some(point) = point {
            let text = transcriber
                .transcribe_segments(trim_trailing_silence(&samples[pos..point]))?
                .join("");
            let text = text.trim().to_string();
            if has_content(&text) {
                let polished = rules.rule_only(&text).ruled;
                println!("  段: {text}  →  {polished}");
                streamer.push_segment(&text, &polished);
            }
            pos = point;
        }

        if end > pos + 3_200 {
            let tail = transcriber
                .transcribe_segments(&samples[pos..end])?
                .join("");
            let tail = tail.trim();
            if has_content(tail) {
                let polished = polish::rule_polish_partial(tail);
                let pause_ms = 500 + silence_after(&samples, pos);
                streamer.see_tail(&polished, pause_ms);
            }
        }
        end += step;
    }

    // 收尾：剩下的一整段当最后一句
    if pos + 3_200 < samples.len() {
        let text = transcriber
            .transcribe_segments(trim_trailing_silence(&samples[pos..]))?
            .join("");
        let text = text.trim().to_string();
        if has_content(&text) {
            let polished = rules.rule_only(&text).ruled;
            println!("  尾: {text}  →  {polished}");
            streamer.push_segment(&text, &polished);
        }
    }

    let final_text = streamer.finish();
    println!("\n定稿: {final_text}");
    println!("原文: {}", streamer.raw());
    println!(
        "断点: 句号 {}  逗号 {}",
        streamer.breaks.iter().filter(|b| b.decision == "period").count(),
        streamer.breaks.iter().filter(|b| b.decision == "comma").count()
    );
    for b in &streamer.breaks {
        println!(
            "  - {}ms  [{}] + [{}] → {}",
            b.pause_ms, b.left, b.right, b.decision
        );
    }
    Ok(())
}

/// 麦克风自检：开麦 N 秒，报告采样点数和电平。
fn run_mic_check(secs: u64) {
    println!("麦克风自检：采 {secs} 秒…");
    let engine = match audio::AudioEngine::start() {
        Ok(e) => e,
        Err(e) => {
            eprintln!("✗ 打不开麦克风: {e}");
            std::process::exit(1);
        }
    };

    engine.arm();
    std::thread::sleep(Duration::from_secs(secs));
    let samples = engine.stop();

    let rms = if samples.is_empty() {
        0.0
    } else {
        (samples.iter().map(|s| (*s as f64) * (*s as f64)).sum::<f64>() / samples.len() as f64)
            .sqrt()
    };
    let peak = samples.iter().fold(0f32, |m, s| m.max(s.abs()));

    println!(
        "采样点数: {}（期望约 {}）",
        samples.len(),
        secs as usize * 16_000
    );
    println!("时长    : {:.2}s", samples.len() as f32 / 16_000.0);
    println!("RMS     : {rms:.4}");
    println!("峰值    : {peak:.4}");

    if samples.is_empty() {
        eprintln!("✗ 一个采样都没收到");
        std::process::exit(1);
    }
    if peak < 0.001 {
        println!("⚠ 收到数据但几乎静音（麦克风可能被静音，或环境确实很安静）");
    } else {
        println!("✓ 麦克风工作正常");
    }
}

/// 内存基线：把识别模型真正加载起来，然后驻留 N 秒，让外部脚本采样常驻内存。
fn run_mem_check(secs: u64) {
    println!("加载模型…");
    let started = std::time::Instant::now();
    let asr_cfg = asr::AsrConfig::load(&asr::model_root());
    println!(
        "识别模型: {}（{}）",
        asr_cfg.label(),
        asr_cfg.resolved_dir().display()
    );
    let _resident = match asr::Transcriber::load(&asr_cfg) {
        Ok(t) => {
            println!("模型加载完成，用时 {:?}", started.elapsed());
            t
        }
        Err(e) => {
            eprintln!("✗ 模型加载失败: {e}");
            std::process::exit(1);
        }
    };

    println!("MEM_READY 模型已加载，驻留 {secs} 秒供采样…");
    std::thread::sleep(Duration::from_secs(secs));
    println!("结束");
}

#[cfg(test)]
mod commit_tests {
    use super::find_commit_point;

    /// 1 秒语音 + 0.6 秒静音 + 1 秒语音，切点应落在 16000
    #[test]
    fn finds_gap_after_speech() {
        let mut s = vec![0.0f32; 32_000];
        for (i, v) in s.iter_mut().enumerate() {
            *v = 0.05 * ((i as f32) * 0.1).sin();
        }
        s.extend(std::iter::repeat_n(0.0f32, 9_600));
        let mut tail = vec![0.0f32; 16_000];
        for (i, v) in tail.iter_mut().enumerate() {
            *v = 0.05 * ((i as f32) * 0.1).cos();
        }
        s.extend(tail);

        // 切点落在静音段内（开头是 32000，判定需要 5 帧 = 500ms）
        let point = find_commit_point(&s, 0).expect("应找到切点");
        assert!(
            (32_000..=32_000 + 5 * 1_600).contains(&point),
            "切点 {point} 不在静音段内"
        );
    }

    /// 开头的静音不能当成句子边界，要跳过它继续往后找
    #[test]
    fn skips_leading_silence() {
        let mut s = vec![0.0f32; 4_800];
        let speech: Vec<f32> = (0..32_000).map(|i| 0.05 * ((i as f32) * 0.1).sin()).collect();
        s.extend(speech);
        s.extend(std::iter::repeat_n(0.0f32, 9_600));

        let point = find_commit_point(&s, 0).expect("应找到停顿点");
        assert!(point >= 4_800 + 24_000, "切点 {point} 落在语音里了");
    }

    /// 只有很短的一点语音时不该切
    #[test]
    fn does_not_commit_too_early() {
        let mut s: Vec<f32> = (0..3_200).map(|i| 0.05 * ((i as f32) * 0.1).sin()).collect();
        s.extend(std::iter::repeat_n(0.0f32, 9_600));
        assert_eq!(find_commit_point(&s, 0), None);
    }
}