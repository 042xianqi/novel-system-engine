// bridge-smoke.mjs —— 验证 sideBridge 的钩子真的挂上了、且行为正确
//
// 用 jsdom 执行**打包产物**是不可行的（jsdom 不支持 <script type="module">）。
// 所以这里用 Vite SSR 加载 src 模块，手工造一个 window，再调 installSideBridge()，
// 然后逐个验证钩子：存在性、返回值、副作用（清空是否真清、任务状态是否不动）。
//
// 用法：node bridge-smoke.mjs

import { createServer } from 'vite'

let fail = 0
const log = (...a) => console.log(...a)
const check = (name, cond, extra = '') => {
  if (cond) log(`  ✅ ${name}`)
  else { log(`  ❌ ${name} ${extra}`); fail++ }
}

log('=== 启动 Vite（SSR 模块图）===')
const vite = await createServer({
  server: { middlewareMode: true },
  appType: 'custom',
  logLevel: 'error',
})

try {
  // 造一个最小 window（Node 里没有）
  if (typeof globalThis.window === 'undefined') {
    globalThis.window = globalThis
  }
  if (typeof globalThis.window.dispatchEvent !== 'function') {
    const listeners = new Map()
    globalThis.window.dispatchEvent = (ev) => {
      const arr = listeners.get(ev.type) || []
      arr.forEach((fn) => { try { fn(ev) } catch (e) { /* ignore */ } })
      return true
    }
    globalThis.window.addEventListener = (type, fn) => {
      if (!listeners.has(type)) listeners.set(type, [])
      listeners.get(type).push(fn)
    }
    globalThis.window.removeEventListener = (type, fn) => {
      const arr = listeners.get(type) || []
      listeners.set(type, arr.filter((x) => x !== fn))
    }
  }
  if (typeof globalThis.CustomEvent === 'undefined') {
    globalThis.CustomEvent = class CustomEvent {
      constructor(type, init) { this.type = type; this.detail = init?.detail }
    }
  }

  log('\n=== 加载 store + sideBridge ===')
  const store = await vite.ssrLoadModule('./src/store.js')
  const bridge = await vite.ssrLoadModule('./src/sideBridge.js')
  check('store.js 加载', !!store)
  check('sideBridge.js 加载', !!bridge.installSideBridge)

  log('\n=== 安装桥接 ===')
  bridge.installSideBridge()

  log('\n=== 验证钩子存在性 ===')
  for (const h of ['__novClearChat', '__novOpenTasks', '__novStatus', '__novSend', '__novBridgeVersion']) {
    check(`window.${h} 已挂载`, typeof globalThis.window[h] === 'function')
  }

  log('\n=== __novBridgeVersion ===')
  const v = globalThis.window.__novBridgeVersion()
  check('返回版本串', typeof v === 'string' && v.startsWith('nov-fe-'), `got=${v}`)

  log('\n=== __novStatus（只读，必须返回可序列化 JSON）===')
  const stRaw = globalThis.window.__novStatus()
  check('返回字符串', typeof stRaw === 'string')
  let st = null
  try { st = JSON.parse(stRaw) } catch (e) { /* */ }
  check('是合法 JSON', !!st, `got=${String(stRaw).slice(0, 120)}`)
  if (st) {
    check('含 messages 字段', 'messages' in st)
    check('含 pending/running', 'pending' in st && 'running' in st)
    check('含 winState', 'winState' in st)
    check('不含函数（纯数据）', Object.values(st).every((x) => typeof x !== 'function'))
  }

  log('\n=== __novClearChat（副作用验证）===')
  // 先塞几条假消息
  store.chat.messages.push(
    { id: 'a', role: 'user', text: 'hi', ts: Date.now() },
    { id: 'b', role: 'bot', text: 'hello', ts: Date.now() }
  )
  store.win.log.push({ id: 1, ts: Date.now(), text: 'log1', kind: 'info' })
  // 任务状态塞个"假后端值"，验证不被清掉
  store.tasks.counts.pending = 3
  store.tasks.counts.running = 1

  const before = store.chat.messages.length
  check('清空前有消息', before === 2, `got=${before}`)

  const rc = globalThis.window.__novClearChat()
  check('返回 ok', rc === 'ok', `got=${rc}`)
  check('消息已清空', store.chat.messages.length === 0, `got=${store.chat.messages.length}`)
  check('日志已清空', store.win.log.length === 0, `got=${store.win.log.length}`)
  check('**任务状态未被误清**',
    store.tasks.counts.pending === 3 && store.tasks.counts.running === 1,
    `pending=${store.tasks.counts.pending} running=${store.tasks.counts.running}`)

  log('\n=== __novClearChat 幂等（再清一次不能报错）===')
  const rc2 = globalThis.window.__novClearChat()
  check('第二次也返回 ok', rc2 === 'ok', `got=${rc2}`)

  log('\n=== __novOpenTasks ===')
  store.win.state = 'hidden'
  const rc3 = globalThis.window.__novOpenTasks()
  check('返回 ok', rc3 === 'ok', `got=${rc3}`)
  check('悬浮窗已展开', store.win.state === 'expanded', `got=${store.win.state}`)

  log('\n=== __novSend（必须派发事件、不得直接发请求）===')
  let got = null
  globalThis.window.addEventListener('nov:send', (e) => { got = e.detail?.text })
  const rc4 = globalThis.window.__novSend('看当前任务状态')
  check('返回 ok', rc4 === 'ok', `got=${rc4}`)
  check('事件已派发且携带文本', got === '看当前任务状态', `got=${got}`)
  const rc5 = globalThis.window.__novSend('   ')
  check('空文本返回 empty（不派发）', rc5 === 'empty', `got=${rc5}`)

  log('\n=== 重入：安装两次不能坏 ===')
  bridge.installSideBridge()
  check('重新安装后钩子仍可用', typeof globalThis.window.__novClearChat === 'function')
  check('重新安装后仍返回 ok', globalThis.window.__novClearChat() === 'ok')

  log('\n' + '='.repeat(54))
  if (fail === 0) log('🎉 桥接冒烟全通过')
  else log(`⚠ ${fail} 项失败`)
  log('='.repeat(54))
} finally {
  await new Promise((r) => setTimeout(r, 900))
  await vite.close()
}
process.exit(fail === 0 ? 0 : 1)