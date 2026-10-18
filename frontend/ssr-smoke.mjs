// ssr-smoke.mjs —— 用 Vite SSR 真实编译 + 渲染所有 .vue 组件
//
// 为什么用 SSR 而不是 jsdom：
//   jsdom 不支持 <script type="module">（已知限制），所以它无法执行 Vite 打包的
//   ESM 产物，测出来必然白屏——那是工具能力问题，不是代码问题。
//   SSR 走 Vite 的模块图：**真的**编译每个 .vue（模板 → render 函数）、
//   **真的**解析所有 import 链，然后把组件渲染成 HTML 字符串。
//   白屏的绝大多数原因（模板语法错、import 路径错、setup 里抛异常、
//   props 未定义就解构）都会在这里暴露。
//
// 用法：node ssr-smoke.mjs

import { createServer } from 'vite'
import { renderToString } from '@vue/server-renderer'
import { createSSRApp } from 'vue'

let fail = 0
const log = (...a) => console.log(...a)
const check = (name, cond, extra = '') => {
  if (cond) log(`  ✅ ${name}`)
  else { log(`  ❌ ${name} ${extra}`); fail++ }
}

log('=== 1. 启动 Vite（SSR 模式，不监听端口）===')
const vite = await createServer({
  server: { middlewareMode: true },
  appType: 'custom',
  logLevel: 'error',
})

try {
  log('\n=== 2. 逐个加载组件（验证编译 + import 链）===')

  const comps = [
    ['App.vue', './src/App.vue'],
    ['LockBar.vue', './src/components/LockBar.vue'],
    ['ChatStream.vue', './src/components/ChatStream.vue'],
    ['Composer.vue', './src/components/Composer.vue'],
    ['FloatWindow.vue', './src/components/FloatWindow.vue'],
    ['Capsule.vue', './src/components/Capsule.vue'],
    ['ToastHost.vue', './src/components/ToastHost.vue'],
    ['BottomNav.vue', './src/components/BottomNav.vue'],
    ['SystemView.vue', './src/components/SystemView.vue'],
    ['SettingsView.vue', './src/components/SettingsView.vue'],
    ['panes/TaskBoard.vue', './src/components/panes/TaskBoard.vue'],
    ['panes/OutlineTree.vue', './src/components/panes/OutlineTree.vue'],
    ['panes/CardBrowser.vue', './src/components/panes/CardBrowser.vue'],
    ['panes/StatsPane.vue', './src/components/panes/StatsPane.vue'],
    ['panes/CardEditor.vue', './src/components/panes/CardEditor.vue'],
    // 2026-10-17 新增：全局双悬浮球 + 悬浮聊天窗
    ['FloatingBall.vue', './src/components/FloatingBall.vue'],
    ['FloatingChat.vue', './src/components/FloatingChat.vue'],
    // 2026-10-17 新增：UI 控件库（规范 v1）基础组件
    ['base/BaseButton.vue', './src/components/base/BaseButton.vue'],
    ['base/BaseInput.vue', './src/components/base/BaseInput.vue'],
    ['base/BaseCell.vue', './src/components/base/BaseCell.vue'],
    ['base/BaseSwitch.vue', './src/components/base/BaseSwitch.vue'],
    ['base/BaseToast.vue', './src/components/base/BaseToast.vue'],
    ['base/BaseBanner.vue', './src/components/base/BaseBanner.vue'],
    ['base/BaseDialog.vue', './src/components/base/BaseDialog.vue'],
    ['base/BaseSheet.vue', './src/components/base/BaseSheet.vue'],
    ['base/BaseSkeleton.vue', './src/components/base/BaseSkeleton.vue'],
    ['base/SkinPicker.vue', './src/components/base/SkinPicker.vue'],
  ]

  const loaded = {}
  for (const [name, path] of comps) {
    try {
      const m = await vite.ssrLoadModule(path)
      loaded[name] = m.default
      check(`${name} 编译 + 加载`, !!m.default)
    } catch (e) {
      check(`${name} 编译 + 加载`, false, String(e.message).slice(0, 300))
    }
  }

  log('\n=== 3. 加载 composables / store（验证纯 JS 模块）===')
  for (const p of ['./src/store.js', './src/nav.js', './src/api.js', './src/composables/useChat.js', './src/composables/useTasks.js', './src/composables/useSystem.js']) {
    try {
      await vite.ssrLoadModule(p)
      check(`${p} 加载`, true)
    } catch (e) {
      check(`${p} 加载`, false, String(e.message).slice(0, 300))
    }
  }

  log('\n=== 4. SSE 渲染 App（真跑 setup + 模板）===')
  // SSR 下 onMounted 不执行 → 不会真起轮询、不会发请求，安全
  const app = createSSRApp(loaded['App.vue'])
  let html = ''
  try {
    html = await renderToString(app)
    check('App 渲染出 HTML', html.length > 200, `len=${html.length}`)
  } catch (e) {
    check('App 渲染出 HTML', false, String(e.message).slice(0, 400))
  }

  log('\n=== 5. 检查渲染结果里的关键内容 ===')
  check('含标题「残响之河 · 中枢」', html.includes('残响之河'))
  check('含锁条文案', html.includes('真实 LLM'))
  check('底栏三入口齐全', ['系统','对话','设置'].every(k => html.includes(k)))
  check('含输入框 placeholder', html.includes('placeholder') && html.includes('说一句话'))
  check('含 glass 类（样式生效前提）', html.includes('glass'))
  check('含 4 个快捷指令图标', ['🧬', '🔍', '📊', '🩺'].every((i) => html.includes(i)))

  // ── 三块底栏（用户明确要求：任何视图都不能消失）──
  check('底栏含「系统」入口', html.includes('系统'))
  check('底栏含「对话」入口', html.includes('对话'))
  check('底栏含「设置」入口', html.includes('设置'))
  check('底栏是 <nav> 元素', html.includes('<nav'))
  check('底栏有 safe-area 适配', html.includes('safe-area-inset-bottom'))
  // 默认视图是 chat，所以设置中心不该出现在 DOM 可见内容里（v-show 会渲染但 display:none）
  check(
    '默认视图不显示系统面板 iframe',
    !html.includes('/legacy'),
    '系统视图应懒加载，未切过去时不该有 src'
  )

  log('\n=== 6. 驱动层 nav.js 专项（幂等 / 方向 / 状态机）===')
  {
    const navMod = await vite.ssrLoadModule('./src/nav.js')
    const {
      nav,
      goTab,
      goPane,
      openWin,
      minimizeWin,
      closeWin,
      toggleFull,
      autoCapsule,
      setView,
      resetNav,
      TABS,
      PANES,
      tabIndex,
      paneIndex,
      slideName,
    } = navMod

    check('导出 TABS 三项', TABS.length === 3, `→ ${TABS.map((t) => t.key).join(',')}`)
    check('导出 PANES 四项', PANES.length === 4, `→ ${PANES.map((p) => p.key).join(',')}`)
  check('PANES 含 stats', PANES.some(p => p.key === 'stats'))

    // ── 初始态 ──
    resetNav()
    check('初始 tab = chat', nav.tab === 'chat', `→ ${nav.tab}`)
    check('初始 win = hidden', nav.win === 'hidden', `→ ${nav.win}`)

    // ── 切换到最右：dir 应为 +1（往左推）──
    goTab('settings')
    check('chat→settings: tab 正确', nav.tab === 'settings')
    check('chat→settings: dir = 1', nav.dir === 1, `→ ${nav.dir}`)
    check('slideName = slide-left', slideName.value === 'slide-left', `→ ${slideName.value}`)

    // ── 切回最左：dir 应为 -1（往右推）──
    goTab('system')
    check('settings→system: dir = -1', nav.dir === -1, `→ ${nav.dir}`)
    check('slideName = slide-right', slideName.value === 'slide-right', `→ ${slideName.value}`)

    // ── 幂等：重复切同一个 tab 不该改 dir、不该报错 ──
    const dirBefore = nav.dir
    const changed = goTab('system')
    check('重复切同一 tab 返回 false', changed === false)
    check('重复切同一 tab 不改 dir', nav.dir === dirBefore, `→ ${nav.dir}`)

    // ── 非法 key 被拒 ──
    const okBad = goTab('nope')
    check('非法 tab 被拒绝', okBad === false && nav.tab === 'system', `→ ${nav.tab}`)

    // ── 子页切换 ──
    goPane('cards')
    check('pane → cards', nav.pane === 'cards', `→ ${nav.pane}`)
    check('paneIndex = 2', paneIndex.value === 2, `→ ${paneIndex.value}`)
    goPane('outline')
    check('pane cards→outline: dir = -1', nav.dir === -1, `→ ${nav.dir}`)
    goPane('tasks')
    check('pane outline→tasks: dir = -1', nav.dir === -1, `→ ${nav.dir}`)

    // ── setView 一步到位 ──
    resetNav()
    setView('system', 'cards')
    check('setView 同时设 tab + pane', nav.tab === 'system' && nav.pane === 'cards',
      `→ ${nav.tab}/${nav.pane}`)

    // ── 悬浮窗状态机 ──
    resetNav()
    openWin()
    check('hidden → openWin → expanded', nav.win === 'expanded', `→ ${nav.win}`)
    toggleFull()
    check('expanded → toggleFull → full', nav.win === 'full', `→ ${nav.win}`)
    toggleFull()
    check('full → toggleFull → expanded', nav.win === 'expanded', `→ ${nav.win}`)
    minimizeWin()
    check('expanded → minimize → capsule', nav.win === 'capsule', `→ ${nav.win}`)
    closeWin()
    check('capsule → close → hidden', nav.win === 'hidden', `→ ${nav.win}`)

    // ── autoCapsule：只在 hidden 且活跃时才升 ──
    resetNav()
    autoCapsule(0)
    check('无活跃任务时不弹胶囊', nav.win === 'hidden', `→ ${nav.win}`)
    autoCapsule(3)
    check('有活跃任务时升到胶囊', nav.win === 'capsule', `→ ${nav.win}`)
    openWin()
    autoCapsule(3)
    check('已展开时不被打回胶囊', nav.win === 'expanded', `→ ${nav.win}`)

    // ── tabIndex 计算 ──
    resetNav()
    goTab('chat')
    check('tabIndex(chat) = 1', tabIndex.value === 1, `→ ${tabIndex.value}`)

    resetNav()
    check('resetNav 复位', nav.tab === 'chat' && nav.pane === 'tasks' && nav.win === 'hidden')
  }

  log('\n=== 7. 单独渲染各子组件（隔离验证）===')
  for (const name of [
    'LockBar.vue',
    'ChatStream.vue',
    'Composer.vue',
    'Capsule.vue',
    'ToastHost.vue',
    'BottomNav.vue',
    'SystemView.vue',
    'SettingsView.vue',
    'panes/TaskBoard.vue',
    'panes/OutlineTree.vue',
    'panes/CardBrowser.vue',
    'panes/StatsPane.vue',
    'panes/CardEditor.vue',
    'FloatingBall.vue',
    'FloatingChat.vue',
  ]) {
    if (!loaded[name]) continue
    try {
      const h = await renderToString(createSSRApp(loaded[name]))
      check(`${name} 独立渲染`, h.length > 0)
    } catch (e) {
      check(`${name} 独立渲染`, false, String(e.message).slice(0, 200))
    }
  }

  log('\n' + '='.repeat(52))
  if (fail === 0) {
    log('🎉 SSR 冒烟全通过：所有组件编译通过、import 链完整、渲染无异常')
  } else {
    log(`⚠ ${fail} 项失败`)
  }
  log('='.repeat(52))
} finally {
  // optimizeDeps 的 esbuild 扫描在后台异步进行，close 太快会抛
  // "The server is being restarted or closed" —— 那是竞态噪音，不是代码缺陷。
  // 等一拍再关，让扫描自然结束，避免日志里出现吓人的红字。
  await new Promise((r) => setTimeout(r, 900))
  await vite.close()
}
process.exit(fail === 0 ? 0 : 1)