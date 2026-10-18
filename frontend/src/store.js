// store.js —— **业务数据层**（不含界面状态）
//
// ══════════════════════════════════════════════════════════
// 分层约定（2026-10-17 重构，为将来打包 App 做准备）
// ══════════════════════════════════════════════════════════
//
//   nav.js    → 界面状态（tab / pane / 过渡方向 / 悬浮窗形态）   零业务依赖
//   store.js  → 业务数据（聊天 / 任务 / 锁 / toast / 日志）      ← 本文件
//
// 界面状态**已全部搬去 nav.js**，本文件不再持有任何 UI 形态。
//
// 为什么这样分：
//   · 换后端      → 只改 store.js + api.js，nav.js 一行不用动
//   · 换渲染引擎  → 只改组件 + nav.js，业务层全不用动
//   · 两个方向互不牵连 —— 这是「渲染层独立」的实质含义
//
// 历史包袱说明：老组件曾用 `win.state` / `win.log`。
// 已彻底拆分：形态读 `nav.win`（nav.js），日志读 `winLog.items`（本文件）。
// 不留兼容代理 —— 代理返回 ref 会让模板里的 `win.state === 'capsule'` 恒为假，
// 是比多改几个引用点严重得多的隐患。

import { reactive, ref, computed } from 'vue'
import { nav, autoCapsule } from './nav.js'

// ══════════════════════════════════════════ 业务数据

// ── 聊天 ──
export const chat = reactive({
  messages: [],        // {id, role:'user'|'bot', text, typing, plan, ts}
  sending: false,
  // 历史加载（useChat.loadHistory 维护）
  historyLoaded: false,   // 是否已经尝试拉过历史（防重复拉）
  historyLoading: false,  // 拉取中（防并发）
  historyHasMore: false,  // 还有更早的记录可加载
  historyOldestTs: null,  // 当前已显示的最早时间戳（分页游标）
})

// ── 任务（悬浮窗 + 胶囊的数据源）──
export const tasks = reactive({
  items: [],           // 活跃任务（来自 /api/tasks/active）
  counts: { pending: 0, running: 0, blocked: 0, done: 0 },
  executorAlive: false,
  executorPid: null,
  lastDone: null,
  loaded: false,
})

// ── 悬浮窗日志（内容来自任务/聊天事件，本身是业务数据）──
export const winLog = reactive({ items: [] })

// ── 授权锁 ──
export const lock = reactive({
  locked: true,
  unlocked: false,
  label: '',
  color: 'red',
  file: '/sdcard/wogov_real_llm.allow',
  loaded: false,
})

// ── 系统视图数据（大纲 / 卡片 / 看板共用）──
//
// 放在业务层而非界面层：这些是**从后端读来的真实数据**，
// 界面只负责展示。切 tab 不该清空它们（用户切回来要立刻看到，不该重新加载）。
export const systemData = reactive({
  // 任务看板
  taskList: { loading: false, err: '', items: [], count: 0 },
  // 大纲（L0-L3）
  outline: { loading: false, err: '', levels: [], expanded: {} },
  // 卡片（L4）
  cards: {
    loading: false,
    err: '',
    items: [],
    count: 0,
    // 后端真实命中数 / 是否被 limit 截断（truncated=true 时 UI 需提示）
    matched: 0,
    truncated: false,
    // 写操作后的强制刷新若撞上飞行中请求，登记到这里，请求结束后补刷
    pendingReload: false,
    categories: [],
    filterCategory: '',
    filterKey: '',
    page: 0,
    pageSize: 40,
    // 详情缓存：{key: {loading, err, content, chars}}
    detail: {},
    openKey: '',
    // 编辑态：'' | 'create' | 'edit'
    editor: '',
    editorErr: '',
    saving: false,
  },
  // 统计 / 体检
  stats: {
    loading: false,
    err: '',
    loaded: false,
    ts: 0,
    materials: {},
    layers: {},
    funnel: {},
    // 自检独立态（不阻塞统计页其余部分）
    checking: false,
    checkErr: '',
    checkResult: null,
  },
})

// ══════════════════════════════════════════ 工具

// ── 全局 toast ──
export const toasts = ref([])
let toastSeq = 0
export function toast(text, bad = false, ms = 2800) {
  const id = ++toastSeq
  toasts.value.push({ id, text, bad })
  setTimeout(() => {
    toasts.value = toasts.value.filter((t) => t.id !== id)
  }, ms)
}

// ── 日志写入 ──
let logSeq = 0
export function pushLog(text, kind = 'info') {
  winLog.items.push({ id: ++logSeq, ts: Date.now(), text, kind })
  // 只保留最近 200 条，防内存膨胀
  if (winLog.items.length > 200) {
    winLog.items.splice(0, winLog.items.length - 200)
  }
}

/**
 * 任务有活动 → 自动升到胶囊态。
 *
 * 判断依据是**业务数据**（活跃任务数），状态转移在**界面层**（nav.autoCapsule）。
 * 这条边界是刻意划的：以后换数据源，这里只会变；换渲染，只有 nav 会变。
 */
export function syncWinState() {
  autoCapsule(tasks.counts.pending + tasks.counts.running)
}

// ── 悬浮窗是否可见（供 App.vue 决定是否渲染浮层）──
export const winVisible = computed(() => nav.win !== 'hidden')