// BaseDialog.vue —— 居中确认框（规范 .dialog）
//
// 规范细节：
//   · 遮罩用 var(--mask)，不是写死黑色 —— 皮肤换了遮罩深浅也跟着换
//   · 弹出用 pop 关键帧（scale .9 → 1，250ms），比纯淡入更有「实体感」
//   · 按钮区固定右对齐，主按钮在右（符合「主行动在右下」的通用认知）
//
// 无障碍：
//   · role=dialog + aria-modal
//   · Esc 关闭、点遮罩关闭（可通过 maskClosable=false 禁用，防误触）
//   · 打开时锁 body 滚动，关闭时恢复
<template>
  <Teleport to="body">
    <Transition name="dlg">
      <div
        v-if="open"
        class="fixed inset-0 z-[80] flex items-center justify-center p-5"
      >
        <!-- 遮罩 -->
        <div
          class="absolute inset-0"
          :style="{ background: 'var(--mask)' }"
          @click="onMaskClick"
        />

        <!-- 对话框 -->
        <div
          class="dialog-box relative"
          role="dialog"
          aria-modal="true"
          :aria-label="title"
        >
          <div v-if="title" class="dlg-title">{{ title }}</div>
          <div v-if="$slots.default" class="dlg-body">
            <slot />
          </div>

          <div class="dlg-actions">
            <slot name="actions">
              <BaseButton variant="outline-gray" size="sm" @click="onCancel">
                {{ cancelText }}
              </BaseButton>
              <BaseButton
                :variant="danger ? 'danger' : 'primary'"
                size="sm"
                :loading="loading"
                @click="onConfirm"
              >
                {{ confirmText }}
              </BaseButton>
            </slot>
          </div>
        </div>
      </div>
    </Transition>
  </Teleport>
</template>

<script setup>
import { watch, onBeforeUnmount } from 'vue'
import BaseButton from './BaseButton.vue'

const props = defineProps({
  open: { type: Boolean, default: false },
  title: { type: String, default: '' },
  confirmText: { type: String, default: '确定' },
  cancelText: { type: String, default: '取消' },
  /** 危险操作：确认键变红 */
  danger: { type: Boolean, default: false },
  /** 确认键转圈（异步提交中） */
  loading: { type: Boolean, default: false },
  /** 点遮罩是否关闭（危险确认建议关掉，防误触） */
  maskClosable: { type: Boolean, default: true },
})

const emit = defineEmits(['update:open', 'confirm', 'cancel'])

function close() {
  emit('update:open', false)
}
function onConfirm() {
  emit('confirm')
}
function onCancel() {
  emit('cancel')
  close()
}
function onMaskClick() {
  if (props.maskClosable) onCancel()
}

function onKeydown(e) {
  if (e.key === 'Escape' && props.open) onCancel()
}

// 打开时锁滚动 + 挂 Esc；关闭时松手。
// 锁滚动用 overflow:hidden，比 position:fixed 更安全（不会跳位）。
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
.dialog-box {
  width: 300px;
  max-width: 100%;
  background: var(--card);
  border-radius: var(--r-lg);
  padding: 22px;
  box-shadow: var(--shadow-hover);
  border: 1px solid var(--border);
  animation: dlgPop 0.25s var(--ease-spring) both;
}
@keyframes dlgPop {
  from {
    transform: scale(0.9);
    opacity: 0;
  }
  to {
    transform: scale(1);
    opacity: 1;
  }
}

.dlg-title {
  font-size: 16px;
  font-weight: 700;
  margin-bottom: 8px;
  color: var(--text);
}
.dlg-body {
  font-size: 13px;
  color: var(--text-sec);
  line-height: 1.6;
  margin-bottom: 20px;
}
.dlg-actions {
  display: flex;
  justify-content: flex-end;
  gap: 8px;
}

/* 遮罩淡入 + 内容弹入 */
.dlg-enter-active,
.dlg-leave-active {
  transition: opacity var(--dur-2) var(--ease-io);
}
.dlg-enter-from,
.dlg-leave-to {
  opacity: 0;
}
</style>