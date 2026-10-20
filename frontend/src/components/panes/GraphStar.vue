<!-- GraphStar.vue —— 实体关系星图（任务一，Canvas 自绘零依赖）
     参考 Op2 DeviceSpaceGraph{Layout,Painter}.dart：轨道布局 + CustomPaint + 命中检测。
     数据源：GET /api/graph/entity → {nodes:[{k,title,kind}], edges:[{s,d,t,st,c}], count}
     交互：点击节点 → 高亮邻接 + 底部详情条；空白点击 → 取消选中。
     性能：374 边规模 Canvas 2D 每帧重绘无压力；不装 ECharts/D3（省 2MB+ 依赖）。
-->
<script setup>
import { computed, onMounted, onBeforeUnmount, ref, watch } from 'vue'

const canvasEl = ref(null)
const wrapEl = ref(null)
const s = ref({
  loaded: false,
  err: '',
  nodes: [],
  edges: [],
  count: { nodes: 0, edges: 0 },
  selected: '',
  hover: '',
})

// ── 类型配色（赛博朋克控制台风：深底 + 霓虹描边）──
const KIND_COLORS = {
  CHAR: '#4fd1c5', LOCA: '#63b3ed', FACT: '#f6ad55', TECH: '#b794f4',
  CIVI: '#fc8181', CLTR: '#f687b3', POWR: '#f6e05e', EVNT: '#68d391',
  ARTS: '#9f7aea', TIME: '#4299e1', ITEM: '#ed8936', SYSX: '#38b2ac',
  WEIR: '#e53e3e', GEAR: '#dd6b20', FORE: '#a0aec0', XREA: '#ecc94b',
}
const kindColor = (k) => KIND_COLORS[k] || '#718096'

// ── 布局：同心轨道（中心 = 度数最高的节点，内圈=高度数，外圈=低度数）──
const state = { pos: new Map(), scale: 1, ox: 0, oy: 0, dragging: false, lx: 0, ly: 0, raf: 0 }

function layout(nodes, edges) {
  // 度数统计
  const deg = new Map()
  for (const n of nodes) deg.set(n.k, 0)
  for (const e of edges) {
    deg.set(e.s, (deg.get(e.s) || 0) + 1)
    deg.set(e.d, (deg.get(e.d) || 0) + 1)
  }
  // 按度数降序分 3 圈：中心 1 个 + 内圈 + 外圈
  const sorted = [...nodes].sort((a, b) => (deg.get(b.k) || 0) - (deg.get(a.k) || 0))
  const n = sorted.length
  const innerCount = Math.max(1, Math.round(n * 0.28))
  state.pos.clear()
  // 单位圆坐标（渲染时乘半径）：中心节点在原点
  sorted.forEach((nd, i) => {
    if (i === 0) { state.pos.set(nd.k, { x: 0, y: 0, r: 1.6, deg: deg.get(nd.k) || 0 }); return }
    const inner = i <= innerCount
    const ring = inner ? 0.34 : 1.0
    const cnt = inner ? innerCount : n - 1 - innerCount
    const idx = inner ? i - 1 : i - 1 - innerCount
    // 黄金角散布避免重叠
    const ang = idx * 2.399963 + (inner ? 0 : 0.7)
    state.pos.set(nd.k, {
      x: Math.cos(ang) * ring, y: Math.sin(ang) * ring,
      r: inner ? 1.0 : 0.72, deg: deg.get(nd.k) || 0,
    })
  })
}

// ── 邻接表（选中高亮用）──
const adj = computed(() => {
  const m = new Map()
  for (const e of s.value.edges) {
    if (!m.has(e.s)) m.set(e.s, [])
    if (!m.has(e.d)) m.set(e.d, [])
    m.get(e.s).push(e)
    m.get(e.d).push(e)
  }
  return m
})

const selInfo = computed(() => {
  if (!s.value.selected) return null
  const nd = s.value.nodes.find((n) => n.k === s.value.selected)
  if (!nd) return null
  const links = adj.value.get(s.value.selected) || []
  return { ...nd, links: links.length }
})

// ── 渲染 ──
function draw() {
  const cv = canvasEl.value
  if (!cv) return
  const dpr = window.devicePixelRatio || 1
  const w = cv.clientWidth, h = cv.clientHeight
  if (cv.width !== w * dpr) { cv.width = w * dpr; cv.height = h * dpr }
  const ctx = cv.getContext('2d')
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0)
  ctx.clearRect(0, 0, w, h)

  const R = Math.min(w, h) * 0.42 * state.scale
  const cx = w / 2 + state.ox, cy = h / 2 + state.oy
  const sel = s.value.selected
  const P = (k) => {
    const p = state.pos.get(k)
    return p ? { x: cx + p.x * R, y: cy + p.y * R, r: p.r * R * 0.045 + 3 } : null
  }

  // 背景：微网格（Op2 _drawGrid 思想）
  ctx.strokeStyle = 'rgba(120,140,180,0.06)'
  ctx.lineWidth = 1
  for (let gx = 0; gx < w; gx += 40) { ctx.beginPath(); ctx.moveTo(gx, 0); ctx.lineTo(gx, h); ctx.stroke() }
  for (let gy = 0; gy < h; gy += 40) { ctx.beginPath(); ctx.moveTo(0, gy); ctx.lineTo(w, gy); ctx.stroke() }
  // 外圈轨道线
  ctx.strokeStyle = 'rgba(120,140,180,0.12)'
  ctx.beginPath(); ctx.arc(cx, cy, R, 0, Math.PI * 2); ctx.stroke()
  ctx.beginPath(); ctx.arc(cx, cy, R * 0.34, 0, Math.PI * 2); ctx.stroke()

  // 边：选中态只画邻接边（高亮），其余淡出
  for (const e of s.value.edges) {
    const a = P(e.s), b = P(e.d)
    if (!a || !b) continue
    const linked = sel && (e.s === sel || e.d === sel)
    ctx.strokeStyle = linked ? 'rgba(79,209,197,0.85)' : sel ? 'rgba(120,140,180,0.07)' : 'rgba(120,140,180,0.25)'
    ctx.lineWidth = linked ? 1.6 : 1
    ctx.beginPath(); ctx.moveTo(a.x, a.y); ctx.lineTo(b.x, b.y); ctx.stroke()
  }

  // 节点
  for (const nd of s.value.nodes) {
    const p = P(nd.k)
    if (!p) continue
    const isSel = nd.k === sel
    const isAdj = sel && (adj.value.get(sel) || []).some((e) => e.s === nd.k || e.d === nd.k)
    const dim = sel && !isSel && !isAdj
    ctx.globalAlpha = dim ? 0.18 : 1
    const col = kindColor(nd.kind)
    // 光晕（中心节点/选中节点更大）
    const glow = isSel ? p.r * 2.2 : p.r * 1.4
    const g = ctx.createRadialGradient(p.x, p.y, 0, p.x, p.y, glow)
    g.addColorStop(0, col + '66'); g.addColorStop(1, col + '00')
    ctx.fillStyle = g
    ctx.beginPath(); ctx.arc(p.x, p.y, glow, 0, Math.PI * 2); ctx.fill()
    // 本体
    ctx.fillStyle = col
    ctx.beginPath(); ctx.arc(p.x, p.y, isSel ? p.r * 1.5 : p.r, 0, Math.PI * 2); ctx.fill()
    // 高度数节点挂标签（防重叠：只挂度数前 40）
    if ((state.pos.get(nd.k)?.deg || 0) >= 4 || isSel) {
      ctx.fillStyle = isSel ? '#fff' : 'rgba(220,230,245,0.75)'
      ctx.font = `${isSel ? 11 : 9}px ui-monospace,monospace`
      ctx.fillText(nd.title.slice(0, 12), p.x + p.r + 3, p.y + 3)
    }
    ctx.globalAlpha = 1
  }
}

function loop() { draw(); state.raf = requestAnimationFrame(loop) }

// ── 命中检测（点击选节点 / 空白取消）──
function hit(x, y) {
  const cv = canvasEl.value
  const R = Math.min(cv.clientWidth, cv.clientHeight) * 0.42 * state.scale
  const cx = cv.clientWidth / 2 + state.ox, cy = cv.clientHeight / 2 + state.oy
  let best = null, bd = 1e9
  for (const nd of s.value.nodes) {
    const p = state.pos.get(nd.k)
    if (!p) continue
    const px = cx + p.x * R, py = cy + p.y * R
    const d = Math.hypot(x - px, y - py)
    const rad = p.r * R * 0.045 + 10
    if (d < rad && d < bd) { bd = d; best = nd.k }
  }
  return best
}
function onTap(ev) {
  const cv = canvasEl.value
  const rect = cv.getBoundingClientRect()
  const k = hit(ev.clientX - rect.left, ev.clientY - rect.top)
  s.value.selected = k === s.value.selected ? '' : (k || '')
}
function onWheel(ev) {
  ev.preventDefault()
  state.scale = Math.min(3, Math.max(0.4, state.scale * (ev.deltaY < 0 ? 1.12 : 0.89)))
}
function onDown(ev) { state.dragging = true; state.lx = ev.clientX; state.ly = ev.clientY }
function onMove(ev) {
  if (!state.dragging) return
  state.ox += ev.clientX - state.lx; state.oy += ev.clientY - state.ly
  state.lx = ev.clientX; state.ly = ev.clientY
}
function onUp() { state.dragging = false }

// ── 数据加载 ──
async function load() {
  try {
    const r = await fetch('/api/graph/entity')
    const j = await r.json()
    if (j.error) { s.value.err = j.error; return }
    s.value.nodes = j.nodes || []
    s.value.edges = j.edges || []
    s.value.count = j.count || {}
    layout(s.value.nodes, s.value.edges)
    s.value.loaded = true
  } catch (e) {
    s.value.err = String(e)
  }
}

onMounted(() => {
  load()
  loop()
  const el = wrapEl.value
  el.addEventListener('wheel', onWheel, { passive: false })
  window.addEventListener('mouseup', onUp)
})
onBeforeUnmount(() => {
  cancelAnimationFrame(state.raf)
  window.removeEventListener('mouseup', onUp)
})
</script>

<template>
  <div ref="wrapEl" class="relative h-full w-full select-none">
    <canvas
      ref="canvasEl"
      class="h-full w-full cursor-crosshair"
      @click="onTap"
      @mousedown="onDown"
      @mousemove="onMove"
    />
    <!-- 顶部统计胶囊 -->
    <div class="absolute left-2 top-2 flex gap-2 text-[10px] font-mono text-fg-dim">
      <span class="glass rounded px-2 py-0.5">节点 {{ s.count.nodes }}</span>
      <span class="glass rounded px-2 py-0.5">边 {{ s.count.edges }}</span>
      <span class="glass rounded px-2 py-0.5 opacity-60">滚轮缩放 · 拖拽平移</span>
    </div>
    <!-- 选中详情条 -->
    <div
      v-if="selInfo"
      class="glass absolute bottom-2 left-2 right-2 rounded-md px-3 py-2"
    >
      <div class="flex items-center gap-2">
        <span
          class="h-2.5 w-2.5 rounded-full"
          :style="{ background: kindColor(selInfo.kind) }"
        />
        <span class="text-[12.5px] text-fg">{{ selInfo.title }}</span>
        <span class="text-[10px] font-mono text-fg-dim">{{ selInfo.kind }} · {{ selInfo.links }} 条关系</span>
        <button class="ml-auto text-[11px] text-fg-dim" @click="s.selected = ''">✕</button>
      </div>
    </div>
    <!-- 空态 / 错误 -->
    <div v-if="s.err" class="absolute inset-0 grid place-items-center text-[12px] text-bad">{{ s.err }}</div>
    <div
      v-else-if="s.loaded && !s.nodes.length"
      class="absolute inset-0 grid place-items-center text-[12px] text-fg-dim"
    >暂无实体关系数据</div>
  </div>
</template>
