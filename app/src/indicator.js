// 指示器：Rust 以 ~60ms 的节奏推 { state, level }，这里只做视觉映射。
//
// level 是原始 RMS（正常说话 0.03–0.2），直接当比例用会几乎看不出变化，
// 所以取一个软压缩：l = sqrt(min(1, level * 5))，小声也能看到环在动。

const orb = document.getElementById('orb');
const halo = document.getElementById('halo');
const ring = document.getElementById('ring');
const core = document.getElementById('core');

// 电平包络：上升快、回落慢。
// 直接用 60ms 的采样值会一跳一跳；release 拉长之后，说完一句环是「收」回去的，不是「弹」回去的。
let shownLevel = 0;

function render({ state, level }) {
  orb.dataset.state = state;

  const raw = Math.min(1, Math.max(0, level * 5));
  const target = Math.sqrt(raw); // 软压缩，照顾小声说话

  if (state === 'polishing' || state === 'done' || state === 'notice') {
    shownLevel = 0;
  } else {
    const k = target > shownLevel ? 0.5 : 0.14;
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

async function main() {
  const { listen } = window.__TAURI__.event;
  await listen('indicator', (e) => render(e.payload));
  await listen('shown', playEnter);
  await listen('hiding', playLeave);
  try {
    const f = await window.__TAURI__.core.invoke('get_flags');
    if (f && f.reduced_motion) document.documentElement.classList.add('reduced-motion');
  } catch {}
}

main().catch((err) => console.error('指示器初始化失败', err));
