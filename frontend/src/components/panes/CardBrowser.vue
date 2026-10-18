<script setup>
// CardBrowser.vue —— 卡片视图（L4）：浏览 + 新建 + 详情 + 编辑 + 删除
//
// 数据来源：
//   GET /api/library/index?layer=L4&limit=800  → 一次拉全量，前端过滤+分页
//   GET /api/library/categories                → 分类树（筛选条）
//   GET /api/layers/L4/item/{key}              → 单卡正文（点开才拉，拉过缓存）
//   POST/PUT/DELETE /api/layers/L4/...         → 增 / 改 / 删（见 CardEditor）
//
// 为什么前端过滤而不是服务端分页：
//   L4 约 500~700 张，一次拉的 JSON 体积可接受（实测 < 200KB）。
//   好处是切分类 / 搜关键词**零延迟**。代价是首屏一次请求，但缓存后不重拉。
//
// ── 本轮新增（2026-10-17）──────────────────────────────
//   · 右下角悬浮「+ 新建卡片」
//   · 详情抽屉里加「编辑」「删除」
//   · 保存/删除后由 useSystem 统一失效缓存并重拉 → 切回本页立刻看到变化
import { computed, nextTick, onMounted, ref, watch } from 'vue'
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
/**
 * 新建/保存后要「定位到」的那张卡的 key。
 * L4 已有 700+ 张，按 key 排序后新卡可能落在第 18 页——
 * 只把页码拉回第一页等于让用户自己翻 18 页去找，体验不成立。
 * 所以这里反算页码 + 高亮 + 滚动定位。
 */
const highlightKey = ref('')
const page = computed(() => pagedCards())

const detailOpen = computed(() => !!s.openKey)
const detail = computed(() => (s.openKey ? s.detail[s.openKey] || {} : {}))

const TAG_COLORS = [
  'text-acc bg-acc/12',
  'text-ok bg-ok/12',
  'text-warn bg-warn/12',
  'text-bad bg-bad/12',
]
function tagCls(t) {
  let h = 0
  for (let i = 0; i < String(t).length; i++) h = (h * 31 + String(t).charCodeAt(i)) | 0
  return TAG_COLORS[Math.abs(h) % TAG_COLORS.length]
}

function noteDot(st) {
  if (st === 'suspect') return 'bg-warn'
  if (st === 'bad') return 'bg-bad'
  if (st === 'ok') return 'bg-ok'
  return ''
}

function pickCategory(c) {
  s.filterCategory = s.filterCategory === c ? '' : c
  s.page = 0
}
function setPage(p) {
  s.page = Math.max(0, Math.min(p, page.value.pageCount - 1))
}

async function openCard(c) {
  await loadCardDetail(c.key)
}
function closeDetail() {
  closeCardDetail()
  confirmDel.value = false
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
  const inFiltered = filteredCards().some((c) => c.key === ev.key)
  if (!inFiltered) {
    s.filterKey = ''
    s.filterCategory = ''
  }
  const idx = filteredCards().findIndex((c) => c.key === ev.key)
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

// 关键词变化 → 回第一页
watch(() => s.filterKey, () => { s.page = 0 })
// 关掉抽屉时顺手清掉删除确认
watch(() => s.openKey, (v) => { if (!v) confirmDel.value = false })

onMounted(() => loadCards())
</script>

<template>
  <div class="h-full relative flex flex-col">
    <!-- ── 顶部筛选区（不随列表滚动）── -->
    <div class="shrink-0 px-3 pt-3 pb-2 space-y-2 bg-ink-900/60 backdrop-blur z-10">
      <!-- 搜索 + 刷新 -->
      <div class="flex items-center gap-2">
        <div class="flex-1 relative">
          <input
            v-model="s.filterKey"
            class="w-full bg-ink-700 rounded-sm pl-7 pr-2 py-1.5 text-[12px]
                   text-fg outline-none border border-ink-600 focus:border-acc/60"
            placeholder="搜编号 / 标题 / 分类…"
            spellcheck="false"
          />
          <span class="absolute left-2 top-1/2 -translate-y-1/2 text-[11px] text-fg-dim/60">⌕</span>
          <button
            v-if="s.filterKey"
            class="absolute right-1.5 top-1/2 -translate-y-1/2 text-[11px] text-fg-dim px-1"
            @click="s.filterKey = ''"
          >✕</button>
        </div>
        <button
          class="shrink-0 text-[11px] text-fg-dim px-2 py-1.5 glass rounded-sm active:scale-95 disabled:opacity-40"
          :disabled="busy"
          @click="reload"
        >{{ busy ? '…' : '刷新' }}</button>
      </div>

      <!-- 分类 chips（横向滚动） -->
      <div class="flex gap-1.5 overflow-x-auto pb-0.5 -mx-0.5 px-0.5">
        <button
          class="shrink-0 text-[11px] px-2 py-1 rounded-sm border transition-colors"
          :class="!s.filterCategory
            ? 'text-acc bg-acc/12 border-acc/30'
            : 'text-fg-dim bg-ink-700/60 border-ink-600'"
          @click="pickCategory('')"
        >全部 <span class="font-mono opacity-60">{{ s.items.length }}</span></button>
        <button
          v-for="[name, n] in counts"
          :key="name"
          class="shrink-0 text-[11px] px-2 py-1 rounded-sm border transition-colors whitespace-nowrap"
          :class="s.filterCategory === name
            ? 'text-acc bg-acc/12 border-acc/30'
            : 'text-fg-dim bg-ink-700/60 border-ink-600'"
          @click="pickCategory(name)"
        >{{ name }} <span class="font-mono opacity-60">{{ n }}</span></button>
      </div>

      <!-- 结果统计 -->
      <div class="flex items-center gap-2 text-[10px] text-fg-dim/70">
        <span>
          共 <span class="font-mono text-fg-dim">{{ page.total }}</span> 张
          <span v-if="s.filterCategory || s.filterKey">（已筛选）</span>
          <span v-if="s.truncated" class="text-warn ml-1">
            · 已加载 {{ s.count }} / 共 {{ s.matched }} 张
          </span>
          <span v-if="highlightKey" class="text-acc ml-1">· 已定位 {{ highlightKey }}</span>
        </span>
        <span v-if="page.pageCount > 1" class="ml-auto font-mono">
          {{ s.page + 1 }} / {{ page.pageCount }}
        </span>
      </div>
    </div>

    <!-- ── 错误 ── -->
    <div
      v-if="s.err"
      class="mx-3 mb-2 rounded-md px-3 py-2 text-[11px] bg-bad/10 text-bad break-all"
    >{{ s.err }}</div>

    <!-- ── 卡片网格 ── -->
    <div class="flex-1 min-h-0 overflow-y-auto px-3 pb-20">
      <div class="grid grid-cols-2 gap-2">
        <button
          v-for="c in page.slice"
          :key="c.key"
          :data-card-key="c.key"
          class="glass rounded-md px-2.5 py-2 text-left active:scale-[0.98]
                 transition-transform flex flex-col gap-1 min-h-[74px]"
          :class="highlightKey === c.key ? 'ring-2 ring-acc shadow-[0_0_0_3px_rgba(120,200,255,0.12)]' : ''"
          @click="openCard(c)"
        >
          <div class="flex items-center gap-1.5">
            <span class="font-mono text-[10px] text-acc truncate">{{ c.key }}</span>
            <span
              v-if="noteDot(c.note_status)"
              class="shrink-0 w-1.5 h-1.5 rounded-full"
              :class="noteDot(c.note_status)"
            />
            <span v-if="c.dirty" class="shrink-0 w-1.5 h-1.5 rounded-full bg-warn" />
          </div>
          <div class="text-[11.5px] text-fg leading-snug line-clamp-2">
            {{ (c.title || '').replace(/^[A-Z]+-\d+-\d+｜/, '') || '（无标题）' }}
          </div>
          <div class="mt-auto flex items-center gap-1.5 flex-wrap">
            <span class="text-[9.5px] px-1 py-0.5 rounded-sm bg-ink-600/60 text-fg-dim">
              {{ c.category || '未分类' }}
            </span>
            <span class="text-[9.5px] text-fg-dim/60 font-mono">{{ c.chars }}字</span>
            <span
              v-for="t in (c.tags || []).slice(0, 1)"
              :key="t"
              class="text-[9.5px] px-1 py-0.5 rounded-sm"
              :class="tagCls(t)"
            >{{ t }}</span>
          </div>
        </button>
      </div>

      <!-- 空态 -->
      <div
        v-if="!s.loading && !page.total"
        class="glass rounded-md px-4 py-8 text-center mt-2"
      >
        <div class="text-[12px] text-fg-dim">没有匹配的卡片</div>
        <div class="text-[10.5px] text-fg-dim/60 mt-1.5">
          {{ s.filterKey || s.filterCategory ? '试试清空筛选条件' : '点右下角 + 新建第一张' }}
        </div>
      </div>

      <div v-if="s.loading && !s.items.length" class="text-center text-[11px] text-fg-dim py-8">
        加载卡片中…
      </div>

      <!-- 分页 -->
      <div v-if="page.pageCount > 1" class="flex items-center justify-center gap-3 mt-3">
        <button
          class="text-[11px] px-3 py-1.5 glass rounded-sm text-fg-dim active:scale-95 disabled:opacity-30"
          :disabled="s.page === 0"
          @click="setPage(s.page - 1)"
        >上一页</button>
        <span class="text-[10.5px] text-fg-dim font-mono">
          {{ s.page + 1 }} / {{ page.pageCount }}
        </span>
        <button
          class="text-[11px] px-3 py-1.5 glass rounded-sm text-fg-dim active:scale-95 disabled:opacity-30"
          :disabled="s.page >= page.pageCount - 1"
          @click="setPage(s.page + 1)"
        >下一页</button>
      </div>
    </div>

    <!-- ══════ 悬浮「+ 新建卡片」══════ -->
    <button
      v-if="!editorMode"
      class="absolute right-4 bottom-4 z-20 w-12 h-12 rounded-full
             bg-gradient-to-br from-[#1e4a6d] to-[#2b6690] text-[#cfe6f7]
             shadow-glow-acc flex items-center justify-center
             text-[22px] leading-none font-light
             active:scale-90 transition-transform"
      title="新建卡片"
      @click="openCreate"
    >+</button>

    <!-- ══════ 详情抽屉 ══════ -->
    <Transition name="drawer">
      <div
        v-if="detailOpen"
        class="absolute inset-x-0 bottom-0 z-30 max-h-[80%] flex flex-col
               glass-strong rounded-t-xl border-t border-ink-600 shadow-3"
      >
        <div class="shrink-0 flex items-center gap-2 px-3 py-2.5 border-b border-ink-600/60">
          <span class="font-mono text-[11px] text-acc truncate">{{ s.openKey }}</span>
          <span v-if="detail.chars" class="text-[10px] text-fg-dim font-mono">
            {{ detail.chars }} 字
          </span>
          <button
            class="ml-auto shrink-0 text-[11px] px-2 py-1 rounded-sm
                   bg-acc/12 text-acc border border-acc/25 active:scale-95"
            @click="openEdit"
          >编辑</button>
          <button
            class="shrink-0 text-[11px] px-2 py-1 rounded-sm
                   bg-bad/12 text-bad border border-bad/25 active:scale-95"
            @click="confirmDel = true"
          >删除</button>
          <button
            class="shrink-0 text-[12px] text-fg-dim px-1.5 active:scale-95"
            @click="closeDetail"
          >✕</button>
        </div>

        <div class="flex-1 min-h-0 overflow-y-auto px-3 py-2.5">
          <div v-if="detail.loading" class="text-[11.5px] text-fg-dim">读取中…</div>
          <div v-else-if="detail.err" class="text-[11.5px] text-bad break-all">
            {{ detail.err }}
          </div>
          <pre
            v-else
            class="text-[11px] text-fg-dim leading-relaxed whitespace-pre-wrap
                   break-words font-mono"
          >{{ detail.content }}</pre>
        </div>

        <!-- 删除二次确认（覆盖在抽屉内，不弹浏览器 confirm） -->
        <div
          v-if="confirmDel"
          class="absolute inset-0 z-40 flex items-center justify-center bg-black/55 px-6"
        >
          <div class="glass rounded-lg px-4 py-3 w-full max-w-xs space-y-3">
            <div class="text-[12.5px] text-fg">确认删除这张卡？</div>
            <div class="text-[11px] text-fg-dim leading-relaxed">
              <span class="font-mono text-acc">{{ s.openKey }}</span>
              的文件与索引都会移除。后端写前有快照，但删完不会自动恢复。
            </div>
            <div class="flex gap-2">
              <button
                class="flex-1 rounded-sm py-2 text-[12px] glass text-fg-dim active:scale-95"
                @click="confirmDel = false"
              >取消</button>
              <button
                class="flex-1 rounded-sm py-2 text-[12px] bg-bad/20 text-bad
                       border border-bad/40 active:scale-95"
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