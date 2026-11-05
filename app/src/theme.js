// 皮肤：把「配色主题」换算成一组 CSS 变量。
//
// 三个窗口（气泡 / 指示器 / 控制面板预览）共用这一份，颜色算法只此一处。
// 主题里没填的字段按**气泡背景的明暗**自动配，所以只改背景也不会出现黑底黑字：
//   - 深底 → 浅色正文、浅色状态色；浅底 → 深色正文、深色状态色
//   - 未定稿灰字 / 次级文字由正文色和背景混合得到，不需要用户再挑一遍
//
// 字段名与 src-tauri/src/ui.rs 的 Theme 一致（camelCase）：
//   bubbleBg bubbleAlpha bubbleText bubbleTail bubbleBorder bubbleShadow
//   accent ok warn polish
// 颜色只认 #rgb / #rrggbb；后端也会校验并归一化，非法值会退回自动配色。

(function () {
  // 浅底 / 深底两套参考色。默认主题（字段全空）等价于 tokens.css 现有的浅色 / 深色。
  const LIGHT = {
    text: '#111827',
    accent: '#2563eb',
    ok: '#059669',
    warn: '#d97706',
    polish: '#7c3aed',
  };
  const DARK = {
    text: '#e5e7eb',
    accent: '#60a5fa',
    ok: '#34d399',
    warn: '#fbbf24',
    polish: '#a78bfa',
  };
  const FALLBACK = {
    lightBg: { r: 255, g: 255, b: 255 },
    darkBg: { r: 24, g: 26, b: 32 },
    white: { r: 255, g: 255, b: 255 },
    black: { r: 0, g: 0, b: 0 },
    inkDark: { r: 17, g: 24, b: 39 },
  };
  const THEME_KEYS = [
    'bubbleBg',
    'bubbleAlpha',
    'bubbleText',
    'bubbleTail',
    'bubbleBorder',
    'bubbleShadow',
    'accent',
    'ok',
    'warn',
    'polish',
  ];

  function clamp(v, lo, hi) {
    return Math.max(lo, Math.min(hi, v));
  }
  function round2(v) {
    return Math.round(v * 100) / 100;
  }
  function parseHex(value) {
    if (typeof value !== 'string') return null;
    const m = value.trim().match(/^#?([0-9a-f]{3}|[0-9a-f]{6})$/i);
    if (!m) return null;
    let h = m[1];
    if (h.length === 3) h = h.split('').map((c) => c + c).join('');
    return {
      r: parseInt(h.slice(0, 2), 16),
      g: parseInt(h.slice(2, 4), 16),
      b: parseInt(h.slice(4, 6), 16),
    };
  }
  function toHex(c) {
    const part = (v) => Math.round(clamp(v, 0, 255)).toString(16).padStart(2, '0');
    return `#${part(c.r)}${part(c.g)}${part(c.b)}`;
  }
  function rgba(c, alpha) {
    return `rgba(${Math.round(c.r)}, ${Math.round(c.g)}, ${Math.round(c.b)}, ${round2(alpha)})`;
  }
  // t 是 a 的比例：mix(a, b, 0.3) = 30% a + 70% b
  function mix(a, b, t) {
    return {
      r: a.r * t + b.r * (1 - t),
      g: a.g * t + b.g * (1 - t),
      b: a.b * t + b.b * (1 - t),
    };
  }
  function shift(color, amount) {
    return amount >= 0
      ? mix(FALLBACK.white, color, amount)
      : mix(FALLBACK.black, color, -amount);
  }
  // WCAG 相对亮度；只用来判断「这个底色上该配深字还是浅字」。
  function luminance(c) {
    const f = (v) => {
      const s = v / 255;
      return s <= 0.03928 ? s / 12.92 : Math.pow((s + 0.055) / 1.055, 2.4);
    };
    return 0.2126 * f(c.r) + 0.7152 * f(c.g) + 0.0722 * f(c.b);
  }
  // 亮底用深字。0.179 是「黑字/白字哪个对比度更高」的分界（WCAG 公式反推）。
  function prefersDarkInk(bg) {
    return luminance(bg) > 0.179;
  }
  function systemDark() {
    return !!(
      window.matchMedia && window.matchMedia('(prefers-color-scheme: dark)').matches
    );
  }

  // 把主题换算成最终颜色与 CSS 变量。设置面板也用它来显示色块的真实色值。
  function resolve(theme) {
    const t = theme || {};
    const darkSystem = systemDark();
    const bg = parseHex(t.bubbleBg) || (darkSystem ? FALLBACK.darkBg : FALLBACK.lightBg);
    const darkBg = !prefersDarkInk(bg);
    const palette = darkBg ? DARK : LIGHT;

    const text = parseHex(t.bubbleText) || parseHex(palette.text);
    // 灰字默认是正文色压在背景上的混合色（比直接给透明度更好取色）：
    // 深色底色用 42%，浅色底色用 38%，和 tokens.css 的色阶一致。
    const tailAlpha = darkBg ? 0.42 : 0.38;
    const tail = parseHex(t.bubbleTail) || mix(text, bg, tailAlpha);
    const muted = mix(text, bg, darkBg ? 0.72 : 0.65);

    const alpha =
      typeof t.bubbleAlpha === 'number' ? clamp(t.bubbleAlpha, 0.15, 1) : darkBg ? 0.9 : 0.88;
    // 自定义描边按用户选的原色；没选时沿用现在的白色高光（深底更弱）。
    const borderAlpha = darkBg ? 0.08 : 0.55;
    const border = parseHex(t.bubbleBorder);
    const borderCss = border ? toHex(border) : rgba(FALLBACK.white, borderAlpha);
    const borderShown = border || mix(FALLBACK.white, bg, borderAlpha);

    const shadowStrength =
      typeof t.bubbleShadow === 'number' ? clamp(t.bubbleShadow, 0, 2) : 1;
    const shadow =
      shadowStrength <= 0.01
        ? 'none'
        : darkBg
          ? `0 8px 24px rgba(0, 0, 0, ${round2(0.45 * shadowStrength)})`
          : `0 6px 20px rgba(15, 23, 42, ${round2(0.13 * shadowStrength)}), 0 1px 2px rgba(15, 23, 42, ${round2(0.06 * shadowStrength)})`;

    const accent = parseHex(t.accent) || parseHex(palette.accent);
    const ok = parseHex(t.ok) || parseHex(palette.ok);
    const warn = parseHex(t.warn) || parseHex(palette.warn);
    const polish = parseHex(t.polish) || parseHex(palette.polish);

    const onOk = prefersDarkInk(ok) ? toHex(FALLBACK.inkDark) : '#ffffff';

    const vars = {
      // 正文 / 灰字 / 次级文字
      '--text': toHex(text),
      '--text-tail': toHex(tail),
      '--text-muted': toHex(muted),
      // 气泡
      '--bubble-bg': rgba(bg, alpha),
      '--bubble-border': borderCss,
      '--bubble-shadow': shadow,
      // 指示器
      '--accent': toHex(accent),
      '--accent-strong': toHex(shift(accent, darkBg ? 0.18 : -0.12)),
      '--accent-ring': rgba(accent, 0.85),
      '--accent-halo': rgba(accent, 0.35),
      '--accent-halo-0': rgba(accent, 0),
      '--ok': toHex(ok),
      '--ok-ring': rgba(ok, 0.55),
      '--ok-glow': rgba(ok, 0.28),
      '--ok-ring-ind': rgba(ok, 0.8),
      '--on-ok': onOk,
      '--warn': toHex(warn),
      '--warn-ring': rgba(warn, 0.7),
      '--polish': toHex(polish),
      '--polish-soft': rgba(polish, 0.35),
    };

    return {
      systemDark: darkSystem,
      darkBg,
      alpha: round2(alpha),
      shadow: round2(shadowStrength),
      colors: {
        bg: toHex(bg),
        text: toHex(text),
        tail: toHex(tail),
        border: toHex(borderShown),
        accent: toHex(accent),
        ok: toHex(ok),
        warn: toHex(warn),
        polish: toHex(polish),
      },
      vars,
    };
  }

  // 应用到一个元素上（默认整个页面）。三个窗口各调一次即可。
  function apply(theme, target) {
    const resolved = resolve(theme);
    const el = target || document.documentElement;
    for (const key of Object.keys(resolved.vars)) {
      el.style.setProperty(key, resolved.vars[key]);
    }
    return resolved;
  }

  window.Skin = { resolve, apply, parse: parseHex, keys: THEME_KEYS };
  // 给 Rust 侧的 eval 留一个稳定入口（页面还没绑定事件时也能用）。
  window.__applyTheme = apply;
  window.__resolveTheme = resolve;
})();
