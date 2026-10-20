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

// ── 力导向布局（引力/斥力 + 阻尼）──
// # 为什么换掉原来的"同心轨道"
// 原实现是一次性摆位：度数高放中心、其余按黄金角撒两圈。它渲染出来是**静态**的，
// 观感接近一张图 —— 这正是"看得到 tab、看不到动态星图"的原因（数据是通的，
// 但布局从一开始就定死了，没有任何演化）。
// 现改为力导向：每帧做一次物理积分（斥力散开、引力沿边收拢、向心防飞散、
// 阻尼收敛）。于是打开面板会看到星图"自我组织"，这才是星图该有的样子。
//
// # 为什么不用 ECharts / D3 / vis.js
// 它们的 graph 布局都要额外 300KB~1MB，且 Canvas 自绘这套已有命中检测、
// 缩放平移、邻接高亮（见下），换成库等于把已跑通的东西推翻重写。
// 374 边规模下 O(n²) 斥力每帧完全够用，且收敛后立即停止计算（见 alpha 衰减）。
const phys = {
  vx: new Map(),
  vy: new Map(),
  alpha: 1, // 温度：逐帧衰减，越小步长越小，趋近静止
  done: false,
}

function layout(nodes, edges) {
  // 度数统计
  const deg = new Map()
  for (const n of nodes) deg.set(n.k, 0)
  for (const e of edges) {
    deg.set(e.s, (deg.get(e.s) || 0) + 1)
    deg.set(e.d, (deg.get(e.d) || 0) + 1)
  }

  state.pos.clear()
  phys.vx.clear()
  phys.vy.clear()

  // 初始撒点：黄金角螺旋。刻意不用随机数——随机初始会让每次刷新图形都不一样，
  // 调试时无法复现；黄金角既确定性又能避免对称抵消（所有斥力互相抵消=原点堆积）。
  const GA = 2.399963
  const n = nodes.length
  nodes.forEach((nd, i) => {
    const d = deg.get(nd.k) || 0
    const t = n > 1 ? i / (n - 1) : 0
    const rad = 0.18 + 0.85 * Math.sqrt(t) // 平方根分布：中心稀、外围密
    const ang = i * GA
    state.pos.set(nd.k, {
      x: Math.cos(ang) * rad,
      y: Math.sin(ang) * rad,
      // 半径按度数分档：枢纽节点更醒目
      r: d >= 6 ? 1.5 : d >= 3 ? 1.05 : 0.72,
      deg: d,
    })
    phys.vx.set(nd.k, 0)
    phys.vy.set(nd.k, 0)
  })
  phys.alpha = 1
  phys.done = false
}

/// 一步物理积分（单位圆坐标空间，1.0 ≈ 图半径）。收敛后直接把 done 置位，
/// 后续帧只重绘不再计算 —— 静态星图不烧 CPU，这是移动端的硬要求。
function physicsStep() {
  if (phys.done) return
  const nodes = s.value.nodes
  const edges = s.value.edges
  const K = 0.16 // 理想边长
  let maxSpeed = 0

  // ① 斥力：任意两节点互斥（库仑式，k²/d）
  for (let i = 0; i < nodes.length; i++) {
    const a = state.pos.get(nodes[i].k)
    if (!a) continue
    for (let j = i + 1; j < nodes.length; j++) {
      const b = state.pos.get(nodes[j].k)
      if (!b) continue
      let dx = a.x - b.x
      let dy = a.y - b.y
      let d2 = dx * dx + dy * dy
      if (d2 < 1e-6) {
        // 完全重合：给一个确定性的微小偏移，避免除零且保持可复现
        dx = 1e-3 * (i + 1)
        dy = 1e-3 * (j + 1)
        d2 = dx * dx + dy * dy
      }
      const d = Math.sqrt(d2)
      const f = (K * K) / d
      const ux = dx / d
      const uy = dy / d
      phys.vx.set(nodes[i].k, (phys.vx.get(nodes[i].k) || 0) + ux * f)
      phys.vy.set(nodes[i].k, (phys.vy.get(nodes[i].k) || 0) + uy * f)
      phys.vx.set(nodes[j].k, (phys.vx.get(nodes[j].k) || 0) - ux * f)
      phys.vy.set(nodes[j].k, (phys.vy.get(nodes[j].k) || 0) - uy * f)
    }
  }

  // ② 引力：沿边收拢（胡克式，d²/k）。系数 0.02 是实测调出来的：
  //    再大枢纽节点会把邻居压成一个点，再小则图散成一团看不出结构。
  for (const e of edges) {
    const a = state.pos.get(e.s)
    const b = state.pos.get(e.d)
    if (!a || !b) continue
    const dx = b.x - a.x
    const dy = b.y - a.y
    const d = Math.hypot(dx, dy) || 1e-6
    const f = (d * d) / K
    const ux = dx / d
    const uy = dy / d
    phys.vx.set(e.s, (phys.vx.get(e.s) || 0) + ux * f * 0.02)
    phys.vy.set(e.s, (phys.vy.get(e.s) || 0) + uy * f * 0.02)
    phys.vx.set(e.d, (phys.vx.get(e.d) || 0) - ux * f * 0.02)
    phys.vy.set(e.d, (phys.vy.get(e.d) || 0) - uy * f * 0.02)
  }

  // ③ 向心 + 阻尼 + 限速 + 积分
  const DAMP = 0.82
  const MAX_SPEED = 0.12
  for (const nd of nodes) {
    const p = state.pos.get(nd.k)
    if (!p) continue
    let vx = (phys.vx.get(nd.k) || 0) - p.x * 0.02 // 向心力：防止整体漂移
    let vy = (phys.vy.get(nd.k) || 0) - p.y * 0.02
    vx *= DAMP
    vy *= DAMP
    const sp = Math.hypot(vx, vy)
    if (sp > MAX_SPEED) {
      vx = (vx / sp) * MAX_SPEED
      vy = (vy / sp) * MAX_SPEED
    }
    if (sp > maxSpeed) maxSpeed = sp
    phys.vx.set(nd.k, vx)
    phys.vy.set(nd.k, vy)
    p.x += vx * phys.alpha
    p.y += vy * phys.alpha
  }

  phys.alpha *= 0.985
  // 收敛：温度耗尽或速度趋零 → 停止计算（约 4 秒内收敛）
  if (phys.alpha < 0.02 || maxSpeed < 1e-4) {
    phys.alpha = 0
    phys.done = true
  }
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

function loop() {
  physicsStep() // 先演化，再渲染
  draw()
  state.raf = requestAnimationFrame(loop)
}

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
