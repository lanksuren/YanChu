// 气泡：两段文字 + 一行状态。
//   committed —— 已经说完的句子（实色，已过小润色；标点可能还会被断点判据改）
//   tail      —— 正在说、还可能变的片段（浅灰）
//   status    —— 深度整理的提示，平时不显示
//
// 文字只增不减，所以**底部对齐**：新的内容贴着下沿，旧内容从顶部溢出并渐隐。
// 不这么做的话，-webkit-line-clamp 会把最新的几行裁掉，用户永远看不到刚说的话。
//
// 字号/宽高由控制面板决定（models/ui.json），这里只负责应用。

const bubble = document.getElementById('bubble');
const textEl = document.getElementById('text');
const committedEl = document.getElementById('committed');
const tailEl = document.getElementById('tail');
const statusEl = document.getElementById('status');

// 内容超出一屏时给顶部加渐隐，避免看到半截字
function syncOverflow() {
  const over = textEl.scrollHeight > bubble.clientHeight + 1;
  bubble.classList.toggle('clipped', over);
}

function applyUi(ui) {
  if (ui && ui.font_size) {
    document.documentElement.style.setProperty('--bubble-font', `${ui.font_size}px`);
  }
}

// 定稿的句子只在「比上次更长」时闪一下：整段替换（润色/定稿）不闪，
// 否则每次状态切换都在闪，反而变成噪音。
function setCommitted(text) {
  const prev = committedEl.textContent || '';
  const grew = text.length > prev.length && text.startsWith(prev);
  committedEl.textContent = text;
  if (!grew) return;
  committedEl.classList.remove('fresh');
  void committedEl.offsetWidth;
  committedEl.classList.add('fresh');
}

committedEl.addEventListener('animationend', (e) => {
  if (e.animationName === 'commit-flash') committedEl.classList.remove('fresh');
});

function render({ kind, committed, tail, text, status }) {
  bubble.dataset.kind = kind;
  statusEl.textContent = '';

  if (kind === 'polishing' || kind === 'deep') {
    // 润色中：文字留着，整体呼吸，表示还在处理
    setCommitted(committed || text || '');
    tailEl.textContent = '';
    // 文案由主进程给：长文自动切档要等 3–5 秒，和手动切档的一两秒不是一回事
    statusEl.textContent = status || (kind === 'deep' ? '深度整理中，可以等一两秒…' : '');
    syncOverflow();
    return;
  }

  if (kind === 'done' || kind === 'error') {
    // 定稿：整段都用实色
    setCommitted(text || committed || '');
    tailEl.textContent = '';
    syncOverflow();
    return;
  }

  setCommitted(committed || '');
  tailEl.textContent = tail || '';
  syncOverflow();
}

// 动画性能预算：出场后的 4 秒里盯 rAF，连续 3 帧超过 20ms 就自动降级。
// 降级只关动效，不影响文字到达和落字。
function watchFrameBudget() {
  let last = performance.now();
  let slow = 0;
  const deadline = last + 4000;
  function tick(now) {
    const dt = now - last;
    last = now;
    slow = dt > 20 ? slow + 1 : 0;
    if (slow >= 3) {
      document.documentElement.classList.add('reduced-motion');
      console.warn('动画掉帧，已自动降级');
      return;
    }
    if (now < deadline) requestAnimationFrame(tick);
  }
  requestAnimationFrame(tick);
}

// 进场/退场：窗口本身是瞬时 show/hide 的，动画只能在「窗口已显示」之后补。
// 用 animationend 收尾，避免类名常驻导致下一次加不上去。
function playEnter() {
  bubble.classList.remove('leaving', 'entering');
  void bubble.offsetWidth; // 强制重排，让动画重新开始
  bubble.classList.add('entering');
  watchFrameBudget();
}

function playLeave() {
  bubble.classList.remove('entering');
  bubble.classList.add('leaving');
}

bubble.addEventListener('animationend', (e) => {
  if (e.animationName === 'bubble-in') bubble.classList.remove('entering');
  if (e.animationName === 'bubble-out') bubble.classList.remove('leaving');
});

async function main() {
  const { listen } = window.__TAURI__.event;
  const { invoke } = window.__TAURI__.core;
  await listen('bubble', (e) => render(e.payload));
  await listen('ui', (e) => applyUi(e.payload));
  await listen('shown', playEnter);
  await listen('hiding', playLeave);
  // 首帧可能早于页面加载，主动拉一次
  try {
    applyUi(await invoke('get_ui'));
  } catch (e) {
    console.warn('读取气泡外观参数失败', e);
  }
  try {
    const f = await invoke('get_flags');
    if (f && f.reduced_motion) document.documentElement.classList.add('reduced-motion');
  } catch {}
}

main().catch((err) => {
  console.error('气泡初始化失败', err);
  document.body.textContent = '气泡初始化失败';
});
