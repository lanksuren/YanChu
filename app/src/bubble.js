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

// 配色由共享的 theme.js 换算成 CSS 变量（见 theme.js 顶部说明）。
// 设置面板换色时会推一次 theme 事件；页面加载时再主动拉一次兜底。
function applyTheme(theme) {
  if (window.Skin) window.Skin.apply(theme);
}

// 定稿的句子只在「比上次更长」时过渡一下，而且**只过渡新增的那几个字**。
//
// 以前是给整个 #committed 加背景高亮：一秒钟定稿三句就整块闪三下，
// 读起来像屏幕在跳（2026-11-04 用户反馈「整体会闪好几下」）。
// 2026-11-05 再去掉背景：现在只把新增的几个字从灰字色过渡到正文色，
// 不碰背景。只有流式（partial）才需要这个反馈；提示 / 定稿 / 报错是整段替换，
// 加了反而会让错误信息先闪一下正文色。
function setCommitted(text, flash = false) {
  // 内容没变就别动 DOM：每 700ms 一次的空写入会掐断正在播放的过渡动画
  if (text === committedEl.textContent) return;

  const prev = committedEl.dataset.text || '';
  const grew = flash && text.length > prev.length && text.startsWith(prev);
  committedEl.dataset.text = text;

  if (!grew) {
    // 整段替换（润色/定稿/提示）不过渡：那是另一件事，动起来只会变成噪音
    committedEl.textContent = text;
    return;
  }

  committedEl.textContent = text.slice(0, prev.length);
  const added = document.createElement('span');
  added.className = 'fresh';
  added.textContent = text.slice(prev.length);
  committedEl.appendChild(added);
}

function render({ kind, committed, tail, text, status }) {
  bubble.dataset.kind = kind;
  statusEl.textContent = '';

  if (kind === 'polishing' || kind === 'deep') {
    // 润色中：文字留着，整体呼吸，表示还在处理
    setCommitted(committed || text || '');
    // 尾巴**不清空**：松手那一刻灰字正说到一半，清掉会让人以为「刚才说的没了」。
    // 气泡几百毫秒后整体退场，留着比突然消失更稳。
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

  // 只有这里是「灰字正在变成正文」：新定的几个字做颜色过渡
  setCommitted(committed || '', true);
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
  await listen('theme', (e) => applyTheme(e.payload));
  await listen('shown', playEnter);
  await listen('hiding', playLeave);
  // 首帧可能早于页面加载，主动拉一次
  try {
    const ui = await invoke('get_ui');
    applyUi(ui);
    applyTheme(ui.theme);
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
