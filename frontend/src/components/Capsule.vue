<script setup>
// Capsule.vue —— 最小化胶囊（右下角呼吸灯）
//
// 只在 win.state === 'capsule' 时显示。
// 显示活跃任务数与执行器状态；点击展开悬浮窗。

import { computed } from 'vue'
import { tasks } from '../store.js'
import { nav, openWin } from '../nav.js'

const active = computed(() => tasks.counts.pending + tasks.counts.running)

const label = computed(() => {
  if (tasks.counts.running > 0) return `运行中 ${tasks.counts.running}`
  if (tasks.counts.pending > 0) return `待执行 ${tasks.counts.pending}`
  return '空闲'
})

const dotClass = computed(() => {
  if (tasks.counts.running > 0) return 'bg-acc'
  if (tasks.counts.pending > 0) return 'bg-warn'
  return 'bg-ok'
})
</script>

<template>
  <button
    v-if="nav.win === 'capsule'"
    class="fixed right-4 bottom-24 z-40 glass-strong rounded-full pl-3 pr-4 py-2.5
           flex items-center gap-2.5 shadow-3
           transition-all duration-200 ease-out-back
           hover:scale-105 active:scale-95"
    @click="openWin()"
  >
    <!-- 呼吸灯 -->
    <span class="relative flex w-2.5 h-2.5">
      <span
        class="absolute inset-0 rounded-full opacity-60 animate-breathe"
        :class="dotClass"
      />
      <span class="relative w-2.5 h-2.5 rounded-full" :class="dotClass" />
    </span>

    <span class="text-[12.5px] font-medium text-fg whitespace-nowrap">{{ label }}</span>

    <!-- 执行器活着的小标记 -->
    <span
      v-if="tasks.executorAlive"
      class="text-[10px] font-mono text-ok/80 whitespace-nowrap"
    >exec</span>

    <span class="text-fg-dim text-[11px]">▴</span>
  </button>
</template>