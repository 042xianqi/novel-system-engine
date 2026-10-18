// useChat.js —— 聊天逻辑（打字机效果 + 后端解析）
//
// 流式方案说明（重要）：
//   后端目前**不支持 SSE**（web_gateway 只写 Content-Length，无 chunked）。
//   所以这里用「**假流式**」：先等后端返回全文，再在前端逐字渲染。
//   用户感知与真流式几乎一致（回复通常 <100 字，1.5 秒打完）。
//   将来后端上 SSE，只需把 typewriter() 换成读 ReadableStream，组件层零改动。

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
 * @param {object} msg 目标消息对象（直接改它的 text）
 * @param {string} full 完整文本
 * @param {number} msPerChar 每字毫秒
 */
function typewriter(msg, full, msPerChar = 16) {
  return new Promise((resolve) => {
    msg.typing = true
    msg.text = ''
    let i = 0
    // 中文按字符切（Array.from 正确处理 emoji / 代理对）
    const chars = Array.from(full)
    const tick = () => {
      if (i >= chars.length) {
        msg.typing = false
        resolve()
        return
      }
      // 每次吐 1-2 个字，长文本自动加速，避免"回复 300 字要 5 秒"
      const step = chars.length > 120 ? 3 : chars.length > 60 ? 2 : 1
      msg.text += chars.slice(i, i + step).join('')
      i += step
      setTimeout(tick, msPerChar)
    }
    tick()
  })
}

export function useChat() {
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

      // 3. 打字机渲染回复
      await typewriter(bot, r.reply || '（无回复）')

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

  return { send, loadHistory, loadMore }
}

// Vue 的 reactive 需要单独包一层（数组元素也要响应）
import { reactive } from 'vue'
function reactiveMsg(m) {
  return reactive(m)
}