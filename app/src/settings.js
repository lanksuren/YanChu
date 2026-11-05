// 控制面板：状态 / 历史 / 热词 / 润色对比。

const { invoke } = window.__TAURI__.core;

// ---------------- 页签 ----------------
function activateTab(btn) {
  document.querySelectorAll('.tab').forEach((b) => {
    b.classList.remove('active');
    b.setAttribute('aria-selected', 'false');
    b.tabIndex = -1;
  });
  document.querySelectorAll('.pane').forEach((p) => p.classList.remove('active'));
  btn.classList.add('active');
  btn.setAttribute('aria-selected', 'true');
  btn.tabIndex = 0;
  const pane = document.getElementById(`pane-${btn.dataset.tab}`);
  pane.classList.add('active');
  pane.scrollTop = 0;

  if (btn.dataset.tab === 'history') loadHistory();
  if (btn.dataset.tab === 'models') loadModels();
  if (btn.dataset.tab === 'glossary') {
    loadGlossary().catch((e) => console.error('读取词表失败', e));
  }
  if (btn.dataset.tab === 'settings') {
    // 概览和设置合并成一页：切回来时把状态、开关、触发键、气泡一起刷新
    loadState().catch((e) => console.error('刷新开关状态失败', e));
    loadDiagnostics();
    loadBubbleUi().catch((e) => console.error('刷新气泡外观失败', e));
    loadTriggerConfig();
    loadGuideVisibility();
  } else if (capturing) {
    // 切走设置页时结束录入，别让热键一直挂起
    cancelCapture();
  }
}

document.querySelectorAll('.tab').forEach((btn) => {
  btn.addEventListener('click', () => activateTab(btn));
});

// 方向键切换页签（WAI-ARIA tabs 的基本交互；侧边栏是纵向的）
document.querySelector('.tabs').addEventListener('keydown', (e) => {
  if (!['ArrowLeft', 'ArrowRight', 'ArrowUp', 'ArrowDown'].includes(e.key)) return;
  const tabs = [...document.querySelectorAll('.tab')];
  const i = tabs.indexOf(document.activeElement);
  if (i < 0) return;
  const forward = e.key === 'ArrowRight' || e.key === 'ArrowDown';
  const next = tabs[(i + (forward ? 1 : tabs.length - 1)) % tabs.length];
  next.focus();
  next.click();
  e.preventDefault();
});

// 点整行也能切换开关（不用非得点那个 40px 的小滑块）
document.querySelectorAll('.row').forEach((row) => {
  const input = row.querySelector('.switch input');
  if (!input) return;
  row.classList.add('row-clickable');
  row.addEventListener('click', (e) => {
    if (e.target.closest('.switch')) return;
    input.click();
  });
});

// ---------------- 状态 ----------------
// 开关只有一份真源（Rust 侧），这里只负责把状态画出来。
function applyState(state) {
  document.getElementById('dictation').checked = state.dictation;
  document.getElementById('polish').checked = state.polish;

  const deep = document.getElementById('deep');
  deep.checked = state.deep && state.deep_available;
  deep.disabled = !state.deep_available;
  document.getElementById('deepHint').textContent = state.deep_available
    ? '改写与分点，多等 1–2 秒'
    : '需配置云端 API 模型';
}

async function loadState() {
  const state = await invoke('get_state');
  applyState(state);
  document.getElementById('engine').textContent =
    state.engine === 'placeholder'
      ? '占位识别器'
      : state.engine_label || (state.engine === 'qwen3' ? 'Qwen3-ASR 0.6B' : 'SenseVoice int8');

  const llm = await invoke('get_llm_status');
  if (llm.configured) {
    const name = [llm.profile_name, llm.model].filter(
      (v, i, all) => v && all.indexOf(v) === i,
    );
    document.getElementById('llm').textContent = name.join(' · ') || '已配置';
  } else {
    document.getElementById('llm').textContent = '未配置';
  }

  const glossary = await invoke('get_glossary');
  const count = glossary
    .split('\n')
    .filter((l) => l.trim() && !l.trim().startsWith('#')).length;
  document.getElementById('termCount').textContent = `${count} 条`;
}

document.getElementById('dictation').addEventListener('change', (e) =>
  invoke('set_dictation', { on: e.target.checked }).catch((err) =>
    toast(`切换失败：${err}`, 'error'))
);
document.getElementById('polish').addEventListener('change', (e) =>
  invoke('set_polish', { on: e.target.checked }).catch((err) =>
    toast(`切换失败：${err}`, 'error'))
);
document.getElementById('deep').addEventListener('change', (e) =>
  invoke('set_deep', { on: e.target.checked }).catch((err) =>
    toast(`切换失败：${err}`, 'error'))
);

// ---------------- 历史 ----------------
function el(tag, cls, text) {
  const node = document.createElement(tag);
  if (cls) node.className = cls;
  if (text !== undefined) node.textContent = text;
  return node;
}

// 轻反馈：2.2 秒自动消失，不阻塞操作
let toastTimer = null;
function toast(msg, kind = '') {
  const t = document.getElementById('toast');
  if (!t) return;
  t.textContent = msg;
  t.className = `toast show ${kind}`.trim();
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => { t.className = 'toast'; }, 2200);
}

async function copyText(text) {
  if (!text) return;
  try {
    await navigator.clipboard.writeText(text);
  } catch {
    // WebView 里 clipboard API 偶尔不可用，退回到 execCommand
    const ta = document.createElement('textarea');
    ta.value = text;
    ta.style.cssText = 'position:fixed;left:-9999px;top:0;opacity:0';
    document.body.append(ta);
    ta.select();
    try { document.execCommand('copy'); } catch {}
    ta.remove();
  }
  toast('已复制');
}

// 保存类按钮：禁用 + 文案 + busy 样式，避免连点发多次请求
async function withBusy(btn, label, fn) {
  if (!btn || btn.disabled) return undefined;
  const old = btn.textContent;
  btn.disabled = true;
  btn.classList.add('busy');
  btn.textContent = label;
  try {
    return await fn();
  } finally {
    btn.disabled = false;
    btn.classList.remove('busy');
    btn.textContent = old;
  }
}

let historyEntries = [];
let historyQuery = '';

function historySkeleton() {
  const frag = document.createDocumentFragment();
  for (let i = 0; i < 3; i++) {
    const box = el('div', 'entry skeleton');
    box.append(el('div', 'sk-line short'));
    box.append(el('div', 'sk-line'));
    frag.append(box);
  }
  return frag;
}

async function loadHistory(date) {
  const list = document.getElementById('historyList');
  list.replaceChildren(historySkeleton());

  const data = await invoke('get_history', { date: date || null });
  historyDate = data.date;
  historyEntries = data.entries || [];

  // 日期下拉：只显示有记录的天；一条都没有时显示今天
  const sel = document.getElementById('historyDate');
  sel.replaceChildren();
  const days = data.days.length ? data.days : [[data.date, historyEntries.length]];
  for (const [d, n] of days) {
    const opt = document.createElement('option');
    opt.value = d;
    opt.textContent = `${d}（${n} 条）`;
    if (d === data.date) opt.selected = true;
    sel.append(opt);
  }
  renderHistory();
}

function renderHistory() {
  const list = document.getElementById('historyList');
  list.replaceChildren();
  const q = historyQuery.trim().toLowerCase();
  const items = q
    ? historyEntries.filter((e) =>
        `${e.raw || ''} ${e.final_text || ''}`.toLowerCase().includes(q))
    : historyEntries;

  if (!historyEntries.length) {
    const empty = el('div', 'empty');
    empty.append(el('strong', '', '这一天还没有记录'));
    empty.append(el('p', 'hint', '按住触发键说一句，文字会落在你刚才的输入框里。'));
    list.append(empty);
    document.getElementById('historyHint').textContent = '';
    return;
  }
  if (!items.length) {
    const empty = el('div', 'empty');
    empty.append(el('strong', '', '没有匹配的记录'));
    empty.append(el('p', 'hint', '换个关键词试试。'));
    list.append(empty);
    document.getElementById('historyHint').textContent = '0 条';
    return;
  }
  const frag = document.createDocumentFragment();
  for (const e of items) frag.append(buildEntry(e));
  list.append(frag);
  document.getElementById('historyHint').textContent = q ? `${items.length} 条匹配` : `共 ${items.length} 条`;
}

function buildEntry(e) {
  const box = el('div', 'entry');
  box.__entry = e;

  const head = el('div', 'entry-head');
  head.append(el('span', 'time', e.time || ''));
  head.append(el('span', 'meta', `${(e.final_text || '').length} 字 · ${e.ms}ms`));
  if (e.used_llm) {
    head.append(el('span', 'badge llm', e.depth === 'deep' ? '大润色·深度' : '大润色'));
  }

  const actions = el('span', 'entry-actions');
  const copy = el('button', 'mini', '复制');
  copy.type = 'button';
  copy.addEventListener('click', (ev) => {
    ev.stopPropagation();
    copyText(e.final_text || e.raw || '');
  });
  const toggle = el('button', 'mini', '对比');
  toggle.type = 'button';
  toggle.addEventListener('click', (ev) => {
    ev.stopPropagation();
    toggleDetail(box, toggle);
  });
  actions.append(copy, toggle);
  head.append(actions);
  box.append(head);

  const summary = el('div', 'line final');
  summary.append(el('span', 'txt', e.final_text || e.raw || ''));
  box.append(summary);

  // diff 按需渲染：几十条长文本一次性跑 LCS 会卡住主线程
  const detail = el('div', 'detail');
  detail.hidden = true;
  box.append(detail);
  return box;
}

function toggleDetail(box, btn) {
  const detail = box.querySelector('.detail');
  if (!detail) return;
  if (!detail.dataset.rendered) {
    detail.append(buildDetail(box.__entry));
    detail.dataset.rendered = '1';
  }
  detail.hidden = !detail.hidden;
  box.classList.toggle('open', !detail.hidden);
  btn.textContent = detail.hidden ? '对比' : '收起';
}

function buildDetail(e) {
  const detail = el('div', 'detail-body');
  const addLine = (cls, tag, from, to) => {
    const line = el('div', `line ${cls}`);
    line.append(el('span', 'tag', tag));
    const txt = el('span', 'txt');
    renderDiff(from || '', to || '', txt);
    line.append(txt);
    detail.append(line);
  };
  addLine('raw', '识别', '', e.raw);
  addLine('ruled', '小润色', e.raw, e.ruled);
  addLine('final', '最终', e.ruled, e.final_text);
  return detail;
}

let historyDate = '';

/// 逐字 diff：返回 ['same'|'del'|'ins', 字符] 序列（LCS）。
/// 文本都很短（一般 < 500 字），O(n·m) 足够；超长就退化成整段显示。
function diffOps(a, b) {
  const n = a.length;
  const m = b.length;
  const dp = Array.from({ length: n + 1 }, () => new Uint32Array(m + 1));
  for (let i = n - 1; i >= 0; i--) {
    for (let j = m - 1; j >= 0; j--) {
      dp[i][j] =
        a[i] === b[j]
          ? dp[i + 1][j + 1] + 1
          : Math.max(dp[i + 1][j], dp[i][j + 1]);
    }
  }
  const ops = [];
  let i = 0;
  let j = 0;
  while (i < n && j < m) {
    if (a[i] === b[j]) {
      ops.push(['same', a[i]]);
      i++;
      j++;
    } else if (dp[i + 1][j] >= dp[i][j + 1]) {
      ops.push(['del', a[i]]);
      i++;
    } else {
      ops.push(['ins', b[j]]);
      j++;
    }
  }
  while (i < n) ops.push(['del', a[i++]]);
  while (j < m) ops.push(['ins', b[j++]]);
  return ops;
}

function renderDiff(from, to, target) {
  if (!from || !to || from.length * to.length > 250000) {
    target.textContent = to || from || '';
    return;
  }
  let kind = null;
  let buf = '';
  const flush = () => {
    if (!buf) return;
    const span = document.createElement('span');
    if (kind && kind !== 'same') span.className = kind;
    span.textContent = buf;
    target.append(span);
    buf = '';
  };
  for (const [t, ch] of diffOps(from, to)) {
    if (t !== kind) {
      flush();
      kind = t;
    }
    buf += ch;
  }
  flush();
}

document.getElementById('refreshHistory').addEventListener('click', () => loadHistory(historyDate));
document.getElementById('historyDate').addEventListener('change', (e) => loadHistory(e.target.value));
document.getElementById('historySearch').addEventListener('input', (e) => {
  historyQuery = e.target.value;
  renderHistory();
});

// 破坏性操作两步确认：第一次点把按钮变红，3 秒内再点才真的删
let clearArmed = false;
let clearTimer = null;
document.getElementById('clearHistory').addEventListener('click', async () => {
  const btn = document.getElementById('clearHistory');
  if (!clearArmed) {
    clearArmed = true;
    btn.classList.add('danger');
    btn.textContent = '再点一次清空';
    clearTimeout(clearTimer);
    clearTimer = setTimeout(() => {
      clearArmed = false;
      btn.classList.remove('danger');
      btn.textContent = '清空这一天';
    }, 3000);
    return;
  }
  clearTimeout(clearTimer);
  clearArmed = false;
  btn.classList.remove('danger');
  btn.textContent = '清空这一天';
  await invoke('clear_history', { date: historyDate || null });
  toast('已清空这一天');
  loadHistory(historyDate);
  loadGuideVisibility();
});

// ---------------- 词表（表格） / 替换规则 ----------------
let glossaryRows = [];
let glossaryQuery = '';
let glossaryComments = [];

// 表格表达不了 # 注释，保存时原样保留在最前面，别让用户的手写注释被吃掉
function splitGlossary(text) {
  glossaryComments = text
    .split('\n')
    .map((l) => l.trim())
    .filter((l) => l.startsWith('#'));
  return parseGlossary(text);
}

// 文件格式：普通词一行一个；显式纠错写「会被误读成=正确写法」。
// 这里把文件里的 (误读, 正确) 转成表格里的两列，别再把方向弄反。
function parseGlossary(text) {
  return text
    .split('\n')
    .map((l) => l.trim())
    .filter((l) => l && !l.startsWith('#'))
    .map((l) => {
      const i = l.indexOf('=');
      if (i < 0) return { correct: l, misread: '' };
      return {
        correct: l.slice(i + 1).trim(),
        misread: l.slice(0, i).trim(),
      };
    })
    .filter((r) => r.correct);
}

function serializeGlossary(rows) {
  return rows
    .filter((r) => r.correct.trim())
    .map((r) => {
      const correct = r.correct.trim();
      const misread = r.misread.trim();
      return misread ? `${misread}=${correct}` : correct;
    })
    .join('\n');
}

function glossaryPairKey(row) {
  const correct = row.correct.trim().toLowerCase();
  const misread = row.misread.trim().toLowerCase();
  return correct ? `${misread}=>${correct}` : '';
}

function glossaryDuplicateFlags() {
  const counts = {};
  for (const r of glossaryRows) {
    const k = glossaryPairKey(r);
    if (k) counts[k] = (counts[k] || 0) + 1;
  }
  return glossaryRows.map((r) => {
    const k = glossaryPairKey(r);
    return !!k && counts[k] > 1;
  });
}

function setAddHint(text, kind = '') {
  const node = document.getElementById('addHint');
  if (!node) return;
  node.textContent = text;
  node.className = `hint glossary-tip${kind ? ` ${kind}` : ''}`;
}

function setGlossaryHint(text, kind = '') {
  const node = document.getElementById('glossaryHint');
  if (!node) return;
  node.textContent = text;
  node.className = `hint${kind ? ` ${kind}` : ''}`;
}

function syncGlossarySource() {
  const node = document.getElementById('glossaryText');
  if (!node) return;
  node.value = [glossaryComments.join('\n'), serializeGlossary(glossaryRows)]
    .filter(Boolean)
    .join('\n');
}

function renderGlossary() {
  const host = document.getElementById('glossaryTable');
  host.replaceChildren();
  const flags = glossaryDuplicateFlags();
  const q = glossaryQuery.trim().toLowerCase();
  const count = document.getElementById('glossaryCount');
  if (count) count.textContent = `${glossaryRows.length} 条`;

  const head = el('div', 'vrow head');
  head.append(
    el('span', '', '正确写法'),
    el('span', '', '会被误读成（可留空）'),
    el('span', '', ''),
    el('span', '', ''),
  );
  host.append(head);

  let shown = 0;
  glossaryRows.forEach((row, idx) => {
    if (q && !`${row.correct} ${row.misread}`.toLowerCase().includes(q)) return;
    shown += 1;
    const line = el('div', `vrow${flags[idx] ? ' invalid' : ''}`);
    const correct = el('input');
    correct.value = row.correct;
    correct.placeholder = '例如：入参 / helper';
    correct.setAttribute('aria-label', '正确写法');
    const misread = el('input');
    misread.value = row.misread;
    misread.placeholder = '可留空';
    misread.setAttribute('aria-label', '会被误读成');

    correct.addEventListener('input', () => {
      row.correct = correct.value;
      setGlossaryHint('有未保存的修改，点「保存词表」生效', '');
    });
    misread.addEventListener('input', () => {
      row.misread = misread.value;
      setGlossaryHint('有未保存的修改，点「保存词表」生效', '');
    });

    const swap = el('button', 'mini', '对调');
    swap.type = 'button';
    swap.title = '如果两列填反了，点这里对调';
    swap.disabled = !row.misread.trim();
    swap.addEventListener('click', () => {
      const text = row.correct;
      row.correct = row.misread;
      row.misread = text;
      renderGlossary();
      syncGlossarySource();
      setGlossaryHint('有未保存的修改，点「保存词表」生效', '');
    });

    const del = el('button', 'mini delete', '删除');
    del.type = 'button';
    del.addEventListener('click', () => {
      glossaryRows.splice(idx, 1);
      renderGlossary();
      syncGlossarySource();
      setGlossaryHint('有未保存的修改，点「保存词表」生效', '');
    });

    line.append(correct, misread, swap, del);
    host.append(line);
  });

  if (!glossaryRows.length) {
    host.append(el('p', 'hint empty-table', '词表还是空的，在上面填「正确写法」加第一条。'));
  } else if (!shown) {
    host.append(el('p', 'hint empty-table', '没有匹配的词条。'));
  }
}

async function loadGlossary() {
  const text = await invoke('get_glossary');
  glossaryRows = splitGlossary(text);
  document.getElementById('glossaryText').value = text;
  renderGlossary();
  setGlossaryHint('修改后点「保存词表」生效。', '');
  setAddHint('留空时按相近的拼写 / 发音自动纠正。', '');
  document.getElementById('rulesText').value = await invoke('get_rules');
}

function addGlossaryRow() {
  const wordInput = document.getElementById('newGlossaryWord');
  const misreadInput = document.getElementById('newGlossaryMisread');
  const correct = wordInput.value.trim();
  const misread = misreadInput.value.trim();

  if (!correct) {
    setAddHint('先填「正确写法」', 'error');
    wordInput.focus();
    return;
  }
  if (misread && misread.toLowerCase() === correct.toLowerCase()) {
    setAddHint('两种写法一样，不用再加', 'error');
    misreadInput.focus();
    return;
  }

  const key = `${misread.toLowerCase()}=>${correct.toLowerCase()}`;
  if (glossaryRows.some((row) => glossaryPairKey(row) === key)) {
    setAddHint(`「${correct}」${misread ? `（${misread}）` : ''} 已经在列表里了`, 'error');
    wordInput.focus();
    return;
  }

  glossaryRows.unshift({ correct, misread });
  wordInput.value = '';
  misreadInput.value = '';
  glossaryQuery = '';
  document.getElementById('glossarySearch').value = '';
  renderGlossary();
  syncGlossarySource();
  setAddHint(
    `已添加「${correct}」${misread ? `，误读「${misread}」` : ''}。记得点「保存词表」。`,
    'ok',
  );
  setGlossaryHint('有未保存的修改，点「保存词表」生效', '');
  wordInput.focus();
}

document.getElementById('glossarySearch').addEventListener('input', (e) => {
  glossaryQuery = e.target.value;
  renderGlossary();
});

document.getElementById('addGlossary').addEventListener('click', addGlossaryRow);
for (const id of ['newGlossaryWord', 'newGlossaryMisread']) {
  document.getElementById(id).addEventListener('keydown', (e) => {
    if (e.key === 'Enter') {
      e.preventDefault();
      addGlossaryRow();
    }
  });
}

document.getElementById('applyGlossarySource').addEventListener('click', () => {
  glossaryRows = splitGlossary(document.getElementById('glossaryText').value);
  renderGlossary();
  setGlossaryHint('已从源码载入表格，点「保存词表」生效', '');
  toast('已从源码载入表格');
});

document.getElementById('saveGlossary').addEventListener('click', async (ev) => {
  const btn = ev.currentTarget;

  // 只合并完全相同的「正确写法 + 误读」组合；
  // 同一个正确写法允许有多条不同误读（例如 heper=helper、helber=helper）。
  const seen = new Set();
  const rows = [];
  let merged = 0;
  for (const r of glossaryRows) {
    const correct = r.correct.trim();
    if (!correct) continue;
    const misread = r.misread.trim();
    const key = `${misread.toLowerCase()}=>${correct.toLowerCase()}`;
    if (seen.has(key)) {
      merged += 1;
      continue;
    }
    seen.add(key);
    rows.push({ correct, misread });
  }

  const body = serializeGlossary(rows);
  const text = [glossaryComments.join('\n'), body].filter(Boolean).join('\n');
  const n = await withBusy(btn, '保存中…', () => invoke('save_glossary', { text }));
  glossaryRows = rows;
  renderGlossary();
  document.getElementById('glossaryText').value = text;
  setGlossaryHint(`已保存 ${n} 条，下一次落字生效。`, 'ok');
  setAddHint('留空时按相近的拼写 / 发音自动纠正。', '');
  toast(merged ? `已保存 ${n} 条，合并了 ${merged} 条重复` : `已保存 ${n} 条热词`);
  loadState();
});

document.getElementById('saveRules').addEventListener('click', async (ev) => {
  const btn = ev.currentTarget;
  const text = document.getElementById('rulesText').value;
  const n = await withBusy(btn, '保存中…', () => invoke('save_rules', { text }));
  document.getElementById('rulesHint').textContent = `已保存 ${n} 条规则，下一次落字生效。`;
  toast(`已保存 ${n} 条规则`);
});

// ---------------- 润色对比 ----------------
const polishButtons = () =>
  ['runRules', 'runBoth', 'runDeep'].map((id) => document.getElementById(id));

function setPolishBusy(on, active) {
  for (const b of polishButtons()) {
    b.disabled = on;
    b.classList.toggle('busy', on && b === active);
  }
}

async function runPolish(useLlm, deep = false, active) {
  const text = document.getElementById('polishInput').value.trim();
  if (!text) {
    document.getElementById('polishHint').textContent = '先粘一段文本进来';
    return;
  }
  setPolishBusy(true, active);
  document.getElementById('polishHint').textContent = '润色中…';
  try {
    const r = await invoke('run_polish', {
      text,
      useLlm,
      depth: deep ? 'deep' : 'standard',
    });
    document.getElementById('outRuled').textContent = r.ruled || '—';
    document.getElementById('outFinal').textContent = r.final || '—';
    const llmNote = r.used_llm
      ? `${r.depth === 'deep' ? '深度' : '标准'}档 ${r.llm_ms}ms`
      : r.llm_outcome === 'failed'
        ? `大润色回退：${r.llm_note}`
        : r.llm_outcome === 'skipped'
          ? '只有规则层（未配置或文本太短）'
          : '只有规则层（已关闭）';
    const warn = r.warnings && r.warnings.length ? ` · 事实项: ${r.warnings.join('、')}` : '';
    document.getElementById('polishHint').textContent =
      `${r.ms}ms（规则 ${r.ms_rule}ms）· ${llmNote}` +
      (r.fixes && r.fixes.length ? ` · 热词: ${r.fixes.join(', ')}` : '') +
      warn;
  } finally {
    setPolishBusy(false);
  }
}

document.getElementById('runRules').addEventListener('click', (ev) => runPolish(false, false, ev.currentTarget));
document.getElementById('runBoth').addEventListener('click', (ev) => runPolish(true, false, ev.currentTarget));
document.getElementById('runDeep').addEventListener('click', (ev) => runPolish(true, true, ev.currentTarget));

// ---------------- 外观：尺寸 + 皮肤配色 ----------------
const BUBBLE_DEFAULTS = { width: 440, height: 100, font_size: 10.5 };

// 预设是「整组颜色」，点一下换完；想细调再动下面的取色器。
// 字段名与 src-tauri/src/ui.rs 的 Theme 一致；默认预设留空 = 跟随系统。
const SKIN_PRESETS = [
  {
    id: 'auto',
    name: '默认',
    theme: {},
  },
  {
    id: 'cloud',
    name: '云白',
    theme: {
      bubbleBg: '#ffffff',
      bubbleAlpha: 0.9,
      bubbleText: '#111827',
      bubbleTail: '#8b95a5',
      bubbleBorder: '#e2e8f0',
      accent: '#2563eb',
      ok: '#059669',
      warn: '#d97706',
      polish: '#7c3aed',
    },
  },
  {
    id: 'graphite',
    name: '石墨',
    theme: {
      bubbleBg: '#1b1d23',
      bubbleAlpha: 0.92,
      bubbleText: '#e7e9ee',
      bubbleTail: '#9aa3b2',
      bubbleBorder: '#333844',
      accent: '#7aa7ff',
      ok: '#34d399',
      warn: '#fbbf24',
      polish: '#a78bfa',
    },
  },
  {
    id: 'paper',
    name: '米黄',
    theme: {
      bubbleBg: '#fbf4e6',
      bubbleAlpha: 0.95,
      bubbleText: '#2f2415',
      bubbleTail: '#a08a68',
      bubbleBorder: '#e6d6b8',
      accent: '#b45309',
      ok: '#15803d',
      warn: '#b45309',
      polish: '#7c3aed',
    },
  },
  {
    id: 'mint',
    name: '薄荷',
    theme: {
      bubbleBg: '#eafaf1',
      bubbleAlpha: 0.95,
      bubbleText: '#123c2b',
      bubbleTail: '#7aa38f',
      bubbleBorder: '#c9e8d9',
      accent: '#0d9488',
      ok: '#059669',
      warn: '#d97706',
      polish: '#7c3aed',
    },
  },
  {
    id: 'dusk',
    name: '暮紫',
    theme: {
      bubbleBg: '#221b33',
      bubbleAlpha: 0.92,
      bubbleText: '#f1eaff',
      bubbleTail: '#a99bc4',
      bubbleBorder: '#3b3054',
      accent: '#a78bfa',
      ok: '#34d399',
      warn: '#fbbf24',
      polish: '#c4b5fd',
    },
  },
];

// 主题字段 → 取色器 / 色值文本 → theme.js 解析后的颜色名
const SKIN_COLORS = [
  { key: 'bubbleBg', input: 'skinBg', hex: 'skinBgHex', color: 'bg' },
  { key: 'bubbleText', input: 'skinText', hex: 'skinTextHex', color: 'text' },
  { key: 'bubbleTail', input: 'skinTail', hex: 'skinTailHex', color: 'tail' },
  { key: 'accent', input: 'skinAccent', hex: 'skinAccentHex', color: 'accent' },
  { key: 'bubbleBorder', input: 'skinBorder', hex: 'skinBorderHex', color: 'border' },
  { key: 'ok', input: 'skinOk', hex: 'skinOkHex', color: 'ok' },
  { key: 'warn', input: 'skinWarn', hex: 'skinWarnHex', color: 'warn' },
  { key: 'polish', input: 'skinPolish', hex: 'skinPolishHex', color: 'polish' },
];

let skinTheme = {};
let skinResolved = null;
let skinSaveTimer = null;

function setSkinHint(text, kind = '') {
  const el = document.getElementById('skinHint');
  if (!el) return;
  el.textContent = text;
  el.className = kind ? `hint ${kind}` : 'hint';
}

// 只保留主题认识的字段；undefined / null / 空串都算「没填」，
// 由 theme.js 按背景明暗自动配（0 是合法值，不能一起丢掉）。
function normalizeSkinTheme(theme) {
  const keys = window.Skin ? window.Skin.keys : [];
  const out = {};
  for (const key of keys) {
    const value = theme ? theme[key] : undefined;
    if (value !== undefined && value !== null && value !== '') out[key] = value;
  }
  return out;
}

function sameTheme(a, b) {
  const keys = window.Skin ? window.Skin.keys : [];
  return keys.every((key) => {
    const av = a ? a[key] : undefined;
    const bv = b ? b[key] : undefined;
    if (av == null && bv == null) return true;
    if (typeof av === 'number' || typeof bv === 'number') return Number(av) === Number(bv);
    return String(av).toLowerCase() === String(bv).toLowerCase();
  });
}

// 预览和真气泡用的是同一份主题、同一套换算（theme.js）；
// 这里只把 #bubblePreview 作为变量挂载点，别污染控制面板自己的配色。
function syncSkinControls() {
  if (!skinResolved) return;
  const colors = skinResolved.colors;

  for (const item of SKIN_COLORS) {
    const input = document.getElementById(item.input);
    const hex = document.getElementById(item.hex);
    const value = colors[item.color];
    if (input && input.value !== value) input.value = value;
    if (hex) hex.textContent = value;
  }

  const alpha = document.getElementById('skinAlpha');
  if (alpha) alpha.value = Math.round(skinResolved.alpha * 100);
  const alphaValue = document.getElementById('skinAlphaValue');
  if (alphaValue) alphaValue.textContent = `${Math.round(skinResolved.alpha * 100)}%`;

  const shadow = document.getElementById('skinShadow');
  if (shadow) shadow.value = Math.round(skinResolved.shadow * 100);
  const shadowValue = document.getElementById('skinShadowValue');
  if (shadowValue) shadowValue.textContent = `${Math.round(skinResolved.shadow * 100)}%`;

  for (const [id, color] of [
    ['skinOkDot', colors.ok],
    ['skinWarnDot', colors.warn],
    ['skinPolishDot', colors.polish],
  ]) {
    const dot = document.getElementById(id);
    if (dot) dot.style.background = color;
  }

  for (const btn of document.querySelectorAll('#skinPresets .preset')) {
    const preset = SKIN_PRESETS.find((p) => p.id === btn.dataset.preset);
    const active = !!preset && sameTheme(preset.theme, skinTheme);
    btn.classList.toggle('active', active);
    btn.setAttribute('aria-pressed', active ? 'true' : 'false');
  }
}

function renderSkinPresets() {
  const wrap = document.getElementById('skinPresets');
  if (!wrap) return;
  wrap.replaceChildren();
  for (const preset of SKIN_PRESETS) {
    const resolved = window.Skin ? window.Skin.resolve(preset.theme) : null;
    const dots = ['bg', 'text', 'accent'].map((key) => {
      const dot = document.createElement('i');
      if (resolved) dot.style.background = resolved.colors[key];
      return dot;
    });

    const btn = document.createElement('button');
    btn.type = 'button';
    btn.className = 'preset';
    btn.dataset.preset = preset.id;
    btn.setAttribute('aria-pressed', 'false');
    btn.title = `套用「${preset.name}」配色`;
    const mark = document.createElement('span');
    mark.className = 'preset-dots';
    mark.append(...dots);
    const name = document.createElement('span');
    name.textContent = preset.name;
    btn.append(mark, name);
    btn.addEventListener('click', () => {
      applySkin({ ...preset.theme }, true);
      setSkinHint(`已套用「${preset.name}」，可继续细调`);
    });
    wrap.append(btn);
  }
}

function applySkin(theme, persist = false) {
  if (!window.Skin) return;
  skinTheme = normalizeSkinTheme(theme);
  skinResolved = window.Skin.apply(skinTheme, document.getElementById('bubblePreview'));
  syncSkinControls();
  if (persist) scheduleSkinSave();
}

function scheduleSkinSave() {
  if (skinSaveTimer) clearTimeout(skinSaveTimer);
  setSkinHint('正在保存配色…');
  skinSaveTimer = setTimeout(() => {
    skinSaveTimer = null;
    invoke('save_theme', { theme: skinTheme })
      .then((saved) => {
        // 后端会夹紧数值、丢掉非法色值：以后端返回的为准，别再触发一次保存
        applySkin(saved, false);
        setSkinHint('配色已保存');
      })
      .catch((e) => {
        console.error('保存配色失败', e);
        setSkinHint(`保存失败：${e}`, 'error');
      });
  }, 220);
}

for (const item of SKIN_COLORS) {
  const input = document.getElementById(item.input);
  if (!input) continue;
  input.addEventListener('input', () => {
    applySkin({ ...skinTheme, [item.key]: input.value }, true);
  });
}

document.getElementById('skinAlpha').addEventListener('input', (e) => {
  applySkin({ ...skinTheme, bubbleAlpha: Number(e.target.value) / 100 }, true);
});

document.getElementById('skinShadow').addEventListener('input', (e) => {
  applySkin({ ...skinTheme, bubbleShadow: Number(e.target.value) / 100 }, true);
});

document.getElementById('resetSkin').addEventListener('click', async (ev) => {
  const btn = ev.currentTarget;
  await withBusy(btn, '恢复中…', async () => {
    const saved = await invoke('reset_theme');
    applySkin(saved, false);
    setSkinHint('已恢复默认配色（跟随系统）');
  });
  toast('已恢复默认配色');
});

// 系统浅色/深色切换时，默认配色要跟着变；自定义主题不受影响。
if (window.matchMedia) {
  window.matchMedia('(prefers-color-scheme: dark)').addEventListener('change', () => {
    applySkin(skinTheme, false);
  });
}

async function loadBubbleUi() {
  const ui = await invoke('get_ui');
  document.getElementById('bubbleWidth').value = ui.width;
  document.getElementById('bubbleHeight').value = ui.height;
  document.getElementById('bubbleFont').value = ui.font_size;
  updateBubblePreview();
  applySkin(ui.theme || {}, false);
  document.getElementById('bubbleHint').textContent =
    `当前 ${ui.width} × ${ui.height} px · 字号 ${ui.font_size}`;
}

// 所见即所得：数字一改，预览立刻跟着变，不用「说一句试试」
function updateBubblePreview() {
  const w = Number(document.getElementById('bubbleWidth').value) || 440;
  const h = Number(document.getElementById('bubbleHeight').value) || 100;
  const f = Number(document.getElementById('bubbleFont').value) || 10.5;
  const card = document.getElementById('bpCard');
  if (!card) return;
  card.style.width = `${Math.min(w, 680)}px`;
  card.style.height = `${Math.min(h, 260)}px`;
  card.style.fontSize = `${f}px`;
}

renderSkinPresets();

for (const id of ['bubbleWidth', 'bubbleHeight', 'bubbleFont']) {
  document.getElementById(id).addEventListener('input', updateBubblePreview);
}

document.getElementById('saveBubbleUi').addEventListener('click', async (ev) => {
  const btn = ev.currentTarget;
  const width = Number(document.getElementById('bubbleWidth').value);
  const height = Number(document.getElementById('bubbleHeight').value);
  const fontSize = Number(document.getElementById('bubbleFont').value);
  const ui = await withBusy(btn, '保存中…', () => invoke('save_ui', { width, height, fontSize }));
  document.getElementById('bubbleHint').textContent =
    `已保存 ${ui.width} × ${ui.height} px · 字号 ${ui.font_size}`;
  toast('气泡尺寸已保存');
});

document.getElementById('resetBubbleUi').addEventListener('click', async (ev) => {
  const btn = ev.currentTarget;
  await withBusy(btn, '恢复中…', () =>
    invoke('save_ui', {
      width: BUBBLE_DEFAULTS.width,
      height: BUBBLE_DEFAULTS.height,
      fontSize: BUBBLE_DEFAULTS.font_size,
    }));
  await loadBubbleUi();
  document.getElementById('bubbleHint').textContent = '已恢复默认尺寸';
  toast('已恢复默认尺寸');
});

// ---------------- 触发键与输入模式 ----------------
// 键盘事件 → Windows 虚拟键码。字母 / 数字 / F 键用 keyCode，
// 左右修饰键必须看 code，否则分不出右 Alt 和左 Alt。
const MODIFIER_CODES = {
  ShiftLeft: 0xa0,
  ShiftRight: 0xa1,
  ControlLeft: 0xa2,
  ControlRight: 0xa3,
  AltLeft: 0xa4,
  AltRight: 0xa5,
  MetaLeft: 0x5b,
  MetaRight: 0x5c,
};
const MODIFIER_VKS = new Set([0x10, 0x11, 0x12, 0xa0, 0xa1, 0xa2, 0xa3, 0xa4, 0xa5, 0x5b, 0x5c]);
const VK_NAMES = {
  0x08: '退格',
  0x09: 'Tab',
  0x0d: 'Enter',
  0x13: 'Pause',
  0x14: 'CapsLock',
  0x1b: 'Esc',
  0x20: '空格',
  0x21: 'PageUp',
  0x22: 'PageDown',
  0x23: 'End',
  0x24: 'Home',
  0x25: '←',
  0x26: '↑',
  0x27: '→',
  0x28: '↓',
  0x2c: 'PrintScreen',
  0x2d: 'Insert',
  0x2e: 'Delete',
  0x5b: 'Win',
  0x5c: '右 Win',
  0x5d: '菜单键',
  0x90: 'NumLock',
  0x91: 'ScrollLock',
  0xa0: 'Shift',
  0xa1: '右 Shift',
  0xa2: 'Ctrl',
  0xa3: '右 Ctrl',
  0xa4: 'Alt',
  0xa5: '右 Alt',
};
const VK_PUNCT = {
  0x6a: '小键盘*',
  0x6b: '小键盘+',
  0x6d: '小键盘-',
  0x6e: '小键盘.',
  0x6f: '小键盘/',
  0xba: ';',
  0xbb: '=',
  0xbc: ',',
  0xbd: '-',
  0xbe: '.',
  0xbf: '/',
  0xc0: '`',
  0xdb: '[',
  0xdc: '\\',
  0xdd: ']',
  0xde: "'",
};

function keyName(vk) {
  if (VK_NAMES[vk]) return VK_NAMES[vk];
  if (vk >= 0x41 && vk <= 0x5a) return String.fromCharCode(vk);
  if (vk >= 0x30 && vk <= 0x39) return String.fromCharCode(vk);
  if (vk >= 0x60 && vk <= 0x69) return `小键盘${vk - 0x60}`;
  if (vk >= 0x70 && vk <= 0x87) return `F${vk - 0x70 + 1}`;
  if (VK_PUNCT[vk]) return VK_PUNCT[vk];
  return `VK${vk.toString(16).toUpperCase().padStart(2, '0')}`;
}

function isModifierVk(vk) {
  return MODIFIER_VKS.has(vk);
}

function vkFromEvent(e) {
  if (MODIFIER_CODES[e.code]) return MODIFIER_CODES[e.code];
  return e.keyCode || 0;
}

// 和 Rust 侧保持同一个顺序：Ctrl → Shift → Alt → Win，普通键放最后。
function normalizeKeys(keys) {
  const mods = [];
  let primary = null;
  for (const raw of keys) {
    const vk = raw === 0x10 ? 0xa0 : raw === 0x11 ? 0xa2 : raw === 0x12 ? 0xa4 : raw;
    if (isModifierVk(vk)) {
      if (!mods.includes(vk)) mods.push(vk);
    } else if (primary === null) {
      primary = vk;
    }
  }
  const rank = (vk) =>
    vk === 0xa2 || vk === 0xa3 ? 0 : vk === 0xa0 || vk === 0xa1 ? 1 : vk === 0xa4 || vk === 0xa5 ? 2 : 3;
  mods.sort((a, b) => rank(a) - rank(b));
  return primary === null ? mods : [...mods, primary];
}

function keysLabel(keys) {
  return keys.map(keyName).join('+');
}

let trigger = {
  keys: [0xa5],
  mode: 'hold',
  long_press_ms: 0,
  label: '右 Alt',
  hint: '',
};
let capturing = false;
let capturePending = false;
let captureSeenMods = new Set();
let captureHeldMods = [];
let captureTimer = 0;
let triggerCheckSeq = 0;

const captureBtn = document.getElementById('captureKey');
const saveTriggerBtn = document.getElementById('saveTrigger');
const triggerHint = document.getElementById('triggerHint');

function setTriggerMessage(text, kind = '') {
  if (!triggerHint) return;
  triggerHint.textContent = text;
  triggerHint.className = `hint${kind ? ` ${kind}` : ''}`;
}

function updateTriggerUI() {
  document.querySelectorAll('#modeSeg .seg').forEach((btn) => {
    const on = btn.dataset.mode === trigger.mode;
    btn.classList.toggle('active', on);
    btn.setAttribute('aria-checked', String(on));
  });

  const longPress = document.getElementById('longPress');
  if (longPress) {
    const value = String(trigger.long_press_ms || 0);
    if (![...longPress.options].some((o) => o.value === value)) {
      const custom = document.createElement('option');
      custom.value = value;
      custom.textContent = `${value} ms`;
      longPress.append(custom);
    }
    longPress.value = value;
  }

  const label = trigger.label || keysLabel(trigger.keys);
  if (captureBtn) {
    captureBtn.textContent = capturing ? '请按下快捷键…（Esc 取消）' : label;
    captureBtn.classList.toggle('listening', capturing);
  }

  const guideKey = document.getElementById('guideKey');
  if (guideKey) guideKey.textContent = label;
  const how = document.getElementById('guideHow');
  const then = document.getElementById('guideThen');
  if (how) {
    how.textContent =
      trigger.mode === 'toggle' ? '按一下' : trigger.long_press_ms > 0 ? '长按' : '按住';
  }
  if (then) {
    then.textContent =
      trigger.mode === 'toggle' ? '开始说，说完再按一下或 Esc' : '说一句，松手';
  }
}

async function loadTriggerConfig() {
  if (capturing) return;
  triggerCheckSeq += 1;
  try {
    const t = await invoke('get_trigger');
    trigger = t;
    updateTriggerUI();
    setTriggerMessage(t.hint || '', '');
    if (saveTriggerBtn) saveTriggerBtn.disabled = false;
  } catch (e) {
    setTriggerMessage(`读取触发键失败：${e}`, 'error');
  }
}

function onCaptureBlur() {
  cancelCapture('窗口失去焦点，已取消录入');
}

function onCaptureKeyDown(e) {
  if (!capturing) return;
  e.preventDefault();
  e.stopPropagation();

  if (e.key === 'Escape') {
    cancelCapture('已取消录入');
    return;
  }
  if (e.repeat) return;

  const vk = vkFromEvent(e);
  if (!vk) return;

  if (isModifierVk(vk)) {
    captureSeenMods.add(vk);
    if (!captureHeldMods.includes(vk)) captureHeldMods.push(vk);
    if (captureBtn) captureBtn.textContent = `${keysLabel(normalizeKeys(captureHeldMods))} + …`;
    return;
  }
  finishCapture(normalizeKeys([...captureHeldMods, vk]));
}

function onCaptureKeyUp(e) {
  if (!capturing) return;
  e.preventDefault();
  e.stopPropagation();

  const vk = vkFromEvent(e);
  if (!vk) return;
  captureHeldMods = captureHeldMods.filter((k) => k !== vk);

  // 只按了一个修饰键：松开时把它记成单键（右 Alt 就是这样用的）。
  if (captureHeldMods.length === 0 && captureSeenMods.size > 0) {
    if (captureSeenMods.size === 1) {
      finishCapture(normalizeKeys([...captureSeenMods]));
    } else {
      captureSeenMods.clear();
      if (captureBtn) captureBtn.textContent = '请按下快捷键…（Esc 取消）';
      setTriggerMessage('请重新按住修饰键，再按一个普通键（例如 Space 或 F8）', 'error');
    }
  }
}

async function beginCapture() {
  if (capturing || capturePending) return;
  capturePending = true;
  try {
    await invoke('set_capture_mode', { on: true });
  } catch {
    // 挂起失败也继续：至少别让按钮看起来没反应
  }
  capturePending = false;

  capturing = true;
  triggerCheckSeq += 1;
  captureSeenMods = new Set();
  captureHeldMods = [];
  updateTriggerUI();
  setTriggerMessage('现在按下你想要的快捷键…（Esc 取消）', '');
  window.addEventListener('keydown', onCaptureKeyDown, true);
  window.addEventListener('keyup', onCaptureKeyUp, true);
  window.addEventListener('blur', onCaptureBlur, true);
  clearTimeout(captureTimer);
  captureTimer = setTimeout(() => cancelCapture('录入超时，再点一次'), 15000);
  if (captureBtn) captureBtn.focus();
}

function endCapture(resume = true) {
  if (!capturing) return;
  capturing = false;
  captureHeldMods = [];
  clearTimeout(captureTimer);
  window.removeEventListener('keydown', onCaptureKeyDown, true);
  window.removeEventListener('keyup', onCaptureKeyUp, true);
  window.removeEventListener('blur', onCaptureBlur, true);
  if (resume) {
    invoke('set_capture_mode', { on: false }).catch(() => {});
  }
  updateTriggerUI();
}

function cancelCapture(message = '已取消录入') {
  endCapture(true);
  setTriggerMessage(message, '');
}

function finishCapture(keys) {
  endCapture(true);
  trigger.keys = normalizeKeys(keys);
  trigger.label = keysLabel(trigger.keys);
  updateTriggerUI();

  if (saveTriggerBtn) saveTriggerBtn.disabled = true;
  setTriggerMessage('正在检查是否被占用…', '');
  const seq = ++triggerCheckSeq;
  invoke('check_trigger', { keys: trigger.keys })
    .then((res) => {
      if (seq !== triggerCheckSeq) return;
      trigger.keys = res.keys || trigger.keys;
      trigger.label = res.label || keysLabel(trigger.keys);
      updateTriggerUI();
      if (res.ok) {
        setTriggerMessage('可以使用，点「保存输入设置」生效', 'ok');
        if (saveTriggerBtn) saveTriggerBtn.disabled = false;
      } else {
        setTriggerMessage(res.message || '这个快捷键不可用', 'error');
        if (saveTriggerBtn) saveTriggerBtn.disabled = true;
      }
    })
    .catch((err) => {
      if (seq !== triggerCheckSeq) return;
      setTriggerMessage(`检查失败：${err}`, 'error');
      if (saveTriggerBtn) saveTriggerBtn.disabled = true;
    });
}

async function saveTriggerConfig() {
  if (capturing) {
    cancelCapture('请先完成快捷键录入');
    return;
  }
  const longPress = Number(document.getElementById('longPress').value) || 0;
  saveTriggerBtn.disabled = true;
  try {
    const t = await invoke('save_trigger', {
      keys: trigger.keys,
      mode: trigger.mode,
      longPressMs: longPress,
    });
    trigger = t;
    updateTriggerUI();
    setTriggerMessage(`已保存：${t.hint}`, 'ok');
    toast('输入设置已保存');
    loadDiagnostics();
  } catch (e) {
    setTriggerMessage(String(e), 'error');
    toast(`保存失败：${e}`, 'error');
  } finally {
    if (saveTriggerBtn) saveTriggerBtn.disabled = false;
  }
}

async function resetTriggerConfig() {
  if (capturing) cancelCapture();
  triggerCheckSeq += 1;
  trigger = {
    keys: [0xa5],
    mode: 'hold',
    long_press_ms: 0,
    label: '右 Alt',
    hint: '',
  };
  updateTriggerUI();
  await saveTriggerConfig();
}

if (captureBtn) {
  captureBtn.addEventListener('click', () => {
    if (capturing) cancelCapture();
    else beginCapture();
  });
}

document.querySelectorAll('#modeSeg .seg').forEach((btn) => {
  btn.addEventListener('click', () => {
    if (capturing) return;
    trigger.mode = btn.dataset.mode;
    updateTriggerUI();
    setTriggerMessage('点「保存输入设置」生效', '');
  });
});

document.getElementById('longPress').addEventListener('change', (e) => {
  trigger.long_press_ms = Number(e.target.value) || 0;
  updateTriggerUI();
  setTriggerMessage('点「保存输入设置」生效', '');
});

if (saveTriggerBtn) saveTriggerBtn.addEventListener('click', saveTriggerConfig);
const resetTriggerBtn = document.getElementById('resetTrigger');
if (resetTriggerBtn) resetTriggerBtn.addEventListener('click', resetTriggerConfig);

// ---------------- 模型切换 ----------------
let modelState = {
  asr: { models: [] },
  polish: { active: '', profiles: [] },
};
let editingProfileId = null;
let modelsBusy = false;
let deleteProfileId = null;
let deleteProfileTimer = 0;

function setAsrHint(text, kind = '') {
  const node = document.getElementById('asrHint');
  if (!node) return;
  node.textContent = text;
  node.className = `hint model-tip${kind ? ` ${kind}` : ''}`;
}

function setModelsHint(text, kind = '') {
  const node = document.getElementById('modelsHint');
  if (!node) return;
  node.textContent = text;
  node.className = `hint${kind ? ` ${kind}` : ''}`;
}

function setProfileHint(text, kind = '') {
  const node = document.getElementById('profileHint');
  if (!node) return;
  node.textContent = text;
  node.className = `hint${kind ? ` ${kind}` : ''}`;
}

function profilePayload(profile) {
  return {
    id: profile.id || '',
    name: profile.name || '',
    provider: profile.provider || 'ollama',
    url: profile.url || '',
    key: profile.key || '',
    model: profile.model || '',
    timeout_ms: Number(profile.timeout_ms) || 900,
  };
}

async function loadModels() {
  try {
    const data = await invoke('get_models');
    modelState = data;
    renderAsrModels();
    renderPolishProfiles();
    setModelsHint('');
    return data;
  } catch (e) {
    setModelsHint(`读取模型配置失败：${e}`, 'error');
    return null;
  }
}

function renderAsrModels() {
  const asr = modelState.asr || {};
  document.getElementById('asrCurrent').textContent = asr.label || '—';
  document.getElementById('asrDir').textContent = asr.dir || '—';

  const host = document.getElementById('asrModels');
  host.replaceChildren();
  for (const model of asr.models || []) {
    const row = el('div', `model-row${model.active ? ' active' : ''}`);
    const main = el('div', 'model-main');
    const head = el('div', 'model-head');
    head.append(el('strong', '', model.label));
    if (model.active) head.append(el('span', 'badge active', '使用中'));
    if (!model.available) head.append(el('span', 'badge', '未安装'));
    else if (model.note === '自定义目录') head.append(el('span', 'badge', '自定义'));
    main.append(head, el('p', 'model-meta', model.dir));

    const actions = el('div', 'model-actions');
    if (!model.active && model.available) {
      const button = el('button', 'mini', '切换');
      button.type = 'button';
      button.addEventListener('click', () => switchAsrModel(model));
      actions.append(button);
    }
    row.append(main, actions);
    host.append(row);
  }
}

async function switchAsrModel(model) {
  if (modelsBusy || model.active) return;
  if (!model.available) {
    toast(`${model.label} 的模型文件不完整`, 'error');
    return;
  }
  modelsBusy = true;
  setAsrHint(`正在加载 ${model.label}…`, '');
  document.querySelectorAll('#asrModels button').forEach((b) => {
    b.disabled = true;
  });
  try {
    const data = await invoke('switch_asr', { engine: model.engine });
    modelState = data;
    renderAsrModels();
    renderPolishProfiles();
    setAsrHint(`已切换到 ${model.label}`, 'ok');
    toast(`识别模型：${model.label}`);
    loadState().catch(() => {});
    loadDiagnostics();
  } catch (e) {
    setAsrHint(String(e), 'error');
    toast(`切换失败：${e}`, 'error');
    renderAsrModels();
  } finally {
    modelsBusy = false;
  }
}

function renderPolishProfiles() {
  const polish = modelState.polish || { active: '', profiles: [] };
  const host = document.getElementById('polishProfiles');
  host.replaceChildren();
  const active = polish.active || '';

  if (!(polish.profiles || []).length) {
    host.append(el('div', 'model-empty', '还没有润色配置，点右上角「新增配置」添加本地 Ollama 或 API。'));
    return;
  }

  for (const profile of polish.profiles) {
    const isActive = profile.id === active;
    const row = el('div', `model-row${isActive ? ' active' : ''}`);
    const main = el('div', 'model-main');
    const head = el('div', 'model-head');
    head.append(el('strong', '', profile.name || profile.model || '未命名配置'));
    if (isActive) head.append(el('span', 'badge active', '使用中'));
    head.append(
      el(
        'span',
        `badge ${profile.provider === 'ollama' ? 'local' : 'api'}`,
        profile.provider === 'ollama' ? '本地' : 'API',
      ),
    );
    main.append(
      head,
      el('p', 'model-meta', `${profile.model} · ${profile.url}`),
    );

    const actions = el('div', 'model-actions');
    if (!isActive) {
      const use = el('button', 'mini', '使用');
      use.type = 'button';
      use.addEventListener('click', () => activatePolishProfile(profile.id));
      actions.append(use);
    }

    const test = el('button', 'mini', '测试');
    test.type = 'button';
    test.addEventListener('click', async () => {
      setModelsHint(`正在测试「${profile.name}」…`, '');
      test.disabled = true;
      try {
        const r = await invoke('test_polish_profile', {
          profile: profilePayload(profile),
        });
        setModelsHint(`「${profile.name}」连接成功 · ${r.ms}ms · 回复：${r.reply}`, 'ok');
      } catch (e) {
        setModelsHint(`「${profile.name}」连接失败：${e}`, 'error');
      } finally {
        test.disabled = false;
      }
    });

    const edit = el('button', 'mini', '编辑');
    edit.type = 'button';
    edit.addEventListener('click', () => openProfileForm(profile));

    const del = el('button', 'mini delete', '删除');
    del.type = 'button';
    del.addEventListener('click', () => {
      if (deleteProfileId === profile.id) {
        clearTimeout(deleteProfileTimer);
        deleteProfileId = null;
        removePolishProfile(profile);
        return;
      }
      deleteProfileId = profile.id;
      del.textContent = '确认删除';
      del.classList.add('danger');
      clearTimeout(deleteProfileTimer);
      deleteProfileTimer = setTimeout(() => {
        deleteProfileId = null;
        renderPolishProfiles();
      }, 3000);
    });

    actions.append(test, edit, del);
    row.append(main, actions);
    host.append(row);
  }
}

async function persistPolishProfiles(profiles, active, message) {
  if (modelsBusy) return false;
  modelsBusy = true;
  setModelsHint('保存中…', '');
  try {
    const data = await invoke('save_polish_profiles', {
      profiles: profiles.map(profilePayload),
      active: active || '',
    });
    modelState = data;
    renderAsrModels();
    renderPolishProfiles();
    setModelsHint(message || '已保存', 'ok');
    toast(message || '润色配置已更新');
    loadState().catch(() => {});
    loadDiagnostics();
    return true;
  } catch (e) {
    setModelsHint(String(e), 'error');
    toast(`保存失败：${e}`, 'error');
    renderPolishProfiles();
    return false;
  } finally {
    modelsBusy = false;
  }
}

async function activatePolishProfile(id) {
  await persistPolishProfiles(
    modelState.polish.profiles,
    id,
    `已切换到「${(modelState.polish.profiles.find((p) => p.id === id) || {}).name || id}」`,
  );
}

async function removePolishProfile(profile) {
  const profiles = (modelState.polish.profiles || []).filter((p) => p.id !== profile.id);
  let active = modelState.polish.active;
  if (active === profile.id) active = profiles[0]?.id || '';
  await persistPolishProfiles(profiles, active, `已删除「${profile.name}」`);
}

const PROFILE_DEFAULTS = {
  ollama: 'http://127.0.0.1:11434/v1',
  openai: 'https://api.deepseek.com/v1',
};

function updateProfileProviderHints() {
  const provider = document.getElementById('profileProvider').value;
  const url = document.getElementById('profileUrl');
  const other = provider === 'ollama' ? PROFILE_DEFAULTS.openai : PROFILE_DEFAULTS.ollama;
  const current = url.value.trim();
  if (!current || current === other) url.value = PROFILE_DEFAULTS[provider];
  document.getElementById('profileModelHint').textContent =
    provider === 'ollama'
      ? '例如 qwen3.5:2b；可点「刷新本地模型」'
      : '例如 deepseek-chat / gpt-4o-mini';
  document.getElementById('profileKey').placeholder =
    provider === 'ollama' ? '本地 Ollama 可留空' : 'sk-…';
}

function openProfileForm(profile) {
  editingProfileId = profile ? profile.id : null;
  document.getElementById('polishFormTitle').textContent = profile
    ? '编辑润色配置'
    : '新增润色配置';
  document.getElementById('profileName').value = profile?.name || '';
  document.getElementById('profileProvider').value = profile?.provider || 'ollama';
  document.getElementById('profileUrl').value =
    profile?.url || PROFILE_DEFAULTS[profile?.provider || 'ollama'];
  document.getElementById('profileModel').value = profile?.model || '';
  document.getElementById('profileKey').value = profile?.key || '';
  document.getElementById('profileTimeout').value = profile?.timeout_ms || 900;
  updateProfileProviderHints();
  setProfileHint('');
  const form = document.getElementById('polishForm');
  form.hidden = false;
  form.scrollIntoView({ behavior: 'smooth', block: 'start' });
  if (!profile) document.getElementById('profileName').focus();
}

function closeProfileForm() {
  editingProfileId = null;
  document.getElementById('polishForm').hidden = true;
  setProfileHint('');
}

function profileFromForm() {
  const name = document.getElementById('profileName').value.trim();
  const provider = document.getElementById('profileProvider').value;
  const url = document.getElementById('profileUrl').value.trim();
  const model = document.getElementById('profileModel').value.trim();
  const key = document.getElementById('profileKey').value.trim();
  const timeout = Number(document.getElementById('profileTimeout').value) || 900;
  if (!url) {
    setProfileHint('先填接口地址', 'error');
    return null;
  }
  if (!model) {
    setProfileHint('先填模型名', 'error');
    return null;
  }
  return {
    id: editingProfileId || `profile-${Date.now().toString(36)}`,
    name: name || model,
    provider,
    url,
    key,
    model,
    timeout_ms: timeout,
  };
}

async function saveProfileForm() {
  const profile = profileFromForm();
  if (!profile) return;
  const profiles = [...(modelState.polish.profiles || [])];
  const index = profiles.findIndex((p) => p.id === profile.id);
  if (index >= 0) profiles[index] = profile;
  else profiles.push(profile);
  let active = modelState.polish.active;
  if (!active) active = profile.id;
  const ok = await persistPolishProfiles(profiles, active, `已保存「${profile.name}」`);
  if (ok) closeProfileForm();
}

async function testProfileForm() {
  const profile = profileFromForm();
  if (!profile) return;
  const button = document.getElementById('testPolishProfile');
  button.disabled = true;
  setProfileHint('连接中…', '');
  try {
    const r = await invoke('test_polish_profile', {
      profile: profilePayload(profile),
    });
    setProfileHint(`连接成功 · ${r.ms}ms · 回复：${r.reply}`, 'ok');
  } catch (e) {
    setProfileHint(String(e), 'error');
  } finally {
    button.disabled = false;
  }
}

async function refreshOllamaModels() {
  const formUrl = document.getElementById('profileUrl').value.trim();
  const provider = document.getElementById('profileProvider').value;
  const firstLocal = (modelState.polish.profiles || []).find(
    (p) => p.provider === 'ollama',
  );
  // 正在编辑 API 配置时，别拿 API 地址去问 Ollama
  const url =
    provider === 'ollama'
      ? formUrl || PROFILE_DEFAULTS.ollama
      : firstLocal?.url || PROFILE_DEFAULTS.ollama;
  setModelsHint('正在读取本地 Ollama 模型…', '');
  try {
    const names = await invoke('list_ollama_models', { url });
    const datalist = document.getElementById('ollamaModels');
    datalist.replaceChildren();
    for (const name of names) {
      const option = document.createElement('option');
      option.value = name;
      datalist.append(option);
    }
    setModelsHint(
      names.length ? `找到 ${names.length} 个本地模型，可在「模型名」里下拉选择` : 'Ollama 里还没有模型',
      names.length ? 'ok' : '',
    );
  } catch (e) {
    setModelsHint(String(e), 'error');
  }
}

document.getElementById('addPolishProfile').addEventListener('click', () => openProfileForm(null));
document.getElementById('refreshOllama').addEventListener('click', refreshOllamaModels);
document.getElementById('savePolishProfile').addEventListener('click', saveProfileForm);
document.getElementById('cancelPolishProfile').addEventListener('click', closeProfileForm);
document.getElementById('testPolishProfile').addEventListener('click', testProfileForm);
document.getElementById('profileProvider').addEventListener('change', updateProfileProviderHints);

// ---------------- 启动 ----------------
// 托盘切了开关，面板要跟着变（Rust 侧只发事件，不直接改 DOM）
window.__TAURI__.event
  .listen('state-changed', (e) => applyState(e.payload))
  .catch((e) => console.error('订阅状态事件失败', e));

// 触发键可能被别处（托盘/另一个面板）改了：保持界面同步。
window.__TAURI__.event
  .listen('trigger-changed', (e) => {
    if (capturing) return;
    trigger = e.payload;
    updateTriggerUI();
    setTriggerMessage(e.payload.hint || '', '');
    if (saveTriggerBtn) saveTriggerBtn.disabled = false;
  })
  .catch((e) => console.error('订阅触发键事件失败', e));

// 配色可能在别处被改（托盘重置、以后的多面板）：预览跟着走。
// 自己保存时后端也会回发这一条，所以不要再触发保存。
window.__TAURI__.event
  .listen('theme', (e) => applySkin(e.payload, false))
  .catch((e) => console.error('订阅配色事件失败', e));

// ---------------- 概览：就绪状态 ----------------
function setDot(id, cls) {
  const d = document.getElementById(id);
  const next = `dot ${cls}`.trim();
  if (d && d.className !== next) d.className = next;
}

// 只在内容变了才动 DOM：面板每 8 秒轮询一次，无谓的文本替换会让读屏和
// UI 自动化一直看到「树在变」，也白白触发重绘。
function setText(id, text) {
  const el = document.getElementById(id);
  if (el && el.textContent !== text) el.textContent = text;
}

async function loadDiagnostics() {
  try {
    const d = await invoke('get_diagnostics');

    setDot('dotMic', d.mic ? 'ok' : 'err');
    setText('txtMic', d.mic ? '可用' : '打不开');
    const retry = document.getElementById('retryMic');
    if (retry && retry.hidden !== d.mic) retry.hidden = d.mic;

    if (d.placeholder) {
      setDot('dotAsr', 'err');
      setText('txtAsr', '占位识别器');
    } else if (!d.asr_available) {
      setDot('dotAsr', 'err');
      setText('txtAsr', '缺文件');
    } else if (d.asr === 'ready') {
      setDot('dotAsr', 'ok');
      setText('txtAsr', '就绪');
    } else if (d.asr === 'busy') {
      setDot('dotAsr', 'wait');
      setText('txtAsr', '识别中');
    } else {
      setDot('dotAsr', 'wait');
      setText('txtAsr', '预热中');
    }

    setDot('dotHotkey', d.hotkey ? 'ok' : 'err');
    setText(
      'txtHotkey',
      d.hotkey ? `${d.hotkey_label} · ${d.hotkey_mode}` : '注册失败 / 被占用',
    );

    setDot('dotPolish', d.deep ? 'ok' : 'warn');
    setText('txtPolish', d.deep ? '深度可用' : '仅标准档');
  } catch (e) {
    console.error('读取诊断状态失败', e);
  }
}

async function loadGuideVisibility() {
  const guide = document.getElementById('guideCard');
  if (!guide) return;
  try {
    const data = await invoke('get_history', { date: null });
    // 已经有历史记录就不再占着设置页顶部；没有记录时显示三步引导
    guide.hidden = (data.days || []).length > 0;
  } catch {
    guide.hidden = false;
  }
}

document.getElementById('retryMic').addEventListener('click', async (ev) => {
  const ok = await withBusy(ev.currentTarget, '重连中…', () => invoke('retry_audio'));
  toast(ok ? '麦克风已恢复' : '还是打不开，检查系统权限或占用', ok ? '' : 'error');
  loadDiagnostics();
});

// ---------------- 开发者模式：日志 ----------------
const devPanel = document.getElementById('devPanel');

async function loadLogs() {
  const view = document.getElementById('logView');
  try {
    const lines = await invoke('get_logs', { limit: 300 });
    view.textContent = lines.length ? lines.join('\n') : '（还没有日志）';
    view.scrollTop = view.scrollHeight;
    document.getElementById('logHint').textContent = `最近 ${lines.length} 行 · 只在内存里`;
  } catch (e) {
    view.textContent = `读取日志失败：${e}`;
  }
}

devPanel.addEventListener('toggle', () => {
  if (devPanel.open) loadLogs();
});
document.getElementById('logRefresh').addEventListener('click', loadLogs);
document.getElementById('logCopy').addEventListener('click', () => {
  copyText(document.getElementById('logView').textContent);
});
document.getElementById('logClear').addEventListener('click', async () => {
  await invoke('clear_logs');
  loadLogs();
  toast('日志已清空');
});

// 退出：Rust 侧会先卸载本地润色模型再结束进程
document.getElementById('quitApp').addEventListener('click', () => {
  invoke('quit_app');
});

loadState().catch((e) => console.error('控制面板初始化失败', e));
loadTriggerConfig();
loadBubbleUi().catch((e) => console.error('读取气泡外观失败', e));
loadModels();
// 版本号从 Cargo.toml 来（get_version），不在 HTML 里手写：
// 以前侧边栏写死 v0.2，发版时界面和 exe 就对不上。
invoke('get_version')
  .then((v) => {
    const el = document.getElementById('appVersion');
    if (el && v) el.textContent = 'v' + v;
  })
  .catch(() => {});
invoke('get_flags')
  .then((f) => {
    if (f && f.reduced_motion) document.documentElement.classList.add('reduced-motion');
  })
  .catch(() => {});
loadDiagnostics();
loadGuideVisibility();
setInterval(() => {
  // 隐藏时不要刷新 DOM：既省电，也避免自动化/读屏一直看到树在变
  if (document.hidden) return;
  loadDiagnostics();
}, 8000);
