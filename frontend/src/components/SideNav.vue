<script setup>
// SideNav.vue —— 全局导航壳（同一套组件、两种形态）
//
// ══════════════════════════════════════════════════════════
// 形态
// ══════════════════════════════════════════════════════════
//   ≥768px（md）：左侧固定侧边栏，w-[196px]，常驻（shell 不滚动，主区自己滚）
//   <768px      ：收成底部 Dock，h-[52px]，padding-bottom 吃 env(safe-area-inset-bottom)
//
// 两种形态由同一份 ITEMS 渲染，只有外壳类名不同 —— 不会出现「两套导航」。
//
// ══════════════════════════════════════════════════════════
// 状态来源：src/nav.js（**本组件零自有路由状态**）
// ══════════════════════════════════════════════════════════
// 导航条目直接由 nav.js 的 PANES（任务/大纲/卡片/星图/统计，顺序与图标同源）
// 加「设置」tab 派生，高亮由 (nav.tab, nav.pane) 映射而来：
//   nav.tab==='settings' → 高亮「设置」
//   nav.tab==='system'   → 高亮 nav.pane 对应项
// 点击只调 nav.js 的 goTab()/goPane()（幂等纯函数），不写第二份路由状态，
// 因此侧边栏桥接（sideBridge → goTab/goPane）切页时高亮必然同步。
//
// nav.js **无需扩展字段**：现有 TABS/PANES/tab/pane 已能完整表达新导航，
// 故本次未改动 nav.js（详见交付说明）。
import { computed } from 'vue'
import { nav, goTab, goPane, PANES } from '../nav.js'

// 侧栏文案比看板标题更短（196px 宽度下「任务看板」会挤）
const SHORT_LABEL = { tasks: '任务' }

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

const activeKey = computed(() => {
  if (nav.tab === 'settings') return 'settings'
  if (nav.tab === 'system') return `pane:${nav.pane}`
  return '' // 对话已收进抽屉，主区无「对话页」，故不高亮任何侧栏项
})

function pick(it) {
  if (it.pane) {
    goTab('system')
    goPane(it.pane)
  } else {
    goTab(it.tab)
  }
}
</script>

<template>
  <!-- ════════ 形态 A：≥768px 左侧固定侧边栏 ════════ -->
  <aside
    aria-label="主导航"
    class="hidden md:flex md:flex-col w-[196px] shrink-0 h-full glass border-r border-line relative z-20"
  >
    <!-- 品牌位（纯文字，不做大色块） -->
    <div class="h-[52px] shrink-0 flex items-center gap-2 px-4">
      <span class="w-1.5 h-1.5 rounded-full bg-primary" aria-hidden="true" />
      <span class="text-[12px] tracking-wide text-content-sec truncate">残响之河</span>
    </div>

    <div class="px-4 pb-1 shrink-0 text-[10px] tracking-wider text-content-sec/70">
      工作台
    </div>

    <nav class="flex-1 min-h-0 overflow-y-auto px-2 py-1 flex flex-col gap-0.5">
      <button
        v-for="it in ITEMS"
        :key="it.key"
        type="button"
        class="relative flex items-center gap-2.5 w-full h-9 pl-3.5 pr-2 rounded-lg text-left
               transition-transform active:scale-[0.98]"
        :class="activeKey === it.key ? 'bg-primary/12 text-primary' : 'text-content-sec hover:text-content'"
        :aria-current="activeKey === it.key ? 'page' : undefined"
        :title="it.label"
        @click="pick(it)"
      >
        <!-- 左侧高亮条：只动 opacity（合成器属性） -->
        <span
          class="absolute left-0 top-1.5 bottom-1.5 w-[2px] rounded-full bg-primary pointer-events-none
                 transition-opacity duration-200"
          :class="activeKey === it.key ? 'opacity-100' : 'opacity-0'"
          aria-hidden="true"
        />
        <span
          class="w-4 text-center leading-none text-[14px] transition-transform duration-200"
          :class="activeKey === it.key ? 'scale-[1.12]' : ''"
          aria-hidden="true"
        >{{ it.icon }}</span>
        <span class="text-[12.5px] leading-none truncate">{{ it.label }}</span>
      </button>
    </nav>
  </aside>

  <!-- ════════ 形态 B：<768px 底部 Dock（保留 safe-area） ════════ -->
  <nav
    aria-label="主导航"
    class="md:hidden fixed bottom-0 left-0 right-0 z-30 glass border-t border-line"
    style="padding-bottom: env(safe-area-inset-bottom, 0px)"
  >
    <div class="flex items-stretch h-[52px]">
      <button
        v-for="it in ITEMS"
        :key="it.key"
        type="button"
        class="flex-1 min-w-0 flex flex-col items-center justify-center gap-0.5
               transition-transform active:scale-[0.96]"
        :class="activeKey === it.key ? 'text-primary' : 'text-content-sec'"
        :aria-current="activeKey === it.key ? 'page' : undefined"
        :aria-label="it.label"
        @click="pick(it)"
      >
        <span
          class="leading-none text-[15px] transition-transform duration-200"
          :class="activeKey === it.key ? 'scale-[1.14]' : ''"
          aria-hidden="true"
        >{{ it.icon }}</span>
        <span class="text-[10px] leading-none truncate max-w-full px-0.5">{{ it.label }}</span>
      </button>
    </div>
  </nav>
</template>
