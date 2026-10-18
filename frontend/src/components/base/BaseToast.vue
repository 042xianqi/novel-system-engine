// BaseToast.vue —— 轻提示（规范 #toast 四语义色）
//
// 说明：项目里已有一个 ToastHost.vue 负责「队列与生命周期」，
// 本组件只负责**单条 Toast 的视觉**，由 ToastHost 复用。
// 这样拆的好处：视觉与调度解耦，将来换视觉不用动队列逻辑。
<template>
  <div class="toast" :class="`toast-${tone}`" role="status" aria-live="polite">
    <span class="ic" aria-hidden="true">
      <template v-if="tone === 'ok'">✓</template>
      <template v-else-if="tone === 'err'">✕</template>
      <template v-else-if="tone === 'warn'">!</template>
      <template v-else>i</template>
    </span>
    <span class="msg">{{ text }}</span>
  </div>
</template>

<script setup>
defineProps({
  /** dark | ok | err | warn | info */
  tone: { type: String, default: 'dark' },
  text: { type: String, default: '' },
})
</script>

<style scoped>
.toast {
  display: inline-flex;
  align-items: center;
  gap: 8px;
  max-width: 78vw;
  padding: 10px 16px;
  border-radius: var(--r-full);
  font-size: 12.5px;
  font-weight: 500;
  line-height: 1.45;
  box-shadow: var(--shadow-hover);
  border: 1px solid transparent;
  /* 从下方 10px 处淡入，符合「从底部弹出」的心理预期 */
  animation: toastIn var(--dur-3) var(--ease-out) both;
  word-break: break-word;
}
@keyframes toastIn {
  from {
    opacity: 0;
    transform: translateY(10px) scale(0.96);
  }
  to {
    opacity: 1;
    transform: none;
  }
}

.toast-dark {
  background: var(--text);
  color: var(--bg);
}
.toast-info {
  background: var(--primary-soft);
  color: var(--primary);
}
.toast-ok {
  background: var(--success-soft);
  color: var(--success);
}
.toast-err {
  background: var(--danger-soft);
  color: var(--danger);
}
.toast-warn {
  background: var(--warning-soft);
  color: var(--warning);
}

.ic {
  width: 15px;
  height: 15px;
  border-radius: 50%;
  display: flex;
  align-items: center;
  justify-content: center;
  font-size: 10px;
  font-weight: 800;
  flex-shrink: 0;
  border: 1.5px solid currentColor;
}

.msg {
  min-width: 0;
}
</style>