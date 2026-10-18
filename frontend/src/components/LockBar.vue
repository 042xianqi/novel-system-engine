<script setup>
// LockBar.vue —— 顶部授权锁条
//
// 极简一条，不抢焦点：平时是一行细字，点开才显示完整说明。
// 铁则：解锁必须二次确认（防误触导致真跑烧钱/烧时间）。

import { ref, computed } from 'vue'
import { lock, toast, chat, pushLog } from '../store.js'
import { useTasks } from '../composables/useTasks.js'
const { unlockLLM, lockLLM, refresh } = useTasks()
const confirming = ref(false)
const busy = ref(false)
const expanded = ref(false)
const menuOpen = ref(false)
const clearing = ref(false)
const tone = computed(() => (lock.unlocked ? 'warn' : 'ok'))

/**
 * 清空聊天记录。
 *
 * 为什么放在这里（LockBar 顶部条）：
 *   ToolPkg 底部那个「☒ 清空聊天」全局按钮已删（与新底栏视觉冲突）。
 *   这里是「对话」视图唯一常驻的顶栏，右上角就是用户说的位置。
 *
 * 为什么只清消息不清任务：
 *   任务状态是独立数据源（来自后端），清掉会与后端不一致。
 *   所以这里只重置 chat.messages 与悬浮窗日志。
 */
async function clearChat() {
  if (clearing.value) return
  if (!chat.messages.length) {
    toast('本来就没有对话')
    menuOpen.value = false
    return
  }
  clearing.value = true
  const n = chat.messages.length
  chat.messages.splice(0, chat.messages.length)
  pushLog(`已清空聊天记录（${n} 条）`, 'warn')
  clearing.value = false
  menuOpen.value = false
  toast(`已清空 ${n} 条对话`)
}
const label = computed(() =>
  lock.unlocked ? '🔓 真实 LLM 已解锁' : '🔒 真实 LLM 已锁定'
)

async function askUnlock() {
  if (lock.unlocked) return
  confirming.value = true
}

async function confirmUnlock() {
  busy.value = true
  const ok = await unlockLLM()
  busy.value = false
  confirming.value = false
  if (ok) refresh()
}

async function doLock() {
  busy.value = true
  const ok = await lockLLM()
  busy.value = false
  if (ok) refresh()
}
</script>

<template>
  <div class="shrink-0 px-3 pt-2">
    <!-- 细条 -->
    <div
      class="glass rounded-lg px-3 py-2 flex items-center gap-2
             transition-all duration-200 ease-ease cursor-pointer select-none"
      :class="lock.unlocked ? 'border-warn/40' : ''"
      @click="expanded = !expanded"
    >
      <span
        class="w-2 h-2 rounded-full shrink-0"
        :class="lock.unlocked ? 'bg-warn animate-breathe' : 'bg-ok'"
      />
      <span
        class="text-[12px] font-medium"
        :class="lock.unlocked ? 'text-warn' : 'text-ok'"
      >{{ label }}</span>

      <span v-if="lock.unlocked" class="text-[11px] text-fg-dim">
        真跑将调用真实模型
      </span>

      <span class="ml-auto text-[10.5px] text-fg-dim/60 font-mono">
        {{ expanded ? '▴' : '▾' }}
      </span>

      <!-- 右上角菜单：清空聊天（原 ToolPkg 全局按钮迁到这里）-->
      <span
        class="relative shrink-0"
        @click.stop
      >
        <button
          class="px-1.5 py-0.5 rounded-sm text-[12px] text-fg-dim active:scale-90
                 hover:text-fg transition-colors"
          title="更多"
          @click="menuOpen = !menuOpen"
        >⋯</button>

        <!-- 下拉菜单 -->
        <div
          v-if="menuOpen"
          class="absolute right-0 top-full mt-1 z-50 glass-strong rounded-md
                 border border-ink-600 shadow-3 overflow-hidden min-w-[124px]"
        >
          <button
            class="w-full text-left px-3 py-2 text-[11.5px] text-fg-dim
                   active:bg-ink-700/60 whitespace-nowrap"
            :disabled="clearing"
            @click="clearChat"
          >⌫ 清空聊天</button>
        </div>
      </span>
    </div>

    <!-- 菜单外点关闭：透明遮罩（不挡滚动，只捕点击）-->
    <div
      v-if="menuOpen"
      class="fixed inset-0 z-40"
      @click="menuOpen = false"
    />

    <!-- 展开区 -->
    <div
      v-if="expanded"
      class="mt-1.5 glass rounded-lg px-3 py-2.5 text-[11.5px] text-fg-dim
             animate-fade-in space-y-1.5"
    >
      <div class="font-mono text-[10.5px] opacity-70 break-all">
        {{ lock.file }}
      </div>
      <div>
        该文件存在 = 允许真实 LLM 出网。任务跑完会自动上锁（成功/失败/超时都锁）。
      </div>
      <div class="flex items-center gap-2 pt-1">
        <button
          v-if="!lock.unlocked"
          class="btn btn-warn !px-3 !py-1.5 !text-[11.5px]"
          :disabled="busy"
          @click="askUnlock"
        >解锁</button>
        <button
          v-else
          class="btn btn-ghost !px-3 !py-1.5 !text-[11.5px] !border-ok/40 !text-ok"
          :disabled="busy"
          @click="doLock"
        >立即上锁</button>

        <span class="text-[10.5px] opacity-60">
          预演（real=false）不受影响，随时可跑
        </span>
      </div>
    </div>

    <!-- 二次确认 -->
    <Teleport to="body">
      <div
        v-if="confirming"
        class="fixed inset-0 z-[70] bg-black/60 backdrop-blur-sm
               flex items-center justify-center p-6 animate-fade-in"
        @click.self="confirming = false"
      >
        <div class="glass-strong rounded-xl p-5 max-w-sm w-full shadow-3 animate-rise">
          <div class="text-[15px] font-semibold text-warn mb-2">⚠ 确认解锁真实 LLM？</div>
          <div class="text-[13px] text-fg-dim leading-relaxed space-y-1.5">
            <p>解锁后，任何真跑（<code class="font-mono text-warn">real=true</code>）都会真实调用本地模型网关，消耗时间与算力。</p>
            <p>任务结束时会自动上锁。若中途异常退出，请回来手动上锁。</p>
          </div>
          <div class="mt-4 flex gap-2 justify-end">
            <button class="btn btn-ghost !px-3.5 !py-2 !text-[12.5px]" @click="confirming = false">
              取消
            </button>
            <button
              class="btn btn-warn !px-3.5 !py-2 !text-[12.5px]"
              :disabled="busy"
              @click="confirmUnlock"
            >{{ busy ? '解锁中…' : '确认解锁' }}</button>
          </div>
        </div>
      </div>
    </Teleport>
  </div>
</template>