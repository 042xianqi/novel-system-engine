<script setup>
// Composer.vue —— 底部输入区
//
// 交互细节：
//   · Enter 发送，Shift+Enter 换行
//   · 输入时 textarea 自动增高（最多 5 行）
//   · 发送中禁用按钮 + 显示 loading
//   · 移动端键盘弹出时不被遮挡（用 env(safe-area-inset-bottom)）

import { ref, computed, nextTick, watch } from 'vue'
import { chat } from '../store.js'
import { useChat } from '../composables/useChat.js'

const { send } = useChat()

const text = ref('')
const ta = ref(null)

const canSend = computed(() => !!text.value.trim() && !chat.sending)

function autoGrow() {
  const el = ta.value
  if (!el) return
  el.style.height = 'auto'
  // 5 行上限（15px * 1.65 ≈ 24.75 行高，5 行 ≈ 124px）
  el.style.height = Math.min(el.scrollHeight, 124) + 'px'
}

watch(text, () => nextTick(autoGrow))

async function doSend() {
  if (!canSend.value) return
  const t = text.value
  text.value = ''
  await nextTick(autoGrow)
  await send(t)
}

function onKeydown(e) {
  // IME 组合中不发送（中文输入法选词会触发 Enter）
  if (e.isComposing || e.keyCode === 229) return
  if (e.key === 'Enter' && !e.shiftKey) {
    e.preventDefault()
    doSend()
  }
}

/** 外部填入（快捷指令按钮调用） */
function fill(t) {
  text.value = t
  nextTick(() => {
    autoGrow()
    ta.value?.focus()
  })
}

defineExpose({ fill })
</script>

<template>
  <div
    class="shrink-0 px-3 pt-2 border-t border-white/[0.06] bg-ink-900/80 backdrop-blur"
    style="padding-bottom: max(0.75rem, env(safe-area-inset-bottom))"
  >
    <div class="max-w-3xl mx-auto flex items-end gap-2">
      <div class="flex-1 relative">
        <textarea
          ref="ta"
          v-model="text"
          rows="1"
          placeholder="说一句话下指令…"
          class="input resize-none leading-relaxed !py-2.5 !pr-11 max-h-[124px] overflow-y-auto"
          @keydown="onKeydown"
          @input="autoGrow"
        />
        <!-- 字数（超长才提示） -->
        <span
          v-if="text.length > 200"
          class="absolute right-2.5 bottom-1 text-[10px] font-mono text-fg-dim/60"
        >{{ text.length }}</span>
      </div>

      <button
        class="shrink-0 h-[42px] w-[42px] rounded-lg flex items-center justify-center
               transition-all duration-200 ease-out-back
               active:scale-95 disabled:opacity-35 disabled:scale-100"
        :class="canSend
          ? 'bg-gradient-to-br from-[#1e4a6d] to-[#2b6690] text-[#cfe6f7] shadow-glow-acc'
          : 'bg-ink-700 text-fg-dim'"
        :disabled="!canSend"
        :title="chat.sending ? '发送中…' : '发送'"
        @click="doSend"
      >
        <!-- loading 圈 -->
        <svg
          v-if="chat.sending"
          class="w-4 h-4 animate-spin"
          viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.4"
        >
          <path d="M12 3a9 9 0 1 0 9 9" stroke-linecap="round" />
        </svg>
        <svg
          v-else
          class="w-4 h-4"
          viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2"
        >
          <path d="M4 12h15M13 6l6 6-6 6" stroke-linecap="round" stroke-linejoin="round" />
        </svg>
      </button>
    </div>

    <div class="max-w-3xl mx-auto mt-1.5 text-[10.5px] text-fg-dim/50 font-mono flex items-center gap-3">
      <span>Enter 发送 · Shift+Enter 换行</span>
      <span class="ml-auto" :class="chat.sending ? 'text-acc' : ''">
        {{ chat.sending ? '解析中…' : '就绪' }}
      </span>
    </div>
  </div>
</template>