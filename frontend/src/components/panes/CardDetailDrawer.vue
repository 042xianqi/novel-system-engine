<script setup>
// CardDetailDrawer.vue —— 卡片详情：右侧全屏大抽屉（方案 C）
// ══════════════════════════════════════════════════════════════
// 重构目标：把「卡片详情」统一为方案 C 的**全屏大抽屉**：
//   · 容器 .cy-drawer（style.css 骨架层）＝ min(85vw, 640px) + 右滑入
//     translateX(100%) → 0 / 300ms / cubic-bezier(.22,1,.36,1)，玻璃底内建。
//   · 正文 .cy-drawer-md ＝ 15px / 行高 1.85，h1 22px、h2 18px、h3 15px。
//   · Markdown 复用 MarkdownBlock.vue（marked + 净化 + 代码高亮，不新增依赖）。
//   · 三重关闭：遮罩点击 / 右上角 ✕ / Esc。
//   · 打开时 document.body.style.overflow='hidden' 锁背景滚动，关闭恢复；
//     **组件卸载时也恢复**（否则 drawer 打开时切页会永久锁死滚动）。
//   · 动效只用 transform / opacity（DESIGN-v2 §3）。
//
// 组件边界（沿用 CardBrowser 既有约定，不另开请求）：
//   · 详情数据来源 = store.js 的 systemData.cards.openKey + detail[key]
//     （由 useSystem.js 的 loadCardDetail() 写入缓存，本组件只读展示）。
//   · 关闭走 useSystem.js 的 closeCardDetail()（等价于把 openKey 置空）。
//   · 保存 / 删除复用 useSystem.js 的 updateCard() / removeCard()
//     （二者内部已负责失效缓存并重拉列表，见 useSystem.js 注释）。
//   对外 props/emit 契约保持不变：emits = ['close','saved','deleted']。
//
// 保留原有全部功能：字段编辑（标题 + 正文）、保存、删除（二次确认）、关闭。
// ══════════════════════════════════════════════════════════════
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { systemData, toast, pushLog } from '../../store.js'
import { closeCardDetail, updateCard, removeCard } from '../../composables/useSystem.js'
import MarkdownBlock from '../MarkdownBlock.vue'

const emit = defineEmits(['close', 'saved', 'deleted'])

const s = systemData.cards

/** 抽屉是否打开：完全由 store 的 openKey 决定（与 CardBrowser 同源） */
const open = computed(() => !!s.openKey)
/** 当前卡详情缓存：{loading, err, content, chars} */
const detail = computed(() => (s.openKey ? s.detail[s.openKey] || {} : {}))
const rawContent = computed(() => String(detail.value.content || ''))

/** 列表项（用于取分类 / 标题；详情接口只回正文） */
const listItem = computed(() => s.items.find((x) => x.key === s.openKey) || null)
const category = computed(() => listItem.value?.category || '')

/** 大标题：优先取列表标题，去掉编号前缀；缺失则回退正文首个 # 标题 */
const title = computed(() => {
  const fromList = String(listItem.value?.title || '')
    .replace(/^[A-Z]+-\d+-\d+[｜|]\s*/, '')
    .trim()
  if (fromList) return fromList
  const m = rawContent.value.match(/^\s*#\s+(.+)$/m)
  if (m) return m[1].replace(/^[A-Z]+-\d+-\d+[｜|]\s*/, '').trim()
  return '（无标题）'
})

// ── 分类 badge 色相：与 CardBrowser 同规则（同分类同色、无随机）──
const CAT_HUE_STEPS = [0, 28, 56, 96, 150, 196, 232, 288]
function hashStr(str) {
  let h = 2166136261
  const s0 = String(str == null ? '' : str)
  for (let i = 0; i < s0.length; i++) {
    h ^= s0.charCodeAt(i)
    h = Math.imul(h, 16777619)
  }
  return h >>> 0
}
const catStyle = computed(() => {
  const k = category.value || '未分类'
  const deg = CAT_HUE_STEPS[hashStr(k) % CAT_HUE_STEPS.length]
  return { filter: `hue-rotate(${deg}deg)` }
})

// ── 内联编辑态（保留「字段编辑 + 保存」）────────────────────────
const editing = ref(false)
const fKey = ref('')
const fTitle = ref('')
const fContent = ref('')
const saving = ref(false)
const err = ref('')

const charCount = computed(() => fContent.value.replace(/\s/g, '').length)

function startEdit() {
  if (!s.openKey || detail.value.loading) return
  editing.value = true
  confirmDel.value = false
  err.value = ''
  fKey.value = s.openKey
  fTitle.value = title.value === '（无标题）' ? '' : title.value
  fContent.value = rawContent.value
}
function cancelEdit() {
  editing.value = false
  err.value = ''
}

async function saveEdit() {
  err.value = ''
  if (!fContent.value.trim()) {
    err.value = '正文不能为空'
    return
  }
  saving.value = true
  try {
    const r = await updateCard(fKey.value, {
      title: fTitle.value.trim(),
      content: fContent.value,
    })
    if (!r.ok) {
      err.value = r.error || '保存失败'
      return
    }
    editing.value = false
    emit('saved', { mode: 'edit', key: fKey.value })
  } catch (e) {
    err.value = String(e.message || e)
  } finally {
    saving.value = false
  }
}

// ── 删除（抽屉内二次确认，不弹浏览器 confirm）──────────────────
const confirmDel = ref(false)
const deleting = ref(false)
async function doDelete() {
  if (!s.openKey) return
  const k = s.openKey
  deleting.value = true
  try {
    const r = await removeCard(k)
    if (!r.ok) {
      toast(r.error || '删除失败', true)
      return
    }
    confirmDel.value = false
    pushLog(`已删除卡片 ${k}`, 'warn')
    toast('已删除')
    emit('deleted', { key: k })
  } finally {
    deleting.value = false
  }
}

// ── 关闭：走 useSystem 约定 ────────────────────────────────────
function close() {
  if (saving.value || deleting.value) return
  editing.value = false
  confirmDel.value = false
  err.value = ''
  closeCardDetail()
  emit('close')
}

// ── Esc 关闭（编辑/删除确认态优先回退一级）─────────────────────
function onKeydown(e) {
  if (e.key !== 'Escape' || !open.value) return
  if (confirmDel.value) {
    confirmDel.value = false
    return
  }
  if (editing.value) {
    cancelEdit()
    return
  }
  close()
}

// ── 背景滚动锁：打开锁 body，关闭 / 卸载都恢复 ────────────────
// 先记录进入前的取值再恢复（不覆盖别人写的 overflow），并用
// scrollLocked 标记保证幂等；onBeforeUnmount 兜底，
// 绝不把 overflow:hidden 永久留在 body 上（常见坑）。
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
// open 变化即同步锁（immediate 兜住「挂载时已是打开态」）
watch(open, (v) => setScrollLock(v), { immediate: true })

onMounted(() => {
  window.addEventListener('keydown', onKeydown)
  setScrollLock(open.value)
})
onBeforeUnmount(() => {
  window.removeEventListener('keydown', onKeydown)
  // 卸载兜底：组件被 v-if 摘掉时同样恢复背景滚动
  setScrollLock(false)
})

// 切换卡片时退出编辑/确认态，避免残留
watch(() => s.openKey, () => {
  editing.value = false
  confirmDel.value = false
  err.value = ''
})

// 进入编辑态时聚焦正文
function focusBody(el) {
  if (el) nextTick(() => el.focus())
}
</script>

<template>
  <!-- Teleport 到 body：避免被 pane 里 transform / backdrop-filter 祖先
       劫持 fixed 定位（否则 fixed 抽屉会被裁在内容区里） -->
  <Teleport to="body">
    <!-- 外层：遮罩淡入淡出（只动 opacity）。:duration 兜底，
         保证 300ms 滑出动画跑完才卸载 DOM。 -->
    <Transition name="cy-mask" :duration="{ enter: 200, leave: 320 }">
      <div v-if="open" class="cdd-root fixed inset-0 z-[60]">
        <!-- ── 关闭① 遮罩点击 ── -->
        <div class="absolute inset-0 bg-black/45" @click="close" />

        <!-- ── 方案 C 全屏大抽屉：.cy-drawer（min(85vw,640px) / 右滑入 300ms / 玻璃底内建）── -->
        <Transition name="cy-drawer" appear>
          <aside
            class="cdd-panel cy-drawer"
            role="dialog"
            aria-modal="true"
            :aria-label="`卡片详情 ${s.openKey}`"
          >
            <!-- ══ 头部：编号 / 字数 / 分类 + 大标题 + 关闭② ✕ ══ -->
            <header class="shrink-0 px-4 pt-3.5 pb-3 border-b border-line-divider space-y-1.5">
              <div class="flex items-center gap-2">
                <span class="font-mono text-[11px] text-primary truncate">{{ s.openKey }}</span>
                <span
                  v-if="detail.chars"
                  class="text-[10px] text-content-sec font-mono shrink-0"
                >{{ detail.chars }} 字</span>
                <span
                  v-if="category"
                  class="shrink-0 inline-flex items-center px-1.5 py-0.5 rounded-[6px]
                         text-[10px] leading-none border border-primary/30
                         bg-primary/12 text-primary"
                  :style="catStyle"
                >{{ category }}</span>
                <button
                  class="ml-auto shrink-0 w-7 h-7 grid place-items-center rounded-md
                         text-[15px] leading-none text-content-sec
                         hover:text-content active:scale-90 transition-transform"
                  aria-label="关闭"
                  @click="close"
                >✕</button>
              </div>
              <h2 class="cdd-title text-[20px] leading-snug font-bold text-content break-words">
                {{ title }}
              </h2>
            </header>

            <!-- ══ 正文（查看 / 内联编辑）：.cy-drawer-md 大字排版 ══ -->
            <div class="cdd-body cy-drawer-md flex-1 min-h-0">
              <!-- 编辑态 -->
              <div v-if="editing" class="space-y-3">
                <label class="block space-y-1">
                  <span class="text-[10px] text-content-sec">编号（已存在的卡不能改）</span>
                  <input
                    :value="fKey"
                    disabled
                    class="w-full bg-surface-card-2 rounded-md px-2.5 py-1.5 text-[12px] font-mono
                           text-content border border-line disabled:opacity-50"
                    spellcheck="false"
                  />
                </label>
                <label class="block space-y-1">
                  <span class="text-[10px] text-content-sec">标题（不含编号前缀）</span>
                  <input
                    v-model="fTitle"
                    class="w-full bg-surface-card-2 rounded-md px-2.5 py-1.5 text-[13px]
                           text-content border border-line outline-none focus:border-primary/60"
                    placeholder="白鸦（柒玖）"
                    spellcheck="false"
                  />
                </label>
                <label class="block space-y-1">
                  <span class="flex items-center gap-2 text-[10px] text-content-sec">
                    <span>正文（Markdown）</span>
                    <span class="ml-auto font-mono opacity-60">{{ charCount }} 字</span>
                  </span>
                  <textarea
                    :ref="focusBody"
                    v-model="fContent"
                    rows="18"
                    class="w-full bg-surface-card-2 rounded-md px-2.5 py-2 text-[12.5px] font-mono
                           text-content border border-line outline-none focus:border-primary/60
                           leading-relaxed resize-y min-h-[40vh]"
                    placeholder="# 编号｜标题&#10;&#10;> 胶囊：一句话概括&#10;&#10;## 详细信息&#10;…"
                    spellcheck="false"
                  />
                </label>
                <div
                  v-if="err"
                  class="rounded-md px-3 py-2 text-[11px] bg-danger-soft text-danger break-all"
                >{{ err }}</div>
                <div class="text-[10px] text-content-sec/60 leading-relaxed">
                  保存时后端会写前快照（可回滚），并把条目登记进 library_index。
                </div>
              </div>

              <!-- 查看态：Markdown 大字排版（复用 MarkdownBlock） -->
              <template v-else>
                <div v-if="detail.loading" class="text-[12px] text-content-sec py-8 text-center">
                  读取原文中…
                </div>
                <div
                  v-else-if="detail.err"
                  class="rounded-md px-3 py-2 text-[12px] bg-danger-soft text-danger break-all"
                >{{ detail.err }}</div>
                <MarkdownBlock v-else :source="detail.content" class="cdd-md" />
              </template>
            </div>

            <!-- ══ 底部操作条：查看态 [编辑][删除] / 编辑态 [取消][保存] ══ -->
            <footer
              class="shrink-0 px-4 py-2.5 border-t border-line-divider flex items-center gap-2"
              style="padding-bottom: calc(0.625rem + env(safe-area-inset-bottom))"
            >
              <template v-if="editing">
                <button
                  class="rounded-md px-3 py-2 text-[12px] glass text-content-sec active:scale-95"
                  :disabled="saving"
                  @click="cancelEdit"
                >取消</button>
                <button
                  class="ml-auto rounded-md px-4 py-2 text-[12px]
                         bg-primary/15 text-primary border border-primary/30
                         active:scale-95 disabled:opacity-40"
                  :disabled="saving"
                  @click="saveEdit"
                >{{ saving ? '保存中…' : '保存' }}</button>
              </template>
              <template v-else>
                <button
                  class="rounded-md px-3 py-2 text-[12px]
                         bg-primary/12 text-primary border border-primary/25 active:scale-95
                         disabled:opacity-40"
                  :disabled="detail.loading || !!detail.err"
                  @click="startEdit"
                >编辑</button>
                <button
                  class="rounded-md px-3 py-2 text-[12px]
                         bg-danger-soft text-danger border border-danger/25 active:scale-95"
                  @click="confirmDel = true"
                >删除</button>
                <button
                  class="ml-auto rounded-md px-3 py-2 text-[12px] glass text-content-sec active:scale-95"
                  @click="close"
                >关闭</button>
              </template>
            </footer>

            <!-- ══ 删除二次确认（覆盖在抽屉内）══ -->
            <div
              v-if="confirmDel"
              class="absolute inset-0 z-40 flex items-center justify-center bg-black/55 px-6"
            >
              <div class="glass-strong rounded-xl px-4 py-3 w-full max-w-xs space-y-3">
                <div class="text-[13px] text-content">确认删除这张卡？</div>
                <div class="text-[11px] text-content-sec leading-relaxed">
                  <span class="font-mono text-primary">{{ s.openKey }}</span>
                  的文件与索引都会移除。后端写前有快照，但删完不会自动恢复。
                </div>
                <div class="flex gap-2">
                  <button
                    class="flex-1 rounded-md py-2 text-[12px] glass text-content-sec active:scale-95"
                    :disabled="deleting"
                    @click="confirmDel = false"
                  >取消</button>
                  <button
                    class="flex-1 rounded-md py-2 text-[12px] bg-danger-soft text-danger
                           border border-danger/40 active:scale-95 disabled:opacity-40"
                    :disabled="deleting"
                    @click="doDelete"
                  >{{ deleting ? '删除中…' : '确认删除' }}</button>
                </div>
              </div>
            </div>
          </aside>
        </Transition>
      </div>
    </Transition>
  </Teleport>
</template>

<style scoped>
/* ── 抽屉本体：尺寸 / 底色 / 玻璃全部来自 style.css 的 .cy-drawer，
      这里只补方案 C 规范「抽屉左侧 0 右侧 12px」的圆角与渲染提示 ── */
.cdd-panel {
  border-radius: 0 12px 12px 0;
  overflow: hidden;
  will-change: transform;
}

/* ── 正文容器：.cy-drawer-md 负责 15px / 1.85 / padding 20px / overflow-y ── */
.cdd-body {
  overscroll-behavior: contain;
}

/* ── Markdown 大字排版 ──
   MarkdownBlock 自身用 em 相对字号（h1 1.35em / h2 1.2em / h3 1.08em），
   这里以 .cdd-body【父级】为锚点 + :deep 提高优先级，把标题钉成绝对 px
   （22 / 18 / 15，比 em 折算后的实际字号更大更稳），正文 15px/1.85 由
   .cy-drawer-md 提供基线。 */
.cdd-body :deep(.md-block h1) {
  font-size: 22px;
  font-weight: 650;
  line-height: 1.4;
  margin: 18px 0 10px;
}
.cdd-body :deep(.md-block h2) {
  font-size: 18px;
  font-weight: 620;
  line-height: 1.45;
  margin: 16px 0 8px;
}
.cdd-body :deep(.md-block h3) {
  font-size: 15px;
  font-weight: 600;
  line-height: 1.5;
  margin: 14px 0 6px;
}
.cdd-body :deep(.md-block h4),
.cdd-body :deep(.md-block h5),
.cdd-body :deep(.md-block h6) {
  font-size: 15px;
  font-weight: 600;
  margin: 12px 0 4px;
}
.cdd-body :deep(.md-block h1:first-child),
.cdd-body :deep(.md-block h2:first-child),
.cdd-body :deep(.md-block h3:first-child) {
  margin-top: 0;
}
.cdd-body :deep(.md-block p) {
  margin: 0.55em 0;
  line-height: 1.85;
}
.cdd-body :deep(.md-block li) {
  margin: 0.22em 0;
  line-height: 1.85;
}
.cdd-body :deep(.md-block ul),
.cdd-body :deep(.md-block ol) {
  padding-left: 1.4em;
  margin: 0.5em 0;
}
.cdd-body :deep(.md-block blockquote) {
  border-left: 3px solid rgb(var(--primary-rgb) / 0.5);
  padding-left: 0.85em;
  margin: 0.7em 0;
}
.cdd-body :deep(.md-block a) {
  color: rgb(var(--primary-rgb));
  text-decoration: underline;
}
.cdd-body :deep(.md-block code) {
  font-size: 0.9em;
}

/* ── 遮罩淡出延长到 300ms：让抽屉滑完再撤暗底（进入仍用 200ms）── */
.cy-mask-leave-active {
  transition: opacity 300ms ease;
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
</style>
