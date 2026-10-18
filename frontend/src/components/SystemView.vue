<script setup>
// SystemView.vue —— 系统视图（**已摆脱 iframe**，2026-10-17 重构）
//
// ══════════════════════════════════════════════════════════
// 改造前 → 改造后
// ══════════════════════════════════════════════════════════
//
//   改造前：<iframe src="/legacy">  旧面板 98KB 单文件 HTML
//           · 样式与新前端不统一（旧面板是自成一体的深色）
//           · iframe 里滚动/触摸与外壳割裂
//           · 无法共享 store，切 tab 状态全丢
//
//   改造后：三个原生 Vue 子页 + 顶部子导航
//           · 任务看板 / 大纲 / 卡片，全部走统一 glass 风格
//           · 直接读写 store.systemData，切子页不丢状态、不重拉
//           · 子页切换走 <Transition> 滑入滑出
//
// 旧面板仍然保留在 /legacy（不删），需要时用浏览器直接开；
// 侧边栏 ToolPkg 的 WebView 加载的是新前端，所以它看到的就是这三个子页。
//
// 子导航为什么放在视图内部而不是底栏：
//   底栏是「顶层路由」（系统/对话/设置），子页是「系统内的局部导航」。
//   混在一起会让底栏变成 5 个入口，主次不分。且底栏空间要留给主入口。

import { computed, onMounted } from 'vue'
import { nav, goPane, PANES, paneIndex } from '../nav.js'
import TaskBoard from './panes/TaskBoard.vue'
import OutlineTree from './panes/OutlineTree.vue'
import CardBrowser from './panes/CardBrowser.vue'
import StatsPane from './panes/StatsPane.vue'

const PANE_MAP = {
  tasks: TaskBoard,
  outline: OutlineTree,
  cards: CardBrowser,
  stats: StatsPane,
}

const current = computed(() => PANE_MAP[nav.pane] || TaskBoard)
const curIdx = computed(() => paneIndex.value)

/** 子页过渡方向：往右切 → 新页从右进 */
const paneDir = computed(() => (nav.dir >= 0 ? 'pane-left' : 'pane-right'))

// 首帧预取：任务看板是最常看的，挂载时就拉一次
onMounted(() => {
  // TaskBoard 自己 onMounted 会拉；这里只保证 nav.pane 合法
  if (!PANE_MAP[nav.pane]) nav.pane = 'tasks'
})
</script>

<template>
  <div class="h-full flex flex-col bg-ink-900">
    <!-- ── 子导航（分段控件）── -->
    <div class="shrink-0 px-3 pt-2 pb-2">
      <div class="glass rounded-md p-0.5 flex items-stretch relative">
        <!-- 滑动指示块（60fps：只用 transform，不触发 layout） -->
        <div
          class="absolute top-0.5 bottom-0.5 rounded-sm bg-acc/15 border border-acc/30
                 transition-transform duration-300 ease-out-back will-change-transform"
          :style="{
            width: `calc((100% - 4px) / ${PANES.length})`,
            transform: `translateX(calc(${curIdx} * 100%))`,
          }"
        />
        <button
          v-for="p in PANES"
          :key="p.key"
          class="flex-1 relative z-10 flex items-center justify-center gap-1
                 py-1.5 text-[11.5px] rounded-sm transition-colors"
          :class="nav.pane === p.key ? 'text-acc' : 'text-fg-dim'"
          @click="goPane(p.key)"
        >
          <span class="text-[10px] leading-none">{{ p.icon }}</span>
          <span>{{ p.label }}</span>
        </button>
      </div>
    </div>

    <!-- ── 子页容器 ── -->
    <div class="flex-1 min-h-0 relative overflow-hidden">
      <Transition :name="paneDir">
        <component
          :is="current"
          :key="nav.pane"
          class="absolute inset-0"
        />
      </Transition>
    </div>
  </div>
</template>