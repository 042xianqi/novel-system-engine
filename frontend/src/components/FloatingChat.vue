<script setup>
// FloatingChat.vue —— 全局悬浮聊天窗
//
// 从「💬 对话球」弹出，不切主视图、不打断当前工作。
// 与主对话视图（ChatStream + Composer）共用同一个 chat.messages 数据源，
// 所以在悬浮窗里发的消息、收到的回复，切回主视图一样能看到 —— 不复制状态。
//
// ── 为什么是「悬浮卡片」而不是新页面 ──────────────────────
// 用户明确要求：信息展示要用悬浮卡片，不要页面跳转。
// 这里用毛玻璃卡片 + 背景遮罩，关掉后底下的视图位置原封不动。
import { ref, computed, nextTick, watch, onUnmounted } from 'vue'
import { chat, toast } from '../store.js'
import { useChat, stopChatTyping } from '../composables/useChat.js'

const props = defineProps({
  open: { type: Boolean, default: false },
})
const emit = defineEmits(['close'])
const { send } = useChat()

// 【R12】悬浮窗关闭/卸载时停掉逐字动画，防孤儿定时器继续写屏。
onUnmounted(() => { stopChatTyping() })
const text = ref('')
const listEl = ref(null)
const inputEl = ref(null)
const sendingLocal = ref(false)

const msgs = computed(() => chat.messages)

function scrollToBottom() {
  nextTick(() => {
    const el = listEl.value
    if (el) el.scrollTop = el.scrollHeight
  })
}

watch(() => msgs.value.length, scrollToBottom)
watch(() => props.open, (v) => {
  if (v) {
    scrollToBottom()
    nextTick(() => inputEl.value?.focus())
  }
})

async function onSend() {
  const t = text.value.trim()
  if (!t || sendingLocal.value) return
  text.value = ''
  sendingLocal.value = true
  try {
    await send(t)
  } catch (e) {
    toast('发送失败：' + (e?.message || e), true)
  } finally {
    sendingLocal.value = false
    scrollToBottom()
  }
}

function onKeydown(e) {
  if (e.key === 'Enter' && !e.shiftKey) {
    e.preventDefault()
    onSend()
  }
}
</script>

<template>
  <Teleport to="body">
    <Transition name="fc">
      <div v-if="open" class="fixed inset-0 z-[66]">
        <!-- 背景遮罩：点击关闭 -->
        <div
          class="absolute inset-0 bg-black/45 backdrop-blur-[2px]"
          @click="emit('close')"
        />
        <!-- 悬浮卡片 -->
        <div
          class="absolute left-3 right-3 bottom-24 top-24
                 glass-strong rounded-2xl overflow-hidden
                 flex flex-col
                 shadow-[0_18px_60px_rgba(0,0,0,.55)]
                 ring-1 ring-white/10"
        >
          <!-- 头部 -->
          <div class="shrink-0 flex items-center gap-2 px-3.5 py-2.5
                      border-b border-ink-600/60">
            <span class="text-acc text-[15px]">◈</span>
            <span class="text-[13px] font-semibold text-fg">对话中枢</span>
            <span class="text-[10.5px] text-fg-dim/70 font-mono">
              {{ msgs.length }} 条
            </span>
            <button
              class="ml-auto w-7 h-7 rounded-lg bg-ink-700/70 text-fg-dim
                     flex items-center justify-center text-[15px]
                     active:scale-95 transition-transform"
              @click="emit('close')"
              aria-label="关闭"
            >✕</button>
          </div>
          <!-- 消息流 -->
          <div ref="listEl" class="flex-1 min-h-0 overflow-y-auto px-3 py-3 space-y-2.5">
            <div v-if="!msgs.length" class="h-full flex items-center justify-center">
              <div class="text-center">
                <div class="text-[28px] mb-2 opacity-50">🌊</div>
                <div class="text-[12px] text-fg-dim/70">
                  在这里下指令，不必离开当前界面
                </div>
              </div>
            </div>
            <div
              v-for="m in msgs"
              :key="m.id"
              class="flex"
              :class="m.role === 'user' ? 'justify-end' : 'justify-start'"
            >
              <div
                class="max-w-[86%] rounded-xl px-3 py-2 text-[12.5px] leading-relaxed
                       whitespace-pre-wrap break-words"
                :class="m.role === 'user'
                  ? 'bg-acc/18 text-fg border border-acc/25'
                  : 'bg-ink-700/70 text-fg/92 border border-ink-600/60'"
              >
                {{ m.text }}
                <!-- 计划步骤（如果有） -->
                <ol v-if="m.plan && m.plan.length" class="mt-1.5 pl-4 space-y-0.5
                                                          text-[11px] text-fg-dim/80 list-decimal">
                  <li v-for="(p, i) in m.plan" :key="i">{{ p.action || p }}</li>
                </ol>
              </div>
            </div>
            <div v-if="chat.sending" class="flex justify-start">
              <!-- 三点输入中（规范 .dots）：比自己写的 animate-bounce 更有辨识度，
                   因为三点的节奏是 0/150/300ms 依次抬起，看起来像真人在敲字 -->
              <div class="dots" aria-label="正在生成回复">
                <i /><i /><i />
              </div>
            </div>
          </div>
          <!-- 输入区 -->
          <div class="shrink-0 p-2.5 border-t border-ink-600/60">
            <div class="flex items-end gap-2">
              <textarea
                ref="inputEl"
                v-model="text"
                rows="1"
                class="flex-1 resize-none bg-ink-700/80 rounded-xl px-3 py-2
                       text-[12.5px] text-fg placeholder:text-fg-dim/50
                       outline-none focus:ring-1 focus:ring-acc/40
                       max-h-28 leading-relaxed"
                placeholder="说一句话下指令…"
                @keydown="onKeydown"
              />
              <button
                class="shrink-0 h-9 px-3.5 rounded-xl text-[12.5px] font-semibold
                       bg-acc/85 text-ink-900 active:scale-95
                       transition-transform disabled:opacity-40"
                :disabled="!text.trim() || sendingLocal"
                @click="onSend"
              >发送</button>
            </div>
          </div>
        </div>
      </div>
    </Transition>
  </Teleport>
</template>

<style scoped>
/* 卡片从底部弹入，带轻微缩放 —— 统一弹跳曲线 */
.fc-enter-active {
  transition: opacity 220ms ease;
}
.fc-enter-active > div:last-child {
  transition: transform 300ms cubic-bezier(.22, 1.35, .36, 1),
              opacity 260ms ease;
}
.fc-leave-active {
  transition: opacity 180ms ease;
}
.fc-leave-active > div:last-child {
  transition: transform 200ms cubic-bezier(.4, 0, 1, 1),
              opacity 180ms ease;
}
.fc-enter-from { opacity: 0; }
.fc-enter-from > div:last-child {
  transform: translateY(18px) scale(.97);
  opacity: 0;
}
.fc-leave-to { opacity: 0; }
.fc-leave-to > div:last-child {
  transform: translateY(12px) scale(.985);
  opacity: 0;
}
</style>