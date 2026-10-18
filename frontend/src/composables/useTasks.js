// useTasks.js —— 任务状态轮询 + 授权锁同步
//
// 【为什么用轮询而不是 SSE】
//   后端 web_gateway 是零依赖手写 HTTP，只写 Content-Length、无 chunked，
//   所以 SSE 需要改 Rust 侧。当前用轮询：有活跃任务 3 秒一次，空闲 8 秒一次。
//   实测 /api/tasks/active 响应 6~12ms，轮询成本可忽略。
//   将来上 SSE 只需改这里，组件层零改动。

import { tasks, lock, pushLog, syncWinState, toast } from '../store.js'
import { TaskAPI, LockAPI } from '../api.js'

const FAST_MS = 3000   // 有任务在跑
const SLOW_MS = 8000   // 空闲

let timer = null
let stopped = false
let lastDoneId = null
let firstPoll = true

/** 拉一次任务摘要 */
async function pollOnce() {
  try {
    const r = await TaskAPI.active()

    const prevActive = tasks.counts.pending + tasks.counts.running
    tasks.items = r.items || []
    tasks.counts.pending = r.pending || 0
    tasks.counts.running = r.running || 0
    // 【2026-10-18 修 · 数字口径错配】blocked 卡片标题写的是「阻塞 / 失败」，
    // 但这里只取了 totals.blocked（实测=1），把 failed(16) + rejected(4) 全漏了。
    // 结果：看板顶部显示「1」，而下方列表实际有 21 条，用户一眼看出对不上。
    // 改为三者之和，与卡片标题、与列表口径保持一致。
    tasks.counts.blocked =
      (r.totals?.blocked || 0) + (r.totals?.failed || 0) + (r.totals?.rejected || 0)
    tasks.counts.done = r.totals?.done || 0
    tasks.executorAlive = !!r.executor_alive
    tasks.executorPid = r.executor_pid
    tasks.lastDone = r.last_done || null
    tasks.loaded = true

    // 锁状态跟随后端（面板开关或终端 touch 都能反映）
    lock.locked = !r.llm_unlocked
    lock.unlocked = !!r.llm_unlocked
    lock.loaded = true

    const nowActive = tasks.counts.pending + tasks.counts.running

    // 任务全部结束 → 记一条日志
    if (!firstPoll && prevActive > 0 && nowActive === 0) {
      pushLog('执行队列已清空', 'ok')
    }
    // 新完成任务 → 提示
    const ld = r.last_done
    if (ld && ld.id && ld.id !== lastDoneId) {
      if (lastDoneId !== null) {
        pushLog(`任务完成：${ld.id}`, ld.state === 'done' ? 'ok' : 'bad')
        toast(ld.state === 'done' ? '任务已完成' : `任务${ld.state}`, ld.state !== 'done')
      }
      lastDoneId = ld.id
    }

    syncWinState()
    firstPoll = false
  } catch (e) {
    // 后端没起来时不刷屏，只在首次提示
    if (!tasks.loaded) {
      tasks.loaded = true
      toast('后端未连接（8270）', true)
    }
  }
}

/** 自适应间隔循环 */
async function loop() {
  if (stopped) return
  await pollOnce()
  if (stopped) return
  const active = tasks.counts.pending + tasks.counts.running
  timer = setTimeout(loop, active > 0 ? FAST_MS : SLOW_MS)
}

export function useTasks() {
  function start() {
    if (timer) return
    stopped = false
    loop()
  }

  function stop() {
    stopped = true
    if (timer) clearTimeout(timer)
    timer = null
  }

  /** 手动刷新一次（点按钮时用，比等轮询快） */
  async function refresh() {
    await pollOnce()
  }

  /** 派发执行器：不动任务状态，只是让后端 spawn 一个执行进程 */
  async function dispatch(id, limit = 1) {
    try {
      const r = await TaskAPI.dispatch(id, limit)
      if (r && r.ok === false) throw new Error(r.error || '派发被拒绝')
      pushLog(`已派发执行器（${id}）`, 'task')
      toast('执行器已启动')
      // 稍等再刷新，让执行器有时间把状态改成 running
      setTimeout(refresh, 900)
      return true
    } catch (e) {
      toast('派发失败：' + e.message, true)
      return false
    }
  }

  /** 解锁真实 LLM（需二次确认，由调用方负责） */
  async function unlockLLM() {
    try {
      const r = await LockAPI.unlock()
      if (r && r.ok === false) throw new Error(r.error || '解锁失败')
      lock.locked = false
      lock.unlocked = true
      pushLog('已解锁真实 LLM 授权', 'warn')
      toast('已解锁：真跑将调用真实模型')
      return true
    } catch (e) {
      toast('解锁失败：' + e.message, true)
      return false
    }
  }

  /** 上锁 */
  async function lockLLM() {
    try {
      const r = await LockAPI.lock()
      if (r && r.ok === false) throw new Error(r.error || '上锁失败')
      lock.locked = true
      lock.unlocked = false
      pushLog('已上锁（真跑被禁止）', 'ok')
      toast('已上锁')
      return true
    } catch (e) {
      toast('上锁失败：' + e.message, true)
      return false
    }
  }

  return { start, stop, refresh, dispatch, unlockLLM, lockLLM }
}
