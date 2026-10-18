<script setup>
// BottomNav.vue —— 三块底部导航（系统 / 对话 / 设置）
//
// 设计约束（用户明确要求）：
//   · 底部固定，**任何视图下都不能消失**（随时能点中间回对话）
//   · 中间「对话」是主入口，视觉上更突出
//   · safe-area-inset-bottom 适配手势条
//
// ══════════════════════════════════════════════════════════
// 状态来源：nav.js（**本组件不持有 tab 状态**）
// ══════════════════════════════════════════════════════════
//
// 改造点（2026-10-17）：
//   之前是 v-model 双向绑定（父传 modelValue，子 emit update）。
//   现在直接读 nav.tab、调 goTab()。为什么改：
//     · v-model 会在 App.vue 里留一份 modelValue 副本，两个事实源容易不同步
//     · 侧边栏桥接（sideBridge）也要切 tab，它只能调 nav.goTab()，
//       如果底栏走自己的 v-model，桥接切的 tab 底栏就显示不出来
//   现在只有一个事实源，任何入口切换都会立刻反映到高亮上。
import { computed } from 'vue'
import { nav, goTab, TABS, tabIndex } from '../nav.js'

const curIdx = computed(() => tabIndex.value)

function pick(k) {
  goTab(k)
}
</script>

<template>
  <nav
    class="shrink-0 relative z-20 glass border-t border-ink-600/70"
    style="padding-bottom: env(safe-area-inset-bottom, 0px)"
  >
    <div class="flex items-stretch h-[52px] relative">
      <!-- 滑动指示条（只用 transform，60fps） -->
      <div
        class="absolute bottom-0 h-[2px] transition-transform duration-300 ease-out-back
               will-change-transform pointer-events-none"
        :style="{
          width: `calc(100% / ${TABS.length})`,
          transform: `translateX(calc(${curIdx} * 100%))`,
        }"
      >
        <div class="mx-auto w-4 h-[2px] bg-acc rounded-full" />
      </div>

      <button
        v-for="it in TABS"
        :key="it.key"
        class="flex-1 flex flex-col items-center justify-center gap-0.5
               transition-colors active:scale-[0.97]"
        :class="nav.tab === it.key ? 'text-acc' : 'text-fg-dim'"
        @click="pick(it.key)"
      >
        <span
          class="leading-none transition-all duration-200"
          :class="nav.tab === it.key ? 'text-[17px]' : 'text-[15px]'"
        >{{ it.icon }}</span>
        <span class="text-[10px] leading-none">{{ it.label }}</span>
      </button>
    </div>
  </nav>
</template>