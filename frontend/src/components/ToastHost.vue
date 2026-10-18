<script setup>
// ToastHost.vue —— 全局轻提示
//
// 从底部浮起、3 秒渐隐。同时最多 3 条，避免刷屏。

import { computed } from 'vue'
import { toasts } from '../store.js'

const shown = computed(() => toasts.value.slice(-3))
</script>

<template>
  <Teleport to="body">
    <div class="fixed left-0 right-0 bottom-32 z-[80] flex flex-col items-center gap-2 pointer-events-none px-4">
      <TransitionGroup
        enter-active-class="transition-all duration-300 ease-out-back"
        enter-from-class="opacity-0 translate-y-3 scale-95"
        enter-to-class="opacity-100 translate-y-0 scale-100"
        leave-active-class="transition-all duration-200 ease-ease"
        leave-from-class="opacity-100"
        leave-to-class="opacity-0 scale-95"
      >
        <div
          v-for="t in shown"
          :key="t.id"
          class="glass-strong rounded-lg px-4 py-2.5 text-[12.5px] shadow-3 max-w-sm
                 pointer-events-auto"
          :class="t.bad ? 'text-bad border-bad/35' : 'text-fg'"
        >
          <span v-if="t.bad" class="mr-1">⚠</span>{{ t.text }}
        </div>
      </TransitionGroup>
    </div>
  </Teleport>
</template>