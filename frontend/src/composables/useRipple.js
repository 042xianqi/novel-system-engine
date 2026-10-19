// useRipple.js —— 全局点击涟漪
//
// 规范要求「所有按钮和列表行都有物理确认感」。
// 两种用法：
//   ① 组合式：const { onTap } = useRipple(); <button @pointerdown="onTap">
//   ② 全自动：installRipple() 在 App 挂载后调一次，
//      用事件委托监听整个文档，凡是带 .ripple-host / .btn / .cell 的元素
//      点一下自动出涟漪 —— 这样老组件不用改一行也能有物理确认感。
//
// 实现要点：
//   · 用 pointerdown 而不是 click：手指按下去的瞬间就该有反馈，
//     等 click 要等抬手，晚了 100ms 就不跟手了。
//   · 涟漪直径取「元素对角线」，保证从任何位置点都能铺满整个元素。
//   · transform-origin 设在点击点，看起来像从指尖扩散。
//   · 监听 pointerup/cancel 不清理，让动画自然结束（0.55s 很短）。
//   · 用一个自增序号做 key，避免连点时被复用的 span 打断动画。

let seq = 0

/** 在指定元素内、指定坐标处生成一枚涟漪 */
export function spawnRipple(el, clientX, clientY) {
  if (!el || typeof document === 'undefined') return
  const rect = el.getBoundingClientRect()
  if (!rect.width || !rect.height) return

  // 元素对角线长度 = 能覆盖全元素的最小圆直径
  const dia = Math.hypot(rect.width, rect.height)
  const size = Math.round(dia * 2) // 留一倍余量，scale(1) 时足够铺满

  const x = (clientX ?? rect.left + rect.width / 2) - rect.left
  const y = (clientY ?? rect.top + rect.height / 2) - rect.top

  const span = document.createElement('span')
  span.className = 'ripple'
  span.dataset.rid = String(++seq)
  span.style.width = size + 'px'
  span.style.height = size + 'px'
  span.style.left = x - size / 2 + 'px'
  span.style.top = y - size / 2 + 'px'
  // 让扩散圆心落在点击点
  span.style.transformOrigin = 'center'

  el.appendChild(span)
  // 动画结束自删（0.55s + 余量）
  setTimeout(() => {
    if (span.parentNode) span.parentNode.removeChild(span)
  }, 700)
}

/** 组合式 API：给组件手动挂涟漪 */
export function useRipple() {
  function onTap(e) {
    const el = e.currentTarget
    if (!el) return
    // 禁用态不出涟漪
    if (el.disabled || el.getAttribute('aria-disabled') === 'true') return
    spawnRipple(el, e.clientX, e.clientY)
  }
  return { onTap }
}

/**
 * 全局自动涟漪：事件委托，一次注册，全站生效。
 *
 * ⚠ 关键教训（实测踩到的）：
 *   本项目的老业务组件**全是手工拼 tailwind class**，
 *   真正的 `.btn` / `.cell` 数量是 0 —— 只靠那套选择器，
 *   涟漪等于没上。所以选择器必须覆盖原生 `button`。
 *
 * 但有两个必须排除的：
 *   · .fb-ball   悬浮球：它的角标 badge 故意画在 -top-0.5 -right-0.5
 *                边界之外，一旦套 overflow:hidden 就会被裁掉半个。
 *   · .no-ripple 逃生口：个别按钮（如全屏遮罩层里的关闭键）
 *                不需要涟漪，显式关掉。
 *
 * 为了让 overflow:hidden 能生效又不污染布局，
 * 这里在首次点击时才给元素补一个 .ripple-ready（见 style.css），
 * 而不是一开始就给所有 button 加样式 —— 那样会一次性改变全站布局。
 */
export function installRipple(root = document) {
  if (typeof document === 'undefined') return

  const SELECTOR =
    '.btn, .cell, .seg button, .ripple-host, ' +
    'button:not(.fb-ball):not(.no-ripple), ' +
    '[role="button"]:not(.no-ripple)'

  /** 元素是否已具备"能装涟漪"的定位与裁剪 */
  function ensureReady(host) {
    if (host.classList.contains('ripple-ready')) return
    // CSS 类里做了 position:relative + overflow:hidden
    host.classList.add('ripple-ready')
  }

// ── 幂等 + 可卸载 ────────────────────────────────────────────
  // 【R12】原实现对 root 挂捕获监听后既不幂等也不移除：
  //   · installRipple() 被调两次就会叠两条监听 → 一次点击生成两层涟漪；
  //   · 模块级常驻影响虽低，但缺 remove 就等于把「谁装的、谁能卸」的契约丢了，
  //     热重载（vite HMR）时会越叠越多。
  //   现在排除已挂载情况，并提供 uninstallRipple。
  if (root.__rippleInstalled) return

  const handler = (e) => {
    const t = e.target
    if (!t || typeof t.closest !== 'function') return
    const host = t.closest(SELECTOR)
    if (!host) return
    if (host.disabled || host.getAttribute('aria-disabled') === 'true') return
    ensureReady(host)
    spawnRipple(host, e.clientX, e.clientY)
  }
  // 捕获阶段：确保即使标签内部 stopPropagation 也能收到
  const opts = { capture: true, passive: true }
  root.addEventListener('pointerdown', handler, opts)
  root.__rippleInstalled = handler
}

/** 卸载全局涟漪监听（热重载 / 单测隔离用）。 */
export function uninstallRipple(root = document) {
  const handler = root && root.__rippleInstalled
  if (!handler) return
  root.removeEventListener('pointerdown', handler, { capture: true })
  root.__rippleInstalled = null
}