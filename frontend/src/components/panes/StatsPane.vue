<script setup>
// StatsPane.vue —— 数据统计 / 系统体检
//
// 数据来源（全部只读，可并发拉）：
//   /api/pipeline/materials → 声明 vs 实有：缺卡清单、漂移数、分类计数
//   /api/layers/list        → 分层条目数
//   /api/pipeline/funnel    → 漏斗进度
//   POST /api/system/self-check → 真跑一遍检查（按钮触发，不自动）
//
// 为什么自检不自动跑：
//   它是 POST 且后端会真扫描资料库，耗时数秒。自动跑会让"点开统计页"变卡。
//   这里做成显式按钮，且状态独立（s.checking），不阻塞上面三块的渲染。
import { computed, onMounted } from 'vue'
import { systemData } from '../../store.js'
import {
  loadStats,
  runSelfCheck,
  missingCards,
  groupCounts,
} from '../../composables/useSystem.js'

const s = systemData.stats

const decl = computed(() => s.materials?.declared || {})
const missing = computed(() => missingCards())
const groups = computed(() => groupCounts())
const layers = computed(() => s.layers?.actual || [])
const funnel = computed(() => s.funnel || {})

/** 骨架缺口：用户点名要补的四类 */
const SKELETON = ['功法', '势力', '地点', '因果线']

function totalOf(name) {
  const g = groups.value.find((x) => x.name === name)
  return g ? g.count : 0
}
const skeletonRows = computed(() =>
  SKELETON.map((n) => ({ name: n, count: totalOf(n) }))
)

function fmtTime(ts) {
  if (!ts) return ''
  const d = new Date(ts)
  const p = (x) => String(x).padStart(2, '0')
  return `${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}`
}

/** 自检结果里的计数（结构不确定，做宽松解析） */
const checkSummary = computed(() => {
  const r = s.checkResult
  if (!r) return null
  const issues = r.issues || r.problems || r.findings || []
  const arr = Array.isArray(issues) ? issues : []
  const sev = (k) =>
    arr.filter((x) => String(x.severity || x.level || '').toLowerCase() === k).length
  return {
    total: arr.length,
    p0: sev('p0') || sev('error') || sev('red'),
    p1: sev('p1') || sev('warn') || sev('yellow'),
    ok: r.ok !== false && arr.length === 0,
    raw: r,
  }
})

async function refresh() {
  await loadStats(true)
}
async function doCheck() {
  await runSelfCheck()
}

onMounted(() => loadStats())
</script>

<template>
  <div class="h-full overflow-y-auto px-3 py-3 space-y-3">
    <!-- ── 顶部：缺卡总览 ── -->
    <div class="glass rounded-md px-3 py-2.5 space-y-2">
      <div class="flex items-center gap-2">
        <span class="text-[12px] text-fg">卡片盘点</span>
        <span
          v-if="decl.missing && decl.missing.length"
          class="text-[10px] px-1.5 py-0.5 rounded-sm bg-warn/15 text-warn"
        >缺 {{ decl.missing.length }}</span>
        <span
          v-else
          class="text-[10px] px-1.5 py-0.5 rounded-sm bg-ok/15 text-ok"
        >无缺卡</span>
        <button
          class="ml-auto text-[11px] text-fg-dim active:scale-95 disabled:opacity-40"
          :disabled="s.loading"
          @click="refresh"
        >{{ s.loading ? '刷新中…' : '刷新' }}</button>
      </div>

      <div class="grid grid-cols-3 gap-2">
        <div class="text-center">
          <div class="font-mono text-[15px] text-fg leading-none">
            {{ decl.refs_total ?? '—' }}
          </div>
          <div class="text-[10px] text-fg-dim mt-1">声明引用</div>
        </div>
        <div class="text-center">
          <div class="font-mono text-[15px] text-ok leading-none">
            {{ decl.existing_total ?? '—' }}
          </div>
          <div class="text-[10px] text-fg-dim mt-1">实有卡片</div>
        </div>
        <div class="text-center">
          <div
            class="font-mono text-[15px] leading-none"
            :class="decl.drift_count ? 'text-warn' : 'text-fg-dim'"
          >{{ decl.drift_count ?? 0 }}</div>
          <div class="text-[10px] text-fg-dim mt-1">漂移</div>
        </div>
      </div>
    </div>

    <!-- ── 骨架四类（用户点名要补的）── -->
    <div class="glass rounded-md px-3 py-2.5 space-y-1.5">
      <div class="text-[12px] text-fg">骨架缺口</div>
      <div class="grid grid-cols-2 gap-2">
        <div
          v-for="r in skeletonRows"
          :key="r.name"
          class="flex items-center justify-between rounded-sm px-2 py-1.5"
          :class="r.count <= 1 ? 'bg-warn/10' : 'bg-ink-700/40'"
        >
          <span class="text-[11.5px] text-fg">{{ r.name }}</span>
          <span
            class="font-mono text-[12px]"
            :class="r.count <= 1 ? 'text-warn' : 'text-ok'"
          >{{ r.count }}</span>
        </div>
      </div>
      <div class="text-[10px] text-fg-dim/60 leading-relaxed">
        数量 ≤1 的类目高亮（骨架明显偏薄，优先补）
      </div>
    </div>

    <!-- ── 缺卡清单 ── -->
    <div v-if="missing.length" class="glass rounded-md px-3 py-2.5 space-y-1.5">
      <div class="text-[12px] text-fg">缺卡清单</div>
      <div
        v-for="m in missing"
        :key="m"
        class="text-[11px] text-warn font-mono break-all"
      >· {{ m }}</div>
      <div
        v-if="decl.note"
        class="text-[10px] text-fg-dim/70 leading-relaxed pt-1"
      >{{ decl.note }}</div>
    </div>

    <!-- ── 分类计数 ── -->
    <div class="glass rounded-md px-3 py-2.5 space-y-1.5">
      <div class="text-[12px] text-fg">分类计数</div>
      <div class="grid grid-cols-2 gap-x-3 gap-y-1">
        <div
          v-for="g in groups"
          :key="g.name"
          class="flex items-center justify-between text-[11px]"
        >
          <span class="text-fg-dim truncate">{{ g.name }}</span>
          <span class="font-mono text-fg shrink-0 ml-1">{{ g.count }}</span>
        </div>
      </div>
    </div>

    <!-- ── 分层计数 ── -->
    <div class="glass rounded-md px-3 py-2.5 space-y-1.5">
      <div class="flex items-center gap-2">
        <span class="text-[12px] text-fg">分层条目</span>
        <span class="text-[10px] text-fg-dim font-mono">
          共 {{ s.layers?.total ?? '—' }}
        </span>
      </div>
      <div class="flex flex-wrap gap-1.5">
        <span
          v-for="l in layers"
          :key="l.layer"
          class="text-[10.5px] px-1.5 py-0.5 rounded-sm bg-ink-700/60 text-fg-dim font-mono"
        >{{ l.layer }} {{ l.count }}</span>
      </div>
    </div>

    <!-- ── 漏斗 ── -->
    <div class="glass rounded-md px-3 py-2.5 space-y-1">
      <div class="flex items-center gap-2">
        <span class="text-[12px] text-fg">漏斗进度</span>
        <span class="font-mono text-[12px] text-acc">{{ funnel.progress || '—' }}</span>
      </div>
      <div
        v-if="funnel.next"
        class="text-[10.5px] text-fg-dim leading-relaxed"
      >
        下一步：{{ funnel.next.action }}（{{ funnel.next.layer }} {{ funnel.next.name }}）
      </div>
    </div>

    <!-- ── 系统自检 ── -->
    <div class="glass rounded-md px-3 py-2.5 space-y-2">
      <div class="flex items-center gap-2">
        <span class="text-[12px] text-fg">系统体检</span>
        <button
          class="ml-auto text-[11px] px-2 py-1 rounded-sm border border-acc/30
                 bg-acc/10 text-acc active:scale-95 disabled:opacity-40"
          :disabled="s.checking"
          @click="doCheck"
        >{{ s.checking ? '体检中…' : '跑一次体检' }}</button>
      </div>

      <div v-if="s.checkErr" class="text-[11px] text-bad break-all">
        {{ s.checkErr }}
      </div>

      <template v-else-if="checkSummary">
        <div class="flex items-center gap-2 text-[11px]">
          <span
            class="px-1.5 py-0.5 rounded-sm"
            :class="checkSummary.ok ? 'bg-ok/15 text-ok' : 'bg-warn/15 text-warn'"
          >{{ checkSummary.ok ? '通过' : `发现 ${checkSummary.total} 项` }}</span>
          <span v-if="checkSummary.p0" class="text-bad font-mono">
            P0 {{ checkSummary.p0 }}
          </span>
          <span v-if="checkSummary.p1" class="text-warn font-mono">
            P1 {{ checkSummary.p1 }}
          </span>
        </div>
        <pre
          class="text-[10px] text-fg-dim/80 leading-relaxed max-h-40 overflow-y-auto
                 whitespace-pre-wrap break-words font-mono"
        >{{ JSON.stringify(checkSummary.raw, null, 1).slice(0, 2000) }}</pre>
      </template>

      <div v-else class="text-[10.5px] text-fg-dim/60 leading-relaxed">
        点上方按钮跑一次只读体检（不改数据），结果会显示在这里。
      </div>
    </div>

    <div v-if="s.ts" class="text-center text-[10px] text-fg-dim/50 font-mono">
      统计时间 {{ fmtTime(s.ts) }}
    </div>

    <div
      v-if="s.err"
      class="rounded-md px-3 py-2 text-[11px] bg-bad/10 text-bad break-all"
    >{{ s.err }}</div>

    <div class="h-2" />
  </div>
</template>