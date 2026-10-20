// events.js —— 类型化应用事件总线（Phase-1 架构重构 · 任务三）
//
// # 为什么需要这一层
//
// 重构前，跨组件通信靠散落的 `window.dispatchEvent(new CustomEvent('nov:xxx'))`：
//   · 事件名是**字符串字面量**，写错一个字母就静默失效（nov:skin 甚至长期没有监听者）；
//   · 载荷结构靠 `e.detail.???` 口头约定，改字段名不会报错，只会 `undefined`；
//   · 监听/卸载分散在各组件，容易漏 removeEventListener（内存泄漏 + 重复触发）。
//
// # 设计原则
//
// 1. **无损兼容**：底层传输仍是 window CustomEvent。
//    外部 ToolPkg 通过 `controller.evaluateJavascript(...)` 派发的
//    `new CustomEvent('nov:send', { detail: { text } })` **照旧生效**，
//    因为 bus.on 就是往 window 上挂监听，只是顺手帮你 unwrap 了 detail。
// 2. **单一事件清单**：所有事件名收敛到 `EV`（冻结对象），写错即 undefined 报错。
// 3. **载荷有契约**：`SCHEMA` 声明每个事件的载荷形状，DEV 下 emit 时校验并告警。
// 4. **on 返回 unsubscribe**：调用方不用记 removeEventListener，卸载时一行 `off()`。
// 5. **零依赖**：不引入 mitt/eventemitter3，与「前端只依赖 vue」的现状一致。

// ══════════════════════════════════════════ 事件清单（唯一事实源）

/**
 * 全部应用事件名。**新增事件必须登记在这里**，否则 DEV 下 emit 会告警。
 * @readonly
 */
export const EV = Object.freeze({
  /** 侧边栏 ToolPkg 要求发送一条消息。载荷：{@link SideSendPayload} */
  SIDE_SEND: 'nov:send',
  /** 侧边栏 ToolPkg 要求切换视图。载荷：{@link SideViewPayload} */
  SIDE_VIEW: 'nov:view',
  /** 皮肤已切换（供需要重绘的 canvas 等监听）。载荷：皮肤 id 字符串 */
  SKIN_CHANGED: 'nov:skin',
  /** 前端桥接就绪（宿主可据此判断可调用钩子）。载荷：无 */
  APP_READY: 'nov:ready',
})

/**
 * @typedef {Object} SideSendPayload
 * @property {string} text 要发送的消息正文（非空）
 */

/**
 * @typedef {Object} SideViewPayload
 * @property {'system'|'chat'|'settings'} [tab] 目标视图（新形态）
 * @property {string} [pane] 目标子页（可选，如 'tasks'）
 * @property {string} [view] 目标视图（旧形态，等价 tab，保留兼容）
 */

// ══════════════════════════════════════════ 载荷契约

/** @type {Record<string, (p: any) => boolean>} */
const SCHEMA = {
  [EV.SIDE_SEND]: (p) => !!p && typeof p === 'object' && typeof p.text === 'string' && p.text.length > 0,
  [EV.SIDE_VIEW]: (p) => !!p && typeof p === 'object' && !!(p.tab || p.view),
  [EV.SKIN_CHANGED]: (p) => typeof p === 'string' && p.length > 0,
  [EV.APP_READY]: () => true,
}

/** DEV 模式探测：Vite 下为 import.meta.env.DEV；非 Vite 环境（如单测）安全降级为 false */
const DEV = (() => {
  try {
    return !!(import.meta && import.meta.env && import.meta.env.DEV)
  } catch {
    return false
  }
})()

// ══════════════════════════════════════════ 总线

/**
 * 派发事件。
 * @param {string} event 必须是 {@link EV} 中的值
 * @param {any} [payload] 载荷，形状见 SCHEMA
 */
export function emit(event, payload) {
  if (DEV) {
    if (!(event in SCHEMA)) {
      console.warn(`[bus] 未登记的事件名：${event}（请先加到 EV）`)
    } else if (!SCHEMA[event](payload)) {
      console.warn(`[bus] 事件 ${event} 的载荷不符合契约：`, payload)
    }
  }
  if (typeof window === 'undefined') return
  window.dispatchEvent(new CustomEvent(event, { detail: payload }))
}

/**
 * 订阅事件。**回调收到的是解包后的 payload**（不是 Event 对象）。
 * @param {string} event {@link EV} 中的值
 * @param {(payload: any) => void} handler
 * @returns {() => void} unsubscribe（幂等，可重复调用）
 */
export function on(event, handler) {
  if (typeof window === 'undefined') return () => {}
  if (typeof handler !== 'function') {
    if (DEV) console.warn(`[bus] on(${event}) 的 handler 不是函数，已忽略`)
    return () => {}
  }
  const wrapped = (e) => {
    try {
      handler(e ? e.detail : undefined)
    } catch (err) {
      // 单个监听者抛错不能打断其他监听者（事件总线的基本容错）
      console.error(`[bus] 事件 ${event} 的监听者抛错：`, err)
    }
  }
  window.addEventListener(event, wrapped)
  let done = false
  return () => {
    if (done) return
    done = true
    window.removeEventListener(event, wrapped)
  }
}

/** 总线门面（与 on/emit 等价的命名空间形式，便于 `bus.on(...)` 阅读） */
export const bus = { emit, on }
