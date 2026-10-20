<script setup>
// ══════════════════════════════════════════════════════════════════
// Dashboard.vue —— 数据驾驶舱
//
// 数据源（全部为同源相对路径真实端点，字段已实测）：
//   GET /api/library/categories → {categories:[{name,members,archived,...}]}
//   GET /api/graph/entity       → {nodes:[{k,title,kind}],edges:[...],count:{nodes,edges}}
//   GET /api/tasks/active       → {active_count,running,pending,...}
//   GET /api/tasks/list         → {tasks:[{id,summary,state,updated_ts,...}]}
//   GET /api/system/status      → {gate_l1:{open,status,...}, ...}
//
// 设计契约（DESIGN-v2.md）：
//   · 颜色只读 CSS 变量 / 语义 token，不硬编码与皮肤冲突的色值
//   · 卡片只用 .glass / .card-cyber，动效不用 transition-all
//   · 图表容器必须有明确高度，否则 echarts 渲染成 0 高
// ══════════════════════════════════════════════════════════════════
import { ref, computed, onMounted, onBeforeUnmount } from 'vue'
// 注意：这里**没有**静态 import echarts。
// ══════════════════════════════════════════════════════════════
// 为什么必须动态导入（code-split）
// ══════════════════════════════════════════════════════════════
// 原先静态 `import * as echarts from 'echarts/core'` 会把图表库全部打进首屏包，
// 实测主包从 266K 涨到 **789K**（构建也告警 chunk > 600kB）。手机 WebView 上
// 这是实打实的首屏负担。
//
// 现改为按需 `await import(...)`：首屏只加载业务代码，图表库被 Vite 切成
// 独立 chunk，**只有当用户真的进入驾驶舱时才异步拉取**。
// 因为驾驶舱是默认页，首次进入仍会加载它，但：
//   ① 首屏渲染不再被 520KB 解析阻塞（并行拉取，先出骨架与指标卡）
//   ② 其它页面（任务/卡片/大纲/星图）从此完全不背这个包
// 用 Promise 缓存保证并发调用只加载一次。
import { pushLog } from '../../store.js'
// 顶层路由 / 子页跳转（指标卡点击导航用；nav.js 零业务依赖）
import { goTab, goPane } from '../../nav.js'

// ── 实体 kind → 中文 ──
// 环形图（标签/tooltip/图例）与任何展示 kind 的地方统一走 kindZh()。
// 映射不到时回退原串，绝不显示 undefined。
const KIND_ZH = {
  CHAR: '角色',
  LOCA: '地点',
  FACT: '势力',
  CIVI: '文明',
  TECH: '技术',
  CLTR: '文化',
  POWR: '力量体系',
  SYSX: '系统规则',
  ARTS: '物品',
  EVNT: '事件',
  ITEM: '条目',
  FORE: '伏笔',
  GEAR: '装备',
  TIME: '时间',
  WEIR: '异象',
  XREA: '未知区域',
}
function kindZh(k) {
  const s = stripId(k)
  if (!s) return ''
  return KIND_ZH[s.toUpperCase()] || s
}

let _ec = null
let _ecPromise = null
function ensureEcharts() {
  if (_ec) return Promise.resolve(_ec)
  if (!_ecPromise) {
    _ecPromise = Promise.all([
      import('echarts/core'),
      import('echarts/charts'),
      import('echarts/components'),
      import('echarts/renderers'),
    ]).then(([core, charts, comps, renderers]) => {
      core.use([
        charts.BarChart,
        charts.PieChart,
        comps.GridComponent,
        comps.TooltipComponent,
        comps.LegendComponent,
        renderers.CanvasRenderer,
      ])
      _ec = core
      return core
    })
  }
  return _ecPromise
}

// ── 内部 id 一律不外显（用户只认任务名）──
const ID_RE = /\bwt-[0-9a-zA-Z-]{2,}\b/g
function stripId(s) {
  return String(s == null ? '' : s).replace(ID_RE, '').replace(/\s+/g, ' ').trim()
}

// ── 状态色点（语义 token，随皮肤走）──
const STATE_META = {
  running: { label: '执行中', dot: 'bg-primary' },
  pending: { label: '排队中', dot: 'bg-warning' },
  done: { label: '已完成', dot: 'bg-success' },
  failed: { label: '失败', dot: 'bg-danger' },
  rejected: { label: '已拒绝', dot: 'bg-danger' },
  archived: { label: '已归档', dot: 'bg-content-disabled' },
}
function stateMeta(raw) {
  const k = String(raw || '').toLowerCase()
  return STATE_META[k] || { label: k ? k : '未知', dot: 'bg-content-disabled' }
}

// ── 视图状态 ──
const loading = ref(true)
const err = ref('')
const updatedAt = ref('')
const stats = ref({ totalCards: 0, edges: 0, unfinished: 0, red: 0 })
const cats = ref([]) // [{name,members}] 降序 Top12
// 12 个文明的核心规模（来自 /api/stats.by_civ_core；后端未就绪时为空数组）
const civCore = ref([])
const kinds = ref([]) // [{name,value}] 实体类型计数
const recent = ref([]) // [{key,name,stateLabel,dot,time}]
// 系统状态区块（③.5）：全部复用本轮已取到的数据，不新增请求
//   llm  ← /api/tasks/active.llm_unlocked
//   exec ← /api/tasks/active.executor_alive
//   size ← /api/system/status.storage.bytes
//   cards← /api/stats.total
// 字段缺失一律显示「—」，不显示 undefined。
const sysStatus = ref({
  llm: '—',
  llmTone: 'text-content-sec',
  exec: '—',
  execTone: 'text-content-sec',
  size: '—',
  sizeTone: 'text-content-sec',
  cards: '—',
})

const metrics = computed(() => [
  {
    label: '总卡数',
    value: stats.value.totalCards,
    tone: 'text-primary',
    go: () => {
      goTab('system')
      goPane('cards')
    },
  },
  {
    label: '实体关系数',
    value: stats.value.edges,
    tone: 'text-content',
    go: () => goPane('graph'),
  },
  {
    label: '未完成任务',
    value: stats.value.unfinished,
    tone: stats.value.unfinished > 0 ? 'text-warning' : 'text-content',
    go: () => goPane('tasks'),
  },
  {
    label: '系统红灯数',
    value: stats.value.red,
    tone: stats.value.red > 0 ? 'text-danger' : 'text-success',
    go: () => goPane('stats'),
  },
])

// ── 工具 ──
function fmtTs(ts) {
  if (!ts) return ''
  const n = Number(ts)
  const ms = n < 1e12 ? n * 1000 : n
  const d = new Date(ms)
  if (Number.isNaN(d.getTime())) return ''
  const p = (x) => String(x).padStart(2, '0')
  return `${p(d.getMonth() + 1)}-${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}`
}

async function getJSON(path) {
  const r = await fetch(path, { headers: { Accept: 'application/json' } })
  if (!r.ok) throw new Error(`HTTP ${r.status} ${path}`)
  return r.json()
}

// 字节 → 人类可读（缺失/非法一律「—」，不显示 undefined）
function fmtBytes(n) {
  const v = Number(n)
  if (!Number.isFinite(v) || v < 0) return '—'
  if (v < 1024) return `${v} B`
  const kb = v / 1024
  if (kb < 1024) return `${kb.toFixed(1)} KB`
  const mb = kb / 1024
  if (mb < 1024) return `${mb.toFixed(1)} MB`
  return `${(mb / 1024).toFixed(2)} GB`
}

// ── 主题色：全部读 CSS 变量（皮肤切换零组件改动）──
function cssVar(name, fallback = '') {
  const v = getComputedStyle(document.documentElement).getPropertyValue(name)
  const s = (v || '').trim()
  return s || fallback
}
function readTheme() {
  const primary = cssVar('--primary', '#4f8cff')
  return {
    primary,
    gradB: cssVar('--grad-b', primary),
    text: cssVar('--text', '#1a1a2e'),
    textSec: cssVar('--text-sec', '#8c8fa6'),
    border: cssVar('--border', '#e6e8f0'),
    divider: cssVar('--divider', '#eef0f5'),
    card: cssVar('--card', '#ffffff'),
  }
}
function parseColor(str) {
  if (!str) return null
  const s = String(str).trim()
  let m
  if ((m = /^#([0-9a-f]{3})$/i.exec(s))) {
    return {
      r: parseInt(m[1][0] + m[1][0], 16),
      g: parseInt(m[1][1] + m[1][1], 16),
      b: parseInt(m[1][2] + m[1][2], 16),
    }
  }
  if ((m = /^#([0-9a-f]{6})/i.exec(s))) {
    return {
      r: parseInt(m[1].slice(0, 2), 16),
      g: parseInt(m[1].slice(2, 4), 16),
      b: parseInt(m[1].slice(4, 6), 16),
    }
  }
  if ((m = /^rgba?\(([^)]+)\)$/i.exec(s))) {
    const p = m[1].split(/[,\s/]+/).filter(Boolean).map(Number)
    if (p.length >= 3) return { r: p[0], g: p[1], b: p[2] }
  }
  return null
}
function rgbToHue({ r, g, b }) {
  const R = r / 255
  const G = g / 255
  const B = b / 255
  const max = Math.max(R, G, B)
  const min = Math.min(R, G, B)
  const d = max - min
  if (d === 0) return 217
  let h
  if (max === R) h = ((G - B) / d) % 6
  else if (max === G) h = (B - R) / d + 2
  else h = (R - G) / d + 4
  h *= 60
  if (h < 0) h += 360
  return h
}
// 环形图配色：色相以 --primary 为起点按数量均匀展开 → 不脱离当前皮肤主色系
function paletteFor(n) {
  const base = parseColor(cssVar('--primary')) || { r: 79, g: 140, b: 255 }
  const h0 = rgbToHue(base)
  const count = Math.max(n, 1)
  const step = 360 / Math.max(count, 6)
  const out = []
  for (let i = 0; i < count; i++) {
    const h = (h0 + i * step) % 360
    const sat = 58 + (i % 2) * 8
    const light = 52 + (i % 3) * 7
    out.push(`hsl(${h.toFixed(0)}, ${sat}%, ${light}%)`)
  }
  return out
}

// ── 数据加载 ──
async function load() {
  loading.value = true
  err.value = ''
  const [statsR, catsR, graphR, actR, listR, sysR] = await Promise.allSettled([
    getJSON('/api/stats'),
    getJSON('/api/library/categories'),
    getJSON('/api/graph/entity'),
    getJSON('/api/tasks/active'),
    getJSON('/api/tasks/list'),
    getJSON('/api/system/status'),
  ])
  const bad = []

  // ① 总卡数 + 分类柱状图
  // 【2026-10-20 注入 Operit 逻辑】口径必须优先读 /api/stats：
  //   /api/library/categories 的 members 是**陈旧**的（实测总 636，
  //   而真实 861；势力 4 vs 真 55、力量体系 21 vs 真 71 —— 柱子严重误导）。
  //   /api/stats 才是权威口径，categories 仅作回退（宁可偏小也不白屏）。
  if (statsR.status === 'fulfilled' && statsR.value && typeof statsR.value === 'object') {
    const st = statsR.value
    stats.value.totalCards = Number(st.total) || 0
    const arr = Array.isArray(st.by_category) ? st.by_category : []
    if (arr.length) {
      cats.value = arr
        .map((c) => ({
          name: stripId(c?.name || c?.category || c?.key) || '未命名',
          members: Number(c?.count ?? c?.members) || 0,
        }))
        .sort((a, b) => b.members - a.members)
        .slice(0, 12)
    }
    // 12 个文明的核心规模（总指挥要的"按文明分布"正解）
    const civ = Array.isArray(st.by_civ_core) ? st.by_civ_core : []
    if (civ.length) civCore.value = civ.slice(0, 12)
  } else if (catsR.status === 'fulfilled') {
    const all = Array.isArray(catsR.value?.categories) ? catsR.value.categories : []
    stats.value.totalCards = all.reduce((n, c) => n + (Number(c?.members) || 0), 0)
    cats.value = all
      .map((c) => ({ name: stripId(c?.name) || '未命名', members: Number(c?.members) || 0 }))
      .sort((a, b) => b.members - a.members)
      .slice(0, 12)
    bad.push('stats 回退到 categories（数值可能偏小）')
  } else {
    bad.push('分类')
  }

  // ② 实体关系数 + 实体类型环形图
  let unfinished = null
  if (graphR.status === 'fulfilled') {
    const g = graphR.value || {}
    const cnt = g.count || {}
    stats.value.edges = Number(cnt.edges ?? (Array.isArray(g.edges) ? g.edges.length : 0)) || 0
    const tally = new Map()
    for (const n of Array.isArray(g.nodes) ? g.nodes : []) {
      // kind 先汉化再聚合：环形图标签/tooltip/图例统一中文
      const k = kindZh(n?.kind) || 'UNKN'
      tally.set(k, (tally.get(k) || 0) + 1)
    }
    kinds.value = [...tally.entries()]
      .map(([name, value]) => ({ name, value }))
      .sort((a, b) => b.value - a.value)
  } else {
    bad.push('关系图')
  }

  // ③ 未完成任务
  if (actR.status === 'fulfilled') {
    const a = actR.value || {}
    unfinished = (Number(a.running) || 0) + (Number(a.pending) || 0)
    stats.value.unfinished = unfinished
  } else {
    bad.push('任务状态')
  }

  // ④ 系统红灯数（gate_l1.open === false 计 1）
  if (sysR.status === 'fulfilled') {
    const g1 = sysR.value?.gate_l1
    stats.value.red = g1 && g1.open === false ? 1 : 0
  } else {
    bad.push('系统状态')
  }

  // ⑤ 最近 5 条任务（禁止显示 wt- 内部 id）
  let rows = []
  if (listR.status === 'fulfilled') {
    const t = listR.value?.tasks
    rows = Array.isArray(t) ? t : []
  } else if (actR.status === 'fulfilled') {
    const it = actR.value?.items
    rows = Array.isArray(it) ? it : []
  }
  recent.value = rows
    .slice()
    .sort(
      (a, b) =>
        (Number(b?.updated_ts) || Number(b?.created_ts) || 0) -
        (Number(a?.updated_ts) || Number(a?.created_ts) || 0)
    )
    .slice(0, 5)
    .map((t, i) => {
      const m = stateMeta(t?.state)
      const name = stripId(t?.summary || t?.title || t?.kind) || '未命名任务'
      const ts = t?.updated_ts || t?.created_ts
      return {
        key: `${stripId(t?.id) || 't'}-${i}`,
        name,
        stateLabel: m.label,
        dot: m.dot,
        time: fmtTs(ts),
      }
    })

  // ③.5 系统状态区块 —— 全部复用上面已取到的数据，不新增请求
  const TONE_OK = 'text-success'
  const TONE_BAD = 'text-danger'
  const TONE_NA = 'text-content-sec'
  const s = sysStatus.value
  // LLM 授权锁 + 执行器存活：同源于 /api/tasks/active
  if (actR.status === 'fulfilled' && actR.value && typeof actR.value === 'object') {
    const a = actR.value
    if (typeof a.llm_unlocked === 'boolean') {
      s.llm = a.llm_unlocked ? '已解锁' : '已锁定'
      s.llmTone = a.llm_unlocked ? TONE_OK : TONE_BAD
    }
    if (typeof a.executor_alive === 'boolean') {
      s.exec = a.executor_alive ? '在线' : '离线'
      s.execTone = a.executor_alive ? TONE_OK : TONE_BAD
    }
  }
  // 数据库占用：/api/system/status.storage
  if (sysR.status === 'fulfilled' && sysR.value && typeof sysR.value === 'object') {
    const stg = sysR.value.storage
    if (stg && Number.isFinite(Number(stg.bytes))) {
      s.size = fmtBytes(stg.bytes) + (stg.truncated ? '+' : '')
      s.sizeTone = TONE_OK
    }
  }
  // 库卡数：/api/stats.total（与①同源，权威口径）
  if (statsR.status === 'fulfilled' && statsR.value && typeof statsR.value === 'object') {
    const tot = Number(statsR.value.total)
    if (Number.isFinite(tot)) {
      s.cards = String(tot)
    }
  }
  void TONE_NA

  if (bad.length) {
    err.value = `部分数据未取到：${bad.join(' / ')}（其余仍按已取到的真实数据显示）`
    pushLog(`驾驶舱：${err.value}`, 'warn')
  }
  updatedAt.value = fmtTs(Date.now())
  loading.value = false
}

// ── 图表 ──
const barEl = ref(null)
const pieEl = ref(null)
let barChart = null
let pieChart = null
let ro = null
let mo = null

function barOption(t) {
  return {
    animationDuration: 420,
    grid: { left: 6, right: 10, top: 14, bottom: 2, containLabel: true },
    tooltip: {
      trigger: 'axis',
      axisPointer: { type: 'shadow', shadowStyle: { color: t.divider } },
      backgroundColor: t.card,
      borderColor: t.border,
      textStyle: { color: t.text, fontSize: 11 },
    },
    xAxis: {
      type: 'category',
      data: cats.value.map((c) => c.name),
      axisLabel: { color: t.textSec, fontSize: 10, interval: 0, rotate: 32 },
      axisLine: { lineStyle: { color: t.divider } },
      axisTick: { show: false },
    },
    yAxis: {
      type: 'value',
      splitLine: { lineStyle: { color: t.divider } },
      axisLabel: { color: t.textSec, fontSize: 10 },
    },
    series: [
      {
        type: 'bar',
        name: '卡片数',
        data: cats.value.map((c) => c.members),
        barMaxWidth: 24,
        itemStyle: {
          borderRadius: [4, 4, 0, 0],
          // 纯对象渐变（不依赖 echarts.graphic，树摇包更稳）
          color: {
            type: 'linear',
            x: 0,
            y: 0,
            x2: 0,
            y2: 1,
            colorStops: [
              { offset: 0, color: t.primary },
              { offset: 1, color: t.gradB },
            ],
          },
        },
      },
    ],
  }
}

function pieOption(t) {
  return {
    animationDuration: 420,
    color: paletteFor(kinds.value.length),
    tooltip: {
      trigger: 'item',
      formatter: '{b}: {c}（{d}%）',
      backgroundColor: t.card,
      borderColor: t.border,
      textStyle: { color: t.text, fontSize: 11 },
    },
    legend: {
      type: 'scroll',
      orient: 'vertical',
      right: 0,
      top: 'middle',
      itemWidth: 8,
      itemHeight: 8,
      textStyle: { color: t.textSec, fontSize: 10 },
    },
    series: [
      {
        type: 'pie',
        radius: ['48%', '72%'],
        center: ['36%', '50%'],
        avoidLabelOverlap: true,
        itemStyle: { borderColor: t.card, borderWidth: 2 },
        label: { show: false },
        labelLine: { show: false },
        emphasis: { scale: true, scaleSize: 4 },
        data: kinds.value.map((k) => ({ name: k.name, value: k.value })),
      },
    ],
  }
}

function renderCharts() {
  const t = readTheme()
  if (barChart) barChart.setOption(barOption(t), true)
  if (pieChart) pieChart.setOption(pieOption(t), true)
}

function resizeCharts() {
  barChart?.resize()
  pieChart?.resize()
}

onMounted(async () => {
  await load()
  // 动态拉取图表库（首屏不背它）；失败也不能让整页挂掉 —— 指标卡与任务列表
  // 是纯 DOM，不依赖 echarts，照常可用。
  try {
    const ec = await ensureEcharts()
    if (barEl.value) barChart = ec.init(barEl.value)
    if (pieEl.value) pieChart = ec.init(pieEl.value)
    renderCharts()
  } catch (e) {
    pushLog(`图表库加载失败（指标卡与列表不受影响）：${e && e.message ? e.message : e}`, 'warn')
  }

  // 容器尺寸变化 → 重绘（移动端旋转 / 侧栏展收）
  if (typeof ResizeObserver !== 'undefined') {
    ro = new ResizeObserver(() => resizeCharts())
    if (barEl.value) ro.observe(barEl.value)
    if (pieEl.value) ro.observe(pieEl.value)
  }
  // 皮肤切换 → 重新取色重绘
  mo = new MutationObserver(() => renderCharts())
  mo.observe(document.documentElement, { attributes: true, attributeFilter: ['data-skin'] })
})

onBeforeUnmount(() => {
  ro?.disconnect()
  ro = null
  mo?.disconnect()
  mo = null
  barChart?.dispose()
  barChart = null
  pieChart?.dispose()
  pieChart = null
})
</script>

<template>
  <section class="h-full flex flex-col gap-3 p-3 min-w-0">
    <!-- 标题行 -->
    <header class="shrink-0 flex items-center justify-between gap-2">
      <h2 class="text-content text-[13px] font-semibold">数据驾驶舱</h2>
      <span class="text-content-sec text-[11px] tabular-nums">
        <template v-if="loading">加载中…</template>
        <template v-else-if="updatedAt">更新于 {{ updatedAt }}</template>
      </span>
    </header>

    <!-- 局部失败提示（不阻断其余真实数据展示） -->
    <div
      v-if="err"
      class="shrink-0 rounded-[var(--r-sm)] bg-danger-soft border border-danger/40 border-l-[3px] px-2.5 py-1.5 text-[11px] text-content"
      role="status"
    >
      {{ err }}
    </div>

    <!-- 内容区自己滚，禁止页面级滚动条 -->
    <div class="flex-1 min-w-0 overflow-y-auto">
      <!-- ① 4 个指标卡（可点击跳转，键盘可达） -->
      <div class="grid grid-cols-2 md:grid-cols-4 gap-2">
        <div
          v-for="m in metrics"
          :key="m.label"
          role="button"
          tabindex="0"
          :aria-label="`${m.label}：${m.value}，点击跳转`"
          class="glass card-cyber rounded-[var(--r-md)] px-3 py-2.5 min-w-0 cursor-pointer select-none outline-none transition-[transform,opacity] duration-150 ease-out hover:opacity-90 active:scale-[0.98] focus-visible:ring-2 focus-visible:ring-primary/50 motion-reduce:transition-none"
          @click="m.go && m.go()"
          @keydown.enter.prevent="m.go && m.go()"
          @keydown.space.prevent="m.go && m.go()"
        >
          <div class="text-[22px] leading-none font-semibold tabular-nums" :class="m.tone">
            {{ m.value }}
          </div>
          <div class="mt-1 text-[11px] text-content-sec truncate">{{ m.label }}</div>
        </div>
      </div>

      <!-- ②③ 图表：移动端单列 / 桌面双列 -->
      <div class="mt-3 grid grid-cols-1 md:grid-cols-2 gap-3">
        <div class="glass card-cyber rounded-[var(--r-md)] p-2.5 min-w-0">
          <div class="flex items-center justify-between mb-1">
            <span class="text-[12px] text-content">各分类卡片数量</span>
            <span class="text-[10px] text-content-sec">Top {{ cats.length }}</span>
          </div>
          <!-- 必须有明确高度，否则 echarts 渲染成 0 高 -->
          <div ref="barEl" class="h-[220px] w-full"></div>
        </div>

        <div class="glass card-cyber rounded-[var(--r-md)] p-2.5 min-w-0">
          <div class="flex items-center justify-between mb-1">
            <span class="text-[12px] text-content">实体类型占比</span>
            <span class="text-[10px] text-content-sec">{{ kinds.length }} 类</span>
          </div>
          <div ref="pieEl" class="h-[220px] w-full"></div>
        </div>
      </div>

      <!-- ③.5 系统状态（复用本轮已取数据，缺失显示 —） -->
      <div class="mt-3 grid grid-cols-2 md:grid-cols-4 gap-2">
        <div class="glass card-cyber rounded-[var(--r-md)] px-3 py-2.5 min-w-0">
          <div class="text-[11px] text-content-sec truncate">LLM 授权锁</div>
          <div class="mt-1 text-[16px] leading-none font-semibold truncate" :class="sysStatus.llmTone">
            {{ sysStatus.llm }}
          </div>
        </div>
        <div class="glass card-cyber rounded-[var(--r-md)] px-3 py-2.5 min-w-0">
          <div class="text-[11px] text-content-sec truncate">执行器</div>
          <div
            class="mt-1 text-[16px] leading-none font-semibold truncate"
            :class="sysStatus.execTone"
          >
            {{ sysStatus.exec }}
          </div>
        </div>
        <div class="glass card-cyber rounded-[var(--r-md)] px-3 py-2.5 min-w-0">
          <div class="text-[11px] text-content-sec truncate">数据库大小</div>
          <div
            class="mt-1 text-[16px] leading-none font-semibold truncate tabular-nums"
            :class="sysStatus.sizeTone"
          >
            {{ sysStatus.size }}
          </div>
        </div>
        <div class="glass card-cyber rounded-[var(--r-md)] px-3 py-2.5 min-w-0">
          <div class="text-[11px] text-content-sec truncate">库卡数</div>
          <div class="mt-1 text-[16px] leading-none font-semibold truncate tabular-nums text-content">
            {{ sysStatus.cards }}
          </div>
        </div>
      </div>

      <!-- ④ 最近 5 条任务状态（滚动列表，无内部 id） -->
      <div class="mt-3 mb-1 glass card-cyber rounded-[var(--r-md)] p-2.5">
        <div class="flex items-center justify-between mb-1.5">
          <span class="text-[12px] text-content">最近任务</span>
          <span class="text-[10px] text-content-sec">最新 {{ recent.length }} 条</span>
        </div>
        <ul class="max-h-[168px] overflow-y-auto">
          <li
            v-for="t in recent"
            :key="t.key"
            class="flex items-center gap-2 px-1 py-1.5 border-b border-line-divider last:border-0"
          >
            <span class="w-1.5 h-1.5 rounded-full shrink-0" :class="t.dot" aria-hidden="true"></span>
            <span class="flex-1 min-w-0 truncate text-[12px] text-content">{{ t.name }}</span>
            <span class="shrink-0 text-[10px] text-content-sec">{{ t.stateLabel }}</span>
            <span class="shrink-0 w-[74px] text-right text-[10px] text-content-sec tabular-nums">
              {{ t.time }}
            </span>
          </li>
          <li v-if="!recent.length && !loading" class="px-1 py-2 text-[11px] text-content-sec">
            暂无任务记录
          </li>
        </ul>
      </div>
    </div>
  </section>
</template>
