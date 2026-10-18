<script setup>
// OutlineTree.vue —— 大纲视图（L0-L3，可折叠）
//
// 数据来源：/api/layers/{L}/items + /api/pipeline/funnel
//   · funnel 给「每层 done / pending」徽章
//   · layers 给每层的条目（细纲每卷一条、总纲一条…）
//
// 交互：
//   · 层头点击折叠/展开（状态在 store.systemData.outline.expanded，切页不丢）
//   · 条目点击展开看正文（按需拉 /api/layers/{L}/item/{key}，拉过就缓存）
//   · 顶部显示漏斗进度 6/7 与下一步提示
//
// 为什么层顺序是 L0.5→L1x→L1→L2→L3→L0：
//   这是**创作时的逆向漏斗顺序**（从最上游的整本概括往下收敛），
//   和数据库里的字典序不同，界面按创作顺序排才好用。
import { computed, onMounted, ref } from 'vue'
import { systemData, pushLog } from '../../store.js'
import {
  loadOutline,
  toggleOutlineLevel,
  loadOutlineItem,
} from '../../composables/useSystem.js'

const s = systemData.outline
const busy = ref(false)

// ── 悬浮卡片状态 ──
//   openKey      '' 表示无卡片；形如 "L3:卷01" 表示当前打开的条目
//   openItemData 卡片头部要显示的条目元数据（key/title/chars）
//   openLoading  正文拉取中
//   openErr      拉取失败原因
//   openContent  正文
// 抽出独立 ref 而非塞进 itemBody 字典：卡片同一时刻只显示一条，
// 用字典只会留下永不回收的内存垃圾。
const openKey = ref('')
const openItemData = ref(null)
const openLoading = ref(false)
const openErr = ref('')
const openContent = ref('')

const levels = computed(() => s.levels || [])

/** 每层头部状态徽章 */
function badge(L) {
  if (L.err) return { t: '读取失败', c: 'text-bad bg-bad/12 border-bad/25' }
  if (L.done) return { t: '已完成', c: 'text-ok bg-ok/12 border-ok/25' }
  const st = String(L.status || '')
  if (st.startsWith('chapters')) {
    return { t: st.replace('chapters_', '章节 '), c: 'text-warn bg-warn/12 border-warn/25' }
  }
  if (st) return { t: st, c: 'text-fg-dim bg-ink-600/40 border-ink-600' }
  return { t: '未开始', c: 'text-fg-dim bg-ink-600/40 border-ink-600' }
}

function isOpen(id) {
  return !!s.expanded[id]
}

/** 打开条目悬浮卡片：点同一条则收起，点别的条则换内容 */
async function toggleItem(L, it) {
  const k = `${L.id}:${it.key}`
  if (openKey.value === k) {
    closeItem()
    return
  }
  openKey.value = k
  openItemData.value = { key: it.key, title: it.title, chars: it.chars }
  openContent.value = ''
  openErr.value = ''
  openLoading.value = true
  try {
    const r = await loadOutlineItem(L.id, it.key)
    openContent.value = r.content || ''
    // 后端可能给出更准的字数，优先用它
    if (r.chars != null) openItemData.value = { ...openItemData.value, chars: r.chars }
  } catch (e) {
    openErr.value = String((e && e.message) || e)
  } finally {
    openLoading.value = false
  }
}

/** 关闭卡片并清空正文，避免下次打开闪现上一份内容 */
function closeItem() {
  openKey.value = ''
  openItemData.value = null
  openLoading.value = false
  openErr.value = ''
  openContent.value = ''
}

async function reload() {
  busy.value = true
  await loadOutline(true)
  busy.value = false
  pushLog('大纲已刷新', 'info')
}

function fmtK(n) {
  if (!n) return '0'
  return n >= 10000 ? (n / 10000).toFixed(1) + '万' : String(n)
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
        >{{ busy ? '刷新中…' : '刷新' }}</button>
      </div>
      <!-- 进度条 -->
      <div class="h-1 rounded-full bg-ink-600 overflow-hidden">
        <div
          class="h-full bg-gradient-to-r from-acc to-ok transition-all duration-500"
          :style="{
            width:
              s.funnelDone && levels.length
                ? Math.min(100, (s.funnelDone / 7) * 100) + '%'
                : '0%',
          }"
        />
      </div>
      <div class="text-[10px] text-fg-dim/70 leading-relaxed">
        L4 资料 → L3 细纲 → L2 分卷 → L1x 宏大对抗 → L1 总纲 → L0 概括，最后顺向写 L5 正文
      </div>
    </div>

    <div
      v-if="s.err"
      class="rounded-md px-3 py-2 text-[11px] bg-bad/10 text-bad break-all"
    >{{ s.err }}</div>

    <!-- ── 每一层（手风琴）── -->
    <div
      v-for="L in levels"
      :key="L.id"
      class="glass rounded-md overflow-hidden card-lift"
    >
      <!-- 层头：点击折叠/展开 -->
      <button
        class="acc-head w-full flex items-center gap-2 px-3 py-2.5"
        :aria-expanded="isOpen(L.id) ? 'true' : 'false'"
        @click="toggleOutlineLevel(L.id)"
      >
        <span class="font-mono text-[11px] text-acc w-9 shrink-0">{{ L.id }}</span>
        <span class="text-[12px] text-fg shrink-0">{{ L.name }}</span>
        <span
          class="shrink-0 text-[10px] px-1.5 py-0.5 rounded-sm border"
          :class="badge(L).c"
        >{{ badge(L).t }}</span>
        <span class="ml-auto shrink-0 text-[10px] text-fg-dim font-mono">
          {{ L.count }} 条
        </span>
        <!-- 手风琴箭头：展开时旋转 180°（规范要求的动效） -->
        <svg
          class="acc-arrow shrink-0"
          :class="isOpen(L.id) ? 'open' : ''"
          viewBox="0 0 24 24"
          aria-hidden="true"
        >
          <path d="m6 9 6 6 6-6" />
        </svg>
      </button>

      <!-- 层内容 -->
      <div v-if="isOpen(L.id)" class="border-t border-ink-600/60">
        <div v-if="L.err" class="px-3 py-2 text-[11px] text-bad break-all">
          {{ L.err }}
        </div>
        <div
          v-else-if="!L.items.length"
          class="px-3 py-3 text-[11px] text-fg-dim/70"
        >该层暂无条目</div>

        <button
          v-for="it in L.items"
          :key="it.key"
          class="w-full text-left px-3 py-2 border-b border-ink-600/40 last:border-0
                 active:bg-ink-700/30"
          @click="toggleItem(L, it)"
        >
          <div class="flex items-center gap-2">
            <span class="font-mono text-[10.5px] text-fg-dim shrink-0">
              {{ it.key }}
            </span>
            <span class="flex-1 min-w-0 text-[11.5px] text-fg truncate">
              {{ it.title || '（无标题）' }}
            </span>
            <span class="shrink-0 text-[10px] text-fg-dim/70 font-mono">
              {{ fmtK(it.chars) }} 字
            </span>
            <span class="shrink-0 text-[10px] text-fg-dim">
              {{ openKey === L.id + ':' + it.key ? '▴' : '▾' }}
            </span>
          </div>

          <!-- 来源路径 -->
          <div
            v-if="it.source"
            class="mt-0.5 text-[10px] text-fg-dim/50 font-mono truncate"
          >{{ it.source }}</div>
          <!-- 条目正文：改成「悬浮卡片」而非行内展开 -->
        </button>
      </div>
    </div>

    <!-- ══════ 大纲条目悬浮卡片（半屏抽屉）══════
         用户要求：点某卷某章要弹悬浮卡片展示章纲，不要覆盖整个侧栏。
         这里用底部上滑的半屏卡片 + 毛玻璃遮罩；关掉后列表位置原封不动。 -->
    <Teleport to="body">
      <Transition name="ol-card">
        <div v-if="openKey" class="fixed inset-0 z-[56]">
          <!-- 遮罩 -->
          <div
            class="ol-mask absolute inset-0 bg-black/45 backdrop-blur-[2px]"
            @click="closeItem"
          />
          <!-- 悬浮卡片 -->
          <div
            class="ol-sheet absolute left-2 right-2 bottom-2 max-h-[74vh]
                   glass-strong rounded-2xl overflow-hidden flex flex-col
                   shadow-[0_18px_60px_rgba(0,0,0,.55)] ring-1 ring-white/10"
          >
            <!-- 头部：编号 + 标题 + 字数 + 关闭 -->
            <div class="shrink-0 flex items-center gap-2 px-3.5 py-2.5
                        border-b border-ink-600/60">
              <span class="font-mono text-[11px] text-acc shrink-0">
                {{ openItemData?.key || '' }}
              </span>
              <span class="flex-1 min-w-0 text-[12.5px] font-semibold text-fg truncate">
                {{ openItemData?.title || '（无标题）' }}
              </span>
              <span class="shrink-0 text-[10px] text-fg-dim/70 font-mono">
                {{ fmtK(openItemData?.chars || 0) }} 字
              </span>
              <button
                class="shrink-0 w-7 h-7 rounded-lg bg-ink-700/70 text-fg-dim
                       flex items-center justify-center text-[15px] active:scale-95
                       transition-transform"
                @click="closeItem"
                aria-label="关闭"
              >✕</button>
            </div>
            <!-- 正文 -->
            <div class="flex-1 min-h-0 overflow-y-auto px-3.5 py-3">
              <div v-if="openLoading" class="text-[11.5px] text-fg-dim">读取中…</div>
              <div v-else-if="openErr" class="text-[11.5px] text-bad break-all">
                {{ openErr }}
              </div>
              <pre
                v-else
                class="text-[11px] text-fg/88 leading-relaxed whitespace-pre-wrap
                       break-words font-mono"
              >{{ openContent }}</pre>
            </div>
          </div>
        </div>
      </Transition>
    </Teleport>


    <!-- 首屏加载：用骨架屏而不是转圈/纯文字。
         骨架的条块形状尽量贴近真实列表（层头 + 三条条目），
         这样真实数据填进来时视觉是"接上"的，不会跳一下。 -->
    <div v-if="s.loading && !levels.length" class="space-y-3">
      <div v-for="i in 4" :key="i" class="glass rounded-md px-3 py-3 space-y-2">
        <div class="flex items-center gap-2">
          <span class="skl" style="width: 30px; height: 12px" />
          <span class="skl" style="width: 96px; height: 12px" />
          <span class="skl ml-auto" style="width: 40px; height: 12px" />
        </div>
        <div class="skl" style="width: 100%; height: 10px" />
        <div class="skl" style="width: 72%; height: 10px" />
      </div>
    </div>
    <div class="h-2" />
  </div>
</template>

<style>
/* ── 手风琴层头：悬停/按下反馈 ── */
.acc-head {
  transition: background-color var(--dur-2) var(--ease-io);
}
.acc-head:hover {
  background: rgb(var(--card-2-rgb) / 0.6);
}
.acc-head:active {
  background: rgb(var(--card-2-rgb) / 0.9);
}

/* ── 手风琴箭头：展开时 180° 旋转（规范动效）──
   用 transform 而非旋转字符，因为它走合成器，不掉帧。
   spring 曲线让"翻开"有一点点过冲，手感更实。 */
.acc-arrow {
  width: 14px;
  height: 14px;
  stroke: var(--text-disabled);
  fill: none;
  stroke-width: 2.2;
  stroke-linecap: round;
  stroke-linejoin: round;
  transition: transform var(--dur-3) var(--ease-spring),
    stroke var(--dur-2) var(--ease-io);
}
.acc-arrow.open {
  transform: rotate(180deg);
  stroke: var(--primary);
}

/* ── 大纲条目悬浮卡片：底部上滑 + 毛玻璃遮罩淡入 ──
   与 App.vue 的 .ball-pop / FloatingChat 共用同一条缓动曲线
   cubic-bezier(.22, 1.4, .36, 1)，保证全局「弹跳手感」一致。
   只在 enter/leave 期间挂 transition，静默期零开销。 */
.ol-card-enter-active .ol-mask,
.ol-card-leave-active .ol-mask {
  transition: opacity 220ms ease;
}
.ol-card-enter-active .ol-sheet {
  transition: transform 320ms cubic-bezier(.22, 1.4, .36, 1),
    opacity 200ms ease;
}
.ol-card-leave-active .ol-sheet {
  transition: transform 200ms cubic-bezier(.4, 0, 1, 1), opacity 170ms ease;
}
.ol-card-enter-from .ol-mask,
.ol-card-leave-to .ol-mask {
  opacity: 0;
}
.ol-card-enter-from .ol-sheet,
.ol-card-leave-to .ol-sheet {
  opacity: 0;
  transform: translate3d(0, 100%, 0) scale(0.98);
}
/* 底部安全区：卡片贴住屏幕底部时别被手势条压住 */
.ol-sheet {
  padding-bottom: env(safe-area-inset-bottom, 0px);
  will-change: transform;
}
</style>