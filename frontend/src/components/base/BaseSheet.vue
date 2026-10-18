// BaseSheet.vue —— 底部动作面板（规范 .sheet）
//
// 规范细节：
//   · 顶部有一根 36x4 的小横条（sheet-handle），暗示"可以下拉"
//   · 从底部滑上来（up 关键帧），并带 safe-area 内边距
//   · 每个动作一行、居中、细分割线；危险动作文字变红；
//     最后一个「取消」用灰色且无分割线
//
// 与 BaseDialog 的分工：
//   Dialog = 需要用户做「是/否」判断（居中、小而聚焦）
//   Sheet  = 需要用户「从多个动作里选一个」（底部、可放很多行）
<template>
  <Teleport to="body">
    <Transition name="sheet">
      <div v-if="open" class="fixed inset-0 z-[80] flex items-end">
        <!-- 遮罩 -->
        <div
          class="absolute inset-0"
          :style="{ background: 'var(--mask)' }"
          @click="onCancel"
        />

        <!-- 面板 -->
        <div
          class="sheet-box relative w-full mx-auto"
          role="dialog"
          aria-modal="true"
          :aria-label="title || '操作面板'"
        >
          <div class="sheet-handle" aria-hidden="true" />
          <div v-if="title" class="sheet-title">{{ title }}</div>

          <!-- 自定义内容 -->
          <div v-if="$slots.default" class="pb-2">
            <slot />
          </div>

          <!-- 标准动作列表 -->
          <button
            v-for="(it, i) in actions"
            :key="i"
            type="button"
            class="sheet-item"
            :class="it.danger ? 'danger' : ''"
            @click="onPick(it)"
          >
            {{ it.text }}
          </button>

          <!-- 取消 -->
          <button type="button" class="sheet-item cancel" @click="onCancel">
            {{ cancelText }}
          </button>
        </div>
      </div>
    </Transition>
  </Teleport>
</template>

<script setup>
import { watch, onBeforeUnmount } from 'vue'

const props = defineProps({
  open: { type: Boolean, default: false },
  title: { type: String, default: '' },
  /** [{ text, value, danger }] */
  actions: { type: Array, default: () => [] },
  cancelText: { type: String, default: '取消' },
})

const emit = defineEmits(['update:open', 'pick', 'cancel'])

function close() {
  emit('update:open', false)
}
function onPick(it) {
  emit('pick', it)
  close()
}
function onCancel() {
  emit('cancel')
  close()
}

function onKeydown(e) {
  if (e.key === 'Escape' && props.open) onCancel()
}

watch(
  () => props.open,
  (v) => {
    if (typeof document === 'undefined') return
    if (v) {
      document.body.style.overflow = 'hidden'
      window.addEventListener('keydown', onKeydown)
    } else {
      document.body.style.overflow = ''
      window.removeEventListener('keydown', onKeydown)
    }
  },
  { immediate: true }
)

onBeforeUnmount(() => {
  if (typeof document === 'undefined') return
  document.body.style.overflow = ''
  window.removeEventListener('keydown', onKeydown)
})
</script>

<style scoped>
.sheet-box {
  max-width: 420px;
  background: var(--card);
  border-radius: var(--r-lg) var(--r-lg) 0 0;
  padding: 8px 16px calc(16px + env(safe-area-inset-bottom, 0px));
  box-shadow: var(--shadow-hover);
  border-top: 1px solid var(--border);
}
.sheet-handle {
  width: 36px;
  height: 4px;
  border-radius: 2px;
  background: var(--divider);
  margin: 6px auto 12px;
}
.sheet-title {
  font-size: 13px;
  color: var(--text-sec);
  text-align: center;
  margin-bottom: 8px;
}
.sheet-item {
  width: 100%;
  padding: 14px;
  text-align: center;
  font-size: 15px;
  background: none;
  border: none;
  border-bottom: 1px solid var(--divider);
  color: var(--text);
  cursor: pointer;
  font-family: var(--font);
  transition: background var(--dur-1) var(--ease-io);
}
.sheet-item:hover {
  background: var(--card-2);
}
.sheet-item:active {
  background: var(--primary-soft);
}
.sheet-item.danger {
  color: var(--danger);
}
.sheet-item.cancel {
  border-bottom: none;
  color: var(--text-sec);
  margin-top: 6px;
  border-radius: var(--r-sm);
}

/* 从底部滑入 */
.sheet-enter-active {
  transition: opacity var(--dur-2) var(--ease-io);
}
.sheet-leave-active {
  transition: opacity var(--dur-2) var(--ease-io);
}
.sheet-enter-from,
.sheet-leave-to {
  opacity: 0;
}
.sheet-enter-active .sheet-box {
  animation: sheetUp var(--dur-3) var(--ease-out) both;
}
.sheet-leave-active .sheet-box {
  animation: sheetUp var(--dur-2) var(--ease-io) reverse both;
}
@keyframes sheetUp {
  from {
    transform: translateY(100%);
  }
  to {
    transform: none;
  }
}
</style>