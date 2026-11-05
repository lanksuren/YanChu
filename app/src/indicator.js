// 指示器：Rust 以 ~60ms 的节奏推 { state, level }，这里只做视觉映射。
//
// 电平映射用「相对最近峰值」，不用固定系数。
// 原来写的是 `sqrt(min(1, level * 5))`：正常说话的 RMS 就有 0.1–0.2，乘 5 直接顶格，
// 于是环永远在最大振幅、看不出在说什么（2026-11-04 反馈）。
// 换成本机自适应之后，麦克风开不开 AGC、增益高低都能看到行程：
//   峰值的 1/1.2 以上就算满 —— 常态说话落在 0.6~0.8，只有真放大声才顶到 1。

const PEAK_FLOOR = 0.008; // 低于这个当静音，别让底噪把增益顶到天上
const PEAK_DECAY = 0.995; // 60ms 一拍 ≈ 8 秒半衰：句间停顿不会让增益突然掉下来
const HEADROOM = 1.2;     // 峰值只到 ~0.83 满，常态说话留出可见的行程

const orb = document.getElementById('orb');
const halo = document.getElementById('halo');
const ring = document.getElementById('ring');
const core = document.getElementById('core');

// 电平包络：上升快、回落慢。
// 直接用 60ms 的采样值会一跳一跳；release 拉长之后，说完一句环是「收」回去的，不是「弹」回去的。
let shownLevel = 0;
let peak = 0.05;

function toUnit(level) {
  const lvl = Math.max(0, level);
  peak = Math.max(PEAK_FLOOR, Math.max(lvl, peak * PEAK_DECAY));
  const unit = Math.min(1, lvl / (peak * HEADROOM));
  return Math.pow(unit, 0.75);
}

function render({ state, level }) {
  orb.dataset.state = state;

  // 0.45 的上升系数 ≈ 3 拍到八成，0.12 的回落 ≈ 0.5 秒收完：
  // 这样「开口」和「收尾」都是渐变，不会一秒里跳好几次。
  const target = toUnit(level);

  if (state === 'polishing' || state === 'done' || state === 'notice') {
    shownLevel = 0;
  } else {
    const k = target > shownLevel ? 0.45 : 0.12;
    shownLevel += (target - shownLevel) * k;
  }
  const l = shownLevel;

  ring.style.transform = `scale(${1 + l * 0.55})`;
  halo.style.transform = `scale(${1 + l * 0.9})`;
  halo.style.opacity = String(0.18 + l * 0.55);
  core.style.transform = `scale(${1 + l * 0.6})`;
}

// 进场/退场由 Rust 的 shown / hiding 事件触发：
// 窗口是瞬时 show 的，纯 CSS 动画会在页面加载（仍隐藏）时跑完。
function playEnter() {
  orb.classList.remove('leaving', 'entering');
  void orb.offsetWidth;
  orb.classList.add('entering');
}

function playLeave() {
  orb.classList.remove('entering');
  orb.classList.add('leaving');
}

orb.addEventListener('animationend', (e) => {
  if (e.animationName === 'orb-in') orb.classList.remove('entering');
  if (e.animationName === 'orb-out') orb.classList.remove('leaving');
});

// 配色由共享的 theme.js 换算成 CSS 变量；和气泡同一份主题。
function applyTheme(theme) {
  if (window.Skin) window.Skin.apply(theme);
}

async function main() {
  const { listen } = window.__TAURI__.event;
  const { invoke } = window.__TAURI__.core;
  await listen('indicator', (e) => render(e.payload));
  await listen('theme', (e) => applyTheme(e.payload));
  await listen('shown', playEnter);
  await listen('hiding', playLeave);
  // 启动时主动拉一次配色：窗口是隐藏创建的，别等第一次换色才生效
  try {
    applyTheme(await invoke('get_theme'));
  } catch (e) {
    console.warn('读取指示器配色失败', e);
  }
  try {
    const f = await invoke('get_flags');
    if (f && f.reduced_motion) document.documentElement.classList.add('reduced-motion');
  } catch {}
}

main().catch((err) => console.error('指示器初始化失败', err));
