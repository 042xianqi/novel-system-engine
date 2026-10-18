<script setup>
// FloatWindow.vue —— 毛玻璃悬浮任务窗
//
// 四种形态由 nav.win 驱动：
//   hidden   → 不渲染
//   capsule  → 由 Capsule.vue 渲染
//   expanded → 本组件的悬浮窗形态
//   full     → 本组件的全屏日志形态
//
// 职责：
//   · 展示活跃任务 + 进度条 + 派发/取消按钮
//   · 展示流水线日志
//   · 所有写操作都走 useTasks（不在这里直接 fetch）

import { computed, ref } from 'vue'
import { tasks, winLog } from '../store.js'
import { nav, minimizeWin, closeWin, toggleFull } from '../nav.js'
import { useTasks } from '../composables/useTasks.js'

const { dispatch, refresh } = useTasks()

const busy = ref({})   // { [taskId]: true } 防连点

const isFull = computed(() => nav.win === 'full')

const activeCount = computed(() => tasks.counts.pending + tasks.counts.running)

// 时间格式
function hhmmss(ts) {
  if (!ts) return '--:--:--'
  return new Date(ts * 1000).toLocaleTimeString('zh-CN', { hour12: false })
}

// 每条任务的状态徽章
function badgeClass(state) {
  if (state === 'running') return 'badge badge-run'
  if (state === 'pending') return 'badge badge-pend'
  if (state === 'done') return 'badge badge-done'
  return 'badge badge-bad'
}

function stateLabel(state) {
  const map = {
    pending: '待执行',
    running: '运行中',
    done: '已完成',
    failed: '失败',
    rejected: '被拒',
    blocked: '被锁'
  }
  return map[state] || state
}

// 任务标题：cardgen 显示「文明 + 规格」，chapter_gen 显示卷/章
function taskTitle(t) {
  if (t.kind === 'cardgen') {
    return `${t.civ || '未指定文明'} · 建卡`
  }
  if (t.kind === 'chapter_gen') {
    return `${t.volume || '?'} 第 ${t.chapter || '?'} 章`
  }
  return t.kind
}

// 进度百分比（cardgen 有 total/done）
function pct(t) {
  if (!t.total) return null
  return Math.min(100, Math.round((t.done / t.total) * 100))
}

async function doDispatch(t) {
  if (busy.value[t.id]) return
  busy.value[t.id] = true
  try {
    await dispatch(t.id, 1)
  } finally {
    busy.value[t.id] = false
  }
}

async function doRefresh() {
  await refresh()
}

// 日志颜色
function logClass(kind) {
  if (kind === 'ok') return 'text-ok'
  if (kind === 'bad') return 'text-bad'
  if (kind === 'warn') return 'text-warn'
  if (kind === 'task') return 'text-acc'
  return 'text-fg-dim'
}
</script>

<template>
  <div
    v-if="nav.win === 'expanded' || nav.win === 'full'"
    class="fixed z-50 glass-strong shadow-3 animate-slide-up flex flex-col"
    :class="isFull
      ? 'inset-0 rounded-none'
      : 'right-3 left-3 bottom-24 rounded-xl max-h-[62vh] sm:right-4 sm:left-auto sm:w-[420px]'"
  >
    <!-- 标题栏 -->
    <div class="shrink-0 flex items-center gap-2 px-3.5 py-2.5 border-b border-white/[0.08]">
      <span class="relative flex w-2 h-2">
        <span
          class="absolute inset-0 rounded-full opacity-60 animate-breathe"
          :class="activeCount > 0 ? 'bg-acc' : 'bg-ok'"
        />
        <span
          class="relative w-2 h-2 rounded-full"
          :class="activeCount > 0 ? 'bg-acc' : 'bg-ok'"
        />
      </span>

      <span class="text-[13px] font-semibold text-fg">任务中枢</span>

      <span class="text-[11px] font-mono text-fg-dim">
        {{ tasks.counts.running }} 跑 / {{ tasks.counts.pending }} 待
      </span>

      <span
        v-if="tasks.executorAlive"
        class="badge badge-run !py-0 !text-[10px]"
      >exec {{ tasks.executorPid }}</span>

      <div class="ml-auto flex items-center gap-1">
        <button
          class="w-7 h-7 rounded-md text-fg-dim hover:text-fg hover:bg-white/[0.08]
                 transition-all duration-150 active:scale-95 flex items-center justify-center text-[13px]"
          title="刷新"
          @click="doRefresh"
        >⟳</button>
        <button
          class="w-7 h-7 rounded-md text-fg-dim hover:text-fg hover:bg-white/[0.08]
                 transition-all duration-150 active:scale-95 flex items-center justify-center text-[13px]"
          :title="isFull ? '收起' : '全屏日志'"
          @click="toggleFull()"
        >{{ isFull ? '⤡' : '⤢' }}</button>
        <button
          class="w-7 h-7 rounded-md text-fg-dim hover:text-fg hover:bg-white/[0.08]
                 transition-all duration-150 active:scale-95 flex items-center justify-center text-[13px]"
          title="最小化"
          @click="minimizeWin()"
        >—</button>
        <button
          class="w-7 h-7 rounded-md text-fg-dim hover:text-bad hover:bg-bad/10
                 transition-all duration-150 active:scale-95 flex items-center justify-center text-[13px]"
          title="关闭"
          @click="closeWin()"
        >✕</button>
      </div>
    </div>

    <!-- 任务列表 -->
    <div class="flex-1 min-h-0 overflow-y-auto px-3 py-2.5 space-y-2">
      <!-- 空 -->
      <div
        v-if="!tasks.items.length"
        class="py-6 text-center text-[12.5px] text-fg-dim"
      >
        没有活跃任务<br>
        <span class="text-[11.5px] opacity-70">在下方对话框输入指令即可派活</span>
      </div>

      <!-- 任务卡 -->
      <div
        v-for="t in tasks.items"
        :key="t.id"
        class="rounded-lg bg-white/[0.035] border border-white/[0.06] px-3 py-2.5
               animate-rise"
      >
        <div class="flex items-start gap-2">
          <div class="min-w-0 flex-1">
            <div class="flex items-center gap-2 flex-wrap">
              <span class="text-[13px] font-medium text-fg truncate">{{ taskTitle(t) }}</span>
              <span :class="badgeClass(t.state)">{{ stateLabel(t.state) }}</span>
              <span
                v-if="t.real"
                class="badge badge-bad !py-0 !text-[10px]"
              >REAL</span>
            </div>
            <div class="mt-0.5 text-[10.5px] font-mono text-fg-dim/70 truncate">
              {{ t.id }} · {{ hhmmss(t.updated_ts) }}
            </div>
          </div>

          <button
            v-if="t.state === 'pending'"
            class="btn btn-primary !px-2.5 !py-1 !text-[11.5px] shrink-0"
            :disabled="busy[t.id]"
            @click="doDispatch(t)"
          >{{ busy[t.id] ? '…' : '执行' }}</button>
        </div>

        <!-- 进度条（cardgen） -->
        <div v-if="pct(t) !== null" class="mt-2">
          <div class="prog" :class="{ 'prog-indet': t.total && !t.done }">
            <i :style="{ width: (pct(t) ?? 0) + '%' }" />
          </div>
          <div class="mt-1 flex items-center justify-between text-[10.5px] font-mono text-fg-dim">
            <span>{{ t.done }}/{{ t.total }} · 成功 {{ t.success }} · 失败 {{ t.failed }}</span>
            <span v-if="t.current" class="truncate max-w-[45%] text-acc">{{ t.current }}</span>
          </div>
        </div>
      </div>
    </div>

    <!-- 日志（全屏态才展开，避免悬浮窗太挤） -->
    <div
      v-if="isFull"
      class="shrink-0 h-[36%] border-t border-white/[0.08] overflow-y-auto
             px-3 py-2 font-mono text-[11.5px] leading-relaxed bg-black/20"
    >
      <div v-if="!winLog.items.length" class="text-fg-dim/60">（暂无日志）</div>
      <div v-for="l in winLog.items" :key="l.id" class="flex gap-2">
        <span class="shrink-0 text-fg-dim/50">{{ hhmmss(l.ts / 1000) }}</span>
        <span :class="logClass(l.kind)" class="break-all">{{ l.text }}</span>
      </div>
    </div>
  </div>
</template>