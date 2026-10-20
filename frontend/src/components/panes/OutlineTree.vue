<script setup>
// OutlineTree.vue —— 大纲视图（L0→L5 真·折叠树 / Tree View）
//
// ══════════════════════════════════════════════════════════
// 数据来源（**完全沿用原有加载链路，未新增/未改动任何端点**）：
//   · loadOutline()      → systemData.outline.levels
//        levels[i] = { id, name, items[], count, err, status, done, file }
//        items[j]  = { layer, key, title, chars, source, dirty }
//        （/api/layers/{L}/items?limit=200 + /api/pipeline/funnel，见 useSystem.js）
//   · loadOutlineItem(L,key) → { content, chars }
//        （/api/layers/{L}/item/{key}，点开详情时按需拉）
//   展开态沿用 store 的 s.expanded（切页不丢），未新增任何字段。
//
// 本轮（outlineG）相对 outline3 的改动 —— 由「干瘪缩进行」→「带连线的卡片树」：
//   ① 【主改】每个节点（根 / 层 / 条目）渲染为一张**紧凑小卡片**：
//        采用 `.cy-card` 皮肤（1px 边框 / 10px 圆角 / 玻璃底），
//        但 `.ot-card.ot-card` 双类名把 padding 收到 10px（p-2.5）、min-height 压到 0、
//        标题基准 14px —— 比默认 .cy-card（padding 16px / 标题 20px）紧凑得多。
//   ② 【主改】L0→L5 用「缩进 + 垂直导线」串成树：保留原有 guideLefts 竖线逻辑，
//        竖线仍画在**祖先三角盒列的列中心**（x = 祖先 pad + GUIDE_OFFSET）；
//        另加一段横向「肘」(.ot-elbow) 把父级竖线连到本卡片左缘（+ 端点小圆点），
//        卡片之间留 8px 空隙且空隙**计入节点高度**，故竖线跨过空隙不中断。
//   ③ 【主改】层级差异（L0 最重 → L5 最轻）＝ 缩进 + 卡片边框亮度递减 + 标题字号递减：
//        CARD_STYLE 表给出每档 字号 / 字重 / 边框 alpha / 整卡不透明度（静态字面量）。
//   ④ 【主改】点击**任意层级卡片** → 右侧 85% 全屏大抽屉（`.cy-drawer` /
//        `.cy-drawer-md`），Markdown 大字排版；**绝不出居中小窗**（fixed 全屏遮罩
//        + 右侧滑入 aside）。根 / 层卡片打开抽屉时列出其下级（复用已加载数据，
//        不发新请求），点下级 → 抽屉内栈式下钻，带「返回」。
//   ⑤ 保留：折叠/展开（三角按钮 / ←→ 方向键）、全部展开/折叠、roving tabindex、
//        键盘导航（↑↓←→ Home/End；Enter/Space = 打开详情）、折叠分支不进 DOM、
//        漏斗进度条（transform: scaleX）。
//   ⑥ 抽屉关闭三重保障（遮罩点击 / 右上角 ✕ / Esc）+ 打开时锁 body 背景滚动 +
//        卸载兜底恢复（onBeforeUnmount）。
// ══════════════════════════════════════════════════════════
import { computed, nextTick, onBeforeUnmount, onMounted, reactive, ref, watch } from 'vue'
import { systemData, pushLog } from '../../store.js'
import { loadOutline, loadOutlineItem } from '../../composables/useSystem.js'
import MarkdownBlock from '../MarkdownBlock.vue'

const s = systemData.outline
const busy = ref(false)

// ══ L0 → L5 阶梯 ══
// rank 决定 ① 同级排序 ② 缩进 / 字号 / 色深的递减档位。
// 后端若新增层（或出现未登记 id），rank 兜底到最末，样式按最深档位走，不崩。
const ROOT_ID = 'ROOT'
const LADDER = ['L0', 'L0.5', 'L1', 'L1x', 'L2', 'L3', 'L4', 'L5']
function rankOf(id) {
  const i = LADDER.indexOf(String(id).toUpperCase())
  return i < 0 ? LADDER.length : i
}

// ── 卡片树的几何常量 ──
// INDENT：每下一层的左缩进；RANK_STEP：同级层之间再按 rank 下潜 4px（L0→L5 阶梯）；
// GUIDE_OFFSET：竖线 x = 祖先 pad + 20 —— 20 = 卡片左内边距 10 + 三角盒半宽 10，
//   于是竖线正对上一级三角盒的列中心（沿用旧版「竖线对齐箭头」的思路）；
// GAP：卡片之间的垂直空隙，作为节点 wrapper 的 padding-bottom，计入高度 → 导线不断。
const INDENT = 26
const RANK_STEP = 4
const GUIDE_OFFSET = 20
const GAP = 8

function rankOffset(rank) {
  return rank > 0 ? rank * RANK_STEP : 0
}

// 每档卡片的标题字号 / 字重 / 边框 alpha / 整卡不透明度 —— **L0 最重、L5 最轻**
// （全部静态字面量，避免任何运行时主题依赖；边框用 rgb(var(--line-rgb) / alpha) 走皮肤）
const CARD_STYLE = [
  { fs: 14, w: 660, b: 0.66, op: 1.0 }, // L0
  { fs: 13.6, w: 640, b: 0.58, op: 0.98 }, // L0.5
  { fs: 13.2, w: 620, b: 0.5, op: 0.95 }, // L1
  { fs: 12.9, w: 590, b: 0.44, op: 0.92 }, // L1x
  { fs: 12.5, w: 560, b: 0.38, op: 0.89 }, // L2
  { fs: 12.2, w: 530, b: 0.33, op: 0.86 }, // L3
  { fs: 12, w: 505, b: 0.29, op: 0.83 }, // L4
  { fs: 11.8, w: 480, b: 0.25, op: 0.8 }, // L5
]
const CARD_FALLBACK = { fs: 11.8, w: 480, b: 0.24, op: 0.78 }
const ROOT_CARD = { fs: 14.5, w: 700, b: 0.8, op: 1.0 }

// 层编号徽章的主色档位：越靠上游越亮，越往下游越淡
const RANK_ACC = [
  'text-primary',
  'text-primary',
  'text-primary',
  'text-content',
  'text-content-sec',
  'text-content-sec',
  'text-content-sec',
  'text-content-sec',
]
const RANK_ACC_FALLBACK = 'text-content-sec'
function accOf(rank) {
  return RANK_ACC[rank] || RANK_ACC_FALLBACK
}

// 取某行对应档位的卡片配置：条目比其所属层再轻一档（层级感），根最重。
function cardConf(row) {
  if (row.kind === 'root') return ROOT_CARD
  const r = row.kind === 'item' ? Math.min(CARD_STYLE.length - 1, row.rank + 1) : row.rank
  return CARD_STYLE[r] || CARD_FALLBACK
}
// 节点 wrapper：缩进 + 底部空隙（空隙计入高度，导线跨隙不断）
function nodeStyle(row) {
  return { paddingLeft: row.pad + 'px', paddingBottom: GAP + 'px' }
}
// 卡片：边框亮度随层级递减（L0 最重 → L5 最轻）
function cardStyle(row) {
  const c = cardConf(row)
  return {
    borderColor: 'rgb(var(--line-rgb, 38 39 46) / ' + c.b + ')',
    opacity: c.op,
  }
}
// 卡片标题：字号 / 字重随层级递减
function titleStyle(row) {
  const c = cardConf(row)
  return { fontSize: c.fs + 'px', fontWeight: c.w }
}
// 横向「肘」：从父级竖线连到本卡片左缘；垂直位置 = 卡片竖直中心
// （node 高度 = 卡片高 + GAP，故卡片中心 = calc(50% - GAP/2)）
function elbowStyle(row) {
  const left = row.guideLefts[row.guideLefts.length - 1] || 0
  const w = Math.max(0, row.pad - left)
  return { left: left + 'px', width: w + 'px', top: 'calc(50% - ' + GAP / 2 + 'px)' }
}

// ── 展开态 ──
//   id 规则：ROOT / L:L3（层）/ I:L3:卷01（条目）
//   isOpen 带默认值兜底：第一次进来时根 + 所有层默认展开、条目默认收起，
//   首屏立刻有结构可看，且不会把 600+ 条正文一次性铺开。
function isOpen(id) {
  const v = s.expanded[id]
  if (v === undefined) return id === ROOT_ID || id.startsWith('L:')
  return !!v
}
function toggleNode(id) {
  s.expanded[id] = !isOpen(id)
}

/** 每层头部状态徽章（沿用旧版逻辑与文案，只换语义 token） */
function badge(L) {
  if (L.err) return { t: '读取失败', c: 'text-danger bg-danger-soft border-danger/25' }
  if (L.done) return { t: '已完成', c: 'text-success bg-success-soft border-success/25' }
  const st = String(L.status || '')
  if (st.startsWith('chapters')) {
    return {
      t: st.replace('chapters_', '章节 '),
      c: 'text-warning bg-warning-soft border-warning/25',
    }
  }
  if (st) return { t: st, c: 'text-content-sec bg-surface-card-2 border-line/70' }
  return { t: '未开始', c: 'text-content-sec bg-surface-card-2 border-line/70' }
}

function fmtK(n) {
  if (!n) return '0'
  return n >= 10000 ? (n / 10000).toFixed(1) + '万' : String(n)
}

// ══════════════════════════════════════════════════════
// 树：根 → 层（L0→L5 阶梯升序）→ 条目
// ══════════════════════════════════════════════════════
const tree = computed(() => {
  const layers = [...(s.levels || [])].sort(
    (a, b) => rankOf(a.id) - rankOf(b.id)
  )
  return layers.map((L) => {
    const rank = rankOf(L.id)
    const items = (L.items || []).map((it) => ({
      id: `I:${L.id}:${it.key}`,
      kind: 'item',
      layer: L.id,
      rank,
      key: it.key,
      // 既有字段：title / chars / source（dirty 仅作角标，缺失也不影响）
      label: it.title || '（无标题）',
      chars: it.chars || 0,
      meta: fmtK(it.chars) + ' 字',
      source: it.source || '',
      dirty: !!it.dirty,
      expandable: true, // 条目也能展收：展开 = 行内懒加载正文
      children: [],
    }))
    return {
      id: `L:${L.id}`,
      kind: 'layer',
      layer: L.id,
      rank,
      code: L.id,
      label: L.name,
      badge: badge(L),
      count: L.count ?? items.length,
      err: L.err || '',
      expandable: items.length > 0,
      children: items,
    }
  })
})

const rootNode = computed(() => ({
  id: ROOT_ID,
  kind: 'root',
  rank: -1,
  label: '全书大纲树',
  code: 'L0→L5',
  count: (s.levels || []).length,
  expandable: tree.value.length > 0,
  children: tree.value,
}))

/**
 * 拍平成「可见行」——折叠树的核心：折叠的子树**根本不进数组**，
 * 于是 DOM 里根本不存在那些节点（比 v-if 更彻底），600+ 条 L4 折叠时
 * 只渲染根 + 6 个层头这几行。
 *
 * 每行携带：
 *   depth        缩进档位（0=根）
 *   pad          padding-left = depth*INDENT + rankOffset(rank)
 *   guides[i]    第 i 级祖先的竖线是否要画（该祖先后面还有兄弟 → 续线）
 *   guideLefts[i] 第 i 级祖先的竖线 x 坐标 = 祖先 pad + GUIDE_OFFSET（对齐三角盒中心）
 *   posInSet/setSize  aria-posinset / aria-setsize
 *   parentId     供 ← 键回到父级
 */
const rows = computed(() => {
  const out = []
  const walk = (node, ctx) => {
    const { depth, guides, isLast, index, size, parentId, ancestorPads } = ctx
    const pad = depth * INDENT + rankOffset(node.rank)
    out.push({
      ...node,
      depth,
      pad,
      guides,
      guideLefts: ancestorPads.map((p) => p + GUIDE_OFFSET),
      posInSet: index + 1,
      setSize: size,
      parentId,
    })
    if (!isOpen(node.id)) return
    const kids = node.children || []
    if (!kids.length) return
    // 根是主干，永远续线；其余节点只有「后面还有兄弟」时才续线
    const line = depth === 0 ? true : !isLast
    const childGuides = [...guides, line]
    const childPads = [...ancestorPads, pad]
    kids.forEach((c, i) =>
      walk(c, {
        depth: depth + 1,
        guides: childGuides,
        isLast: i === kids.length - 1,
        index: i,
        size: kids.length,
        parentId: node.id,
        ancestorPads: childPads,
      })
    )
  }
  walk(rootNode.value, {
    depth: 0,
    guides: [],
    isLast: true,
    index: 0,
    size: 1,
    parentId: null,
    ancestorPads: [],
  })
  return out
})

// ── 全部展开 / 全部折叠 ──
const allIds = computed(() => {
  const ids = [ROOT_ID]
  for (const L of tree.value) {
    ids.push(L.id)
    for (const it of L.children) ids.push(it.id)
  }
  return ids
})
function expandAll() {
  for (const id of allIds.value) s.expanded[id] = true
  focusId(ROOT_ID)
}
function collapseAll() {
  for (const id of allIds.value) s.expanded[id] = false
  // 根保持展开，否则整棵树会「消失」，用户找不到入口
  s.expanded[ROOT_ID] = true
  focusId(ROOT_ID)
}

// ══════════════════════════════════════════════════════
// 键盘焦点（roving tabindex）：同一时刻只有 1 行 tabindex=0
// ══════════════════════════════════════════════════════
const activeId = ref(ROOT_ID)
const rowEls = new Map()
function setRowEl(el, id) {
  if (el) rowEls.set(id, el)
  else rowEls.delete(id)
}
const visibleIds = computed(() => rows.value.map((r) => r.id))
function focusId(id) {
  if (!id) return
  activeId.value = id
  nextTick(() => {
    const el = rowEls.get(id)
    if (el && typeof el.focus === 'function') el.focus()
  })
}
function moveFocus(delta) {
  const ids = visibleIds.value
  if (!ids.length) return
  const i = ids.indexOf(activeId.value)
  const next = Math.min(ids.length - 1, Math.max(0, (i < 0 ? 0 : i) + delta))
  focusId(ids[next])
}

// ══════════════════════════════════════════════════════
// 条目正文：懒加载 + 缓存（行内展开 & 右侧抽屉共用同一份数据）
//   detail[id] = { loading, err, content, chars }
// ══════════════════════════════════════════════════════
const detail = reactive({})
function detailOf(id) {
  return detail[id] || null
}
async function ensureDetail(L, row) {
  const id = row.id
  if (detail[id] && !detail[id].err) return
  detail[id] = { loading: true, err: '', content: '', chars: row.chars }
  try {
    const r = await loadOutlineItem(L, row.key)
    detail[id] = {
      loading: false,
      err: '',
      content: r.content || '',
      chars: r.chars ?? row.chars,
    }
  } catch (e) {
    detail[id] = {
      loading: false,
      err: String((e && e.message) || e),
      content: '',
      chars: row.chars,
    }
  }
}

/** 三角按钮：展收子树（层/根）或行内正文（条目，懒加载） */
function toggleRow(row) {
  if (!row.expandable) return
  activeId.value = row.id
  toggleNode(row.id)
  if (row.kind === 'item' && isOpen(row.id)) ensureDetail(row.layer, row)
}

/** 键盘可达性（折叠树标准交互）：Enter/Space = 打开详情，
 *  ←→ = 折叠/展开（或走父/子），↑↓ = 上下，Home/End = 首尾 */
function onRowKey(e, row) {
  switch (e.key) {
    case 'Enter':
    case ' ':
    case 'Spacebar':
      e.preventDefault()
      openDetail(row)
      break
    case 'ArrowDown':
      e.preventDefault()
      moveFocus(1)
      break
    case 'ArrowUp':
      e.preventDefault()
      moveFocus(-1)
      break
    case 'ArrowRight': {
      e.preventDefault()
      if (row.expandable && !isOpen(row.id)) {
        toggleRow(row)
      } else {
        // 已展开 → 聚焦第一个子行（可见行里紧跟本行的就是长子）
        const ids = visibleIds.value
        const i = ids.indexOf(row.id)
        if (i >= 0 && ids[i + 1] && rows.value[i + 1].parentId === row.id) {
          focusId(ids[i + 1])
        }
      }
      break
    }
    case 'ArrowLeft':
      e.preventDefault()
      if (row.expandable && isOpen(row.id)) {
        toggleRow(row) // 层折叠 = 收起子树；条目折叠 = 收起正文
      } else if (row.parentId) {
        focusId(row.parentId)
      }
      break
    case 'Home':
      e.preventDefault()
      focusId(visibleIds.value[0])
      break
    case 'End':
      e.preventDefault()
      focusId(visibleIds.value[visibleIds.value.length - 1])
      break
    default:
      break
  }
}

// ══════════════════════════════════════════════════════
// 右侧全屏大抽屉 Drawer（点击任意层级卡片 → 打开）
//   · 容器用骨架类 .cy-drawer = min(85vw, 640px)，右侧滑入 translateX(100%) → 0
//   · 遮罩 + Esc + 右上角 ✕ 三重关闭；打开时锁 body 背景滚动
//   · 栈式导航：根/层详情 ⇄ 条目正文（带「返回」）；绝不出居中小窗
// ══════════════════════════════════════════════════════
const panel = ref(null) // 当前抽屉节点（root | layer | item）
const panelStack = ref([]) // 返回栈
const isDrawerOpen = computed(() => !!panel.value)
const closeBtn = ref(null)
let lastFocusedEl = null

function openDetail(row) {
  if (!row) return
  activeId.value = row.id
  lastFocusedEl =
    rowEls.get(row.id) ||
    (typeof document !== 'undefined' ? document.activeElement : null)
  panel.value = row
  panelStack.value = []
  if (row.kind === 'item') ensureDetail(row.layer, row)
  nextTick(() => {
    const el = closeBtn.value
    if (el && typeof el.focus === 'function') el.focus()
  })
}

/** 抽屉内下钻：根 → 层 → 条目（压栈，支持返回） */
function drillTo(child) {
  if (!child) return
  panelStack.value = [...panelStack.value, panel.value]
  panel.value = child
  if (child.kind === 'item') ensureDetail(child.layer, child)
}

function backPanel() {
  const st = panelStack.value
  if (!st.length) return
  panel.value = st[st.length - 1]
  panelStack.value = st.slice(0, -1)
}

function closeDrawer() {
  panel.value = null
  panelStack.value = []
  nextTick(() => {
    const el = lastFocusedEl
    if (el && typeof el.focus === 'function') el.focus()
  })
}

/** 抽屉条目正文（仅 item 面板有） */
const panelDetail = computed(() =>
  panel.value && panel.value.kind === 'item' ? detailOf(panel.value.id) : null
)
/** 抽屉当前节点的下级（根 → 层列表；层 → 条目列表；条目 → 空） */
const panelChildren = computed(() => (panel.value && panel.value.children) || [])
/** 抽屉标题右侧的计数 */
const panelChars = computed(() => {
  const p = panel.value
  if (!p) return 0
  if (p.kind === 'item') return panelDetail.value?.chars ?? p.chars ?? 0
  return panelChildren.value.length
})
const panelMeta = computed(() => {
  const p = panel.value
  if (!p) return ''
  if (p.kind === 'item') return fmtK(panelChars.value) + ' 字'
  if (p.kind === 'layer') return panelChars.value + ' 条'
  return panelChars.value + ' 层'
})

function onDrawerKey(e) {
  if (!panel.value) return
  if (e.key === 'Escape') {
    e.stopPropagation()
    closeDrawer()
  }
}

// ── 背景滚动锁：抽屉打开锁 body，关闭 / 卸载都恢复 ─────────────
// 先记录进入前的取值再恢复（不覆盖别人写的 overflow）；scrollLocked
// 保证幂等；onBeforeUnmount 兜底——否则抽屉开着切页会把滚动永久锁死。
let prevBodyOverflow = ''
let scrollLocked = false
function setScrollLock(on) {
  if (typeof document === 'undefined' || !document.body) return
  if (on && !scrollLocked) {
    prevBodyOverflow = document.body.style.overflow
    document.body.style.overflow = 'hidden'
    scrollLocked = true
  } else if (!on && scrollLocked) {
    document.body.style.overflow = prevBodyOverflow || ''
    scrollLocked = false
  }
}
watch(isDrawerOpen, (v) => setScrollLock(v), { immediate: true })

onMounted(() => {
  if (typeof window !== 'undefined') window.addEventListener('keydown', onDrawerKey)
  setScrollLock(isDrawerOpen.value)
  loadOutline()
})
onBeforeUnmount(() => {
  if (typeof window !== 'undefined') window.removeEventListener('keydown', onDrawerKey)
  // 卸载兜底：恢复背景滚动（防止 overflow:hidden 永久残留）
  setScrollLock(false)
})

// ── 漏斗进度条：宽度用 transform: scaleX()，不动 width（DESIGN-v2 §3）──
const barRatio = computed(() => {
  const done = Number(s.funnelDone || 0)
  if (!done || !(s.levels || []).length) return 0
  return Math.min(1, done / 7)
})

async function reload() {
  busy.value = true
  await loadOutline(true)
  busy.value = false
  pushLog('大纲已刷新', 'info')
}
</script>

<template>
  <div class="h-full overflow-y-auto px-3 py-3 space-y-3">
    <!-- ── 漏斗进度 ── -->
    <div class="glass rounded-md px-3 py-2.5 space-y-1.5">
      <div class="flex items-center gap-2">
        <span class="text-[12px] text-content">漏斗进度</span>
        <span class="font-mono text-[12px] text-primary">
          {{ s.funnelProgress || '—' }}
        </span>
        <button
          class="ml-auto text-[11px] text-content-sec active:scale-95 disabled:opacity-40"
          :disabled="busy"
          @click="reload"
        >
          {{ busy ? '刷新中…' : '刷新' }}
        </button>
      </div>
      <!-- 进度条：只动 transform（scaleX），不重排、不重绘 -->
      <div class="h-1 rounded-full bg-line/70 overflow-hidden">
        <div
          class="h-full origin-left bg-primary transition-transform duration-500 ease-out"
          :style="{ transform: 'scaleX(' + barRatio + ')' }"
        />
      </div>
      <div class="text-[10px] text-content-sec/70 leading-relaxed">
        L0 概括 → L1 总纲 → L2 分卷 → L3 细纲 → L4 资料 → L5 正文；越往上游越抽象
      </div>
    </div>

    <div
      v-if="s.err"
      class="rounded-md px-3 py-2 text-[11px] bg-danger-soft text-danger break-all"
    >
      {{ s.err }}
    </div>

    <!-- ── 树工具条：全部展开 / 全部折叠 ── -->
    <div class="flex items-center gap-2">
      <span class="text-[11px] text-content-sec">大纲树</span>
      <span class="text-[10px] text-content-sec/60 font-mono">
        {{ rows.length }} 行可见
      </span>
      <div class="ml-auto flex items-center gap-1.5">
        <button class="ot-tool" type="button" title="展开所有层与条目" @click="expandAll">
          全部展开
        </button>
        <button class="ot-tool" type="button" title="折叠所有分支（保留根）" @click="collapseAll">
          全部折叠
        </button>
      </div>
    </div>

    <div class="px-0.5 text-[10px] text-content-sec/55 leading-relaxed">
      点卡片看全屏详情 · 点 ▸ 展收 · ←→ 折叠展开
    </div>

    <!-- ══ 树本体（带连线的卡片树）══
         role=tree：折叠树标准无障碍语义；每行 treeitem + aria-level/expanded/
         setsize/posinset；roving tabindex（同一时刻只有一行可 Tab）。
         导线的 guideLefts 逻辑与旧版一致，只是缩进/偏移适配了卡片内边距。 -->
    <div class="glass rounded-md py-1.5 px-1.5 ot-tree" role="tree" aria-label="大纲树">
      <!-- 加载骨架 -->
      <div v-if="s.loading && !rows.length" class="px-3 py-3 space-y-2">
        <div class="skl" style="width: 58%; height: 12px" />
        <div class="skl" style="width: 82%; height: 10px" />
        <div class="skl" style="width: 70%; height: 10px" />
      </div>

      <template v-for="row in rows" :key="row.id">
        <!-- ── 一行节点：wrapper 管缩进 + 导线，卡片是内容 ── -->
        <div
          class="ot-node"
          :ref="(el) => setRowEl(el, row.id)"
          role="treeitem"
          :data-row-id="row.id"
          :aria-level="row.depth + 1"
          :aria-posinset="row.posInSet"
          :aria-setsize="row.setSize"
          :aria-expanded="row.expandable ? (isOpen(row.id) ? 'true' : 'false') : undefined"
          :tabindex="row.id === activeId ? 0 : -1"
          :style="nodeStyle(row)"
          @click="openDetail(row)"
          @keydown="onRowKey($event, row)"
        >
          <!-- 垂直引导线：父级 → 子级 1px 竖线，每级祖先一根，
               画在该祖先三角盒的列中心（guideLefts = 祖先 pad + 20） -->
          <span
            v-for="(g, i) in row.guides"
            v-show="g"
            :key="'g' + i"
            class="ot-guide"
            :style="{ left: row.guideLefts[i] + 'px' }"
            aria-hidden="true"
          />
          <!-- 横向「肘」：父级竖线 → 本卡片左缘（+ 端点小圆点） -->
          <span
            v-if="row.depth > 0"
            class="ot-elbow"
            :style="elbowStyle(row)"
            aria-hidden="true"
          />

          <!-- ── 紧凑卡片（.cy-card 皮肤，更紧凑）── -->
          <div
            class="ot-card cy-card"
            :class="
              row.kind === 'root'
                ? 'ot-card-root'
                : row.kind === 'layer'
                  ? 'ot-card-layer'
                  : 'ot-card-item'
            "
            :style="cardStyle(row)"
          >
            <!-- 三角：点击只展收，不冒泡到卡片（避免同时开抽屉） -->
            <button
              class="ot-tri"
              :class="{ open: isOpen(row.id) }"
              type="button"
              :disabled="!row.expandable"
              :aria-label="(isOpen(row.id) ? '折叠 ' : '展开 ') + row.label"
              @click.stop="toggleRow(row)"
            >▸</button>

            <!-- ── 根节点 ── -->
            <template v-if="row.kind === 'root'">
              <span class="ot-title flex-1" :style="titleStyle(row)">📖 {{ row.label }}</span>
              <span
                class="shrink-0 text-[10px] font-mono text-content-sec/70
                       px-1.5 py-0.5 rounded-sm border border-line/60"
              >{{ row.code }}</span>
              <span class="shrink-0 text-[10px] text-content-sec font-mono">
                {{ row.count }} 层
              </span>
            </template>

            <!-- ── 层节点 ── -->
            <template v-else-if="row.kind === 'layer'">
              <span
                class="shrink-0 font-mono text-[11px] font-semibold"
                :class="accOf(row.rank)"
              >{{ row.code }}</span>
              <span class="ot-title flex-1" :style="titleStyle(row)">{{ row.label }}</span>
              <span
                class="shrink-0 text-[10px] px-1.5 py-0.5 rounded-sm border"
                :class="row.badge.c"
              >{{ row.badge.t }}</span>
              <span class="shrink-0 text-[10px] text-content-sec font-mono">
                {{ row.count }} 条
              </span>
            </template>

            <!-- ── 条目节点 ── -->
            <template v-else>
              <span class="shrink-0 font-mono text-[10.5px] text-content-sec/90">
                {{ row.key }}
              </span>
              <span class="ot-title flex-1" :style="titleStyle(row)">{{ row.label }}</span>
              <span
                v-if="row.dirty"
                class="shrink-0 text-[9.5px] px-1 rounded-sm border text-warning bg-warning-soft border-warning/25"
              >脏</span>
              <span class="shrink-0 text-[10px] text-content-sec/70 font-mono">
                {{ row.meta }}
              </span>
            </template>

            <!-- 层读取失败 -->
            <span
              v-if="row.kind === 'layer' && row.err"
              class="shrink-0 max-w-[42%] text-[10px] text-danger truncate"
            >{{ row.err }}</span>

            <!-- 可下钻提示（非交互，卡片整体可点） -->
            <span class="ot-go shrink-0" aria-hidden="true">›</span>
          </div>
        </div>

        <!-- ── 条目展开后的正文：v-if 懒加载，折叠时整个节点不在 DOM ── -->
        <div
          v-if="row.kind === 'item' && isOpen(row.id)"
          class="ot-body"
          :style="{ paddingLeft: row.pad + INDENT + 'px' }"
        >
          <div v-if="!detailOf(row.id) || detailOf(row.id).loading" class="text-content-sec">
            读取中…
          </div>
          <div v-else-if="detailOf(row.id).err" class="text-danger break-all">
            {{ detailOf(row.id).err }}
          </div>
          <div v-else-if="!detailOf(row.id).content" class="text-content-sec/70">
            （空）
          </div>
          <pre v-else class="ot-content">{{ detailOf(row.id).content }}</pre>
          <button
            v-if="detailOf(row.id) && detailOf(row.id).content"
            class="ot-inline-more"
            type="button"
            @click.stop="openDetail(row)"
          >在大抽屉中阅读 →</button>
          <div
            v-if="row.source"
            class="mt-1 text-[10px] text-content-sec/45 font-mono truncate"
          >{{ row.source }}</div>
        </div>
      </template>

      <div
        v-if="!s.loading && !(s.levels || []).length"
        class="px-3 py-3 text-[11px] text-content-sec/70"
      >暂无大纲数据</div>
    </div>

    <!-- ══════ 全屏大抽屉（节点详情）：右侧滑出，绝不出居中小窗 ══════ -->
    <Teleport to="body">
      <!-- 外层：遮罩淡入淡出（只动 opacity）；:duration 兜底，
           保证 300ms 滑出动画跑完才卸载 DOM -->
      <Transition name="cy-mask" :duration="{ enter: 200, leave: 320 }">
        <div v-if="panel" class="fixed inset-0 z-[60]">
          <!-- 关闭① 遮罩：点它关闭 -->
          <div class="absolute inset-0 bg-black/45" @click="closeDrawer" />

          <!-- 抽屉本体：.cy-drawer = min(85vw, 640px) + 右滑入 300ms + 玻璃底内建 -->
          <Transition name="cy-drawer" appear>
            <aside
              class="ot-panel cy-drawer"
              role="dialog"
              aria-modal="true"
              aria-label="大纲节点详情"
            >
              <!-- ── 抽屉头部 ── -->
              <header
                class="shrink-0 flex items-center gap-2 px-4 py-3 border-b border-line/60"
              >
                <button
                  v-if="panelStack.length"
                  class="ot-icon-btn shrink-0"
                  type="button"
                  aria-label="返回上一层"
                  @click="backPanel"
                >‹</button>

                <template v-if="panel.kind === 'item'">
                  <span class="shrink-0 font-mono text-[11px] text-primary">
                    {{ panel.key }}
                  </span>
                  <span class="flex-1 min-w-0 text-[14px] font-semibold text-content truncate">
                    {{ panel.label }}
                  </span>
                </template>
                <template v-else>
                  <span
                    class="shrink-0 font-mono text-[12px] font-semibold"
                    :class="accOf(panel.rank)"
                  >{{ panel.code }}</span>
                  <span class="flex-1 min-w-0 text-[14px] font-semibold text-content truncate">
                    {{ panel.label }}
                  </span>
                </template>

                <span class="shrink-0 text-[10.5px] text-content-sec/70 font-mono">
                  {{ panelMeta }}
                </span>

                <!-- 关闭② 右上角 ✕ -->
                <button
                  ref="closeBtn"
                  class="ot-icon-btn shrink-0"
                  type="button"
                  aria-label="关闭（Esc）"
                  title="关闭（Esc）"
                  @click="closeDrawer"
                >✕</button>
              </header>

              <!-- ── 抽屉内容 ── -->
              <div class="cy-drawer-md flex-1 min-h-0">
                <!-- 根 / 层面板：列出下级（复用已加载数据，不发新请求） -->
                <template v-if="panel.kind !== 'item'">
                  <div v-if="panel.kind === 'layer'" class="flex items-center gap-2 mb-2.5">
                    <span
                      class="text-[10px] px-1.5 py-0.5 rounded-sm border"
                      :class="panel.badge.c"
                    >{{ panel.badge.t }}</span>
                    <span v-if="panel.err" class="text-[10.5px] text-danger break-all">
                      {{ panel.err }}
                    </span>
                  </div>
                  <div
                    v-if="!panelChildren.length"
                    class="text-[12px] text-content-sec/70"
                  >
                    {{ panel.kind === 'root' ? '暂无层级数据' : '该层暂无条目' }}
                  </div>
                  <button
                    v-for="ch in panelChildren"
                    :key="ch.id"
                    class="ot-lay-item"
                    type="button"
                    @click="drillTo(ch)"
                  >
                    <span class="shrink-0 font-mono text-[10.5px] text-content-sec/90">
                      {{ ch.kind === 'layer' ? ch.code : ch.key }}
                    </span>
                    <span class="flex-1 min-w-0 text-content truncate text-left">
                      {{ ch.label }}
                    </span>
                    <span class="shrink-0 text-[10px] text-content-sec/70 font-mono">
                      {{ ch.kind === 'layer' ? ch.count + ' 条' : ch.meta }}
                    </span>
                    <span class="shrink-0 text-content-sec/50 text-[13px]">›</span>
                  </button>
                </template>

                <!-- 条目面板：Markdown 放大排版 -->
                <template v-else>
                  <div
                    v-if="!panelDetail || panelDetail.loading"
                    class="text-[12.5px] text-content-sec"
                  >
                    读取中…
                  </div>
                  <div
                    v-else-if="panelDetail.err"
                    class="text-[12.5px] text-danger break-all"
                  >
                    {{ panelDetail.err }}
                  </div>
                  <div
                    v-else-if="!panelDetail.content"
                    class="text-[12.5px] text-content-sec/70"
                  >
                    （空）
                  </div>
                  <div v-else class="ot-md">
                    <MarkdownBlock :source="panelDetail.content" />
                  </div>

                  <div
                    v-if="panel.source"
                    class="mt-4 pt-2.5 border-t border-line-divider text-[10.5px]
                           text-content-sec/45 font-mono break-all"
                  >{{ panel.source }}</div>
                </template>
              </div>
            </aside>
          </Transition>
        </div>
      </Transition>
    </Teleport>
  </div>
</template>

<style scoped>
/* ── 树容器：给绝对定位的导线一个参照 ── */
.ot-tree {
  position: relative;
}

/* ── 节点 wrapper：负责缩进 + 底部空隙；导线跨隙不断（空隙是 padding，被 top/bottom 覆盖）── */
.ot-node {
  position: relative;
  display: block;
  border-radius: 12px;
  cursor: pointer;
}
.ot-node:focus-visible {
  outline: none;
}
.ot-node:focus-visible .ot-card {
  outline: 2px solid rgb(var(--primary-rgb) / 0.9);
  outline-offset: 1px;
}

/* ── 紧凑卡片：.cy-card 负责皮肤（1px 边框 / 10px 圆角 / 玻璃底 / 过渡），
   这里用双类名提高优先级，把 padding 收到 10px（p-2.5）、最小高度压到 0 ── */
.ot-card {
  position: relative;
  display: flex;
  align-items: center;
  gap: 8px;
  border-radius: 10px;
  overflow: hidden;
}
.ot-card.ot-card {
  padding: 10px;
  min-height: 0; /* 最小高度降下来 */
  border-radius: 10px;
}
.ot-card:active {
  transform: scale(0.99);
}
.ot-card-root {
  font-weight: 700;
}
.ot-card-layer {
  font-weight: 500;
}

/* 卡片标题：字号/字重由内联 titleStyle 给，层级越低越轻 */
.ot-title {
  min-width: 0;
  line-height: 1.35;
  letter-spacing: -0.01em;
  color: rgb(var(--fg-rgb, 233 233 238) / 1);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

/* ── 垂直引导线：父级 → 子级 1px 竖线（用分隔线色，随皮肤走）── */
.ot-guide {
  position: absolute;
  top: 0;
  bottom: 0;
  width: 1px;
  background: rgb(var(--divider-rgb) / 1);
  pointer-events: none;
}

/* ── 横向「肘」：把父级竖线连到本卡片左缘，端点再落一颗小圆点 ── */
.ot-elbow {
  position: absolute;
  height: 1px;
  background: rgb(var(--divider-rgb) / 1);
  pointer-events: none;
}
.ot-elbow::after {
  content: '';
  position: absolute;
  right: -2.5px;
  top: -2px;
  width: 5px;
  height: 5px;
  border-radius: 50%;
  background: rgb(var(--divider-rgb) / 1);
}

/* ── 三角展开箭头：▸ 展开时旋转 90° 变 ▾。只动 transform，走合成器 ── */
.ot-tri {
  flex: 0 0 20px;
  width: 20px;
  height: 20px;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  border-radius: 6px;
  font-size: 11px;
  line-height: 1;
  color: rgb(var(--text-disabled-rgb) / 1);
  background: rgb(var(--card-2-rgb) / 0.6);
  transition:
    transform var(--dur-3, 300ms) var(--ease-io, ease),
    color var(--dur-2, 200ms) var(--ease-io, ease),
    background-color var(--dur-2, 200ms) var(--ease-io, ease);
  will-change: transform;
}
.ot-tri:disabled {
  opacity: 0.25;
  cursor: default;
}
.ot-tri.open {
  transform: rotate(90deg);
}
.ot-card:hover .ot-tri,
.ot-node:focus-visible .ot-tri {
  color: rgb(var(--primary-rgb) / 1);
  background: rgb(var(--card-2-rgb) / 1);
}

/* ── 可下钻提示（非交互）：hover / 聚焦时淡入 ── */
.ot-go {
  font-size: 14px;
  line-height: 1;
  color: rgb(var(--text-disabled-rgb) / 1);
  opacity: 0;
  transform: translateX(-2px);
  transition:
    opacity var(--dur-2, 200ms) var(--ease-io, ease),
    transform var(--dur-2, 200ms) var(--ease-io, ease);
}
.ot-card:hover .ot-go,
.ot-node:focus-visible .ot-go {
  opacity: 0.7;
  transform: translateX(0);
}

/* ── 工具条按钮 ── */
.ot-tool {
  font-size: 10.5px;
  line-height: 1;
  padding: 4px 8px;
  border-radius: 6px;
  color: rgb(var(--text-sec-rgb) / 1);
  background: rgb(var(--card-2-rgb) / 0.7);
  border: 1px solid rgb(var(--border-rgb) / 0.9);
  transition:
    transform var(--dur-1, 100ms) var(--ease-io, ease),
    color var(--dur-2, 200ms) var(--ease-io, ease);
}
.ot-tool:active {
  transform: scale(0.95);
}
.ot-tool:hover {
  color: rgb(var(--primary-rgb) / 1);
}

/* ── 条目正文（行内展开）：懒加载，限高滚动，长文不撑爆侧栏 ── */
.ot-body {
  padding-right: 12px;
  padding-top: 2px;
  padding-bottom: 8px;
  font-size: 11px;
  line-height: 1.6;
}
.ot-content {
  margin: 0;
  white-space: pre-wrap;
  word-break: break-word;
  max-height: 240px;
  overflow-y: auto;
  color: rgb(var(--text-sec-rgb) / 1);
  font-family: inherit;
}
.ot-inline-more {
  margin-top: 6px;
  font-size: 10.5px;
  color: rgb(var(--primary-rgb) / 1);
  transition: opacity var(--dur-2, 200ms) var(--ease-io, ease);
}
.ot-inline-more:active {
  opacity: 0.7;
}

/* ── 抽屉里的「层条目」行 ── */
.ot-lay-item {
  display: flex;
  align-items: center;
  gap: 8px;
  width: 100%;
  padding: 8px 8px;
  border-radius: 8px;
  font-size: 12px;
  transition: background-color var(--dur-2, 200ms) var(--ease-io, ease);
}
.ot-lay-item:hover {
  background: rgb(var(--card-2-rgb) / 0.6);
}
.ot-lay-item + .ot-lay-item {
  border-top: 1px solid rgb(var(--divider-rgb) / 0.7);
}

/* ── 抽屉头部图标按钮（返回 / 关闭）── */
.ot-icon-btn {
  width: 28px;
  height: 28px;
  border-radius: 8px;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  font-size: 15px;
  line-height: 1;
  color: rgb(var(--text-sec-rgb) / 1);
  background: rgb(var(--card-2-rgb) / 0.9);
  transition:
    transform var(--dur-1, 100ms) var(--ease-io, ease),
    color var(--dur-2, 200ms) var(--ease-io, ease);
}
.ot-icon-btn:active {
  transform: scale(0.95);
}
.ot-icon-btn:hover {
  color: rgb(var(--primary-rgb) / 1);
}

/* ── 抽屉正文：Markdown 大字排版 ──
   基线由 .cy-drawer-md 给（15px / 1.85），这里用双类名 + :deep 把
   MarkdownBlock 自身的 em 相对字号钉成绝对 px：标题 22 / 18 / 15px。 */
.ot-md {
  font-size: 15px;
  line-height: 1.85;
  color: rgb(var(--text-rgb) / 1);
}
.ot-md.ot-md :deep(p) {
  margin: 0.55rem 0;
  line-height: 1.85;
}
.ot-md.ot-md :deep(h1) {
  font-size: 22px;
  line-height: 1.4;
  font-weight: 650;
  margin: 1.1rem 0 0.5rem;
}
.ot-md.ot-md :deep(h2) {
  font-size: 18px;
  line-height: 1.45;
  font-weight: 620;
  margin: 1rem 0 0.45rem;
}
.ot-md.ot-md :deep(h3) {
  font-size: 15px;
  line-height: 1.5;
  font-weight: 600;
  margin: 0.85rem 0 0.4rem;
}
.ot-md.ot-md :deep(h4),
.ot-md.ot-md :deep(h5),
.ot-md.ot-md :deep(h6) {
  font-size: 15px;
  font-weight: 600;
  margin: 0.7rem 0 0.35rem;
}
.ot-md.ot-md :deep(h1:first-child),
.ot-md.ot-md :deep(h2:first-child),
.ot-md.ot-md :deep(h3:first-child) {
  margin-top: 0;
}
.ot-md.ot-md :deep(li) {
  margin: 0.22rem 0;
  line-height: 1.8;
}
.ot-md.ot-md :deep(ul),
.ot-md.ot-md :deep(ol) {
  margin: 0.45rem 0;
  padding-left: 1.5rem;
}
.ot-md.ot-md :deep(blockquote) {
  margin: 0.7rem 0;
  padding: 0.35rem 0.9rem;
  border-left: 3px solid rgb(var(--primary-rgb) / 0.55);
}

/* ── 抽屉：容器 / 正文的尺寸、玻璃底、圆角都由骨架类提供 ──
   .cy-drawer = min(85vw, 640px) + 右滑入（translateX(100%) → 0 / 300ms）
   .cy-drawer-md = 15px / 1.85 / padding 20px
   过渡类用 style.css 的 .cy-drawer-*（transform）、.cy-mask-*（opacity），
   只动 transform / opacity（DESIGN-v2 §3）。这里只补 HTML 容器提示。 */
.ot-panel {
  border-radius: 0 12px 12px 0;
  overflow: hidden;
  will-change: transform;
}

/* 遮罩淡出延长到 300ms：让抽屉滑完再撤暗底（进入仍用 200ms） */
.cy-mask-leave-active {
  transition: opacity 300ms ease;
}

/* 正文容器：padding / overflow-y 交给 .cy-drawer-md，这里只禁住滚动链 */
.cy-drawer-md {
  overscroll-behavior: contain;
}

/* 尊重「减少动态效果」偏好 */
@media (prefers-reduced-motion: reduce) {
  .cy-drawer-enter-active,
  .cy-drawer-leave-active,
  .cy-mask-enter-active,
  .cy-mask-leave-active {
    transition: none;
  }
}

/* ── 骨架屏 ── */
.skl {
  border-radius: 4px;
  background: rgb(var(--divider-rgb) / 0.8);
}
</style>
