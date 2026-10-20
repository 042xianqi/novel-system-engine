// sideBridge.js —— 与「侧边栏大脑系统」ToolPkg 的桥接层
//
// # 为什么需要这一层
//
// 侧边栏「大脑系统」是一个 ToolPkg，它的 UI 是：
//   UI.WebView({ url: "http://127.0.0.1:8270/" })   ← 直接加载本前端
//   + 底部一排原生按钮（UI.Row / UI.Button）
//
// 换句话说：**我们的 Vue 应用跑在它的 WebView 里**。
// 原生按钮想操作页面（比如"清空聊天"），只能通过
// `controller.evaluateJavascript(...)` 执行一段 JS。
//
// 那我们就得在 window 上挂一些**明确命名、职责单一**的钩子。
//
// # 设计原则
//
// 1. **只挂必须的**。钩子越多越难维护，且每个都是被外部注入的入口。
// 2. **全部幂等 + 可重入**。ToolPkg 可能重复调用，不能因为调两次就坏掉。
// 3. **不暴露内部状态**。只暴露"动作"（清空/聚焦），不暴露 store 对象，
//    避免外部直接改状态绕过响应式。
// 4. **失败安全**。任何一个钩子出错，都不能让主应用白屏。

import { chat, tasks, winLog, toast, pushLog, syncWinState } from './store.js'
import { nav, openWin, setView } from './nav.js'
import { bus, EV } from './events.js'

/** 挂一个钩子，重复挂不会报错（幂等），返回是否挂成功 */
function mount(name, fn) {
  try {
    // 标记来源，便于在控制台排障时一眼看出是谁挂的
    fn.__novBridge = name
    window[name] = fn
    return true
  } catch (e) {
    console.warn('[sideBridge] 挂载失败', name, e)
    return false
  }
}

export function installSideBridge() {
  // ── 清空聊天记录 ──
  // 对应 ToolPkg 底栏的「⌫ 清空聊天」按钮。
  // 只清对话，**不动任务状态**（任务状态是独立的数据源，清掉会和后端不一致）。
  mount('__novClearChat', () => {
    try {
      const n = chat.messages.length
      chat.messages.splice(0, chat.messages.length)
      chat.sending = false
      // 顺便把悬浮窗日志也清了（它也是内存态，且用户点"清空"通常想全清）
      winLog.items.splice(0, winLog.items.length)
      toast(n > 0 ? `已清空 ${n} 条对话` : '本来就没有对话')
      return 'ok'
    } catch (e) {
      console.warn('[sideBridge] clearChat 失败', e)
      return 'err'
    }
  })

  // ── 打开任务悬浮窗 ──
  // 侧边栏可能想提供一个"看任务"的入口。
  mount('__novOpenTasks', () => {
    try {
      // 直接切到「系统视图 / 任务看板」。
      //
      // 改造前是 dispatch CustomEvent 让 App.vue 切视图 —— 绕了一圈，
      // 而且只能切到 system 的默认子页。现在 nav.js 是单一事实源，
      // 这里直接调 setView，能精确指定到子页（tasks），无需事件绕行。
      setView('system', 'tasks')
      openWin()
      syncWinState()
      return 'ok'
    } catch (e) {
      return 'err'
    }
  })

  // ── 查询当前状态（只读，供 ToolPkg 做条件判断）──
  // 返回**纯数据**，不含函数，可被 evaluateJavascript 序列化回来。
  mount('__novStatus', () => {
    try {
      return JSON.stringify({
        messages: chat.messages.length,
        sending: !!chat.sending,
        pending: tasks.counts.pending || 0,
        running: tasks.counts.running || 0,
        executorAlive: !!tasks.executorAlive,
        winState: nav.win,
        tab: nav.tab,
        pane: nav.pane,
        ts: Date.now(),
      })
    } catch (e) {
      return '{"error":"status-failed"}'
    }
  })

  // ── 发一条消息（供将来 ToolPkg 做"快捷指令"按钮）──
  // 注意：这里**不直接 import useChat**，避免与 Composer 形成循环依赖。
  // 改成派发一个自定义事件，由 App.vue 监听后调 send()。
  mount('__novSend', (text) => {
    try {
      const t = String(text || '').trim()
      if (!t) return 'empty'
      bus.emit(EV.SIDE_SEND, { text: t })
      return 'ok'
    } catch (e) {
      return 'err'
    }
  })

  // ── 版本探测（ToolPkg 可据此判断前端是否为新版）──
  mount('__novBridgeVersion', () => 'nov-fe-1.0.0')

  // 控制台留一行，便于确认桥接已装
  console.log('[sideBridge] 已挂载钩子：__novClearChat / __novOpenTasks / __novStatus / __novSend / __novBridgeVersion')

  // 主动通知宿主「我准备好了」（ToolPkg 若监听 console 或轮询 __novStatus 可感知）
  try {
    bus.emit(EV.APP_READY)
  } catch (e) { /* ignore */ }
}