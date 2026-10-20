<script setup>
// CardBrowser.vue —— 卡片视图（L4）：搜索 + 分类 Chip 筛选 + 浏览 + 新建 + 详情 + 编辑 + 删除
//
// 数据来源（未改动，端点和字段与 useSystem.js 已核对一致）：
//   GET /api/library/index?layer=L4&limit=2000
//        → {count, matched, truncated, items:[{key,title,category,chars,dirty,tags,note_status}]}
//   GET /api/library/categories                 → 分类树（筛选条）
//   GET /api/layers/L4/item/{key}               → 单卡正文（点开才拉，拉过缓存）
//   POST/PUT/DELETE /api/layers/L4/...          → 增 / 改 / 删（见 CardEditor）
//
// 取数与分页：全部复用 useSystem.js 的 loadCards / pagedCards / filteredCards /
// cardCategoryCounts / loadCardDetail / closeCardDetail，本组件不新开任何请求。
//
// ── 本轮：卡片规范对齐「方案 C」（裁定见 docs/UI-设计方案提案.md §三/§四）──
// ① 卡片外层 .cy-card（1px 细边框、无阴影，阴影已全局禁用）；
//    标题 .cy-card-title（20px/650，一行截断），编号前缀一律剥掉。
// ② 摘要 .cy-card-summary（14px，两行截断）。
// ③ 徽章行 .cy-badges + .cy-badge（11px 主色）：依次 所属文明 / 分类 / 字数。
// ④ 内部编号（ARTS-99-902 这类）用 .cy-key：缩小变灰、默认几乎不可见，
//    hover / 长按才提亮；标题里若拼接了编号前缀，必须剥掉（见 titleOf）。
// ⑤ 文明 / 摘要字段多源容错（Operit 正在后端补 civ 与 summary）：
//    pickCiv / pickSummary 依次尝试多个字段名，取不到就给空串，
//    此时对应徽章 / 摘要不渲染（v-if 控制，不留空白占位）。
// ⑥ 保留：搜索框、分类 Chip、分页、编辑 / 删除、以及 <CardDetailDrawer> 引用（未改动）。
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { systemData, pushLog, toast } from '../../store.js'
import {
  loadCards,
  pagedCards,
  filteredCards,
  cardCategoryCounts,
  loadCardDetail,
  closeCardDetail,
  removeCard,
} from '../../composables/useSystem.js'
import CardEditor from './CardEditor.vue'
import CardDetailDrawer from './CardDetailDrawer.vue'

const s = systemData.cards
const busy = ref(false)
const confirmDel = ref(false)

/** 编辑器：'' | 'create' | 'edit' */
const editorMode = ref('')
const editorKey = ref('')

const counts = computed(() => cardCategoryCounts())
/** 「全部 N」里的 N：已加载的 L4 卡总数 */
const totalCards = computed(() => s.items.length)

const detailOpen = computed(() => !!s.openKey)
const detail = computed(() => (s.openKey ? s.detail[s.openKey] || {} : {}))

/**
 * 当前抽屉打开的那张卡在**列表**里的元数据（title/category/tags）。
 * ⚠ 已核对：详情接口 GET /api/layers/L4/item/{key} 只回 {content, chars}，
 *   **没有 category/title**。旧代码在抽屉里写 `v-if="detail.category"`，
 *   永远为 undefined → 抽屉的分类徽章其实从来没显示过（本轮顺手修正：
 *   改为从列表项 openItem 取真实 category）。
 */
const openItem = computed(() => s.items.find((c) => c.key === s.openKey) || {})

/**
 * 新建/保存后要「定位到」的那张卡的 key。
 * L4 已有 700+ 张，按 key 排序后新卡可能落在第 18 页——
 * 只把页码拉回第一页等于让用户自己翻 18 页去找，体验不成立。
 * 所以这里反算页码 + 高亮 + 滚动定位。
 */
const highlightKey = ref('')

// ── 搜索：输入框本地态 → 200ms 防抖 → s.filterKey（store 为唯一真源）──
const searchText = ref(s.filterKey || '')
let debTimer = null
const kw = computed(() => s.filterKey.trim().toLowerCase())

/**
 * 命中判定：编号 / 标题 / 分类 / 标签 子串模糊匹配，
 * 再叠加「已缓存正文」命中（见文件头 ① 的取舍说明）。
 * ⚠ 编号虽然默认不显眼，但**仍可被搜索**（key 一直在 hay 里），功能不回退。
 */
function hit(c, k) {
  const hay = `${c.key || ''} ${c.title || ''} ${c.category || ''} ${(c.tags || []).join(' ')}`
  if (hay.toLowerCase().includes(k)) return true
  const d = s.detail[c.key]
  const body = d && d.content ? String(d.content).toLowerCase() : ''
  return !!body && body.includes(k)
}

/**
 * 搜索态下的命中集合。
 * 分类规则与 useSystem.filteredCards() 完全一致（cat && c.category !== cat 即淘汰）；
 * 之所以在搜索态自算一遍，是因为 filteredCards() 的关键词只覆盖 key/title/category，
 * 会把「标题没命中、正文命中」的卡提前剔掉，正文搜索就永远搜不到。
 */
const searched = computed(() => {
  const k = kw.value
  if (!k) return filteredCards()
  const cat = s.filterCategory
  const pool = cat ? s.items.filter((c) => c.category === cat) : s.items
  return pool.filter((c) => hit(c, k))
})

/** 分页切片：语义与 useSystem.pagedCards() 一致（同 s.page / s.pageSize） */
function paginate(list) {
  const start = s.page * s.pageSize
  return {
    total: list.length,
    slice: list.slice(start, start + s.pageSize),
    pageCount: Math.max(1, Math.ceil(list.length / s.pageSize)),
  }
}
/** 无关键词：直接用 useSystem 的分页；搜索态：同语义本地切片（见 searched 注释） */
const page = computed(() => (kw.value ? paginate(searched.value) : pagedCards()))
/** 是否有任何筛选在生效（用于空态文案 / 清空按钮 / 统计后缀） */
const activeFilter = computed(() => !!(kw.value || s.filterCategory))

/** Chip 选中/未选中样式（选中态只写语义 token，不写死颜色） */
const CHIP_ON = 'bg-primary/12 text-primary border-primary/30'
const CHIP_OFF = 'text-content-sec bg-surface-card-2 border-line'

// ── 人类语言字段（多源容错）─────────────────────────────────────
/**
 * 卡片大标题：去掉编号前缀（"ARTS-99-902｜太玄真经" → "太玄真经"），空则占位。
 * 编号前缀的多种写法都吃掉：全角/半角竖线、冒号、空格。
 */
function titleOf(c) {
  const t = String((c && c.title) || '')
    .replace(/^[A-Z][A-Z0-9]*-\d+-\d+\s*[｜|:：-]?\s*/, '')
    .trim()
  return t || '（无标题）'
}

/**
 * 所属文明 —— 多源容错：依次尝试 item.civ → item.civilization → item.nation → item.civ_name。
 * ⚠ 只接受**字符串**取值：绝不读 `item.civ.title` 这类深层路径，
 *   避免后端某字段是对象时 `.title` 为 undefined 而报错；
 *   对象 / 数字 / null 一律按「取不到」处理 → 返回空串。
 * TODO(后端): civ/summary 字段上线后自动渲染
 */
function pickCiv(item) {
  if (!item) return ''
  const v = item.civ ?? item.civilization ?? item.nation ?? item.civ_name
  return typeof v === 'string' ? v.trim() : ''
}

/**
 * 摘要 / 一句话定位 —— 多源容错：依次尝试
 *   item.summary → item.one_liner → item.descr → item.note。
 * 同样只接受字符串，取不到返回空串（UI 不渲染这一行，不占位、不误导）。
 * 注：列表接口当前不返回正文，为拿摘要而对 700+ 张卡逐个回源不可接受，故不再做正文兜底。
 * TODO(后端): civ/summary 字段上线后自动渲染
 */
function pickSummary(item) {
  if (!item) return ''
  const v = item.summary ?? item.one_liner ?? item.descr ?? item.note
  return typeof v === 'string' ? v.trim() : ''
}

/** 抽屉标题：优先用列表里的标题去前缀，再退回 key */
const openTitle = computed(() => {
  const it = openItem.value
  return it && it.title ? titleOf(it) : s.openKey || '卡片'
})
/** 抽屉摘要（同样多源容错，取不到则空） */
const openSummary = computed(() => pickSummary(openItem.value))

function dot(st) {
  if (st === 'suspect') return 'bg-warning'
  if (st === 'bad') return 'bg-danger'
  if (st === 'ok') return 'bg-success'
  return ''
}

// ── 筛选交互 ──
/** 分类 Chip：单选，再点同一个 = 取消（回到「全部」） */
function pickCategory(c) {
  s.filterCategory = s.filterCategory === c ? '' : c
  s.page = 0
  revealedKey.value = ''
}
/** 清空搜索框（立刻生效，不等防抖） */
function clearSearch() {
  clearTimeout(debTimer)
  searchText.value = ''
  s.filterKey = ''
  s.page = 0
  revealedKey.value = ''
}
/** 一次性清掉搜索 + 分类 */
function clearAll() {
  clearSearch()
  s.filterCategory = ''
  s.page = 0
}
function setPage(p) {
  s.page = Math.max(0, Math.min(p, page.value.pageCount - 1))
  revealedKey.value = ''
}

function openCard(c) {
  loadCardDetail(c.key)
}
function closeDetail() {
  closeCardDetail()
  confirmDel.value = false
}

// ── 操作按钮的显隐：桌面 hover / 移动长按 ────────────────────────
// revealedKey：移动端长按后要露出操作按钮的那张卡。
// CSS 里 `@media (hover:hover)` 负责桌面 hover 显隐；这里只处理长按。
// 编号（.cy-key）也用同一套显隐：默认很淡，hover / 长按 / 聚焦时提亮。
const revealedKey = ref('')
let pressTimer = null
let suppressClick = false

function onCardPointerDown(c) {
  suppressClick = false
  clearTimeout(pressTimer)
  pressTimer = setTimeout(() => {
    revealedKey.value = c.key
    suppressClick = true // 长按已触发，抑制随后的 click（否则会误开详情）
  }, 460)
}
function onCardPointerUp() {
  clearTimeout(pressTimer)
}
function onCardClick(c) {
  if (suppressClick) {
    suppressClick = false
    return
  }
  // 点其它卡片时收起已露出的操作
  if (revealedKey.value && revealedKey.value !== c.key) revealedKey.value = ''
  openCard(c)
}
function viewCard(c) {
  revealedKey.value = ''
  loadCardDetail(c.key)
}
function editCard(c) {
  revealedKey.value = ''
  editorKey.value = c.key
  editorMode.value = 'edit'
}

// ── 新建 / 编辑 ──
function openCreate() {
  editorKey.value = ''
  editorMode.value = 'create'
}
function openEdit() {
  if (!s.openKey) return
  editorKey.value = s.openKey
  editorMode.value = 'edit'
}
function onEditorClose() {
  editorMode.value = ''
  editorKey.value = ''
}
function onSaved(ev) {
  pushLog(
    ev.mode === 'create' ? `已新建卡片 ${ev.key}` : `已保存卡片 ${ev.key}`,
    'ok'
  )
  toast(ev.mode === 'create' ? '卡片已创建' : '已保存')
  // useSystem 里已失效缓存并重拉（loadCards(true) 已在 createCard/updateCard 内 await 完成）。
  // 但「重拉到了」不等于「用户看得见」——700 多张卡按 key 排序，
  // 新建的 TEST-99-903 落在第 18 页。所以这里必须反算页码。
  if (ev.mode === 'edit') {
    // 编辑：原卡就在当前筛选结果里，留在原页即可，重开详情让用户看新内容
    loadCardDetail(ev.key)
    highlightKey.value = ev.key
    nextTick(() => scrollToHighlight())
    return
  }
  // 新建：清掉可能把新卡过滤掉的筛选条件（否则新卡被筛掉 = 永远找不到）
  const inFiltered = searched.value.some((c) => c.key === ev.key)
  if (!inFiltered) {
    clearAll()
  }
  const idx = searched.value.findIndex((c) => c.key === ev.key)
  if (idx >= 0) {
    s.page = Math.floor(idx / s.pageSize)
  } else {
    s.page = 0
  }
  highlightKey.value = ev.key
  nextTick(() => scrollToHighlight())
}
/** 把高亮的那张卡滚到视野内（若在本页） */
function scrollToHighlight() {
  if (!highlightKey.value) return
  const el = document.querySelector(`[data-card-key="${highlightKey.value}"]`)
  if (el && el.scrollIntoView) {
    el.scrollIntoView({ block: 'center', behavior: 'smooth' })
  }
}

// ── 删除（二次确认）──
async function doDelete() {
  if (!s.openKey) return
  const k = s.openKey
  const r = await removeCard(k)
  if (!r.ok) {
    toast(r.error || '删除失败', true)
    return
  }
  confirmDel.value = false
  pushLog(`已删除卡片 ${k}`, 'warn')
  toast('已删除')
}

async function reload() {
  busy.value = true
  await loadCards(true)
  busy.value = false
  pushLog('卡片列表已刷新', 'info')
}

// ── 监听 ──
// 输入 → 防抖 200ms → 再写回 store（写回后由下面的 filterKey watch 统一收口）
watch(searchText, (v) => {
  clearTimeout(debTimer)
  debTimer = setTimeout(() => {
    if (s.filterKey === v) return
    s.filterKey = v
  }, 200)
})
// 关键词变化（不论来自输入还是外部清理）→ 回第一页 + 收起长按露出的操作 + 同步输入框
watch(() => s.filterKey, (v, old) => {
  if (v !== searchText.value) searchText.value = v
  if (v !== old) {
    s.page = 0
    revealedKey.value = ''
  }
})
// 切分类 → 回第一页
watch(() => s.filterCategory, () => {
  s.page = 0
  revealedKey.value = ''
})
// 关掉抽屉时顺手清掉删除确认
watch(() => s.openKey, (v) => { if (!v) confirmDel.value = false })
// 翻页后收起露出的操作按钮
watch(() => s.page, () => { revealedKey.value = '' })

onMounted(() => loadCards())
onBeforeUnmount(() => {
  clearTimeout(pressTimer)
  clearTimeout(debTimer)
})
</script>

<template>
  <div class="h-full relative flex flex-col bg-surface">
    <!-- ── 顶部筛选区（不随列表滚动）── -->
    <div
      class="shrink-0 px-3 pt-3 pb-2 space-y-2 bg-surface/70 backdrop-blur z-10
             border-b border-line-divider"
    >
      <!-- 搜索 + 刷新 -->
      <div class="flex items-center gap-2">
        <div class="flex-1 relative">
          <input
            v-model="searchText"
            type="text"
            class="w-full bg-surface-card-2 rounded-sm pl-7 pr-7 py-2 text-[12.5px]
                   text-content outline-none border border-line focus:border-primary/60"
            placeholder="搜标题 / 正文 / 编号…"
            spellcheck="false"
            autocomplete="off"
            enterkeyhint="search"
            aria-label="搜索卡片（标题、正文、编号）"
          />
          <span
            class="absolute left-2 top-1/2 -translate-y-1/2 text-[12px] text-content-sec/60"
            aria-hidden="true"
          >⌕</span>
          <button
            v-if="searchText"
            class="absolute right-1.5 top-1/2 -translate-y-1/2 text-[11px] text-content-sec px-1"
            aria-label="清空搜索"
            @click="clearSearch"
          >✕</button>
        </div>
        <button
          class="shrink-0 text-[11px] text-content-sec px-2 py-2 glass rounded-sm
                 transition-transform active:scale-95 disabled:opacity-40"
          :disabled="busy"
          @click="reload"
        >{{ busy ? '…' : '刷新' }}</button>
      </div>

      <!-- 分类 chips（横向可滑，单选） -->
      <div
        class="flex gap-1.5 overflow-x-auto pb-0.5 -mx-0.5 px-0.5"
        role="group"
        aria-label="按分类筛选"
      >
        <button
          class="chip shrink-0 text-[11px] px-2 py-1 rounded-sm border"
          :class="!s.filterCategory ? CHIP_ON : CHIP_OFF"
          :aria-pressed="!s.filterCategory"
          @click="pickCategory('')"
        >全部 <span class="font-mono opacity-60">{{ totalCards }}</span></button>
        <button
          v-for="[name, n] in counts"
          :key="name"
          class="chip shrink-0 text-[11px] px-2 py-1 rounded-sm border whitespace-nowrap"
          :class="s.filterCategory === name ? CHIP_ON : CHIP_OFF"
          :aria-pressed="s.filterCategory === name"
          @click="pickCategory(name)"
        >{{ name }} <span class="font-mono opacity-60">{{ n }}</span></button>
      </div>

      <!-- 结果统计 -->
      <div class="flex items-center gap-2 text-[10px] text-content-sec/70" aria-live="polite">
        <span>
          共 <span class="font-mono text-content-sec">{{ page.total }}</span> 张
          <span v-if="activeFilter">（已筛选）</span>
          <span v-if="s.truncated" class="text-warning ml-1">
            · 已加载 {{ s.count }} / 共 {{ s.matched }} 张
          </span>
          <span v-if="highlightKey" class="text-primary ml-1">· 已定位 {{ highlightKey }}</span>
        </span>
        <span class="ml-auto flex items-center gap-2">
          <button
            v-if="activeFilter"
            class="text-[10px] text-primary transition-transform active:scale-95"
            @click="clearAll"
          >清空筛选</button>
          <span v-if="page.pageCount > 1" class="font-mono">
            {{ s.page + 1 }} / {{ page.pageCount }}
          </span>
        </span>
      </div>
    </div>

    <!-- ── 错误 ── -->
    <div
      v-if="s.err"
      class="mx-3 mt-2 rounded-md px-3 py-2 text-[11px] bg-danger-soft text-danger break-all"
    >{{ s.err }}</div>

    <!-- ── 卡片网格 ── -->
    <div class="flex-1 min-h-0 overflow-y-auto px-3 pt-3 pb-24">
      <div class="card-grid">
        <div
          v-for="c in page.slice"
          :key="c.key"
          :data-card-key="c.key"
          class="cy-card card-item relative flex flex-col cursor-pointer select-none outline-none"
          :class="{
            'reveal': revealedKey === c.key,
            'ring-2 ring-primary/60': highlightKey === c.key,
          }"
          tabindex="0"
          role="button"
          :aria-label="`卡片 ${titleOf(c)}（编号 ${c.key}）`"
          @click="onCardClick(c)"
          @keydown.enter.prevent="viewCard(c)"
          @pointerdown="onCardPointerDown(c)"
          @pointerup="onCardPointerUp"
          @pointerleave="onCardPointerUp"
          @pointercancel="onCardPointerUp"
          @contextmenu.prevent="revealedKey = c.key"
        >
          <!-- ① 大标题（.cy-card-title：20px/650，一行截断）+ 状态点 -->
          <div class="flex items-start gap-2">
            <h3 class="cy-card-title flex-1 min-w-0">{{ titleOf(c) }}</h3>
            <span
              v-if="dot(c.note_status)"
              class="mt-1.5 shrink-0 w-1.5 h-1.5 rounded-full"
              :class="dot(c.note_status)"
              title="笔记状态"
            />
            <span
              v-if="c.dirty"
              class="mt-1.5 shrink-0 w-1.5 h-1.5 rounded-full bg-warning"
              title="待同步"
            />
          </div>

          <!-- ② 摘要（.cy-card-summary：14px，两行截断）
               后端 civ/summary 字段上线后自动渲染；取不到则不渲染（v-if，不占位） -->
          <p v-if="pickSummary(c)" class="cy-card-summary">{{ pickSummary(c) }}</p>

          <!-- ③ 徽章行（.cy-badges + .cy-badge，11px 主色）：依次 所属文明 / 分类 / 字数
               文明为新补字段（多源容错），取不到该徽章不渲染；分类同理；字数始终有值 -->
          <div class="cy-badges">
            <span v-if="pickCiv(c)" class="cy-badge">{{ pickCiv(c) }}</span>
            <span v-if="c.category" class="cy-badge">{{ c.category }}</span>
            <span class="cy-badge">{{ c.chars || 0 }} 字</span>
          </div>

          <!-- ④ 内部编号：.cy-key（缩小变灰、默认几乎不可见，hover / 长按才提亮，见全局样式） -->
          <div class="mt-auto pt-3 flex items-center">
            <span class="cy-key truncate">{{ c.key }}</span>
          </div>

          <!-- 操作按钮：仅 hover（桌面）/ 长按（移动端）出现 -->
          <div class="card-actions absolute right-2 top-2 flex items-center gap-1.5">
            <button
              class="act text-primary bg-surface-card border border-primary/30 active:scale-95"
              @click.stop="viewCard(c)"
            >查看</button>
            <button
              class="act text-content bg-surface-card border border-line active:scale-95"
              @click.stop="editCard(c)"
            >编辑</button>
          </div>
        </div>
      </div>

      <!-- 空态 -->
      <div
        v-if="!s.loading && !page.total"
        class="glass rounded-md px-4 py-8 text-center mt-2"
      >
        <div class="text-[12px] text-content-sec">没有匹配的卡片</div>
        <div class="text-[10.5px] text-content-sec/60 mt-1.5">
          {{ activeFilter ? '试试清空筛选条件' : '点右下角 + 新建第一张' }}
        </div>
        <button
          v-if="activeFilter"
          class="mt-3 text-[11px] px-2 py-1 rounded-sm bg-primary/12 text-primary
                 border border-primary/30 transition-transform active:scale-95"
          @click="clearAll"
        >清空筛选</button>
      </div>

      <div v-if="s.loading && !s.items.length" class="text-center text-[11px] text-content-sec py-8">
        加载卡片中…
      </div>

      <!-- 分页 -->
      <div v-if="page.pageCount > 1" class="flex items-center justify-center gap-3 mt-3">
        <button
          class="text-[11px] px-3 py-1.5 glass rounded-sm text-content-sec transition-transform active:scale-95 disabled:opacity-30"
          :disabled="s.page === 0"
          @click="setPage(s.page - 1)"
        >上一页</button>
        <span class="text-[10.5px] text-content-sec font-mono">
          {{ s.page + 1 }} / {{ page.pageCount }}
        </span>
        <button
          class="text-[11px] px-3 py-1.5 glass rounded-sm text-content-sec transition-transform active:scale-95 disabled:opacity-30"
          :disabled="s.page >= page.pageCount - 1"
          @click="setPage(s.page + 1)"
        >下一页</button>
      </div>
    </div>

    <!-- ══════ 悬浮「+ 新建卡片」══════ -->
    <button
      v-if="!editorMode"
      class="absolute right-4 z-20 w-12 h-12 rounded-full
             bg-primary text-on-primary shadow-lg flex items-center justify-center
             text-[22px] leading-none font-light active:scale-90 transition-transform"
      style="bottom: calc(1rem + env(safe-area-inset-bottom))"
      title="新建卡片"
      @click="openCreate"
    >+</button>

    <!-- ══════ 详情抽屉（2026-10-20：由内联底部抽屉改为主子任务交付的
         右侧大抽屉组件 CardDetailDrawer；自带遮罩/Esc/点击关闭，故不再套
         外层 Transition 或 v-if） ══════ -->
    <CardDetailDrawer @saved="onSaved" />

    <!-- ══════ 编辑器（全屏浮层）══════ -->
    <Transition name="pane-left">
      <CardEditor
        v-if="editorMode"
        :mode="editorMode"
        :card-key="editorKey"
        :preset-category="s.filterCategory"
        @close="onEditorClose"
        @saved="onSaved"
      />
    </Transition>
  </div>
</template>

<style scoped>
/* ── 网格：手机 1 列 / 中屏(≥640px) 2 列 / 宽屏(≥1024px) 3 列 ──
   卡片间距 12px（= Tailwind gap-3）*/
.card-grid {
  display: grid;
  gap: 12px;
  grid-template-columns: 1fr;
}
@media (min-width: 640px) {
  .card-grid {
    grid-template-columns: repeat(2, minmax(0, 1fr));
  }
}
@media (min-width: 1024px) {
  .card-grid {
    grid-template-columns: repeat(3, minmax(0, 1fr));
  }
}

/* ── 卡片容器：视觉（边框 / 背景 / 内距 / 圆角）全部交给全局 .cy-card，
   这里只补布局（弹性列 + 最小高度），以及 hover / 长按时的「操作按钮」显隐钩子。 */
.card-item {
  min-height: 128px;
}

/* 内部编号提亮：全局 .cy-key 已覆盖 hover / :active；
   键盘聚焦（focus-within）与移动端长按（.reveal）在这里补齐，
   与「默认几乎不可见、hover / 长按才提亮」一致。 */
.card-item:focus-within .cy-key { opacity: 1; }
.card-item.reveal .cy-key {
  opacity: 1;
  color: rgb(var(--primary-rgb) / 0.85);
}

/* ── Chip：只做按压 transform；选中态靠瞬时换色（不对颜色做过渡）── */
.chip {
  transition: transform 150ms ease;
}
.chip:active {
  transform: scale(0.95);
}

/* ── 操作按钮：默认不占视觉 ── */
.card-actions {
  opacity: 0;
  transform: translateY(-2px);
  pointer-events: none;
  transition:
    opacity 180ms ease,
    transform 180ms ease;
}
.act {
  font-size: 11px;
  line-height: 1;
  padding: 4px 8px;
  border-radius: 7px;
  transition: transform 150ms ease;
}

/* 桌面：hover / 键盘聚焦时露出 */
@media (hover: hover) {
  .card-item:hover .card-actions {
    opacity: 1;
    transform: none;
    pointer-events: auto;
  }
}
.card-item:focus-within .card-actions {
  opacity: 1;
  transform: none;
  pointer-events: auto;
}
/* 移动端：长按（或右键）露出 */
.card-item.reveal .card-actions {
  opacity: 1;
  transform: none;
  pointer-events: auto;
}

/* 尊重「减少动态效果」偏好 */
@media (prefers-reduced-motion: reduce) {
  .card-actions,
  .act,
  .chip,
  .cy-key {
    transition: none;
  }
}

/* 抽屉滑入动画（transform/opacity，符合 DESIGN-v2 动效约束） */
.drawer-enter-active,
.drawer-leave-active {
  transition:
    transform 240ms cubic-bezier(0.22, 1, 0.36, 1),
    opacity 240ms ease;
}
.drawer-enter-from,
.drawer-leave-to {
  transform: translateY(100%);
  opacity: 0;
}
.fade-enter-active,
.fade-leave-active {
  transition: opacity 200ms ease;
}
.fade-enter-from,
.fade-leave-to {
  opacity: 0;
}
</style>
