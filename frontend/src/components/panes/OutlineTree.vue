<script setup>
// OutlineTree.vue —— 大纲视图（L0→L5 折叠树 / Tree View）
//
// ══════════════════════════════════════════════════════════
// 数据来源（**完全沿用原有加载链路，未新增任何端点**）：
//   · loadOutline()      → systemData.outline.levels
//        levels[i] = { id, name, items[], count, err, status, done, file }
//        items[j]  = { layer, key, title, chars, source, dirty }
//        （/api/layers/{L}/items?limit=200 + /api/pipeline/funnel，见 useSystem.js）
//   · loadOutlineItem(L,key) → { content, chars }
//        （/api/layers/{L}/item/{key}，点开看正文时按需拉）
//
// 与旧版的差异（只改「怎么看」，不改「读什么」）：
//   ① 层头手风琴 → 真正的折叠树：三角 ▸/▾、缩进层级、1px 垂直引导线
//   ② 层按 L0→L5 阶梯升序排布，字号/透明度/色深随层级递减 → 一眼可辨
//   ③ 新增「全部展开 / 全部折叠」
//   ④ 条目正文：行内懒加载展开（点击节点即展开/收起），保留原来的半屏悬浮卡片
//   ⑤ 折叠的分支用 v-if 不渲染，展开树的可见行也只在 rows 里 —— 大树不卡
// ══════════════════════════════════════════════════════════
import { computed, onMounted, reactive, ref } from 'vue'
import { systemData, pushLog } from '../../store.js'
import { loadOutline, loadOutlineItem } from '../../composables/useSystem.js'

const s = systemData.outline
const busy = ref(false)

// ── 展开态 ──
//   沿用 store 里的 s.expanded（切页不丢），id 规则见下：
//     ROOT             根节点
//     L:L3             层节点
//     I:L3:卷01        条目节点
//   isOpen 带默认值兜底：第一次进来（s.expanded 里没有这些 key）时
//   根 + 所有层默认展开、条目默认收起，首屏立刻有结构可看。
function isOpen(id) {
  const v = s.expanded[id]
  if (v === undefined) return id === ROOT_ID || id.startsWith('L:')
  return !!v
}
function toggleNode(id) {
  s.expanded[id] = !isOpen(id)
}

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
  { fs: '13px', op: 1, cls: 'text-fg' }, // L0
  { fs: '12.5px', op: 0.94, cls: 'text-fg' }, // L0.5
  { fs: '12px', op: 0.88, cls: 'text-fg' }, // L1
  { fs: '11.8px', op: 0.82, cls: 'text-fg-dim' }, // L1x
  { fs: '11.6px', op: 0.78, cls: 'text-fg-dim' }, // L2
  { fs: '11.4px', op: 0.74, cls: 'text-fg-dim' }, // L3
  { fs: '11.2px', op: 0.7, cls: 'text-fg-dim' }, // L4
  { fs: '11px', op: 0.66, cls: 'text-fg-dim' }, // L5
]
const RANK_FALLBACK = { fs: '11px', op: 0.6, cls: 'text-fg-dim' }
// 层编号徽章的主色档位：越靠上游越亮，越往下游越淡
const RANK_ACC = [
  'text-acc',
  'text-acc',
  'text-acc',
  'text-fg',
  'text-fg-dim',
  'text-fg-dim',
  'text-fg-dim',
  'text-fg-dim',
]
const RANK_ACC_FALLBACK = 'text-fg-dim'

// 缩进：每层 14px；同级层之间再按 rank 微推 4px，让 L0→L5 呈阶梯下潜
const INDENT = 14
const RANK_STEP = 4
function rankOffset(rank) {
  return rank > 0 ? rank * RANK_STEP : 0
}
function styleOf(rank) {
  return RANK_STYLE[rank] || RANK_FALLBACK
}
function accOf(rank) {
  return RANK_ACC[rank] || RANK_ACC_FALLBACK
}

/** 每层头部状态徽章（沿用旧版逻辑与文案） */
function badge(L) {
  if (L.err) return { t: '读取失败', c: 'text-bad bg-bad/12 border-bad/25' }
  if (L.done) return { t: '已完成', c: 'text-ok bg-ok/12 border-ok/25' }
  const st = String(L.status || '')
  if (st.startsWith('chapters')) {
    return {
      t: st.replace('chapters_', '章节 '),
      c: 'text-warn bg-warn/12 border-warn/25',
    }
  }
  if (st) return { t: st, c: 'text-fg-dim bg-ink-600/40 border-ink-600' }
  return { t: '未开始', c: 'text-fg-dim bg-ink-600/40 border-ink-600' }
}

function fmtK(n) {
  if (!n) return '0'
  return n >= 10000 ? (n / 10000).toFixed(1) + '万' : String(n)
}

// ══════════════════════════════════════════════════════
// 树：根 → 层（L0→L5 升序）→ 条目
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
      expandable: true, // 见下方：点击行内懒加载展开正文
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
 * 拍平成「可见行」。折叠分支直接不进数组 → DOM 里根本不存在，
 * 所以 600+ 条 L4 也不会有渲染压力（折叠时只渲染可见的几十行）。
 * guides[i] = 第 i 层祖先是否需要画那根 1px 竖线（不是最后一个兄弟就要续线）。
 */
const rows = computed(() => {
  const out = []
  const walk = (node, depth, guides, isLast) => {
    out.push({
      ...node,
      depth,
      guides,
      pad: depth * INDENT + rankOffset(node.rank),
    })
    if (!isOpen(node.id)) return
    const kids = node.children || []
    if (!kids.length) return
    // 根是主干，永远续线；其余节点只有「后面还有兄弟」时才续线
    const line = depth === 0 ? true : !isLast
    const childGuides = [...guides, line]
    kids.forEach((c, i) =>
      walk(c, depth + 1, childGuides, i === kids.length - 1)
    )
  }
  walk(rootNode.value, 0, [], true)
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
function guideStyle(row, i) {
  return { left: i * INDENT + rankOffset(row.rank) + 'px' }
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
}
function collapseAll() {
  for (const id of allIds.value) s.expanded[id] = false
  // 根保持展开，否则整棵树会「消失」，用户找不到入口
  s.expanded[ROOT_ID] = true
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
  toggleNode(row.id)
  if (row.kind === 'item' && isOpen(row.id)) ensureDetail(row.layer, row)
}

/** 键盘可达性：Enter / Space 等价点击 */
function onRowKey(e, row) {
  if (e.key === 'Enter' || e.key === ' ' || e.key === 'Spacebar') {
    e.preventDefault()
    onRowClick(row)
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
        <span class="text-[12px] text-fg">漏斗进度</span>
        <span class="font-mono text-[12px] text-acc">
          {{ s.funnelProgress || '—' }}
        </span>
        <button
          class="ml-auto text-[11px] text-fg-dim active:scale-95 disabled:opacity-40"
          :disabled="busy"
          @click="reload"
        >
          {{ busy ? '刷新中…' : '刷新' }}
        </button>
      </div>
      <!-- 进度条（宽度变化是 paint，非 transform，但只在数据刷新时发生一次，可接受） -->
      <div class="h-1 rounded-full bg-ink-600 overflow-hidden">
        <div
          class="h-full bg-gradient-to-r from-acc to-ok transition-all duration-500"
          :style="{
            width:
              s.funnelDone && (s.levels || []).length
                ? Math.min(100, (s.funnelDone / 7) * 100) + '%'
                : '0%',
          }"
        />
      </div>
      <div class="text-[10px] text-fg-dim/70 leading-relaxed">
        L0 概括 → L1 总纲 → L2 分卷 → L3 细纲 → L4 资料 → L5 正文；越往上游越抽象
      </div>
    </div>

    <div
      v-if="s.err"
      class="rounded-md px-3 py-2 text-[11px] bg-bad/10 text-bad break-all"
    >
      {{ s.err }}
    </div>

    <!-- ── 树工具条：全部展开 / 全部折叠 ── -->
    <div class="flex items-center gap-2">
      <span class="text-[11px] text-fg-dim">大纲树</span>
      <span class="text-[10px] text-fg-dim/60 font-mono">
        {{ rows.length }} 行可见
      </span>
      <div class="ml-auto flex items-center gap-1.5">
        <button
          class="ot-tool"
          type="button"
          @click="expandAll"
        >
          全部展开
        </button>
        <button
          class="ot-tool"
          type="button"
          @click="collapseAll"
        >
          全部折叠
        </button>
      </div>
    </div>

    <!-- ══ 树本体 ══
         role=tree：折叠树的标准无障碍语义；每行是 treeitem + aria-level/expanded。 -->
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
            'ot-' + row.kind,
          ]"
          :style="rowStyle(row)"
          role="treeitem"
          :aria-level="row.depth + 1"
          :aria-expanded="row.expandable ? isOpen(row.id) : undefined"
          tabindex="0"
          @click="onRowClick(row)"
          @keydown="onRowKey($event, row)"
        >
          <!-- 垂直引导线：父级到子级 1px 竖线，每个祖先层一根 -->
          <span
            v-for="(g, i) in row.guides"
            v-show="g"
            :key="'g' + i"
            class="ot-guide"
            :style="guideStyle(row, i)"
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
            <span class="font-semibold text-fg shrink-0">📖 全书大纲树</span>
            <span class="shrink-0 text-[10px] font-mono text-fg-dim/70">
              {{ row.code }}
            </span>
            <span class="ml-auto shrink-0 text-[10px] text-fg-dim font-mono">
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
            <span class="ml-auto shrink-0 text-[10px] text-fg-dim font-mono">
              {{ row.count }} 条
            </span>
          </template>

          <!-- ── 条目节点 ── -->
          <template v-else>
            <span class="shrink-0 font-mono text-[10.5px] text-fg-dim/90">
              {{ row.key }}
            </span>
            <span class="flex-1 min-w-0 text-fg truncate">{{ row.label }}</span>
            <span
              v-if="row.dirty"
              class="shrink-0 text-[9.5px] px-1 rounded-sm border text-warn bg-warn/12 border-warn/25"
            >脏</span>
            <span class="shrink-0 text-[10px] text-fg-dim/70 font-mono">
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
            class="ml-2 shrink-0 text-[10px] text-bad truncate"
          >{{ row.err }}</span>
        </div>

        <!-- ── 条目展开后的正文（懒加载，独立一行，缩进 + 引导线下方）── -->
        <div
          v-if="row.kind === 'item' && isOpen(row.id)"
          class="ot-body"
          :style="{ paddingLeft: (row.depth + 1) * INDENT + rankOffset(row.rank) + 'px' }"
        >
          <div v-if="!detailOf(row.id) || detailOf(row.id).loading" class="text-fg-dim">
            读取中…
          </div>
          <div v-else-if="detailOf(row.id).err" class="text-bad break-all">
            {{ detailOf(row.id).err }}
          </div>
          <div
            v-else-if="!detailOf(row.id).content"
            class="text-fg-dim/70"
          >（空）</div>
          <pre
            v-else
            class="ot-content"
          >{{ detailOf(row.id).content }}</pre>
          <div
            v-if="row.source"
            class="mt-1 text-[10px] text-fg-dim/45 font-mono truncate"
          >{{ row.source }}</div>
        </div>
      </template>

      <div
        v-if="!s.loading && rows.length <= 1"
        class="px-3 py-3 text-[11px] text-fg-dim/70"
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
                   shadow-[0_18px_60px_rgba(0,0,0,.55)] ring-1 ring-white/10"
          >
            <div
              class="shrink-0 flex items-center gap-2 px-3.5 py-2.5
                     border-b border-ink-600/60"
            >
              <span class="font-mono text-[11px] text-acc shrink-0">
                {{ sheetRow.key }}
              </span>
              <span
                class="flex-1 min-w-0 text-[12.5px] font-semibold text-fg truncate"
              >{{ sheetRow.label }}</span>
              <span class="shrink-0 text-[10px] text-fg-dim/70 font-mono">
                {{ fmtK(sheetData?.chars || 0) }} 字
              </span>
              <button
                class="shrink-0 w-7 h-7 rounded-lg bg-ink-700/70 text-fg-dim
                       flex items-center justify-center text-[15px] active:scale-95
                       transition-transform"
                @click="closeSheet"
                aria-label="关闭"
              >✕</button>
            </div>
            <div class="flex-1 min-h-0 overflow-y-auto px-3.5 py-3">
              <div v-if="!sheetData || sheetData.loading" class="text-[11.5px] text-fg-dim">
                读取中…
              </div>
              <div v-else-if="sheetData.err" class="text-[11.5px] text-bad break-all">
                {{ sheetData.err }}
              </div>
              <div v-else-if="!sheetData.content" class="text-[11.5px] text-fg-dim/70">
                （空）
              </div>
              <pre v-else class="ot-content">{{ sheetData.content }}</pre>
              <div
                v-if="sheetRow.source"
                class="mt-2 text-[10px] text-fg-dim/45 font-mono break-all"
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
  outline: 2px solid var(--primary);
  outline-offset: -2px;
}
.ot-row-root {
  font-weight: 600;
  cursor: default;
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
  background: var(--divider, rgb(var(--divider-rgb) / 1));
  pointer-events: none;
}

/* ── 三角箭头：▸ 展开时旋转 90° 变 ▾。只动 transform，走合成器 ── */
.ot-tri {
  display: inline-block;
  width: 12px;
  flex: 0 0 12px;
  text-align: center;
  color: var(--text-disabled, rgb(var(--text-sec-rgb) / 1));
  transition: transform var(--dur-3, 300ms) var(--ease-io, ease),
    opacity var(--dur-2, 200ms) var(--ease-io, ease);
  will-change: transform;
}
.ot-tri.open {
  transform: rotate(90deg);
}
.ot-row:hover .ot-tri,
.ot-row:focus-visible .ot-tri {
  color: var(--primary);
}

/* ── 工具条按钮 ── */
.ot-tool {
  font-size: 10.5px;
  line-height: 1;
  padding: 4px 8px;
  border-radius: 6px;
  color: var(--text-sec, rgb(var(--text-sec-rgb) / 1));
  background: rgb(var(--card-2-rgb) / 0.7);
  border: 1px solid rgb(var(--border-rgb) / 0.9);
  transition: transform var(--dur-1, 100ms) var(--ease-io, ease),
    color var(--dur-2, 200ms) var(--ease-io, ease);
}
.ot-tool:active {
  transform: scale(0.95);
}
.ot-tool:hover {
  color: var(--primary);
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
  color: var(--text-sec, rgb(var(--text-sec-rgb) / 1));
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
  color: var(--text-sec, rgb(var(--text-sec-rgb) / 1));
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
