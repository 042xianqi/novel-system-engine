<script setup>
// SideNav.vue —— 全局导航壳（同一份 ITEMS，两种形态）
//
// ══════════════════════════════════════════════════════════
// 形态（2026-10-20 第二轮修正：针对手机 WebView 专门重做）
// ══════════════════════════════════════════════════════════
//   ≥768px（md）：左侧固定侧边栏 w-[196px]，常驻，列出全部入口。
//   <768px      ：底部只保留 **3 个** 主入口（工作台 / 对话 / 设置），
//                 其余全部功能（任务/大纲/卡片/星图/统计）收进左上角
//                 **汉堡 → 左侧滑出抽屉**。
//
// 为什么这样改（第一轮的错误）：
//   最初把移动端做成「6 个 Tab 的底部 Dock」。总指挥在 Operit 侧边栏
//   （手机窄屏 WebView）验收时明确指出：底部塞 6 个图标太挤、上方内容更拥挤。
//   现按「3 个主入口 + 抽屉装长尾」的移动端范式重做：
//     · 底部 = 高频三件事，尺寸够大、点得到
//     · 长尾 = 按需滑出，主界面完整让给内容
//
// 状态来源：src/nav.js（**本组件零自有路由状态**）。
// 高亮映射：(nav.tab, nav.pane) → 侧栏项；nav.floatChat → 「对话」高亮。
import { computed, ref, onMounted, onUnmounted, watch } from 'vue'
import { nav, goTab, goPane, toggleFloatChat, PANES } from '../nav.js'

// 侧栏文案比看板标题更短（196px 宽度下「任务看板」会挤）
const SHORT_LABEL = { tasks: '任务' }

// 侧栏 / 抽屉里的**全部**入口
const ITEMS = [
  ...PANES.map((p) => ({
    key: `pane:${p.key}`,
    label: SHORT_LABEL[p.key] || p.label,
    icon: p.icon,
    tab: 'system',
    pane: p.key,
  })),
  { key: 'settings', label: '设置', icon: '⚙', tab: 'settings', pane: null },
]

// 移动端底部：只留 3 个高频入口
const MOBILE_MAIN = [
  { key: 'workbench', label: '工作台', icon: '▦' },
  { key: 'chat', label: '对话', icon: '💬' },
  { key: 'settings', label: '设置', icon: '⚙' },
]

const activeKey = computed(() => {
  if (nav.tab === 'settings') return 'settings'
  if (nav.tab === 'system') return `pane:${nav.pane}`
  return '' // 对话在抽屉里，主区无「对话页」，故不高亮侧栏项
})

const mobileActiveKey = computed(() => {
  if (nav.floatChat) return 'chat'
  if (nav.tab === 'settings') return 'settings'
  return 'workbench' // 其余情况都属于工作台
})

function pick(it) {
  if (it.pane) {
    goTab('system')
    goPane(it.pane)
  } else {
    goTab(it.tab)
  }
  close()
}

function pickMobile(m) {
  if (m.key === 'chat') {
    toggleFloatChat(true)
  } else if (m.key === 'settings') {
    goTab('settings')
  } else {
    goTab('system') // 工作台
  }
}

// ── 移动端抽屉开关（仅 <768px 用；桌面侧栏常驻，与此无关） ──
const open = ref(false)
function toggle() {
  open.value = !open.value
}
function close() {
  open.value = false
}
function onKey(e) {
  if (e.key === 'Escape') close()
}
onMounted(() => window.addEventListener('keydown', onKey))
onUnmounted(() => window.removeEventListener('keydown', onKey))
watch(() => [nav.tab, nav.pane], close)
</script>

<template>
  <!-- ════════ 形态 A：≥768px 左侧固定侧栏 ════════ -->
  <aside
    class="hidden md:flex shrink-0 flex-col w-[196px] glass border-r border-line"
    aria-label="主导航"
  >
    <div class="h-[52px] shrink-0 flex items-center gap-2 px-4">
      <span class="w-1.5 h-1.5 rounded-full bg-primary" aria-hidden="true" />
      <span class="text-[12px] tracking-wide text-content-sec truncate">残响之河</span>
    </div>
    <div class="px-4 pb-1 shrink-0 text-[10px] tracking-wider text-content-sec/70">工作台</div>
    <nav class="flex-1 min-h-0 overflow-y-auto px-2 py-1 flex flex-col gap-0.5">
      <button
        v-for="it in ITEMS"
        :key="it.key"
        type="button"
        class="relative flex items-center gap-2.5 w-full h-9 pl-3.5 pr-2 rounded-lg text-left
               transition-transform active:scale-[0.98]"
        :class="activeKey === it.key ? 'bg-primary/12 text-primary' : 'text-content-sec'"
        :aria-current="activeKey === it.key ? 'page' : undefined"
        @click="pick(it)"
      >
        <span
          class="absolute left-0 top-1.5 bottom-1.5 w-[2px] rounded-full bg-primary pointer-events-none
                 transition-opacity duration-200"
          :class="activeKey === it.key ? 'opacity-100' : 'opacity-0'"
          aria-hidden="true"
        />
        <span class="w-4 text-center leading-none text-[14px]" aria-hidden="true">{{ it.icon }}</span>
        <span class="text-[13px] leading-none truncate">{{ it.label }}</span>
      </button>
    </nav>
  </aside>

  <!-- ════════ 形态 B：<768px 汉堡 + 左侧抽屉 + 底部三入口 ════════ -->
  <button
    type="button"
    class="md:hidden fixed top-2 left-2 z-[60] w-9 h-9 rounded-lg glass-strong
           flex items-center justify-center text-[15px] leading-none
           transition-transform active:scale-[0.94]"
    :aria-expanded="open"
    aria-controls="side-drawer"
    aria-label="打开导航"
    @click="toggle"
  >
    <span aria-hidden="true">{{ open ? '✕' : '☰' }}</span>
  </button>

  <!-- 层级：汉堡 z-60 > 抽屉 z-50 > 遮罩 z-40。
       汉堡必须在最上：否则抽屉打开时遮罩(z-40，DOM 更靠后)会盖住它，
       导致「打开后点不到汉堡关闭」——实测踩过。 -->
  <Transition name="nav-mask">
    <div
      v-if="open"
      class="md:hidden fixed inset-0 z-40 bg-black/50"
      @click="close"
      aria-hidden="true"
    />
  </Transition>

  <Transition name="nav-drawer">
    <aside
      v-if="open"
      id="side-drawer"
      role="dialog"
      aria-label="全部功能"
      class="md:hidden fixed top-0 left-0 bottom-0 z-50 w-[250px] glass-strong
             border-r border-line flex flex-col"
      style="padding-bottom: env(safe-area-inset-bottom, 0px)"
    >
      <div class="h-[52px] shrink-0 flex items-center gap-2 px-4">
        <span class="w-1.5 h-1.5 rounded-full bg-primary" aria-hidden="true" />
        <span class="text-[12px] tracking-wide text-content-sec truncate">残响之河</span>
      </div>
      <div class="px-4 pb-1 shrink-0 text-[10px] tracking-wider text-content-sec/70">
        全部功能
      </div>
      <nav class="flex-1 min-h-0 overflow-y-auto px-2 py-1 flex flex-col gap-0.5">
        <button
          v-for="it in ITEMS"
          :key="it.key"
          type="button"
          class="relative flex items-center gap-2.5 w-full h-11 pl-3.5 pr-2 rounded-lg text-left
                 transition-transform active:scale-[0.98]"
          :class="activeKey === it.key ? 'bg-primary/12 text-primary' : 'text-content-sec'"
          :aria-current="activeKey === it.key ? 'page' : undefined"
          @click="pick(it)"
        >
          <span
            class="absolute left-0 top-1.5 bottom-1.5 w-[2px] rounded-full bg-primary pointer-events-none
                   transition-opacity duration-200"
            :class="activeKey === it.key ? 'opacity-100' : 'opacity-0'"
            aria-hidden="true"
          />
          <span class="w-4 text-center leading-none text-[15px]" aria-hidden="true">{{ it.icon }}</span>
          <span class="text-[13px] leading-none truncate">{{ it.label }}</span>
        </button>
      </nav>
    </aside>
  </Transition>

  <!-- 底部三入口：工作台 / 对话 / 设置
       ⚠️ 必须 fixed：App 壳是**横向 flex**（侧栏 | 主区），若此栏留在文档流里
       就会变成「横向 flex 的一个子项」，实测宽度被压到 72px、三个图标挤在左边。
       fixed + inset-x-0 让它脱离该 flex，真正横贯整屏。 -->
  <nav
    class="md:hidden fixed inset-x-0 bottom-0 z-30 h-[56px] glass border-t border-line flex items-stretch"
    style="padding-bottom: env(safe-area-inset-bottom, 0px)"
    aria-label="主入口"
  >
    <button
      v-for="m in MOBILE_MAIN"
      :key="m.key"
      type="button"
      class="flex-1 flex flex-col items-center justify-center gap-0.5
             transition-transform active:scale-[0.96]"
      :class="mobileActiveKey === m.key ? 'text-primary' : 'text-content-sec'"
      :aria-current="mobileActiveKey === m.key ? 'page' : undefined"
      @click="pickMobile(m)"
    >
      <span class="text-[16px] leading-none" aria-hidden="true">{{ m.icon }}</span>
      <span class="text-[10px] leading-none">{{ m.label }}</span>
    </button>
  </nav>
</template>

<style scoped>
/* 只动 transform / opacity（合成器属性） */
.nav-drawer-enter-active,
.nav-drawer-leave-active {
  transition: transform 260ms cubic-bezier(0.22, 1, 0.36, 1);
}
.nav-drawer-enter-from,
.nav-drawer-leave-to {
  transform: translateX(-100%);
}
.nav-mask-enter-active,
.nav-mask-leave-active {
  transition: opacity 200ms ease;
}
.nav-mask-enter-from,
.nav-mask-leave-to {
  opacity: 0;
}
</style>
