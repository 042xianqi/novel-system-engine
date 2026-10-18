// nav.js —— **UI 驱动层**（纯界面状态，不含任何业务数据）
//
// ══════════════════════════════════════════════════════════
// 为什么单独拆一个文件（用户明确要求：渲染层必须独立）
// ══════════════════════════════════════════════════════════
//
// store.js 里混着两类东西：
//   · 业务数据：chat.messages / tasks.items / lock.locked
//   · 界面状态：当前在哪个 tab、悬浮窗什么形态
//
// 未来要打包成 App、可能换渲染引擎，**界面状态是必须能复用的**，
// 业务数据则可能整个换掉（换后端、换协议）。所以这里只放：
//
//   ① 顶层路由（tab）：system | chat | settings
//   ② 系统视图内部子 tab：tasks | outline | cards
//   ③ 过渡方向（决定滑入滑出往哪边推，用于 60fps 动画）
//   ④ 悬浮窗形态机（hidden/capsule/expanded/full）
//
// 本文件**零业务依赖**：不 import api.js、不 import store.js。
// 任何组件都可以 import 它，它永远不产生网络请求、不读数据库。
//
// ── 设计约束 ──────────────────────────────────────────────
// · 纯响应式状态 + 纯函数转移，没有副作用（不写 localStorage、不发请求）
// · 转移函数全部幂等：重复调用同一目标不会产生额外状态变化
//   （这一点很重要：侧边栏 ToolPkg 可能重复发同一指令）
// · 过渡方向由「新索引 - 旧索引」的符号决定，索引固定，
//   所以即使从设置跳回系统（跨越 2 格），动画方向仍然正确

import { reactive, computed } from 'vue'

/** 顶层三个入口，**数组顺序即视觉顺序**（底栏从左到右） */
export const TABS = [
  { key: 'system', label: '系统', icon: '▤' },
  { key: 'chat', label: '对话', icon: '◈' },
  { key: 'settings', label: '设置', icon: '⚙' },
]

/** 系统视图内部的三个子页 */
export const PANES = [
  { key: 'tasks', label: '任务看板', icon: '▶' },
  { key: 'outline', label: '大纲', icon: '≡' },
  { key: 'cards', label: '卡片', icon: '▦' },
  { key: 'stats', label: '统计', icon: '◑' },
]

const TAB_INDEX = Object.fromEntries(TABS.map((t, i) => [t.key, i]))
const PANE_INDEX = Object.fromEntries(PANES.map((p, i) => [p.key, i]))

/** 悬浮窗形态：hidden（不可见）| capsule（胶囊）| expanded（浮窗）| full（全屏） */
export const WIN_STATES = ['hidden', 'capsule', 'expanded', 'full']

// ══════════════════════════════════════════ 状态
export const nav = reactive({
  /** 顶层：system | chat | settings */
  tab: 'chat',
  /** 系统视图子页：tasks | outline | cards */
  pane: 'tasks',
  /** 过渡方向：1 = 新页从右进（往左推）；-1 = 新页从左进 */
  dir: 1,
  /** 悬浮窗形态 */
  win: 'hidden',
  /** 是否已经切过一次（首帧不做动画，避免开机就滑一下） */
  booted: false,
  /**
   * 全局悬浮聊天窗是否打开（由 💬 对话球驱动）。
   *
   * 与 nav.tab='chat' 是两个独立维度：
   *   · tab='chat'  → 主视图就是对话，此时对话球自动隐藏
   *   · floatChat   → 在其它视图上浮一层对话卡片，主视图位置不动
   * 互斥关系在 App.vue 里保证，不在这里加副作用。
   */
  floatChat: false,
})

// ══════════════════════════════════════════ 计算
export const tabIndex = computed(() => TAB_INDEX[nav.tab] ?? 0)
export const paneIndex = computed(() => PANE_INDEX[nav.pane] ?? 0)
/** Vue <Transition> 用的方向名 */
export const slideName = computed(() => (nav.dir >= 0 ? 'slide-left' : 'slide-right'))

// ══════════════════════════════════════════ 转移（纯函数，幂等）
/**
 * 切顶层 tab。
 *
 * 幂等：目标与当前相同时**不动 dir**（否则重复点同一个 tab 会让过渡方向乱跳）。
 * 返回是否真的发生了切换，调用方可据此决定要不要播动画。
 */
export function goTab(key) {
  if (!(key in TAB_INDEX)) return false
  if (key === nav.tab) return false
  nav.dir = TAB_INDEX[key] > TAB_INDEX[nav.tab] ? 1 : -1
  nav.tab = key
  nav.booted = true
  return true
}

/**
 * 切系统视图子页。同样幂等。
 *
 * 子页过渡方向**独立计算**，不受顶层 dir 影响 ——
 * 因为顶层已经用 v-show 缓存，子页在内部动，两者叠加会互相干扰。
 */
export function goPane(key) {
  if (!(key in PANE_INDEX)) return false
  if (key === nav.pane) return false
  nav.dir = PANE_INDEX[key] > PANE_INDEX[nav.pane] ? 1 : -1
  nav.pane = key
  return true
}

/** 悬浮窗：展开 */
export function openWin() {
  if (nav.win === 'hidden' || nav.win === 'capsule') nav.win = 'expanded'
}

/** 悬浮窗：最小化到胶囊 */
export function minimizeWin() {
  nav.win = 'capsule'
}

/** 悬浮窗：彻底隐藏 */
export function closeWin() {
  nav.win = 'hidden'
}

/** 悬浮窗：展开/全屏互切 */
export function toggleFull() {
  nav.win = nav.win === 'full' ? 'expanded' : 'full'
}

/**
 * 业务侧驱动的自动弹胶囊（有活跃任务时）。
 *
 * 只负责「从 hidden 升到 capsule」，不做降级 ——
 * 降级由用户操作或任务真正结束决定，避免任务一结束界面自己缩回去。
 */
export function autoCapsule(activeCount) {
  if (activeCount > 0 && nav.win === 'hidden') nav.win = 'capsule'
}

/** 从外部（侧边栏桥接）一次性设置视图，做了参数校验 */
export function setView(tab, pane) {
  const changed = goTab(tab)
  if (pane) goPane(pane)
  return changed
}

/** 悬浮聊天窗：开/关（幂等） */
export function toggleFloatChat(force) {
  const next = typeof force === 'boolean' ? force : !nav.floatChat
  nav.floatChat = next
  return next
}

/** 调试/测试用：把状态复位到初始值 */
export function resetNav() {
  nav.tab = 'chat'
  nav.pane = 'tasks'
  nav.dir = 1
  nav.win = 'hidden'
  nav.floatChat = false
  nav.booted = false
}