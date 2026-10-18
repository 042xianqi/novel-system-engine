// BaseBanner.vue —— 横幅提示（规范 .banner 四语义色）
//
// 与 Toast 的区别：
//   Toast  = 一次性反馈，自己会消失，不占布局
//   Banner = 常驻提示，属于页面内容，用户手动关闭
// 典型用法：授权锁未开、服务离线、数据不一致。
<template>
  <div
    class="banner"
    :class="`banner-${tone}`"
    role="status"
  >
    <!-- 语义图标 -->
    <svg viewBox="0 0 24 24" aria-hidden="true">
      <template v-if="tone === 'err'">
        <circle cx="12" cy="12" r="9" />
        <path d="M12 8v5M12 16.5v.01" />
      </template>
      <template v-else-if="tone === 'warn'">
        <path d="M12 3.5 21 19.5H3L12 3.5Z" />
        <path d="M12 10v4M12 17v.01" />
      </template>
      <template v-else-if="tone === 'success'">
        <circle cx="12" cy="12" r="9" />
        <path d="m8 12.5 3 3 5-6" />
      </template>
      <template v-else>
        <circle cx="12" cy="12" r="9" />
        <path d="M12 11v5M12 8v.01" />
      </template>
    </svg>

    <div class="flex-1 min-w-0">
      <div v-if="title" class="font-semibold leading-snug">{{ title }}</div>
      <div v-if="$slots.default" class="leading-relaxed" :class="title ? 'mt-0.5 opacity-90' : ''">
        <slot />
      </div>
    </div>

    <!-- 关闭 -->
    <button
      v-if="closable"
      type="button"
      class="close shrink-0"
      aria-label="关闭"
      @click="emit('close')"
    >
      <svg viewBox="0 0 24 24" aria-hidden="true">
        <path d="m6 6 12 12M18 6 6 18" />
      </svg>
    </button>
  </div>
</template>

<script setup>
defineProps({
  /** info | warn | err | success */
  tone: { type: String, default: 'info' },
  title: { type: String, default: '' },
  closable: { type: Boolean, default: false },
})
const emit = defineEmits(['close'])
</script>

<style scoped>
.banner {
  display: flex;
  gap: 10px;
  align-items: flex-start;
  padding: 12px 14px;
  border-radius: var(--r-md);
  font-size: 13px;
  line-height: 1.5;
  animation: fadeUp var(--dur-3) var(--ease-out) both;
}
.banner > svg {
  width: 17px;
  height: 17px;
  flex-shrink: 0;
  margin-top: 1px;
  stroke: currentColor;
  fill: none;
  stroke-width: 2;
  stroke-linecap: round;
  stroke-linejoin: round;
}
.banner-info {
  background: var(--primary-soft);
  color: var(--primary);
}
.banner-warn {
  background: var(--warning-soft);
  color: var(--warning);
}
.banner-err {
  background: var(--danger-soft);
  color: var(--danger);
}
.banner-success {
  background: var(--success-soft);
  color: var(--success);
}
.close {
  margin-left: auto;
  opacity: 0.7;
  background: none;
  border: none;
  padding: 0;
  cursor: pointer;
  color: inherit;
  transition: opacity var(--dur-1) var(--ease-io);
}
.close:hover {
  opacity: 1;
}
.close > svg {
  width: 14px;
  height: 14px;
  stroke: currentColor;
  fill: none;
  stroke-width: 2.2;
  stroke-linecap: round;
}
</style>