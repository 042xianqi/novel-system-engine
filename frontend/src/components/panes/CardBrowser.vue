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
// ── 本轮新增：搜索框 + 分类 Chip 筛选 ─────────────────────────────
// ① 搜索：子串模糊命中「编号 / 标题 / 分类 / 标签 + 已缓存正文」，输入防抖 200ms
//    才写回 s.filterKey（否则每敲一个字符都要对 700 张卡重跑过滤 + 分页）。
//    ⚠ 正文命中只对「详情已被拉取过（s.detail[key].content 有缓存）」的卡生效：
//      列表接口不返回正文，为搜正文而对 700 张卡逐个回源 = 700 次请求，不可接受。
//      所以这里不做“全量正文索引”，只吃已有缓存 —— 有意取舍，写在这里而不是偷偷加请求。
// ② 分类 Chip：**单选**。理由：store 的 s.filterCategory 是单值字符串，
//    useSystem.filteredCards() 与 cardCategoryCounts() 都按单值实现；
//    改多选必须动 composable（超出本次写入范围），且会让「每个分类 N 张」的计数语义变模糊。
//    带「全部 N」入口，一键回到不筛选。
// ③ 卡片结构：大号粗体标题 → 一行小号彩色 Badge（编号/分类/字数）+ 底部微弱阴影。
//    Badge 颜色按分类名 hash → 固定色相偏移（hue-rotate 作用在语义 token --primary 上），
//    不用随机数、不写死颜色，刷新/换皮肤都稳定。
// ④ 网格：手机 1 列 / 中屏(≥640px) 2 列 / 宽屏(≥1024px) 3 列。
// ⑤ 动效只动 transform / opacity：无 transition-all；Chip 选中是瞬时换色（不对颜色做过渡），
//    只保留按压 transform。
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

// ── Badge 配色：分类 → 稳定色相偏移 ─────────────────────────────
// 基准是语义 token `--primary`（随皮肤变），再按分类 hash 做固定角度 hue-rotate。
// 同一分类 → 同一角度 → 同色；刷新不变；换皮肤仍是该皮肤的主色系，不会突兀。
// 注意：不使用随机数、不使用写死颜色。
const CAT_HUE_STEPS = [0, 28, 56, 96, 150, 196, 232, 288] // 8 档色相偏移

/** FNV-1a 字符串 hash（稳定、无随机） */
function hashStr(str) {
  let h = 2166136261
  const s0 = String(str == null ? '' : str)
  for (let i = 0; i < s0.length; i++) {
    h ^= s0.charCodeAt(i)
    h = Math.imul(h, 16777619)
  }
  return h >>> 0
}
/** 分类 → 固定色相偏移（deg），用于内联 filter */
function catHue(category) {
  const k = category || '未分类'
  return CAT_HUE_STEPS[hashStr(k) % CAT_HUE_STEPS.length]
}
function catStyle(category) {
  return { filter: `hue-rotate(${catHue(category)}deg)` }
}

/** 卡片大标题：去掉编号前缀，空则占位 */
function titleOf(c) {
  const t = String(c.title || '').replace(/^[A-Z]+-\d+-\d+[｜|]\s*/, '').trim()
  return t || '（无标题）'
}
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
          class="glass card-cyber card-item shadow-1 relative flex flex-col gap-2 p-3
                 cursor-pointer select-none outline-none border border-line/60"
          :class="{
            'reveal': revealedKey === c.key,
            'ring-2 ring-primary/60': highlightKey === c.key,
          }"
          tabindex="0"
          role="button"
          @click="onCardClick(c)"
          @keydown.enter.prevent="viewCard(c)"
          @pointerdown="onCardPointerDown(c)"
          @pointerup="onCardPointerUp"
          @pointerleave="onCardPointerUp"
          @pointercancel="onCardPointerUp"
          @contextmenu.prevent="revealedKey = c.key"
        >
          <!-- 大号粗体标题 + 状态点 -->
          <div class="flex items-start gap-1.5">
            <h3 class="card-title flex-1 min-w-0 text-content font-bold line-clamp-2">
              {{ titleOf(c) }}
            </h3>
            <span
              v-if="dot(c.note_status)"
              class="mt-1 shrink-0 w-1.5 h-1.5 rounded-full"
              :class="dot(c.note_status)"
              title="笔记状态"
            />
            <span
              v-if="c.dirty"
              class="mt-1 shrink-0 w-1.5 h-1.5 rounded-full bg-warning"
              title="待同步"
            />
          </div>

          <!-- Badge 行：编号 / 分类 / 字数（编号与分类同色：同分类同色，hash 稳定） -->
          <div class="flex items-center gap-1.5 flex-wrap">
            <span
              class="badge text-primary bg-primary/12 border-primary/30 font-mono"
              :style="catStyle(c.category)"
            >{{ c.key }}</span>
            <span
              class="badge text-primary bg-primary/12 border-primary/30"
              :style="catStyle(c.category)"
            >{{ c.category || '未分类' }}</span>
            <span class="badge text-content-sec bg-surface-card-2 border-line font-mono">
              {{ c.chars || 0 }}字
            </span>
            <span
              v-for="t in (c.tags || []).slice(0, 1)"
              :key="t"
              class="badge text-content-sec bg-surface-card-2 border-line"
            >{{ t }}</span>
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

    <!-- ══════ 详情抽屉 ══════ -->
    <Transition name="drawer">
      <div
        v-if="detailOpen"
        class="absolute inset-x-0 bottom-0 z-30 max-h-[80%] flex flex-col
               glass-strong rounded-t-xl border-t border-line"
      >
        <div class="shrink-0 flex items-center gap-2 px-3 py-2.5 border-b border-line-divider">
          <span class="font-mono text-[11px] text-primary truncate">{{ s.openKey }}</span>
          <span v-if="detail.chars" class="text-[10px] text-content-sec font-mono">
            {{ detail.chars }} 字
          </span>
          <span
            v-if="detail.category"
            class="badge text-primary bg-primary/12 border-primary/30"
            :style="catStyle(detail.category)"
          >{{ detail.category }}</span>
          <button
            class="ml-auto shrink-0 text-[11px] px-2 py-1 rounded-sm
                   bg-primary/12 text-primary border border-primary/25 active:scale-95"
            @click="openEdit"
          >编辑</button>
          <button
            class="shrink-0 text-[11px] px-2 py-1 rounded-sm
                   bg-danger-soft text-danger border border-danger/25 active:scale-95"
            @click="confirmDel = true"
          >删除</button>
          <button
            class="shrink-0 text-[12px] text-content-sec px-1.5 active:scale-95"
            @click="closeDetail"
          >✕</button>
        </div>

        <div class="flex-1 min-h-0 overflow-y-auto px-3 py-2.5">
          <div v-if="detail.loading" class="text-[11.5px] text-content-sec">读取中…</div>
          <div v-else-if="detail.err" class="text-[11.5px] text-danger break-all">
            {{ detail.err }}
          </div>
          <pre
            v-else
            class="text-[11px] text-content-sec leading-relaxed whitespace-pre-wrap
                   break-words font-mono"
          >{{ detail.content }}</pre>
        </div>

        <!-- 删除二次确认（覆盖在抽屉内，不弹浏览器 confirm） -->
        <div
          v-if="confirmDel"
          class="absolute inset-0 z-40 flex items-center justify-center bg-black/55 px-6"
        >
          <div class="glass-strong rounded-lg px-4 py-3 w-full max-w-xs space-y-3">
            <div class="text-[12.5px] text-content">确认删除这张卡？</div>
            <div class="text-[11px] text-content-sec leading-relaxed">
              <span class="font-mono text-primary">{{ s.openKey }}</span>
              的文件与索引都会移除。后端写前有快照，但删完不会自动恢复。
            </div>
            <div class="flex gap-2">
              <button
                class="flex-1 rounded-sm py-2 text-[12px] glass text-content-sec active:scale-95"
                @click="confirmDel = false"
              >取消</button>
              <button
                class="flex-1 rounded-sm py-2 text-[12px] bg-danger-soft text-danger
                       border border-danger/40 active:scale-95"
                @click="doDelete"
              >确认删除</button>
            </div>
          </div>
        </div>
      </div>
    </Transition>

    <!-- 抽屉遮罩 -->
    <Transition name="fade">
      <div
        v-if="detailOpen"
        class="absolute inset-0 z-20 bg-black/40"
        @click="closeDetail"
      />
    </Transition>

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
/* ── 网格：手机 1 列 / 中屏(≥640px) 2 列 / 宽屏(≥1024px) 3 列 ── */
.card-grid {
  display: grid;
  gap: 10px;
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

/* ── 卡片本体 ──
   「底部微弱阴影」用 Tailwind 的 shadow-1（→ var(--shadow) token），换皮肤自动跟随，
   不写死任何颜色；hover 抬升由 .card-cyber 提供（transform + 主色辉光）。 */
.card-item {
  min-height: 96px;
}

/* 大号粗体标题 */
.card-title {
  font-size: 17px;
  font-weight: 700;
  line-height: 1.35;
  word-break: break-word;
}

/* ── Badge：一行小号彩色标签 ── */
.badge {
  display: inline-flex;
  align-items: center;
  max-width: 100%;
  padding: 3px 6px;
  border-radius: 6px;
  border: 1px solid transparent;
  font-size: 10px;
  line-height: 1;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
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
  .chip {
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
