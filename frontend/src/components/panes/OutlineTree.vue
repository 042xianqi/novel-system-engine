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
//        （/api/layers/{L}/item/{key}，点开看正文时按需拉）
//   展开态沿用 store 的 s.expanded（切页不丢），未新增任何字段。
//
// 本轮（outline2）相对上一版的**小步完善**（不动数据链路）：
//   ① 引导线定位修正：竖线画在「每一级祖先三角」的列中心（GUIDE_OFFSET），
//      层级越深越对齐；条数不变（每行 = 祖先数，仍随折叠态变化，全展开时 54 条）。
//   ② 无障碍补全：role=tree / treeitem + aria-level / aria-expanded /
//      aria-setsize / aria-posinset；roving tabindex（同一时刻只有 1 个可 Tab 行）。
//   ③ 键盘导航：Enter/空格 展收（等价点击）、↑↓ 上下移动、→ 展开或进首个子孙、
//      ← 折叠或回到父级、Home/End 首尾。焦点用元素 Map 精确定位，不用 scrollIntoView 硬拉。
//   ④ 修掉一处设计契约违规：漏斗进度条原来对「全部属性」做 transition 并动 width，
//      改为 transform: scaleX()（DESIGN-v2 §3：只准动 transform / opacity）。
//   ⑤ 颜色改用 DESIGN-v2 §1 语义 token（surface / content / line / primary /
//      success / warning / danger），与 tailwind.config.js 的语义色一一对应。
//   ⑥ 折叠分支不进 DOM：可见行由 rows 拍平数组产出（折叠的子树根本不进数组），
//      条目正文/悬浮卡片一律 v-if —— 600+ 条 L4 折叠时不产生任何节点。
// ══════════════════════════════════════════════════════════
import { computed, nextTick, onMounted, reactive, ref } from 'vue'
import { systemData, pushLog } from '../../store.js'
import { loadOutline, loadOutlineItem } from '../../composables/useSystem.js'

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
// 条目正文：懒加载 + 缓存（行内展开 & 半屏卡片共用同一份数据）
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

// ── 半屏悬浮卡片（保留旧版交互：大段正文用卡片看更舒服）──
const sheetRow = ref(null)
async function openSheet(row) {
  sheetRow.value = row
  await ensureDetail(row.layer, row)
}
function closeSheet() {
  sheetRow.value = null
}
const sheetData = computed(() =>
  sheetRow.value ? detailOf(sheetRow.value.id) : null
)

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

onMounted(() => loadOutline())
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
            <!-- 大段正文改到卡片看；stop 避免同时触发展收 -->
            <button
              class="ot-open shrink-0"
              type="button"
              title="在卡片中打开"
              aria-label="在卡片中打开"
              @click.stop="openSheet(row)"
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

    <!-- ══════ 条目悬浮卡片（半屏抽屉，沿用旧版交互）══════ -->
    <Teleport to="body">
      <Transition name="ot-card">
        <div v-if="sheetRow" class="fixed inset-0 z-[56]">
          <div
            class="ot-mask absolute inset-0 bg-black/45 backdrop-blur-[2px]"
            @click="closeSheet"
          />
          <div
            class="ot-sheet absolute left-2 right-2 bottom-2 max-h-[74vh]
                   glass-strong rounded-2xl overflow-hidden flex flex-col
                   shadow-2 ring-1 ring-line/50"
          >
            <div
              class="shrink-0 flex items-center gap-2 px-3.5 py-2.5
                     border-b border-line/60"
            >
              <span class="font-mono text-[11px] text-primary shrink-0">
                {{ sheetRow.key }}
              </span>
              <span
                class="flex-1 min-w-0 text-[12.5px] font-semibold text-content truncate"
              >{{ sheetRow.label }}</span>
              <span class="shrink-0 text-[10px] text-content-sec/70 font-mono">
                {{ fmtK(sheetData?.chars || 0) }} 字
              </span>
              <button
                class="shrink-0 w-7 h-7 rounded-lg bg-surface-card-2 text-content-sec
                       flex items-center justify-center text-[15px] active:scale-95
                       transition-transform"
                @click="closeSheet"
                aria-label="关闭"
              >✕</button>
            </div>
            <div class="flex-1 min-h-0 overflow-y-auto px-3.5 py-3">
              <div v-if="!sheetData || sheetData.loading" class="text-[11.5px] text-content-sec">
                读取中…
              </div>
              <div v-else-if="sheetData.err" class="text-[11.5px] text-danger break-all">
                {{ sheetData.err }}
              </div>
              <div v-else-if="!sheetData.content" class="text-[11.5px] text-content-sec/70">
                （空）
              </div>
              <pre v-else class="ot-content">{{ sheetData.content }}</pre>
              <div
                v-if="sheetRow.source"
                class="mt-2 text-[10px] text-content-sec/45 font-mono break-all"
              >{{ sheetRow.source }}</div>
            </div>
          </div>
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

/* ── 条目行内的「打开卡片」按钮：hover / 聚焦才现身（opacity 过渡）── */
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
  opacity: 0;
  transition: opacity var(--dur-2, 200ms) var(--ease-io, ease),
    transform var(--dur-1, 100ms) var(--ease-io, ease);
}
.ot-row:hover .ot-open,
.ot-row:focus-within .ot-open,
.ot-open:focus-visible {
  opacity: 1;
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

/* ── 悬浮卡片：底部上滑 + 遮罩淡入（只用 transform / opacity）── */
.ot-card-enter-active .ot-mask,
.ot-card-leave-active .ot-mask {
  transition: opacity 220ms ease;
}
.ot-card-enter-active .ot-sheet {
  transition: transform 320ms cubic-bezier(0.22, 1.4, 0.36, 1),
    opacity 200ms ease;
}
.ot-card-leave-active .ot-sheet {
  transition: transform 200ms cubic-bezier(0.4, 0, 1, 1), opacity 170ms ease;
}
.ot-card-enter-from .ot-mask,
.ot-card-leave-to .ot-mask {
  opacity: 0;
}
.ot-card-enter-from .ot-sheet,
.ot-card-leave-to .ot-sheet {
  opacity: 0;
  transform: translate3d(0, 100%, 0) scale(0.98);
}
.ot-sheet {
  padding-bottom: env(safe-area-inset-bottom, 0px);
  will-change: transform;
}

/* ── 骨架屏 ── */
.skl {
  border-radius: 4px;
  background: rgb(var(--divider-rgb) / 0.8);
}
</style>
