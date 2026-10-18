<script setup>
// ChatStream.vue —— 对话历史流
//
// 职责边界（重要）：
//   · 只负责「渲染」+「自动滚到底」
//   · 不发请求（发消息在 Composer → useChat）
//   · 不改状态（只读 store.chat）

import { ref, watch, nextTick, onMounted } from 'vue'
import { chat } from '../store.js'
import { LockAPI } from '../api.js'
import { toast } from '../store.js'
import { useChat } from '../composables/useChat.js'

// 只取 loadMore：滚到顶部时补更早的记录。
// 注意不要在 ChatStream 里调 useChat().send —— 发消息是 Composer 的职责。
const { loadMore } = useChat()

const scroller = ref(null)

// 自动滚到底：只在「用户本来就贴着底部」时才滚，
// 否则用户往上翻历史时会被强制拽回来（很烦）。
let stick = true

function onScroll() {
  const el = scroller.value
  if (!el) return
  const gap = el.scrollHeight - el.scrollTop - el.clientHeight
  stick = gap < 120
  // 滚到接近顶部时加载更早的记录
  if (el.scrollTop < 80) loadMore()
}

// ── 计划徽章的文案压缩 ──
//
// 后端 plan 里是完整句子，如「查资料库「星耀」→ 命中 8 张卡」。
// 直接显示太长，把气泡撑得很难看。这里压成 2~8 字的核心短语：
//   「查资料库「星耀」→ 命中 8 张卡」 → 「星耀 · 8 张」
//   「登记建卡任务：星耀 5 张」        → 「建卡 · 星耀 5 张」
//   「查创作漏斗进度」                → 「查漏斗进度」
//
// 完整原文仍放在 title 里，鼠标悬停可见。

/** 按关键词选一个 emoji 当图标 */
function planIcon(p) {
  const s = String(p.action || p.tool || p || '')
  if (s.includes('查资料库') || s.includes('命中')) return '🔍'
  if (s.includes('统计')) return '📊'
  if (s.includes('建卡') || s.includes('生成')) return '🧬'
  if (s.includes('写正文') || s.includes('章节')) return '✍️'
  if (s.includes('失败')) return '⚠️'
  if (s.includes('状态') || s.includes('队列')) return '📋'
  if (s.includes('漏斗') || s.includes('进度')) return '🌀'
  if (s.includes('稽核')) return '🔬'
  if (s.includes('引擎') || s.includes('授权')) return '⚙️'
  return '·'
}

/** 把整句压成短标签 */
function planShort(p) {
  let s = String(p.action || p.tool || p || '').trim()

  // 「查资料库「X」→ 命中 N 张卡」 → 「X · N 张」
  let m = s.match(/查资料库「([^」]+)」.*?(\d+)\s*张/)
  if (m) return `${m[1]} · ${m[2]} 张`

  // 「登记建卡任务：X N 张」 → 「建卡 · X N 张」
  m = s.match(/登记建卡任务[:：]\s*(.+)/)
  if (m) return `建卡 · ${m[1]}`

  // 「登记写正文任务：第 N 章」 → 「写正文 · 第 N 章」
  m = s.match(/登记写正文任务[:：]\s*(.+)/)
  if (m) return `写正文 · ${m[1]}`

  // 「查失败任务 → N 条」 → 「失败 · N 条」
  m = s.match(/查失败任务.*?(\d+)\s*条/)
  if (m) return `失败 · ${m[1]} 条`

  // 「查资料库统计 → 共 N 张卡」 → 「全库 · N 张」
  m = s.match(/查资料库统计.*?(\d+)\s*张/)
  if (m) return `全库 · ${m[1]} 张`

  // 「按卡号查 X」/「查卡号 X」 → 保留
  if (s.startsWith('按卡号查')) return s.replace('按卡号查', '查卡 ·')

  // 兜底：截断到 14 字
  return s.length > 14 ? s.slice(0, 14) + '…' : s
}

async function scrollToBottom(force = false) {
  if (!force && !stick) return
  await nextTick()
  const el = scroller.value
  if (el) el.scrollTop = el.scrollHeight
}

// 新消息 / 打字机文字变化都可能需要跟进
watch(() => chat.messages.length, () => scrollToBottom())
watch(
  () => chat.messages.map((m) => m.text.length).join(','),
  () => scrollToBottom()
)
onMounted(() => scrollToBottom(true))

// 空状态推荐的快捷指令
const QUICK = [
  { icon: '🧬', text: '生成 10 张混沌帝国卡包', fill: '生成 10 张混沌帝国的卡（1 文明 + 3 势力 + 3 地点 + 3 角色）' },
  { icon: '🔍', text: '检查第 4 章设定冲突', fill: '检查第 4 章的设定冲突' },
  { icon: '📊', text: '看当前任务状态', fill: '看当前任务状态' },
  { icon: '🩺', text: '跑一次系统自检', fill: '跑一次系统自检' }
]

const emit = defineEmits(['quick'])

// 解锁（聊天流内联提示用）
async function doUnlock() {
  try {
    const r = await LockAPI.unlock()
    if (r && r.ok === false) throw new Error(r.error || '解锁失败')
    toast('已解锁真实 LLM')
  } catch (e) {
    toast('解锁失败：' + e.message, true)
  }
}
</script>

<template>
  <div
    ref="scroller"
    class="flex-1 overflow-y-auto px-4 pt-3 pb-2"
    @scroll.passive="onScroll"
  >
    <!-- 空状态 -->
    <div
      v-if="!chat.messages.length"
      class="h-full flex flex-col items-center justify-center gap-6 animate-fade-in"
    >
      <div class="text-center select-none">
        <div
          class="w-16 h-16 mx-auto mb-4 rounded-2xl glass flex items-center justify-center text-2xl
                 shadow-2 animate-breathe"
        >
          🌊
        </div>
        <h1 class="text-xl font-semibold tracking-wide text-fg">残响之河 · 中枢</h1>
        <p class="mt-2 text-sm text-fg-dim">
          用一句话下指令，我来选工具、派任务
        </p>
      </div>

      <div class="grid grid-cols-2 gap-2.5 w-full max-w-md">
        <button
          v-for="q in QUICK"
          :key="q.text"
          class="glass rounded-lg px-3.5 py-3 text-left
                 transition-all duration-200 ease-ease
                 hover:scale-[1.025] hover:shadow-2 active:scale-[0.985]"
          @click="emit('quick', q.fill)"
        >
          <div class="text-base mb-1">{{ q.icon }}</div>
          <div class="text-[12.5px] leading-snug text-fg-dim">{{ q.text }}</div>
        </button>
      </div>
    </div>

    <!-- 消息列表 -->
    <div v-else class="max-w-3xl mx-auto space-y-3.5">
      <div
        v-for="m in chat.messages"
        :key="m.id"
        class="flex animate-fade-in"
        :class="m.role === 'user' ? 'justify-end' : 'justify-start'"
      >
        <!-- 机器人气泡 -->
        <div v-if="m.role === 'bot'" class="flex gap-2.5 max-w-[88%]">
          <div
            class="shrink-0 w-7 h-7 mt-0.5 rounded-full glass
                   flex items-center justify-center text-[13px] select-none"
          >
            🌊
          </div>
          <div class="min-w-0">
            <div class="glass rounded-xl rounded-tl-sm px-3.5 py-2.5 shadow-1">
              <!-- 思考中 -->
              <div v-if="m.typing && !m.text" class="flex items-center gap-1.5 py-0.5">
                <span
                  v-for="i in 3"
                  :key="i"
                  class="w-1.5 h-1.5 rounded-full bg-acc"
                  :style="{ animation: `breathe 1.2s ${i * 0.16}s ease-in-out infinite` }"
                />
                <span class="ml-1 text-[12px] text-fg-dim">思考中</span>
              </div>

              <!-- 正文 -->
              <div
                v-else
                class="text-[14.5px] leading-relaxed whitespace-pre-wrap break-words"
                :class="{ caret: m.typing }"
              >{{ m.text }}</div>

              <!-- 计划步骤：渲染成小徽章，不占正文 -->
              <!--
                【为什么改成徽章】原来是一行行纯文本「· 查资料库「星耀」→ 命中 8 张卡」，
                直接印在气泡里，看起来像系统调试日志，不像 AI 助理说的话。
                现在压成一行紧凑的 chip：图标 + 短语，颜色弱化。
                用户想看细节可以点开（title 里有完整文本）。
              -->
              <div
                v-if="m.plan && m.plan.length"
                class="mt-2 flex flex-wrap items-center gap-1.5"
              >
                <span
                  v-for="(p, i) in m.plan"
                  :key="i"
                  class="inline-flex items-center gap-1 px-1.5 py-0.5 rounded
                         bg-white/[0.045] border border-white/[0.07]
                         text-[10.5px] text-fg-dim/75 font-mono
                         animate-rise cursor-default select-none"
                  :style="{ animationDelay: `${i * 48}ms` }"
                  :title="p.tool || p.action || String(p)"
                >
                  <span class="text-acc/70 text-[9px]">{{ planIcon(p) }}</span>
                  <span class="max-w-[220px] truncate">{{ planShort(p) }}</span>
                </span>
              </div>

              <!-- 需要解锁提示 -->
              <div
                v-if="m.needsUnlock"
                class="mt-2.5 flex items-center gap-2 text-[12.5px] text-warn"
              >
                <span>⚠ 此操作需要真实 LLM 授权</span>
                <button class="btn btn-warn !px-2.5 !py-1 !text-[11.5px]" @click="doUnlock">
                  去解锁
                </button>
              </div>
            </div>
            <div class="mt-1 ml-1 text-[10.5px] text-fg-dim/60 font-mono">
              {{ new Date(m.ts).toLocaleTimeString('zh-CN', { hour: '2-digit', minute: '2-digit' }) }}
            </div>
          </div>
        </div>

        <!-- 用户气泡 -->
        <div v-else class="max-w-[85%]">
          <div
            class="rounded-xl rounded-tr-sm px-3.5 py-2.5 text-[14.5px] leading-relaxed
                   whitespace-pre-wrap break-words shadow-1"
            style="background: linear-gradient(135deg, #1e4a6d, #2b6690); color: #e6f2fb"
          >{{ m.text }}</div>
        </div>
      </div>
    </div>
  </div>
</template>