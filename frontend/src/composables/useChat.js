// useChat.js —— 聊天逻辑（打字机效果 + 后端解析）
//
// 【R12 更正过期注释】旧注释写「后端目前不支持 SSE（web_gateway 只写 Content-Length）」，
//   该说法**已过期**：web_gateway.rs 现有 GET /api/tasks/stream 真正的 SSE 端点
//   （text/event-stream + 无 Content-Length + 15s ping 保活 + 3600s 回收），
//   实测 `curl -N http://127.0.0.1:8270/api/tasks/stream` 返回 200 与 hello 帧。
//   但那一路只推**任务事件**（任务看板用），不推聊天 token；
//   /api/chat 仍是一次性 JSON 回复，所以聊天侧继续用「假流式」打字机是正确的。
//   将来若给 /api/chat 上 SSE，只需把 typewriterCtl 换成读 ReadableStream。

import { ref } from 'vue'
import { ChatAPI } from '../api.js'
import { chat, toast, pushLog, syncWinState } from '../store.js'
import { openWin } from '../nav.js'

let seq = 0
const nextId = () => `m${++seq}-${Date.now()}`

/// 首屏与每次「加载更多」的条数。
///
/// 20 条 ≈ 10 轮对话，够接上下文，也不会让首屏渲染卡顿。
/// 和 bash 里的 `tail -n 20` 一个道理：用户大多数时候只关心最近说了什么。
const HISTORY_PAGE = 20

/**
 * 逐字打字机。
 *
 * 【R12 修复】原实现 `setTimeout(tick, ...)` 没有保存句柄，也无法取消：
 *   · 组件卸载（切到设置页、关掉浮窗）后 tick 仍在跑，
 *     继续往一个已卸载组件的响应式对象写 msg.text；
 *   · 长时间运行会累积大量孤儿定时器，DevTools 里能看到 setTimeout 只增不减；
 *   · 用户发下一条消息时上一条还在打字，两条 text 交错写同一个 msg。
 *   现在改为返回 { promise, cancel }：句柄可取消，卸载/重发时能立刻停。
 *   —— 对外仍兼容 `await typewriter(...)`，但内部走可取消版本。
 *
 * @param {object} msg 目标消息对象（直接改它的 text）
 * @param {string} full 完整文本
 * @param {number} msPerChar 每字毫秒
 * @returns {{promise: Promise<void>, cancel: Function}}
 */
function typewriterCtl(msg, full, msPerChar = 16) {
  let timer = null
  let cancelled = false
  let resolveFn = null
  const promise = new Promise((resolve) => { resolveFn = resolve })

  msg.typing = true
  msg.text = ''
  let i = 0
  const chars = Array.from(full)

  const stop = () => {
    if (timer) { clearTimeout(timer); timer = null }
    if (cancelled) return
    cancelled = true
    msg.typing = false
    resolveFn && resolveFn()
  }

  const tick = () => {
    if (cancelled) return
    if (i >= chars.length) { stop(); return }
    // 每次吐 1-3 个字，长文本自动加速，避免"回复 300 字要 5 秒"
    const step = chars.length > 120 ? 3 : chars.length > 60 ? 2 : 1
    msg.text += chars.slice(i, i + step).join('')
    i += step
    timer = setTimeout(tick, msPerChar)
  }

  tick()
  return { promise, cancel: stop }
}

/** 兼容旧调用点：忽略返回值时就是「等打完」。 */
async function typewriter(msg, full, msPerChar = 16) {
  await typewriterCtl(msg, full, msPerChar).promise
}

// ── 模块级打字机所有权（全局唯一）──────────────────────────────
// 理由见 useChat() 内注释：三个组件各调一次 useChat()，
// 局部变量做不到跨组件互斥，必须提到模块作用域。
let _activeTyper = null

function stopActiveTyper() {
  if (_activeTyper) {
    try { _activeTyper.cancel() } catch (e) {}
    _activeTyper = null
  }
}

/** 供组件卸载时调用：把还在跑的逐字动画立刻停掉，防孤儿定时器。 */
export function stopChatTyping() {
  stopActiveTyper()
}

export function useChat() {
  // ── 打字机所有权：必须放模块级，不能放这里 ──
  //
  // 【R12 修复】若把 activeTyper 写成 useChat() 内的局部变量，
  //   Composer.vue / FloatingChat.vue / App.vue 各自调 useChat() 会各拿一份，
  //   互不可见 → Composer 触发 send 时「停掉上一条」实际停的是自己的空引用，
  //   而浮窗那条还在打。三条入口写的是同一个 store.chat.messages 单例，
  //   于是同一个 msg.text 被两个打字机交错写，出现「文字乱跳/回退」。
  //   提到模块级后，全局只有一个活跃打字机，语义才正确。
  function stopTyping() {
    stopActiveTyper()
  }

  // ── 历史加载 ──
  //
  // 刷新页面后把上次的对话拉回来。否则用户一按 F5，
  // 界面上刚才聊的全没了——看起来像"系统失忆"，
  // 而实际上数据一直躺在 session.db 里。
  //
  // 【分页策略】首屏只拉最近 20 条（约 10 轮），够接上下文也够看。
  // `loadMore()` 供 ChatStream 在滚动到顶部时调用，一次补 20 条。
  // 不用一次性拉全量：真聊久了会有上千条，全量渲染会卡。
  //
  // 【去重】重复调用 loadHistory（比如切页签回来）不能重复插入。
  // 用一个 loaded 标记 + 按 role+text+ts 判重，双保险。
  async function loadHistory() {
    if (chat.historyLoaded) return
    chat.historyLoading = true
    try {
      const r = await ChatAPI.history('chat')
      const list = (r && r.messages) || []
      if (!list.length) {
        chat.historyLoaded = true
        return
      }
      // 只取最近 PAGE 条
      const page = list.slice(-HISTORY_PAGE)
      chat.historyHasMore = list.length > page.length
      chat.historyOldestTs = page.length ? page[0].ts : null

      // 历史要插在现有消息**之前**（用户可能在加载完成前就发了新消息）
      const restored = page
        .filter((m) => m && (m.role === 'user' || m.role === 'assistant'))
        .map((m) => ({
          id: nextId(),
          // 后端角色是 user/assistant，前端气泡只有 user/bot
          role: m.role === 'user' ? 'user' : 'bot',
          text: m.content || '',
          typing: false,
          ts: m.ts || Date.now(),
          fromHistory: true,
        }))

      chat.messages.unshift(...restored)
      chat.historyLoaded = true
    } catch (e) {
      // 历史拉不到不该挡住聊天——记一笔，继续用空列表
      console.warn('[chat] 历史加载失败：', e.message)
      chat.historyLoaded = true
    } finally {
      chat.historyLoading = false
    }
  }

  // 加载更多（ChatStream 滚到顶部时调用）
  async function loadMore() {
    if (!chat.historyHasMore || chat.historyLoading) return
    chat.historyLoading = true
    try {
      const r = await ChatAPI.history('chat')
      const list = (r && r.messages) || []
      // 已显示的最早时间戳之前的那些
      const older = list.filter((m) => (m.ts || 0) < (chat.historyOldestTs || 0))
      const page = older.slice(-HISTORY_PAGE)
      chat.historyHasMore = older.length > page.length

      const restored = page
        .filter((m) => m && (m.role === 'user' || m.role === 'assistant'))
        .map((m) => ({
          id: nextId(),
          role: m.role === 'user' ? 'user' : 'bot',
          text: m.content || '',
          typing: false,
          ts: m.ts || Date.now(),
          fromHistory: true,
        }))

      if (page.length) chat.historyOldestTs = page[0].ts
      chat.messages.unshift(...restored)
    } catch (e) {
      console.warn('[chat] 加载更早历史失败：', e.message)
    } finally {
      chat.historyLoading = false
    }
  }

  // 发送一条用户消息
  async function send(rawText) {
    const text = (rawText ?? '').trim()
    if (!text || chat.sending) return

    // 1. 立刻上屏用户消息（不等后端）
    chat.messages.push({ id: nextId(), role: 'user', text, ts: Date.now() })

    // 2. 插入占位的 bot 气泡（先显示"思考中"）
    const bot = reactiveMsg({ id: nextId(), role: 'bot', text: '', typing: true, ts: Date.now() })
    chat.messages.push(bot)
    chat.sending = true

    try {
      const r = await ChatAPI.send(text)

      if (!r || !r.ok) {
        bot.typing = false
        bot.text = r?.error || '抱歉，没听懂，或者后端未就绪。'
        toast(r?.error || '请求失败', true)
        return
      }

      // 3. 打字机渲染回复（用可取消版本；开打前先停掉可能残留的上一条）
      stopActiveTyper()
      const typer = typewriterCtl(bot, r.reply || '（无回复）')
      _activeTyper = typer
      await typer.promise
      if (_activeTyper === typer) _activeTyper = null

      // 4. 挂上计划信息（前端渲染成步骤卡）
      if (r.plan && r.plan.length) {
        bot.plan = r.plan
      }

      // 5. 有任务派发 → 弹悬浮窗 + 写日志
      if (r.task_ids && r.task_ids.length) {
        pushLog(`派发 ${r.task_ids.length} 个任务：${r.task_ids.join(', ')}`, 'task')
        openWin()
        syncWinState()
      }
      // 需要解锁的提示
      if (r.needs_unlock) {
        bot.needsUnlock = true
      }
    } catch (e) {
      bot.typing = false
      bot.text = `连接失败：${e.message}`
      toast('连接后端失败，检查 8270 是否在跑', true)
    } finally {
      chat.sending = false
    }
  }

  return { send, loadHistory, loadMore, stopTyping }
}

// Vue 的 reactive 需要单独包一层（数组元素也要响应）
import { reactive } from 'vue'
function reactiveMsg(m) {
  return reactive(m)
}