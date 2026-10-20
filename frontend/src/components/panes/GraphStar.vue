<!-- GraphStar.vue —— 实体关系星图 v2（Canvas 2D 自绘 + 力导向，零图库依赖）
     数据源：GET /api/graph/entity
       → { nodes:[{k,title,kind}], edges:[{s,d,t,st,c}], count:{nodes,edges} }
       后端实现：crates/ne-agent/src/governance/rel_graph.rs::entity_graph
         · kind  = key 前缀（CHAR-99-001 → CHAR）
         · t     = entity_edge.edge_type
         · st    = entity_edge.state（默认 'stable'）
         · c     = entity_edge.conf（默认 0.7）
       全部字段均由既有端点提供，邻接列表从 edges 自行推导，未臆造后端字段。

     布局（2026-10-20 二次改造）：
       · **全屏画布**：根节点不再套卡片（没有 glass / card-cyber 外壳），
         canvas 直接 absolute inset-0 + h-full w-full 占满主内容区
         （父级 SystemView 是 h-full flex-col + flex-1 min-h-0 的确定高度容器）。
         canvas 会按 clientWidth/clientHeight 生成位图，高度为 0 就什么都画不出来，
         故 metrics() 做了「canvas → 容器 → 放弃本帧」三级高度兜底，
         并在挂载后 ensureSized() 重试若干帧等布局稳定。
       · **左侧悬浮图例面板**：默认展开，可收起为左缘把手（只动 transform/opacity）。
         每行 = 颜色点（点一下=只看该类）+ 勾选框 + 类型名 + 数量，勾选控制显隐；
         顶部保留「全部 N」「核心 ≥3 / 全量」两个原有开关。
         图例占左侧，故图心按图例宽度右移（metrics().cx），展开/收起都会重绘。

     交互（DESIGN-v2.md §5④ 关系星图验收点）：
       ① 点击节点 → 右侧滑出实体详情面板（类型/编号/关系数 + 入边出边邻接列表，
          邻接行显示边类型 t / 状态 st / 置信度 c；点邻接行可跳转聚焦）
       ② 左侧图例面板：按 kind 勾选过滤（selKinds），过滤后**重新布局**并重启物理收敛
       ③ 「重新布局」按钮：重置 alpha + 重新初始化坐标（螺旋相位轮转，保证可复现）
       ④ 物理收敛后 done=true，彻底停止积分；且 rAF 不再空转（DESIGN-v2 §3 硬约束）
       ⑤ 保留原有：滚轮缩放、拖拽平移、点击邻接高亮
          —— 原「底部详情条」与新面板信息重复，按需求合并进右侧面板（不再单独保留）
       ⑥ 性能：斥力 O(n²) 只在未收敛时运行（n≤284 → 约 4 万对/帧），
          收敛后仅按需重绘；节点位置存在普通 Map（非响应式代理）里，避免每帧 proxy 开销。

     Canvas 配色说明：节点本体必须按 kind 上色（类型身份色，跨皮肤稳定，同 §5② 资料库
     「Badge 颜色按分类稳定分配」的语义），因此保留 kind 常量色板；
     网格/轨道/连线/文字这类中性元素一律从 CSS 变量（--text-sec/--primary/--text）解析，
     并监听 html[data-skin] 变化重建，切换皮肤即时生效。
-->
<script setup>
import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue'

const canvasEl = ref(null)
const wrapEl = ref(null)
const panelEl = ref(null)
const refsExpanded = ref(false)
// 左侧图例面板：默认展开，可收起为左缘把手（收起只影响 transform/opacity + 图心偏移）
const legendOpen = ref(true)

const s = ref({
  loaded: false,
  err: '',
  nodes: [],
  edges: [],
  count: { nodes: 0, edges: 0 },
  selected: '',
  hover: '',
})

// 筛选：空数组 = 不筛（全部类型）
const selKinds = ref([])
// 核心节点过滤（2026-10-20）：总指挥在手机窄屏验收时指出「284 个点+369 条线
// 像一团乱线、没有重点」。故默认只看**度数≥3 的枢纽节点**（骨架），
// 需要全量时点「全部节点」。
// 度数按**全量图**统计（不按过滤后的子图），否则筛掉大类后会误判核心。
const CORE_DEG = 3
const coreOnly = ref(true)
const vstat = ref({ nodes: 0, edges: 0 })
const physDone = ref(true)
const progress = ref(100)

// 非响应式容器：物理积分与绘制每帧读它，放进 ref 会产生 proxy 开销
const view = { nodes: [], edges: [] }

const state = {
  pos: new Map(), // k → { x, y, r, deg, label }
  scale: 1,
  ox: 0,
  oy: 0,
  dragging: false,
  moved: false,
  lx: 0,
  ly: 0,
  raf: 0,
  pinch: null,
}
const phys = {
  vx: new Map(),
  vy: new Map(),
  keys: [],
  alpha: 1,
  done: true,
  steps: 0,
  seed: 0, // 重新布局时轮转螺旋相位：确定性但每次不同
}

// ── 类型色板（kind 身份色，跨皮肤稳定）──
const KIND_COLORS = {
  CHAR: '#4fd1c5', LOCA: '#63b3ed', FACT: '#f6ad55', TECH: '#b794f4',
  CIVI: '#fc8181', CLTR: '#f687b3', POWR: '#f6e05e', EVNT: '#68d391',
  ARTS: '#9f7aea', TIME: '#4299e1', ITEM: '#ed8936', SYSX: '#38b2ac',
  WEIR: '#e53e3e', GEAR: '#dd6b20', FORE: '#a0aec0', XREA: '#ecc94b',
}
// 未知 kind 的回退色：按 key 哈希取稳定色（刷新/切页不变色）
const FALLBACK_KIND = ['#8ecae6', '#ffb703', '#fb8500', '#a3b18a', '#c77dff', '#84a98c', '#e07a5f', '#5e9bd6']
function kindHash(k) {
  let h = 0
  for (let i = 0; i < k.length; i++) h = (h * 31 + k.charCodeAt(i)) % 9973
  return h
}
const kindColor = (k) => KIND_COLORS[k] || FALLBACK_KIND[kindHash(String(k)) % FALLBACK_KIND.length]

// 状态色（st）：stable 之外都值得用户注意
const stClass = (st) => (st && st !== 'stable' ? 'text-warn' : 'text-fg-dim')

// ── 中性色从 CSS 变量解析，保证多皮肤一致 ──
const theme = { text: '#e6e8f0', textSec: '#8c8fa6', primary: '#4f8cff' }
function cssVar(name, fb) {
  try {
    const v = getComputedStyle(document.documentElement).getPropertyValue(name).trim()
    return v || fb
  } catch (e) {
    return fb
  }
}
function toRgba(c, a) {
  const col = String(c || '').trim()
  if (col.startsWith('#')) {
    let h = col.slice(1)
    if (h.length === 3) h = h.split('').map((x) => x + x).join('')
    if (h.length === 8) h = h.slice(0, 6)
    const n = parseInt(h, 16)
    if (!Number.isNaN(n)) return `rgba(${(n >> 16) & 255},${(n >> 8) & 255},${n & 255},${a})`
  }
  const m = col.match(/rgba?\(([^)]+)\)/)
  if (m) {
    const p = m[1].split(/[,\s/]+/).filter(Boolean).map(Number)
    if (p.length >= 3) return `rgba(${p[0] | 0},${p[1] | 0},${p[2] | 0},${a})`
  }
  return `rgba(140,143,166,${a})`
}
function refreshTheme() {
  theme.text = cssVar('--text', '#e6e8f0')
  theme.textSec = cssVar('--text-sec', '#8c8fa6')
  theme.primary = cssVar('--primary', '#4f8cff')
}
let skinMo = null

// ── 筛选栏数据 ──
const kinds = computed(() => {
  const m = new Map()
  for (const n of s.value.nodes) m.set(n.kind, (m.get(n.kind) || 0) + 1)
  return [...m.entries()]
    .map(([k, n]) => ({ k, n, color: kindColor(k) }))
    .sort((a, b) => b.n - a.n || a.k.localeCompare(b.k))
})
function toggleKind(k) {
  const cur = selKinds.value
  if (!cur.length) {
    selKinds.value = [k] // 从「全部」点某一类 → 直接单类过滤
  } else {
    const next = cur.includes(k) ? cur.filter((x) => x !== k) : [...cur, k]
    selKinds.value = next // 清空自动回落为「全部」
  }
}
function clearKinds() {
  selKinds.value = []
}

// ── 左侧图例的勾选语义（不改变 selKinds 的既有含义：空数组 = 全部）──
/// 某类型当前是否显示（「全部」态下所有类型都显示）。
const isKindOn = (k) => !selKinds.value.length || selKinds.value.includes(k)
/// 勾选/取消勾选某个类型：
///   · 「全部」态下取消一个 → 显式勾选其余所有类型（即“隐藏这一类”）
///   · 非「全部」态下：勾选加入、取消移除；移除到空则回落「全部」（沿用既有语义）
function toggleKindCheck(k) {
  const all = kinds.value.map((x) => x.k)
  const cur = selKinds.value
  let next
  if (!cur.length) next = all.filter((x) => x !== k)
  else next = cur.includes(k) ? cur.filter((x) => x !== k) : [...cur, k]
  selKinds.value = next.length ? next : []
}

/// 切换「只看核心节点」。切换会重新布局并重启收敛（与 kind 筛选同路径）。
function toggleCoreOnly() {
  coreOnly.value = !coreOnly.value
  if (s.value.loaded) rebuildView(true)
}

// ── 视图重建：筛选后在子图上重新布局（需求②）──
/// 全量图上的度数（用于核心节点判定）。每次 rebuild 重算，规模 284 可忽略。
function degreeMap() {
  const d = new Map()
  for (const n of s.value.nodes) d.set(n.k, 0)
  for (const e of s.value.edges) {
    if (d.has(e.s)) d.set(e.s, d.get(e.s) + 1)
    if (d.has(e.d)) d.set(e.d, d.get(e.d) + 1)
  }
  return d
}

function rebuildView(relayout) {
  const keep = selKinds.value.length ? new Set(selKinds.value) : null
  const deg = coreOnly.value ? degreeMap() : null
  const pass = (n) => (!keep || keep.has(n.kind)) && (!deg || (deg.get(n.k) || 0) >= CORE_DEG)
  view.nodes = s.value.nodes.filter(pass)
  const visK = new Set(view.nodes.map((n) => n.k))
  view.edges = s.value.edges.filter((e) => visK.has(e.s) && visK.has(e.d))
  vstat.value = { nodes: view.nodes.length, edges: view.edges.length }
  if (s.value.selected && !visK.has(s.value.selected)) s.value.selected = ''
  if (s.value.hover && !visK.has(s.value.hover)) s.value.hover = ''
  if (relayout) layout()
  else requestDraw()
}

// ── 力导向布局 ──
// # 为什么用黄金角螺旋而不是随机撒点
// 随机会让每次刷新图形都不一样，调试时不可复现；黄金角既确定性又能打散对称
// （对称初始位形下斥力互相抵消 = 全部堆在原点）。
// # 为什么保留力导向
// 一次性摆位渲染出来是静态图，看不到结构演化；力导向让枢纽自然占据中心。
function layout() {
  const nodes = view.nodes
  const edges = view.edges

  const deg = new Map()
  for (const n of nodes) deg.set(n.k, 0)
  for (const e of edges) {
    deg.set(e.s, (deg.get(e.s) || 0) + 1)
    deg.set(e.d, (deg.get(e.d) || 0) + 1)
  }

  state.pos.clear()
  phys.vx.clear()
  phys.vy.clear()

  const GA = 2.399963
  const phase = phys.seed * 0.7
  const n = nodes.length
  phys.keys = []
  nodes.forEach((nd, i) => {
    const d = deg.get(nd.k) || 0
    const t = n > 1 ? i / (n - 1) : 0
    const rad = 0.18 + 0.85 * Math.sqrt(t)
    const ang = i * GA + phase
    state.pos.set(nd.k, {
      x: Math.cos(ang) * rad,
      y: Math.sin(ang) * rad,
      r: d >= 6 ? 1.5 : d >= 3 ? 1.05 : 0.72, // 枢纽更醒目
      deg: d,
      label: d >= 4, // 只给高度数节点挂标签，防 284 个文字糊成一团
    })
    phys.vx.set(nd.k, 0)
    phys.vy.set(nd.k, 0)
    phys.keys.push(nd.k)
  })

  phys.alpha = 1
  phys.done = false
  phys.steps = 0
  physDone.value = false
  progress.value = 0
  requestDraw()
}

function relayout() {
  phys.seed = (phys.seed + 1) % 9
  layout()
}

const MAX_STEPS = 900 // 兜底上限：极端拓扑下也保证一定收敛停机

/// 一步物理积分（单位圆坐标空间，1.0 ≈ 图半径）。
/// 收敛后 done=true，后续只重绘不算 —— 移动端硬要求（DESIGN-v2 §3）。
function physicsStep() {
  if (phys.done) return
  const nodes = view.nodes
  const edges = view.edges
  const keys = phys.keys
  const pos = state.pos
  const vx = phys.vx
  const vy = phys.vy
  const K = 0.16 // 理想边长
  let maxSpeed = 0

  // ① 斥力：任意两节点互斥（库仑式 k²/d）
  for (let i = 0; i < keys.length; i++) {
    const a = pos.get(keys[i])
    if (!a) continue
    const ax = a.x, ay = a.y
    for (let j = i + 1; j < keys.length; j++) {
      const b = pos.get(keys[j])
      if (!b) continue
      let dx = ax - b.x
      let dy = ay - b.y
      let d2 = dx * dx + dy * dy
      if (d2 < 1e-6) {
        // 完全重合：确定性微扰，避免除零且保持可复现
        dx = 1e-3 * (i + 1)
        dy = 1e-3 * (j + 1)
        d2 = dx * dx + dy * dy
      }
      const d = Math.sqrt(d2)
      const f = (K * K) / d
      const ux = dx / d
      const uy = dy / d
      vx.set(keys[i], (vx.get(keys[i]) || 0) + ux * f)
      vy.set(keys[i], (vy.get(keys[i]) || 0) + uy * f)
      vx.set(keys[j], (vx.get(keys[j]) || 0) - ux * f)
      vy.set(keys[j], (vy.get(keys[j]) || 0) - uy * f)
    }
  }

  // ② 引力：沿边收拢（胡克式 d²/k）。0.02 为实测值：
  //    更大则枢纽把邻居压成一点，更小则整图散开看不出结构。
  for (const e of edges) {
    const a = pos.get(e.s)
    const b = pos.get(e.d)
    if (!a || !b) continue
    const dx = b.x - a.x
    const dy = b.y - a.y
    const d = Math.hypot(dx, dy) || 1e-6
    const f = ((d * d) / K) * 0.02
    const ux = dx / d
    const uy = dy / d
    vx.set(e.s, (vx.get(e.s) || 0) + ux * f)
    vy.set(e.s, (vy.get(e.s) || 0) + uy * f)
    vx.set(e.d, (vx.get(e.d) || 0) - ux * f)
    vy.set(e.d, (vy.get(e.d) || 0) - uy * f)
  }

  // ③ 向心 + 阻尼 + 限速 + 积分
  const DAMP = 0.82
  const MAX_SPEED = 0.12
  for (const nd of nodes) {
    const p = pos.get(nd.k)
    if (!p) continue
    let vxi = (vx.get(nd.k) || 0) - p.x * 0.02
    let vyi = (vy.get(nd.k) || 0) - p.y * 0.02
    vxi *= DAMP
    vyi *= DAMP
    const sp = Math.hypot(vxi, vyi)
    if (sp > MAX_SPEED) {
      vxi = (vxi / sp) * MAX_SPEED
      vyi = (vyi / sp) * MAX_SPEED
    }
    if (sp > maxSpeed) maxSpeed = sp
    vx.set(nd.k, vxi)
    vy.set(nd.k, vyi)
    p.x += vxi * phys.alpha
    p.y += vyi * phys.alpha
  }

  phys.steps++
  phys.alpha *= 0.985
  // 收敛：温度耗尽 / 速度趋零 / 步数兜底 → 停机
  if (phys.alpha < 0.02 || maxSpeed < 1e-4 || phys.steps > MAX_STEPS) {
    phys.alpha = 0
    phys.done = true
  }
}

// ── 渲染调度：只在需要时 rAF，绝不空转 ──
function requestDraw() {
  if (state.raf) return
  state.raf = requestAnimationFrame(tick)
}
function tick() {
  state.raf = 0
  if (!phys.done) {
    physicsStep()
    physDone.value = phys.done
    progress.value = Math.min(100, Math.round((1 - phys.alpha) * 100))
    requestDraw() // 演化中 → 继续排帧
  }
  draw()
}

/// 高度兜底：个别挂载时序下首帧 clientHeight 仍为 0（父级布局未落定），
/// 这里在若干帧内轮询重试重绘，避免画布停在空白（本仓踩过 0 高的坑）。
function ensureSized() {
  let tries = 0
  const step = () => {
    const cv = canvasEl.value
    if (!cv) return
    if (cv.clientHeight > 0 || tries++ > 12) {
      requestDraw()
      return
    }
    requestAnimationFrame(step)
  }
  step()
}

// ── 邻接表（选中高亮 + 详情面板）──
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

const titleMap = computed(() => {
  const m = new Map()
  for (const n of s.value.nodes) m.set(n.k, n)
  return m
})

// 详情面板数据：只用后端既有字段（k/title/kind + edge 的 t/st/c），邻接自 edges 推导
const detail = computed(() => {
  const k = s.value.selected
  if (!k) return null
  const nd = titleMap.value.get(k)
  if (!nd) return null
  const rows = []
  let outs = 0
  let ins = 0
  for (const e of s.value.edges) {
    const isOut = e.s === k
    const isIn = e.d === k
    if (!isOut && !isIn) continue
    if (isOut) outs++
    if (isIn) ins++
    const ok = isOut ? e.d : e.s
    const other = titleMap.value.get(ok)
    rows.push({
      key: ok,
      title: other ? other.title : ok,
      kind: other ? other.kind : String(ok).split('-')[0] || '?',
      dir: isOut && isIn ? '↔' : isOut ? '→' : '←',
      t: e.t || '',
      st: e.st || 'stable',
      c: typeof e.c === 'number' ? e.c : null,
    })
  }
  rows.sort((a, b) => a.kind.localeCompare(b.kind) || a.title.localeCompare(b.title) || a.key.localeCompare(b.key))
  return { ...nd, deg: rows.length, outs, ins, rows }
})
const shownRows = computed(() => {
  if (!detail.value) return []
  return refsExpanded.value ? detail.value.rows : detail.value.rows.slice(0, 24)
})

function focusNode(k) {
  if (!k) return
  // 邻接节点可能被当前筛选隐藏 → 自动回到「全部」再选中，保证一定可见
  if (!view.nodes.some((n) => n.k === k)) selKinds.value = []
  s.value.selected = k
  refsExpanded.value = false
}
function closePanel() {
  s.value.selected = ''
}

// ── 绘制 ──
// 画布坐标系：左侧图例面板默认展开，图心按图例宽度右移让位；
// 顶部留 PAD_T 给右上状态胶囊（≤30px 高，DESIGN §4）。
// 高度来源：canvas 的 clientHeight → 容器 clientHeight → 放弃本帧（避免 0 高位图）。
const PAD_T = 34
function metrics() {
  const cv = canvasEl.value
  const wrap = wrapEl.value
  const w = (cv && cv.clientWidth) || (wrap && wrap.clientWidth) || 0
  const h = (cv && cv.clientHeight) || (wrap && wrap.clientHeight) || 0
  const padL = legendOpen.value ? Math.min(Math.max(w * 0.5, 120), 208) : 0
  const usableW = Math.max(1, w - padL)
  const usableH = Math.max(1, h - PAD_T)
  const R = Math.min(usableW, usableH) * 0.42 * state.scale
  return {
    w,
    h,
    R,
    cx: padL + usableW / 2 + state.ox,
    cy: PAD_T + usableH / 2 + state.oy,
  }
}
function draw() {
  const cv = canvasEl.value
  if (!cv) return
  const m = metrics()
  const w = m.w
  const h = m.h
  if (!w || !h) return // 容器高度为 0：本帧不画，等 ResizeObserver / ensureSized 重试
  const dpr = window.devicePixelRatio || 1
  if (cv.width !== Math.round(w * dpr) || cv.height !== Math.round(h * dpr)) {
    cv.width = Math.round(w * dpr)
    cv.height = Math.round(h * dpr)
  }
  const ctx = cv.getContext('2d')
  if (!ctx) return
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0)
  ctx.clearRect(0, 0, w, h)

  const R = m.R
  const cx = m.cx
  const cy = m.cy
  const sel = s.value.selected
  const hov = s.value.hover
  const adjSel = sel ? adj.value.get(sel) || [] : []
  const nbr = new Set()
  for (const e of adjSel) {
    nbr.add(e.s)
    nbr.add(e.d)
  }
  const P = (k) => {
    const p = state.pos.get(k)
    return p ? { x: cx + p.x * R, y: cy + p.y * R, r: p.r * R * 0.045 + 3 } : null
  }

  // 背景微网格（中性色取自皮肤变量）
  ctx.strokeStyle = toRgba(theme.textSec, 0.1)
  ctx.lineWidth = 1
  for (let gx = 0; gx < w; gx += 40) {
    ctx.beginPath()
    ctx.moveTo(gx, 0)
    ctx.lineTo(gx, h)
    ctx.stroke()
  }
  for (let gy = 0; gy < h; gy += 40) {
    ctx.beginPath()
    ctx.moveTo(0, gy)
    ctx.lineTo(w, gy)
    ctx.stroke()
  }
  // 外圈轨道线
  if (R > 4) {
    ctx.strokeStyle = toRgba(theme.textSec, 0.18)
    ctx.beginPath()
    ctx.arc(cx, cy, R, 0, Math.PI * 2)
    ctx.stroke()
    ctx.beginPath()
    ctx.arc(cx, cy, R * 0.34, 0, Math.PI * 2)
    ctx.stroke()
  }

  // 边：选中态只高亮邻接边，其余淡出
  for (const e of view.edges) {
    const a = P(e.s)
    const b = P(e.d)
    if (!a || !b) continue
    const linked = !!sel && (e.s === sel || e.d === sel)
    ctx.strokeStyle = linked
      ? toRgba(theme.primary, 0.85)
      : toRgba(theme.textSec, sel ? 0.08 : 0.3)
    ctx.lineWidth = linked ? 1.6 : 1
    ctx.beginPath()
    ctx.moveTo(a.x, a.y)
    ctx.lineTo(b.x, b.y)
    ctx.stroke()
  }

  // 节点
  for (const nd of view.nodes) {
    const p = P(nd.k)
    if (!p) continue
    const isSel = nd.k === sel
    const isAdj = !!sel && nbr.has(nd.k)
    const isHover = nd.k === hov
    const dim = !!sel && !isSel && !isAdj
    ctx.globalAlpha = dim ? 0.18 : 1
    const col = kindColor(nd.kind)
    // 光晕
    const glow = isSel ? p.r * 2.2 : isHover ? p.r * 1.8 : p.r * 1.4
    const g = ctx.createRadialGradient(p.x, p.y, 0, p.x, p.y, glow)
    g.addColorStop(0, toRgba(col, 0.4))
    g.addColorStop(1, toRgba(col, 0))
    ctx.fillStyle = g
    ctx.beginPath()
    ctx.arc(p.x, p.y, glow, 0, Math.PI * 2)
    ctx.fill()
    // 本体
    ctx.fillStyle = col
    ctx.beginPath()
    ctx.arc(p.x, p.y, isSel ? p.r * 1.5 : isHover ? p.r * 1.25 : p.r, 0, Math.PI * 2)
    ctx.fill()
    // 选中节点加描边环
    if (isSel) {
      ctx.strokeStyle = toRgba(theme.primary, 0.9)
      ctx.lineWidth = 1.5
      ctx.beginPath()
      ctx.arc(p.x, p.y, p.r * 2.4, 0, Math.PI * 2)
      ctx.stroke()
    }
    // 标签：仅高度数 / 选中 / hover
    const pv = state.pos.get(nd.k)
    if ((pv && pv.label) || isSel || isHover) {
      ctx.fillStyle = isSel ? theme.text : toRgba(theme.text, 0.75)
      ctx.font = `${isSel ? 11 : 9}px ui-monospace,SFMono-Regular,Menlo,monospace`
      ctx.fillText(String(nd.title).slice(0, 12), p.x + p.r + 3, p.y + 3)
    }
    ctx.globalAlpha = 1
  }
}

// ── 命中检测（视图内节点）──
function hit(x, y) {
  if (!canvasEl.value) return ''
  const m = metrics()
  const R = m.R
  const cx = m.cx
  const cy = m.cy
  let best = ''
  let bd = Infinity
  for (const nd of view.nodes) {
    const p = state.pos.get(nd.k)
    if (!p) continue
    const d = Math.hypot(x - (cx + p.x * R), y - (cy + p.y * R))
    const rad = p.r * R * 0.045 + 10
    if (d < rad && d < bd) {
      bd = d
      best = nd.k
    }
  }
  return best
}
function localXY(ev) {
  const cv = canvasEl.value
  const rect = cv.getBoundingClientRect()
  return { x: ev.clientX - rect.left, y: ev.clientY - rect.top }
}
function onTap(ev) {
  if (state.moved) return // 拖拽结束的那次 click 不算选中
  const { x, y } = localXY(ev)
  const k = hit(x, y)
  s.value.selected = k === s.value.selected ? '' : k
  refsExpanded.value = false
}

// ── 指针交互：单指拖拽平移 / 双指捏合缩放 / 滚轮缩放 ──
const ptrs = new Map()
function onPointerDown(ev) {
  try {
    canvasEl.value.setPointerCapture?.(ev.pointerId)
  } catch (e) {
    /* 老浏览器无 pointer capture：忽略 */
  }
  ptrs.set(ev.pointerId, { x: ev.clientX, y: ev.clientY })
  state.moved = false
  if (ptrs.size === 1) {
    state.dragging = true
    state.lx = ev.clientX
    state.ly = ev.clientY
  } else if (ptrs.size === 2) {
    const [a, b] = [...ptrs.values()]
    state.pinch = { d: Math.hypot(a.x - b.x, a.y - b.y) || 1, scale: state.scale }
    state.dragging = false
  }
}
function onPointerMove(ev) {
  const p = ptrs.get(ev.pointerId)
  if (p) {
    p.x = ev.clientX
    p.y = ev.clientY
  }
  if (ptrs.size === 2 && state.pinch) {
    const [a, b] = [...ptrs.values()]
    const d = Math.hypot(a.x - b.x, a.y - b.y) || 1
    state.scale = Math.min(3, Math.max(0.4, (state.pinch.scale * d) / state.pinch.d))
    state.moved = true
    requestDraw()
    return
  }
  if (state.dragging && ptrs.size === 1) {
    const dx = ev.clientX - state.lx
    const dy = ev.clientY - state.ly
    if (Math.abs(dx) + Math.abs(dy) > 3) state.moved = true
    state.ox += dx
    state.oy += dy
    state.lx = ev.clientX
    state.ly = ev.clientY
    requestDraw()
    return
  }
  if (ptrs.size) return
  // hover 命中（仅改光标与高亮，代价 O(n)）
  const { x, y } = localXY(ev)
  const k = hit(x, y)
  if (k !== s.value.hover) {
    s.value.hover = k
    requestDraw()
  }
}
function onPointerUp(ev) {
  ptrs.delete(ev.pointerId)
  if (ptrs.size < 2) state.pinch = null
  if (ptrs.size === 0) state.dragging = false
  else if (ptrs.size === 1) {
    // 双指抬起一根 → 以剩下那根重新作为拖拽基准
    const [a] = [...ptrs.values()]
    state.dragging = true
    state.lx = a.x
    state.ly = a.y
  }
  try {
    canvasEl.value.releasePointerCapture?.(ev.pointerId)
  } catch (e) {
    /* ignore */
  }
}
function onPointerLeave() {
  if (s.value.hover) {
    s.value.hover = ''
    requestDraw()
  }
}
function onWheel(ev) {
  ev.preventDefault()
  state.scale = Math.min(3, Math.max(0.4, state.scale * (ev.deltaY < 0 ? 1.12 : 0.89)))
  requestDraw()
}
function onKeydown(ev) {
  if (ev.key === 'Escape' && s.value.selected) s.value.selected = ''
}

// ── 数据加载 ──
async function load() {
  try {
    const r = await fetch('/api/graph/entity')
    const j = await r.json()
    if (j.error) {
      s.value.err = j.error
      return
    }
    s.value.nodes = Array.isArray(j.nodes) ? j.nodes : []
    s.value.edges = Array.isArray(j.edges) ? j.edges : []
    s.value.count = j.count || { nodes: s.value.nodes.length, edges: s.value.edges.length }
    s.value.loaded = true
    rebuildView(true)
  } catch (e) {
    s.value.err = String(e)
  }
}

let ro = null
watch(
  () => selKinds.value.join('|'),
  () => {
    if (s.value.loaded) rebuildView(true) // 筛选变化 → 重新布局 + 重启收敛
  }
)
watch(
  () => legendOpen.value,
  () => {
    // 图例收起/展开 → 图心水平偏移变化，重绘（不做过渡动画，避免每帧 rAF）
    requestDraw()
  }
)
watch(
  () => s.value.selected,
  (k, old) => {
    requestDraw()
    if (k && !old) {
      // 打开面板时把焦点交给面板（Esc 可关、Tab 可进邻接行）
      requestAnimationFrame(() => panelEl.value?.focus?.())
    }
  }
)

onMounted(() => {
  refreshTheme()
  skinMo = new MutationObserver(() => {
    refreshTheme()
    requestDraw()
  })
  skinMo.observe(document.documentElement, { attributes: true, attributeFilter: ['data-skin'] })

  load()
  const el = wrapEl.value
  el.addEventListener('wheel', onWheel, { passive: false })
  window.addEventListener('keydown', onKeydown)
  if (typeof ResizeObserver !== 'undefined') {
    ro = new ResizeObserver(() => requestDraw())
    ro.observe(el)
  } else {
    window.addEventListener('resize', requestDraw)
  }
  ensureSized()
})

onBeforeUnmount(() => {
  if (state.raf) cancelAnimationFrame(state.raf)
  state.raf = 0
  phys.done = true
  const el = wrapEl.value
  el?.removeEventListener('wheel', onWheel)
  window.removeEventListener('keydown', onKeydown)
  window.removeEventListener('resize', requestDraw)
  ro?.disconnect()
  skinMo?.disconnect()
  skinMo = null
})
</script>

<template>
  <!-- 全屏画布：**不套卡片**（无 glass / card-cyber 外壳）。
       根节点吃父级给的 absolute inset-0，故 h-full 具备明确高度；
       min-h-[240px] 只是兜底，避免父级无高度约束时 canvas 被压成 0 高。 -->
  <div
    ref="wrapEl"
    class="relative h-full min-h-[240px] w-full select-none overflow-hidden"
  >
    <canvas
      ref="canvasEl"
      class="absolute inset-0 block h-full w-full touch-none"
      :class="s.hover ? 'cursor-pointer' : 'cursor-crosshair'"
      role="img"
      :aria-label="`实体关系星图：${vstat.nodes} 个实体、${vstat.edges} 条关系。滚轮或双指缩放，拖拽平移，点击实体查看详情。`"
      @click="onTap"
      @pointerdown="onPointerDown"
      @pointermove="onPointerMove"
      @pointerup="onPointerUp"
      @pointercancel="onPointerUp"
      @pointerleave="onPointerLeave"
    />

    <!-- ── 左侧悬浮图例面板（默认展开；颜色点=只看该类，勾选框=显隐）── -->
    <div
      class="legend-panel pointer-events-none absolute left-0 top-0 bottom-0 z-20 flex w-[64%] max-w-[208px] flex-col"
      :class="legendOpen ? '' : 'legend-closed'"
    >
      <div class="glass-strong pointer-events-auto m-2 mr-1 flex min-h-0 flex-1 flex-col rounded-md">
        <!-- 面板头 -->
        <div class="flex shrink-0 items-center gap-1.5 border-b border-line-divider px-2 py-1.5">
          <span class="text-[11px] text-fg">图例</span>
          <span class="font-mono text-[10px] text-fg-dim">{{ kinds.length }} 类</span>
          <button
            class="ml-auto shrink-0 rounded px-1.5 py-0.5 text-[10.5px] text-fg-dim transition-colors hover:text-acc"
            aria-label="收起图例面板"
            :aria-expanded="legendOpen"
            @click="legendOpen = false"
          >
            ‹ 收起
          </button>
        </div>

        <!-- 总开关：保留原有「全部」「核心 ≥3 / 全量」 -->
        <div class="flex shrink-0 items-center gap-1.5 border-b border-line-divider px-2 py-1.5">
          <button
            class="rounded border px-2 py-0.5 text-[10.5px] transition-colors"
            :class="selKinds.length === 0 ? 'border-acc/40 bg-acc/15 text-acc' : 'border-line text-fg-dim hover:text-fg'"
            :aria-pressed="selKinds.length === 0"
            @click="clearKinds"
          >
            全部 {{ s.count.nodes }}
          </button>
          <!-- 核心/全量开关：窄屏默认只看骨架，避免 284 点糊成乱线 -->
          <button
            class="rounded border px-2 py-0.5 text-[10.5px] transition-colors"
            :class="coreOnly ? 'border-acc/40 bg-acc/15 text-acc' : 'border-line text-fg-dim hover:text-fg'"
            :aria-pressed="coreOnly"
            :title="coreOnly ? '当前：只看度数≥3 的核心节点' : '当前：全部节点'"
            @click="toggleCoreOnly"
          >
            {{ coreOnly ? '核心 ≥3' : '全量' }}
          </button>
        </div>

        <!-- 类型列表：颜色点 + 类型名 + 数量 + 勾选框 -->
        <div class="min-h-0 flex-1 overflow-y-auto px-1 py-1" role="group" aria-label="实体类型显隐">
          <div
            v-for="kd in kinds"
            :key="kd.k"
            class="flex items-center rounded px-0.5 transition-colors hover:bg-acc/10"
          >
            <button
              class="shrink-0 rounded p-1"
              :title="`只看 ${kd.k}（${kd.n} 个实体）`"
              :aria-label="`只看 ${kd.k}`"
              @click="toggleKind(kd.k)"
            >
              <span class="block h-2 w-2 rounded-full" :style="{ background: kd.color }" />
            </button>
            <button
              class="flex min-w-0 flex-1 items-center gap-1.5 rounded px-1 py-1 text-left"
              role="checkbox"
              :aria-checked="isKindOn(kd.k)"
              :title="`${isKindOn(kd.k) ? '隐藏' : '显示'} ${kd.k}`"
              @click="toggleKindCheck(kd.k)"
            >
              <span
                class="flex h-3 w-3 shrink-0 items-center justify-center rounded-sm border text-[9px] leading-none"
                :class="isKindOn(kd.k) ? 'border-acc/50 bg-acc/20 text-acc' : 'border-line text-transparent'"
              >✓</span>
              <span
                class="min-w-0 flex-1 truncate font-mono text-[10.5px]"
                :class="isKindOn(kd.k) ? 'text-fg' : 'text-fg-dim'"
              >{{ kd.k }}</span>
              <span class="shrink-0 font-mono text-[9.5px] text-fg-dim">{{ kd.n }}</span>
            </button>
          </div>
          <div v-if="!kinds.length" class="px-2 py-3 text-center text-[10.5px] text-fg-dim">暂无类型数据</div>
        </div>
      </div>
    </div>

    <!-- 收起后的展开把手：常驻左缘，面板收起时只剩它 -->
    <button
      v-show="!legendOpen"
      class="glass-strong absolute left-0 top-1/2 z-20 -translate-y-1/2 rounded-r-md border border-l-0 border-line px-1.5 py-3 text-[10.5px] text-fg-dim transition-colors hover:text-acc"
      style="writing-mode: vertical-rl"
      aria-label="展开图例面板"
      :aria-expanded="legendOpen"
      @click="legendOpen = true"
    >
      图例 ›
    </button>

    <!-- ── 右上状态胶囊（≤30px 高，DESIGN §4）+ 重新布局 ── -->
    <div class="absolute right-2 top-2 z-20 flex items-center gap-1.5">
      <div class="glass-strong hidden items-center gap-2 rounded-md px-2 py-1 font-mono text-[10px] text-fg-dim sm:flex">
        <span>
          实体 {{ vstat.nodes }}<span v-if="vstat.nodes !== s.count.nodes" class="opacity-60">/{{ s.count.nodes }}</span>
        </span>
        <span>
          关系 {{ vstat.edges }}<span v-if="vstat.edges !== s.count.edges" class="opacity-60">/{{ s.count.edges }}</span>
        </span>
        <span class="flex items-center gap-1" :title="physDone ? '力导向已收敛，停止计算' : '力导向演化中'">
          <span class="h-1.5 w-1.5 rounded-full" :class="physDone ? 'bg-ok' : 'bg-warn animate-pulse'" />
          {{ physDone ? '已收敛 · 已停算' : `收敛中 ${progress}%` }}
        </span>
      </div>
      <button
        class="glass-strong shrink-0 rounded-md border border-line px-2 py-1 text-[10.5px] text-fg-dim transition-colors hover:border-acc/40 hover:text-acc"
        title="重置 alpha 并重新初始化节点位置"
        @click="relayout"
      >
        ↻ 重新布局
      </button>
    </div>

    <!-- ── 右侧实体详情面板（滑出；原底部详情条已合并到此）── -->
    <Transition name="drawer">
      <aside
        v-if="detail"
        ref="panelEl"
        tabindex="-1"
        role="dialog"
        aria-label="实体详情"
        class="glass-strong absolute right-0 top-0 bottom-0 z-30 flex w-[86%] max-w-[330px] flex-col outline-none"
      >
        <!-- 头部 -->
        <div class="shrink-0 flex items-start gap-2 border-b border-line-divider px-3 py-2.5">
          <span class="mt-1 h-2.5 w-2.5 shrink-0 rounded-full" :style="{ background: kindColor(detail.kind) }" />
          <div class="min-w-0 flex-1">
            <div class="truncate text-[13px] leading-tight text-fg" :title="detail.title">{{ detail.title }}</div>
            <div class="mt-0.5 flex items-center gap-1.5 text-[10px] font-mono">
              <span
                class="rounded px-1 py-px"
                :style="{ background: kindColor(detail.kind) + '22', color: kindColor(detail.kind) }"
              >{{ detail.kind }}</span>
              <span class="truncate text-fg-dim" :title="detail.k">{{ detail.k }}</span>
            </div>
          </div>
          <button
            class="shrink-0 rounded px-1.5 py-0.5 text-[12px] text-fg-dim transition-colors hover:text-bad"
            aria-label="关闭详情面板"
            @click="closePanel"
          >✕</button>
        </div>

        <!-- 关键指标 -->
        <div class="shrink-0 grid grid-cols-3 gap-1.5 px-3 py-2 text-center">
          <div class="rounded border border-line-divider py-1">
            <div class="text-[15px] leading-none text-fg">{{ detail.deg }}</div>
            <div class="mt-0.5 text-[9.5px] text-fg-dim">关系数</div>
          </div>
          <div class="rounded border border-line-divider py-1">
            <div class="text-[15px] leading-none text-fg">{{ detail.outs }}</div>
            <div class="mt-0.5 text-[9.5px] text-fg-dim">出边</div>
          </div>
          <div class="rounded border border-line-divider py-1">
            <div class="text-[15px] leading-none text-fg">{{ detail.ins }}</div>
            <div class="mt-0.5 text-[9.5px] text-fg-dim">入边</div>
          </div>
        </div>

        <!-- 邻接列表（由 edges 推导：方向 / 边类型 t / 状态 st / 置信度 c）-->
        <div class="min-h-0 flex-1 overflow-y-auto px-3 pb-2">
          <div class="mb-1 flex items-center text-[10px] text-fg-dim">
            <span>关联实体</span>
            <span class="ml-auto font-mono opacity-70">{{ detail.rows.length }}</span>
          </div>
          <div v-if="!detail.rows.length" class="rounded border border-line-divider px-2 py-3 text-center text-[11px] text-fg-dim">
            该实体暂无关系边
          </div>
          <button
            v-for="r in shownRows"
            :key="`${r.dir}-${r.key}-${r.t}`"
            class="mb-1 flex w-full items-center gap-1.5 rounded border border-line-divider px-2 py-1 text-left transition-colors hover:bg-acc/10"
            :title="`跳转聚焦 ${r.title}`"
            @click="focusNode(r.key)"
          >
            <span class="w-3 shrink-0 text-center text-[11px] text-acc">{{ r.dir }}</span>
            <span class="h-1.5 w-1.5 shrink-0 rounded-full" :style="{ background: kindColor(r.kind) }" />
            <span class="min-w-0 flex-1 truncate text-[11.5px] text-fg">{{ r.title }}</span>
            <span v-if="r.t" class="shrink-0 rounded bg-acc/12 px-1 text-[9.5px] font-mono text-acc">{{ r.t }}</span>
            <span v-if="r.st !== 'stable'" class="shrink-0 text-[9.5px] font-mono" :class="stClass(r.st)">{{ r.st }}</span>
            <span v-if="r.c !== null" class="shrink-0 text-[9.5px] font-mono text-fg-dim">{{ r.c.toFixed(2) }}</span>
          </button>
          <button
            v-if="detail.rows.length > shownRows.length"
            class="mt-1 w-full rounded border border-line px-2 py-1 text-[10.5px] text-fg-dim transition-colors hover:text-acc"
            @click="refsExpanded = true"
          >
            展开全部 {{ detail.rows.length }} 条
          </button>
          <button
            v-else-if="refsExpanded && detail.rows.length > 24"
            class="mt-1 w-full rounded border border-line px-2 py-1 text-[10.5px] text-fg-dim transition-colors hover:text-acc"
            @click="refsExpanded = false"
          >
            收起
          </button>
        </div>

        <div
          class="shrink-0 border-t border-line-divider px-3 py-1.5 text-[9.5px] text-fg-dim"
          style="padding-bottom: calc(0.375rem + env(safe-area-inset-bottom))"
        >
          Esc 关闭 · 点关联实体可跳转聚焦
        </div>
      </aside>
    </Transition>

    <!-- 空态 / 错误 -->
    <div v-if="s.err" class="absolute inset-0 grid place-items-center text-[12px] text-bad">{{ s.err }}</div>
    <div
      v-else-if="s.loaded && !s.nodes.length"
      class="absolute inset-0 grid place-items-center text-[12px] text-fg-dim"
    >暂无实体关系数据</div>
  </div>
</template>

<style scoped>
/* 左侧图例面板收起/展开：只动 transform / opacity（DESIGN-v2 §3，禁 transition-all） */
.legend-panel {
  transform: translateX(0);
  opacity: 1;
  transition: transform 0.26s ease, opacity 0.2s ease;
  will-change: transform;
}
.legend-panel.legend-closed {
  transform: translateX(-102%);
  opacity: 0;
  pointer-events: none; /* 收起后不挡画布的拖拽/点击 */
}

/* 抽屉动效：只动 transform / opacity（DESIGN-v2 §3） */
.drawer-enter-from,
.drawer-leave-to {
  transform: translateX(102%);
  opacity: 0;
}
.drawer-enter-active,
.drawer-leave-active {
  transition: transform 0.26s ease, opacity 0.2s ease;
}
</style>
