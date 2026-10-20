<script setup>
// App.vue —— 应用外壳（三视图 + 固定底栏 + 60fps 过渡）
//
// 布局结构：
//   ┌──────────────────────────────┐
//   │ LockBar（授权锁细条）          │  shrink-0（仅对话视图）
//   ├──────────────────────────────┤
//   │ 视图区（顶层 <Transition>）    │  flex-1
//   │   · system   → SystemView（三子页）
//   │   · chat     → ChatStream + Composer
//   │   · settings → SettingsView
//   ├──────────────────────────────┤
//   │ BottomNav（系统/对话/设置）     │  shrink-0 **永不消失**
//   └──────────────────────────────┘
//   浮层：Capsule / FloatWindow / ToastHost
//
// ══════════════════════════════════════════════════════════
// 架构说明（2026-10-17 驱动层重构）
// ══════════════════════════════════════════════════════════
//
// **本组件不持有任何界面状态**，全部来自 nav.js：
//   当前 tab、过渡方向、悬浮窗形态 …… 都由 nav.js 提供
//   本组件只做「组装 + 生命周期」+ 声明式渲染
//
// 为什么这样做（用户要求：渲染层必须独立，为打包 App 做准备）：
//   换渲染引擎时，只要新引擎能读 nav.js 的状态、调它的转移函数，
//   整个路由与动画逻辑就能原样复用 —— 业务数据层（store.js）完全不用碰。
//
// ── 动画为什么能 60fps ─────────────────────────────────
//   ① 只用 transform + opacity（合成器属性），不碰 width/left/top
//   ② 过渡容器加 will-change: transform，提前提升为合成层
//   ③ 离开的元素 position: absolute，不从文档流里挤出布局抖动
//   ④ 过渡时长 240~300ms，落在「感觉即时」与「看得见」的甜点区
//
// ── 状态保留 ───────────────────────────────────────────
//   三个视图全部常驻（v-show 而非 v-if），所以：
//     · 对话记录不丢
//     · 大纲折叠状态不丢
//     · 卡片筛选/分页不丢
//   代价是三个视图同时在 DOM 里，但它们各自的请求都做了缓存，
//   不会因为常驻而反复打网络。
import { ref, onMounted, onUnmounted, computed } from 'vue'
import LockBar from './components/LockBar.vue'
import ChatStream from './components/ChatStream.vue'
import Composer from './components/Composer.vue'
import FloatWindow from './components/FloatWindow.vue'
import Capsule from './components/Capsule.vue'
import ToastHost from './components/ToastHost.vue'
import BottomNav from './components/BottomNav.vue'
import SystemView from './components/SystemView.vue'
import SettingsView from './components/SettingsView.vue'
import FloatingBall from './components/FloatingBall.vue'
import FloatingChat from './components/FloatingChat.vue'
import { useTasks } from './composables/useTasks.js'
import { useChat } from './composables/useChat.js'
import { installSideBridge } from './sideBridge.js'
import { installRipple } from './composables/useRipple.js'
import { nav, goTab, slideName, toggleFloatChat, openWin } from './nav.js'
import { bus, EV } from './events.js'
import { tasks } from './store.js'

const composer = ref(null)
// 事件订阅的 unsubscribe 句柄（bus.on 返回，卸载时调用；null 表示未订阅）
let offSend = null
let offView = null
const { start, stop } = useTasks()
const { send, loadHistory } = useChat()

/** 顶层过渡名（方向由 nav.js 算，本组件只读） */
const transitionName = computed(() => slideName.value)

// ══════════════════════════════════════════════ 悬浮球
//
// 两个球：💬 对话球 / ⚙ 任务球
// · 对话球：进入「对话」主视图时自动隐藏（那里本来就能打字，球是多余的）
// · 任务球：角标显示待执行+运行中的数量，点击直接展开任务悬浮窗
// 对话球：非对话主视图时显示。
// 注意**不加** !nav.floatChat —— 浮层打开时球必须还在，
// 否则用户看不到自己在哪一层，只能靠浮层里的 ✕ 退出，体验割裂。
const showChatBall = computed(() => nav.tab !== 'chat')
const taskBadge = computed(() => tasks.counts.pending + tasks.counts.running)
const taskBusy = computed(() => tasks.counts.running > 0)

function onChatBallTap() {
  // 点对话球 → 在当前视图上浮出对话卡片（不跳页，主视图位置不动）。
  // 用 toggle 语义：再点一次球就收起，球本身永远在 z-70 最上层，
  // 既是入口也是出口，不依赖卡片里的 ✕。
  toggleFloatChat()
}
function onTaskBallTap() {
  // 任务球是全局入口。FloatWindow 原本只在对话视图挂载，
  // 在系统视图点球会「状态变了但看不见窗口」——所以这里补一条：
  // 若当前不在对话视图，先切过去，再展开窗口。
  if (nav.tab !== 'chat') {
    goTab('chat')
    // 等一帧让 ChatStream 挂载完再开窗，避免窗口在 DOM 还没就位时打开
    requestAnimationFrame(() => openWin())
    return
  }
  openWin()
}

/** 侧边栏 ToolPkg 通过 __novSend 钩子派发的消息在这里落地 */
function onSideSend(p) {
  const t = p?.text
  if (t) send(t)
}

/** 侧边栏要求切视图：{tab, pane} 或旧的 {view} */
function onSideView(d) {
  const tab = d?.tab || d?.view
  if (tab) goTab(tab)
}

onMounted(() => {
  start() // 启动自适应轮询（有任务 3s / 空闲 8s）
  // 拉回上次的对话记录：没有这一步，用户一刷新就"聊天记录全没了"，
  // 而数据其实一直在 session.db 里躺着。
  loadHistory()
  // 与「侧边栏大脑系统」ToolPkg 桥接
  installSideBridge()
  // 全局点击涟漪：事件委托，一次注册全站生效。
  // 这样老组件（没用 BaseButton 的那些）也自动获得物理确认感。
  installRipple()
  offSend = bus.on(EV.SIDE_SEND, onSideSend)
  offView = bus.on(EV.SIDE_VIEW, onSideView)
})

onUnmounted(() => {
  stop()
  if (offSend) offSend()
  if (offView) offView()
})

/** 快捷指令：填入输入框（不自动发送，给用户改的机会） */
function onQuick(text) {
  composer.value?.fill(text)
}
</script>

<style>
/* 对话球出现/消失：弹跳缩放（与其他悬浮卡片共用同一条曲线） */
.ball-pop-enter-active {
  transition: transform 300ms cubic-bezier(.22, 1.4, .36, 1), opacity 220ms ease;
}
.ball-pop-leave-active {
  transition: transform 200ms cubic-bezier(.4, 0, 1, 1), opacity 180ms ease;
}
.ball-pop-enter-from,
.ball-pop-leave-to {
  opacity: 0;
  /* 注意：悬浮球自身用 translate3d 定位，这里的 scale 会与之叠加，
     所以只做透明度 + 轻微缩放，且用 CSS 变量避免覆盖内联 transform。 */
}
.ball-pop-enter-from .fb-ball,
.ball-pop-leave-to .fb-ball {
  filter: brightness(1.3);
}
</style>

<template>
  <div class="h-full flex flex-col bg-ink-900 relative overflow-hidden">
    <!-- 背景微光（提升质感，不影响性能） -->
    <div
      class="pointer-events-none absolute inset-0 opacity-[0.55]"
      style="background:
        radial-gradient(60% 40% at 18% 0%, rgba(90,169,230,.10), transparent 65%),
        radial-gradient(50% 35% at 88% 8%, rgba(90,209,138,.07), transparent 60%),
        radial-gradient(70% 45% at 50% 100%, rgba(90,169,230,.06), transparent 70%)"
    />

    <!-- 顶部锁条：只在对话视图（系统视图自带状态条，重复会挤占空间） -->
    <LockBar v-if="nav.tab === 'chat'" />

    <!-- ══════ 视图区（顶层过渡）══════ -->
    <div class="flex-1 min-h-0 relative z-10 overflow-hidden">
      <!--
        用 v-show 而非 v-if：三个视图常驻，状态不丢。
        Transition 用 mode="out-in" 会让总时长翻倍且中间有一帧空白，
        这里改用「同时存在、互相覆盖」的方式：离开的 absolute 定位，
        进来的从侧边滑入盖住它 —— 视觉上更连续，也不会有空白帧。
      -->
      <Transition :name="transitionName">
        <!-- 系统视图 -->
        <div
          v-show="nav.tab === 'system'"
          key="system"
          class="absolute inset-0"
        >
          <SystemView />
        </div>
      </Transition>

      <Transition :name="transitionName">
        <!-- 对话中枢 -->
        <div
          v-show="nav.tab === 'chat'"
          key="chat"
          class="absolute inset-0 flex flex-col"
        >
          <ChatStream class="flex-1 min-h-0" @quick="onQuick" />
          <Composer ref="composer" class="shrink-0" />
        </div>
      </Transition>

      <Transition :name="transitionName">
        <!-- 设置中心 -->
        <div
          v-show="nav.tab === 'settings'"
          key="settings"
          class="absolute inset-0"
        >
          <SettingsView />
        </div>
      </Transition>
    </div>

    <!-- ══════ 固定底栏：任何视图都不消失 ══════ -->
    <BottomNav />

    <!-- ══════ 全局双悬浮球（可拖拽 / 自动吸边）══════
         z-index 层级约定（全局统一）：
           背景遮罩        z-40 ~ z-50
           业务悬浮卡片    z-50 ~ z-60
           悬浮窗/胶囊     z-60 ~ z-65
           悬浮聊天卡片    z-66
           悬浮球          z-70   ← 永远在最上层，随时能抓
    -->
    <FloatingChat :open="nav.floatChat" @close="toggleFloatChat(false)" />

    <!-- 💬 对话球：只在非对话视图显示（对话主视图本来就能打字） -->
    <Transition name="ball-pop">
      <FloatingBall
        v-if="showChatBall"
        id="chat"
        icon="💬"
        label="打开对话"
        side="left"
        :top="240"
        tone="acc"
        @tap="onChatBallTap"
      />
    </Transition>

    <!-- ⚙ 任务球：角标显示活跃任务数，运行中时呼吸 -->
    <FloatingBall
      id="task"
      icon="⚙"
      label="任务进度"
      side="right"
      :top="330"
      :tone="taskBusy ? 'warn' : 'ok'"
      :badge="taskBadge"
      :pulse="taskBusy"
      @tap="onTaskBallTap"
    />

    <!-- 浮层：只在对话视图显示，避免遮挡系统视图的按钮 -->
    <template v-if="nav.tab === 'chat'">
      <Capsule />
      <FloatWindow />
    </template>
    <ToastHost />
  </div>
</template>