<script setup>
// App.vue —— 全局布局壳（2026-10-20 布局重构：侧栏 + 状态栏 + 聊天抽屉）
//
// ══════════════════════════════════════════════════════════
// 布局
// ══════════════════════════════════════════════════════════
//   ┌────────┬──────────────────────────────────────┐
//   │        │ StatusBar（fixed 右上，不占流）        │
//   │ SideNav├──────────────────────────────────────┤
//   │ 196px  │ 主区（系统 5 子页 / 设置，互斥 v-show） │
//   │ (md+)  │                                      │
//   └────────┴──────────────────────────────────────┘
//   <768px：SideNav 变底部 Dock(h-52px)，主区吃 padding-bottom（含 safe-area）
//
//   聊天：右下角悬浮球 💬 → 点击滑出右侧抽屉（transform + opacity 动画，
//         宽 min(92vw,420px)，玻璃用 .glass-strong）
//   任务执行窗：Capsule / FloatWindow 常驻挂载（入口是状态栏的执行器胶囊）
//
// ══════════════════════════════════════════════════════════
// 状态：**全部来自 ./nav.js，本文件不持有任何路由状态**
// ══════════════════════════════════════════════════════════
//   nav.tab / nav.pane / nav.dir / nav.floatChat  —— 切换与抽屉开关都走 nav.js 的
//   goTab()/goPane()/toggleFloatChat() 幂等转移函数；侧栏桥接（sideBridge）与
//   侧边栏 ToolPkg 因此和 UI 天然共用同一个事实源。
//
//   · 抽屉开合用既有字段 nav.floatChat（未新增字段，nav.js 本次零改动）
//   · 唯一行为适配：对话已收进抽屉 → nav.tab==='chat' 时主区无对应页面，
//     故 (a) 挂载时把 'chat' 归一到 'system'；(b) 桥接要求 chat 时改为「开抽屉」。
//
// ══════════════════════════════════════════════════════════
// 动画（60fps 约定）
// ══════════════════════════════════════════════════════════
//   只用 transform / opacity，且不写 transition-all（避免带动 layout 属性）。
//   主视图沿用 style.css 里既有的 slide-left / slide-right（will-change 已就位）。
import { ref, computed, onMounted, onUnmounted, nextTick } from 'vue'
import ChatStream from './components/ChatStream.vue'
import Composer from './components/Composer.vue'
import FloatWindow from './components/FloatWindow.vue'
import Capsule from './components/Capsule.vue'
import ToastHost from './components/ToastHost.vue'
import SideNav from './components/SideNav.vue'
import StatusBar from './components/StatusBar.vue'
import SystemView from './components/SystemView.vue'
import SettingsView from './components/SettingsView.vue'
import { useTasks } from './composables/useTasks.js'
import { useChat } from './composables/useChat.js'
import { installSideBridge } from './sideBridge.js'
import { installRipple } from './composables/useRipple.js'
import { nav, goTab, slideName, toggleFloatChat } from './nav.js'
import { bus, EV } from './events.js'

const composer = ref(null)
const drawerEl = ref(null)
// 事件订阅句柄（卸载时调用；null = 未订阅）
let offSend = null
let offView = null
const { start, stop } = useTasks()
const { send, loadHistory } = useChat()

/** 顶层过渡名（方向由 nav.js 算，这里只读） */
const transitionName = computed(() => slideName.value)

// ══════════════════════════════════════════════ 聊天抽屉
//
// 抽屉状态 = nav.floatChat（复用既有字段，不新增路由状态）。
// 悬浮球既是入口也是出口：开着的时候球变 ✕，用户不需要满屏找关闭按钮。
const drawerOpen = computed(() => nav.floatChat)

async function toggleDrawer(force) {
  const open = toggleFloatChat(force === undefined ? undefined : !!force)
  if (open) {
    await nextTick()
    // 焦点进抽屉：键盘用户/读屏用户不会「点了球但还在原地」
    drawerEl.value?.focus?.()
  }
}

/** Esc 关抽屉（抽屉里的输入框按 Esc 不应误关，故只在无 IME/无输入焦点时处理） */
function onKeydown(e) {
  if (e.key !== 'Escape' || !drawerOpen.value) return
  const t = e.target
  const typing =
    t && (t.tagName === 'INPUT' || t.tagName === 'TEXTAREA' || t.isContentEditable)
  if (typing) return
  toggleDrawer(false)
}

// ══════════════════════════════════════════════ 视图互斥
//
// 主区只有两块：系统（5 子页）与设置。
// tab==='chat' 视为系统（对话本身在抽屉里），保证任何来源切到 chat 都不会白屏。
const showSystem = computed(() => nav.tab === 'system' || nav.tab === 'chat')
const showSettings = computed(() => nav.tab === 'settings')

/** 快捷指令：填入输入框（不自动发送，给用户改的机会） */
function onQuick(text) {
  composer.value?.fill(text)
}

/** 侧边栏 ToolPkg 通过 __novSend 钩子派发的消息在这里落地 */
function onSideSend(p) {
  const t = p?.text
  if (t) send(t)
}

/** 侧边栏要求切视图：{tab, pane} 或旧的 {view}；chat 一律解释为「开抽屉」 */
function onSideView(d) {
  const tab = d?.tab || d?.view
  if (!tab) return
  if (tab === 'chat') {
    toggleDrawer(true)
    return
  }
  goTab(tab)
}

onMounted(() => {
  // 对话不再是主区页面：开机若停在 chat，主区会空，故归一到系统视图首屏
  if (nav.tab === 'chat') goTab('system')
  start() // 启动自适应轮询（有任务 3s / 空闲 8s）
  loadHistory() // 拉回历史消息
  installSideBridge() // 与「侧边栏大脑系统」ToolPkg 桥接
  installRipple() // 全局点击涟漪（事件委托）
  offSend = bus.on(EV.SIDE_SEND, onSideSend)
  offView = bus.on(EV.SIDE_VIEW, onSideView)
  window.addEventListener('keydown', onKeydown)
})

onUnmounted(() => {
  stop()
  window.removeEventListener('keydown', onKeydown)
  if (offSend) offSend()
  if (offView) offView()
})
</script>

<template>
  <div class="h-full flex bg-surface relative overflow-hidden">
    <!-- 背景微光：只用语义 token 的 CSS 变量，不写死颜色（切皮肤自动跟随） -->
    <div
      class="pointer-events-none absolute inset-0 opacity-[0.55]"
      style="background:
        radial-gradient(60% 40% at 18% 0%, rgb(var(--primary-rgb) / 0.10), transparent 65%),
        radial-gradient(50% 35% at 88% 8%, rgb(var(--success-rgb) / 0.07), transparent 60%),
        radial-gradient(70% 45% at 50% 100%, rgb(var(--primary-rgb) / 0.06), transparent 70%)"
      aria-hidden="true"
    />

    <!-- ══════ 导航壳：桌面左侧栏 / 移动底部 Dock（同一组件两种形态） ══════ -->
    <SideNav />

    <!-- ══════ 主区 ══════ -->
    <main class="shell-main flex-1 min-w-0 flex flex-col relative z-10">
      <div class="flex-1 min-h-0 relative overflow-hidden">
        <!-- 系统视图（含任务/大纲/卡片/星图/统计 5 子页） -->
        <Transition :name="transitionName">
          <div
            v-show="showSystem"
            key="system"
            class="absolute inset-0"
          >
            <SystemView />
          </div>
        </Transition>

        <!-- 设置中心 -->
        <Transition :name="transitionName">
          <div
            v-show="showSettings"
            key="settings"
            class="absolute inset-0"
          >
            <SettingsView />
          </div>
        </Transition>
      </div>
    </main>

    <!-- ══════ 顶部全局状态栏（固定右上：真实 LLM 锁定态 / 执行器状态） ══════ -->
    <StatusBar />

    <!-- ══════ 聊天抽屉（右下角悬浮球滑出） ══════
         层级约定沿用全局：
           遮罩 z-65 / 抽屉 z-66 / 悬浮球 z-70
         width = min(92vw, 420px)；材质 = .glass-strong
    -->
    <div class="fixed inset-0 z-[66] pointer-events-none">
      <!-- 遮罩：只动 opacity -->
      <Transition name="drawer-mask">
        <div
          v-if="drawerOpen"
          class="absolute inset-0 bg-black/45 pointer-events-auto"
          @click="toggleDrawer(false)"
        />
      </Transition>

      <!-- 面板：只动 transform（从右侧滑入） -->
      <Transition name="drawer-panel">
        <aside
          v-if="drawerOpen"
          ref="drawerEl"
          id="chat-drawer"
          tabindex="-1"
          role="dialog"
          aria-modal="true"
          aria-label="对话"
          class="drawer-panel absolute top-0 right-0 h-full glass-strong border-l border-line
                 flex flex-col pointer-events-auto outline-none"
          style="width: min(92vw, 420px); padding-bottom: env(safe-area-inset-bottom, 0px)"
        >
          <header class="h-[52px] shrink-0 flex items-center gap-2 px-3 border-b border-line-divider">
            <span class="w-1.5 h-1.5 rounded-full bg-primary shrink-0" aria-hidden="true" />
            <span class="text-[12.5px] text-content-sec truncate flex-1">对话中枢</span>
            <button
              type="button"
              class="w-8 h-8 rounded-lg text-content-sec flex items-center justify-center
                     transition-transform active:scale-95"
              aria-label="关闭对话"
              @click="toggleDrawer(false)"
            >✕</button>
          </header>

          <ChatStream class="flex-1 min-h-0" @quick="onQuick" />
          <Composer ref="composer" class="shrink-0" />
        </aside>
      </Transition>
    </div>

    <!-- 💬 悬浮球：右下角，既是入口也是出口（开着时变 ✕） -->
    <button
      type="button"
      class="chat-fab fixed right-4 z-[70] w-12 h-12 rounded-full glass-strong border border-line
             flex items-center justify-center text-[20px] leading-none
             transition-transform active:scale-95"
      :aria-label="drawerOpen ? '关闭对话' : '打开对话'"
      :aria-expanded="drawerOpen"
      aria-controls="chat-drawer"
      @click="toggleDrawer()"
    >
      <span aria-hidden="true">{{ drawerOpen ? '✕' : '💬' }}</span>
    </button>

    <!-- ══════ 任务执行浮层：常驻挂载（任意视图都能从状态栏胶囊打开），
         避免旧实现「在系统视图点开却看不见窗口」的问题 ══════ -->
    <Capsule />
    <FloatWindow />

    <ToastHost />
  </div>
</template>

<style scoped>
/* ── 主区底部让位给移动端 Dock（含 safe-area） ── */
.shell-main {
  padding-bottom: calc(52px + env(safe-area-inset-bottom, 0px));
}
@media (min-width: 768px) {
  .shell-main {
    padding-bottom: 0;
  }
}

/* ── 悬浮球定位：移动端抬到 Dock 之上，桌面贴右下 ── */
.chat-fab {
  bottom: calc(52px + env(safe-area-inset-bottom, 0px) + 12px);
}
@media (min-width: 768px) {
  .chat-fab {
    bottom: 20px;
  }
}

/* ── 抽屉动画：只动 transform / opacity，绝不使用 transition-all ── */
.drawer-panel {
  will-change: transform;
}
.drawer-panel-enter-active {
  transition: transform 260ms cubic-bezier(0.22, 1, 0.36, 1), opacity 200ms ease-out;
}
.drawer-panel-leave-active {
  transition: transform 200ms cubic-bezier(0.4, 0, 1, 1), opacity 160ms ease-in;
}
.drawer-panel-enter-from,
.drawer-panel-leave-to {
  transform: translateX(100%);
  opacity: 0.85;
}

.drawer-mask-enter-active,
.drawer-mask-leave-active {
  transition: opacity 200ms ease;
}
.drawer-mask-enter-from,
.drawer-mask-leave-to {
  opacity: 0;
}
</style>
