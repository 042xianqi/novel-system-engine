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
// 本轮（outline3）相对上一版的改动：
//   ① 【主改】节点详情改为方案 C 的 **全屏大抽屉**：容器直接用骨架类
//        .cy-drawer（min(85vw, 640px)，translateX(100%) → 0 / 300ms，
//        玻璃底 + 1px 左边框已内建），不再自定义宽度与材质。
//        遮罩 / Esc / 右上角 ✕ 三重关闭；打开时锁 body 背景滚动。
//   ② 【主改】抽屉正文改用骨架类 .cy-drawer-md（15px / line-height 1.85，
//        标题 22 / 18 / 15px），Markdown 仍复用 MarkdownBlock.vue
//        （保留其 marked + 白名单净化的安全链路），字号比上一版更大。
//        MarkdownBlock 内部用 em 相对字号，故以 .ot-md 的 :deep 覆写钉成 px。
//   ③ 层节点也能开详情：抽屉里列出该层条目（复用已加载数据，不发新请求），
//        点条目 → 抽屉内切换到条目正文，带「返回」按钮（栈式导航）。
//   ④ 详情入口按钮改为「常显」（原实现 hover 才现身，触屏根本点不到），
//        opacity .5 → hover 1，图标 ⤢ + aria-label。
//   ⑤ 【修复】条目正文改为在抽屉里看；行内展开（三角）仍保留，点行仍为展收，
//        因此 DESIGN-v2 §5⑤「点击展收 / 全部展开折叠」完全保持。
//   ⑥ 保留：折叠树、缩进、垂直引导线、roving tabindex、键盘导航（↑↓←→ Home/End）、
//        全部展开/折叠、in-row 懒加载正文、漏斗进度条（transform: scaleX）。
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

// 每档的字号 / 透明度 / 文字色（全部写成静态字面量，Tailwind 能扫到 class）
const RANK_STYLE = [
  { fs: '13px', op: 1, cls: 'text-content' }, // L0
  { fs: '12.5px', op: 0.94, cls: 'text-content' }, // L0.5
  { fs: '12px', op: 0.88, cls: 'text-content' }, // L1
  { fs: '11.8px', op: 0.82, cls: 'text-content-sec' }, // L1x
  { fs: '11.6px', op: 0.78, cls: 'text-content-sec' }, // L2
  { fs: '11.4px', op: 0.74, cls: 'text-content-sec' }, // L3
  { fs: '11.2px', op: 0.7, cls: 'text-content-sec' }, // L4
  { fs: '11px', op: 0.66, cls: 'text-content-sec' }, // L5
]
const RANK_FALLBACK = { fs: '11px', op: 0.6, cls: 'text-content-sec' }
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

// 缩进：每层 14px；同级层之间再按 rank 微推 4px，让 L0→L5 呈阶梯下潜
const INDENT = 14
const RANK_STEP = 4
// 三角宽 12px，引导线画在它的列中心 → 竖线正对上一级的箭头
const GUIDE_OFFSET = 6
function rankOffset(rank) {
  return rank > 0 ? rank * RANK_STEP : 0
}
function styleOf(rank) {
  return RANK_STYLE[rank] || RANK_FALLBACK
}
function accOf(rank) {
  return RANK_ACC[rank] || RANK_ACC_FALLBACK
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
 *   guideLefts[i] 第 i 级祖先的竖线 x 坐标 = 祖先 pad + GUIDE_OFFSET（对齐箭头中心）
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

function rowStyle(row) {
  const st = styleOf(row.rank)
  return {
    paddingLeft: row.pad + 'px',
    fontSize: st.fs,
    opacity: st.op,
  }
}

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

/** 点击节点行：层/根 → 展收子树；条目 → 展收正文（懒加载）*/
function onRowClick(row) {
  if (!row.expandable) return
  activeId.value = row.id
  toggleNode(row.id)
  if (row.kind === 'item' && isOpen(row.id)) ensureDetail(row.layer, row)
}

/** 键盘可达性（折叠树标准交互） */
function onRowKey(e, row) {
  switch (e.key) {
    case 'Enter':
    case ' ':
    case 'Spacebar':
      e.preventDefault()
      onRowClick(row)
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
        onRowClick(row)
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
        toggleNode(row.id) // 条目折叠 = 收起正文
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
// 右侧大抽屉 Drawer（原「底部小卡片」的替代）
//   · 宽度 min(85vw, 560px)；.glass-strong；translateX(100%) → 0
//   · 遮罩 + Esc + 右上角关闭按钮 + 点遮罩关闭
//   · 栈式导航：层详情 ⇄ 条目详情（带「返回」）
// ══════════════════════════════════════════════════════
const panel = ref(null) // 当前抽屉节点（layer | item）
const panelStack = ref([]) // 返回栈（打开条目时压入来源层）
const isDrawerOpen = computed(() => !!panel.value)
const closeBtn = ref(null)
let lastFocusedEl = null

function openDetail(row) {
  if (!row) return
  lastFocusedEl = typeof document !== 'undefined' ? document.activeElement : null
  panel.value = row
  panelStack.value = []
  if (row.kind === 'item') ensureDetail(row.layer, row)
  nextTick(() => {
    const el = closeBtn.value
    if (el && typeof el.focus === 'function') el.focus()
  })
}

/** 从「层详情」里点条目 → 压栈切到条目详情 */
function openItemFromLayer(item) {
  if (!item) return
  panelStack.value = [...panelStack.value, panel.value]
  panel.value = item
  ensureDetail(item.layer, item)
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
/** 抽屉标题右侧的字数 */
const panelChars = computed(() => {
  const p = panel.value
  if (!p) return 0
  if (p.kind === 'item') return panelDetail.value?.chars ?? p.chars ?? 0
  return p.count ?? 0
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

    <!-- ══ 树本体 ══
         role=tree：折叠树标准无障碍语义；每行 treeitem + aria-level/expanded/
         setsize/posinset；roving tabindex（同一时刻只有一行可 Tab）。 -->
    <div class="glass rounded-md py-1 ot-tree" role="tree" aria-label="大纲树">
      <!-- 加载骨架 -->
      <div v-if="s.loading && !rows.length" class="px-3 py-3 space-y-2">
        <div class="skl" style="width: 58%; height: 12px" />
        <div class="skl" style="width: 82%; height: 10px" />
        <div class="skl" style="width: 70%; height: 10px" />
      </div>

      <template v-for="row in rows" :key="row.id">
        <!-- ── 一行节点 ── -->
        <div
          class="ot-row relative"
          :class="[
            row.kind === 'root' ? 'ot-row-root' : '',
            row.kind === 'layer' ? 'ot-row-layer' : '',
          ]"
          :style="rowStyle(row)"
          :ref="(el) => setRowEl(el, row.id)"
          role="treeitem"
          :data-row-id="row.id"
          :aria-level="row.depth + 1"
          :aria-posinset="row.posInSet"
          :aria-setsize="row.setSize"
          :aria-expanded="row.expandable ? (isOpen(row.id) ? 'true' : 'false') : undefined"
          :tabindex="row.id === activeId ? 0 : -1"
          @click="onRowClick(row)"
          @keydown="onRowKey($event, row)"
        >
          <!-- 垂直引导线：父级 → 子级 1px 竖线，每级祖先一根，
               画在该祖先三角的列中心（guideLefts = 祖先 pad + 6） -->
          <span
            v-for="(g, i) in row.guides"
            v-show="g"
            :key="'g' + i"
            class="ot-guide"
            :style="{ left: row.guideLefts[i] + 'px' }"
            aria-hidden="true"
          />

          <!-- 三角展开箭头：▸ 旋转 90° 变 ▾（只动 transform） -->
          <span
            class="ot-tri"
            :class="isOpen(row.id) ? 'open' : ''"
            aria-hidden="true"
          >▸</span>

          <!-- ── 根节点 ── -->
          <template v-if="row.kind === 'root'">
            <span class="font-semibold text-content shrink-0">📖 全书大纲树</span>
            <span class="shrink-0 text-[10px] font-mono text-content-sec/70">
              {{ row.code }}
            </span>
            <span class="ml-auto shrink-0 text-[10px] text-content-sec font-mono">
              {{ row.count }} 层
            </span>
          </template>

          <!-- ── 层节点 ── -->
          <template v-else-if="row.kind === 'layer'">
            <span
              class="shrink-0 font-mono text-[11px] font-semibold w-9"
              :class="accOf(row.rank)"
            >{{ row.code }}</span>
            <span class="shrink-0 truncate">{{ row.label }}</span>
            <span
              class="shrink-0 text-[10px] px-1.5 py-0.5 rounded-sm border"
              :class="row.badge.c"
            >{{ row.badge.t }}</span>
            <span class="ml-auto shrink-0 text-[10px] text-content-sec font-mono">
              {{ row.count }} 条
            </span>
            <!-- 层详情：抽屉里列出该层全部条目 -->
            <button
              class="ot-open shrink-0"
              type="button"
              title="查看该层详情"
              aria-label="查看该层详情"
              @click.stop="openDetail(row)"
            >⤢</button>
          </template>

          <!-- ── 条目节点 ── -->
          <template v-else>
            <span class="shrink-0 font-mono text-[10.5px] text-content-sec/90">
              {{ row.key }}
            </span>
            <span class="flex-1 min-w-0 text-content truncate">{{ row.label }}</span>
            <span
              v-if="row.dirty"
              class="shrink-0 text-[9.5px] px-1 rounded-sm border text-warning bg-warning-soft border-warning/25"
            >脏</span>
            <span class="shrink-0 text-[10px] text-content-sec/70 font-mono">
              {{ row.meta }}
            </span>
            <!-- 条目详情：右侧大抽屉（常显，触屏也能点）-->
            <button
              class="ot-open shrink-0"
              type="button"
              title="查看详情（右侧抽屉）"
              aria-label="查看详情"
              @click.stop="openDetail(row)"
            >⤢</button>
          </template>

          <!-- 层读取失败 -->
          <span
            v-if="row.kind === 'layer' && row.err"
            class="ml-2 shrink-0 text-[10px] text-danger truncate"
          >{{ row.err }}</span>
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

    <!-- ══════ 方案 C 全屏大抽屉（节点详情）══════ -->
    <Teleport to="body">
      <!-- 外层：遮罩淡入淡出（只动 opacity）；:duration 兜底，
           保证 300ms 滑出动画跑完才卸载 DOM -->
      <Transition name="cy-mask" :duration="{ enter: 200, leave: 320 }">
        <div v-if="panel" class="fixed inset-0 z-[60]">
          <!-- 关闭① 遮罩：点它关闭 -->
          <div
            class="absolute inset-0 bg-black/45"
            @click="closeDrawer"
          />

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
                class="shrink-0 w-7 h-7 rounded-lg bg-surface-card-2 text-content-sec
                       flex items-center justify-center text-[15px] active:scale-95
                       transition-transform"
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
                {{ panel.kind === 'item' ? fmtK(panelChars) + ' 字' : panelChars + ' 条' }}
              </span>

              <!-- 右上角关闭按钮 -->
              <button
                ref="closeBtn"
                class="shrink-0 w-7 h-7 rounded-lg bg-surface-card-2 text-content-sec
                       flex items-center justify-center text-[15px] active:scale-95
                       transition-transform"
                type="button"
                aria-label="关闭（Esc）"
                title="关闭（Esc）"
                @click="closeDrawer"
              >✕</button>
            </header>

            <!-- ── 抽屉内容 ── -->
            <div class="cy-drawer-md flex-1 min-h-0">
              <!-- 层面板：该层条目列表（复用已加载数据，不发新请求） -->
              <template v-if="panel.kind === 'layer'">
                <div class="flex items-center gap-2 mb-2.5">
                  <span
                    class="text-[10px] px-1.5 py-0.5 rounded-sm border"
                    :class="panel.badge.c"
                  >{{ panel.badge.t }}</span>
                  <span v-if="panel.err" class="text-[10.5px] text-danger break-all">
                    {{ panel.err }}
                  </span>
                </div>
                <div v-if="!panel.children.length" class="text-[12px] text-content-sec/70">
                  该层暂无条目
                </div>
                <button
                  v-for="it in panel.children"
                  :key="it.id"
                  class="ot-lay-item"
                  type="button"
                  @click="openItemFromLayer(it)"
                >
                  <span class="shrink-0 font-mono text-[10.5px] text-content-sec/90">
                    {{ it.key }}
                  </span>
                  <span class="flex-1 min-w-0 text-content truncate text-left">
                    {{ it.label }}
                  </span>
                  <span class="shrink-0 text-[10px] text-content-sec/70 font-mono">
                    {{ it.meta }}
                  </span>
                  <span class="shrink-0 text-content-sec/50 text-[13px]">›</span>
                </button>
              </template>

              <!-- 条目面板：Markdown 放大排版 -->
              <template v-else>
                <div v-if="!panelDetail || panelDetail.loading" class="text-[12.5px] text-content-sec">
                  读取中…
                </div>
                <div v-else-if="panelDetail.err" class="text-[12.5px] text-danger break-all">
                  {{ panelDetail.err }}
                </div>
                <div v-else-if="!panelDetail.content" class="text-[12.5px] text-content-sec/70">
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
/* ── 行：只做 background 过渡（无需合成层，不会掉帧） ── */
.ot-row {
  display: flex;
  align-items: center;
  gap: 6px;
  padding-right: 12px;
  min-height: 30px;
  cursor: pointer;
  border-radius: 6px;
  transition: background-color var(--dur-2, 200ms) var(--ease-io, ease);
}
.ot-row:hover {
  background: rgb(var(--card-2-rgb) / 0.6);
}
.ot-row:focus-visible {
  outline: 2px solid rgb(var(--primary-rgb) / 1);
  outline-offset: -2px;
}
.ot-row-root {
  font-weight: 600;
}
.ot-row-layer {
  font-weight: 500;
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

/* ── 三角箭头：▸ 展开时旋转 90° 变 ▾。只动 transform，走合成器 ── */
.ot-tri {
  display: inline-block;
  width: 12px;
  flex: 0 0 12px;
  text-align: center;
  color: rgb(var(--text-disabled-rgb) / 1);
  transition: transform var(--dur-3, 300ms) var(--ease-io, ease),
    opacity var(--dur-2, 200ms) var(--ease-io, ease);
  will-change: transform;
}
.ot-tri.open {
  transform: rotate(90deg);
}
.ot-row:hover .ot-tri,
.ot-row:focus-visible .ot-tri {
  color: rgb(var(--primary-rgb) / 1);
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
  transition: transform var(--dur-1, 100ms) var(--ease-io, ease),
    color var(--dur-2, 200ms) var(--ease-io, ease);
}
.ot-tool:active {
  transform: scale(0.95);
}
.ot-tool:hover {
  color: rgb(var(--primary-rgb) / 1);
}

/* ── 行内「详情」按钮：常显（原 hover 才现身，触屏点不到），只动 opacity/transform ── */
.ot-open {
  width: 20px;
  height: 20px;
  border-radius: 6px;
  font-size: 11px;
  line-height: 1;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  color: rgb(var(--text-sec-rgb) / 1);
  background: rgb(var(--card-2-rgb) / 0.8);
  opacity: 0.5;
  transition: opacity var(--dur-2, 200ms) var(--ease-io, ease),
    transform var(--dur-1, 100ms) var(--ease-io, ease);
}
.ot-row:hover .ot-open,
.ot-row:focus-within .ot-open,
.ot-open:focus-visible {
  opacity: 1;
  color: rgb(var(--primary-rgb) / 1);
}
.ot-open:active {
  transform: scale(0.92);
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

/* ── 抽屉正文：Markdown 大字排版 ──
   基线由 .cy-drawer-md 给（15px / 1.85），这里用双类名 + :deep 把
   MarkdownBlock 自身的 em 相对字号钉成绝对 px：标题 22 / 18 / 15px
   （比上一版 20 / 18 / 15 更大，正文 13.5 → 15px，行高仍 1.85）。 */
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
