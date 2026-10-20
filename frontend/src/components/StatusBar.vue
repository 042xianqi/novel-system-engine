<script setup>
// StatusBar.vue —— 顶部全局状态栏（固定右上，两枚极小胶囊）
//
// ══════════════════════════════════════════════════════════
// 设计约束（DESIGN-v2 + 任务书）
// ══════════════════════════════════════════════════════════
// · 固定右上（fixed top-0 right-0），**不占主区高度**，不撑出大色块
// · 胶囊高 26px（≤30px）、text-[11px]、leading-none，色块只有 1.5px 的状态点
// · 只表达两件事：
//     ① 真实 LLM：已锁定 / 已解锁      ← store.js 的 lock
//     ② 执行器：在线 / 离线（+PID、队列数）← store.js 的 tasks
// · 颜色只用语义 token（text-success / text-warning / text-danger / border-line…），
//   切皮肤零改动；禁止硬编码色值
//
// ══════════════════════════════════════════════════════════
// 数据来源与优先级（不新造数据源）
// ══════════════════════════════════════════════════════════
//   ① store.js  lock.unlocked / lock.loaded        ← useTasks 轮询每 3~8s 刷新
//   ② store.js  tasks.executorAlive / executorPid  ← 同一轮询写入（最实时）
//   ③ useSystem.js → store.systemData.taskList.active
//        （executorAlive / executorPid / llmUnlocked）作为兜底：
//        TaskBoard 未挂载时 tasks 可能还没首轮结果，这里用看板已拉到的快照补上，
//        避免状态栏长时间显示「离线」误导用户。
//   挂载时主动调一次 useSystem.loadTaskBoard()（带缓存/去重，不会重复打网络），
//   让状态栏在用户没进过任务看板时也有真实数据。
import { computed, onMounted } from 'vue'
import { lock, tasks, systemData } from '../store.js'
import { loadTaskBoard } from '../composables/useSystem.js'
import { openWin } from '../nav.js'

const active = computed(() => systemData.taskList?.active || {})

/** 真实 LLM 是否解锁：以实时轮询为准，未拉到结果时用看板快照兜底 */
const llmUnlocked = computed(() =>
  lock.loaded ? !!lock.unlocked : !!active.value.llmUnlocked
)

/** 执行器是否在线：同上 */
const execAlive = computed(() =>
  tasks.loaded ? !!tasks.executorAlive : !!active.value.executorAlive
)

const execPid = computed(() => tasks.executorPid ?? active.value.executorPid ?? null)

/** 队列里还在等/在跑的任务数（0 时不显示，避免噪音） */
const queued = computed(() => tasks.counts.pending + tasks.counts.running)

const lockText = computed(() => (llmUnlocked.value ? '真实 LLM 已解锁' : '真实 LLM 已锁定'))
const lockTone = computed(() =>
  llmUnlocked.value ? 'text-warning' : 'text-success'
)
const lockTitle = computed(() =>
  llmUnlocked.value
    ? `已解锁：闸刀文件存在，真实模型调用会真花钱\n${lock.file}`
    : `已锁定：未检测到闸刀文件，走离线/假模型\n${lock.file}`
)

const execText = computed(() => (execAlive.value ? '执行器在线' : '执行器离线'))
const execTone = computed(() => (execAlive.value ? 'text-success' : 'text-danger'))
const execTitle = computed(() =>
  execAlive.value
    ? `执行器进程存活${execPid.value ? `（PID ${execPid.value}）` : ''}；点击打开任务执行窗`
    : '未检测到执行器进程；点击打开任务执行窗查看日志'
)

onMounted(() => {
  // 带缓存：看板已加载过就直接返回，不会重复请求
  loadTaskBoard().catch(() => {})
})
</script>

<template>
  <!-- 固定右上；容器不吃指针事件，只有胶囊本身可点 -->
  <div
    class="fixed top-0 right-0 z-50 flex items-center gap-1.5 p-2 pointer-events-none"
    aria-live="polite"
  >
    <!-- ① 真实 LLM 锁定态 -->
    <span
      class="inline-flex items-center gap-1.5 h-[26px] px-2 rounded-full
             border border-line bg-surface-card-2/85 backdrop-blur-sm
             text-[11px] leading-none whitespace-nowrap"
      :class="lockTone"
      :title="lockTitle"
    >
      <span
        class="w-1.5 h-1.5 rounded-full shrink-0"
        :class="llmUnlocked ? 'bg-warning' : 'bg-success'"
        aria-hidden="true"
      />
      {{ lockText }}
    </span>

    <!-- ② 执行器状态（可点，打开任务执行窗） -->
    <button
      type="button"
      class="pointer-events-auto inline-flex items-center gap-1.5 h-[26px] px-2 rounded-full
             border border-line bg-surface-card-2/85 backdrop-blur-sm
             text-[11px] leading-none whitespace-nowrap
             transition-transform active:scale-[0.97]"
      :class="execTone"
      :title="execTitle"
      :aria-label="`${execText}，打开任务执行窗`"
      @click="openWin()"
    >
      <span
        class="w-1.5 h-1.5 rounded-full shrink-0"
        :class="execAlive ? 'bg-success' : 'bg-danger'"
        aria-hidden="true"
      />
      {{ execText }}
      <span v-if="queued > 0" class="text-content-sec/80 tabular-nums" aria-hidden="true">
        · {{ queued }}
      </span>
    </button>
  </div>
</template>
