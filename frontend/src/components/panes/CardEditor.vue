<script setup>
// CardEditor.vue —— 卡片新建 / 编辑（全屏浮层）
//
// 为什么做成全屏浮层而不是抽屉：
//   编辑一张卡要同时看编号、标题、正文，正文可能上千字。
//   抽屉高度受限，手机上打字会被键盘顶掉。全屏浮层能给足空间。
//
// 三种模式：create（新建）/ edit（编辑已存在）
//
// 关键约束（来自后端 layer_store.rs 实现）：
//   ① 新建必须有 key + content，且 (layer,key) 唯一 → 建完不能改 key
//   ② 编辑传 content 是**整体覆写文件**，所以进入编辑必须先拉原文
//   ③ 后端会把 layer/层 固定成 L4，路径 02_资料库/_webui/L4/{key}.md
//
// 保存后由父组件负责刷新列表（这里只 emit 事件，不直接改列表），
// 保持"组件只管自己这块"的边界。
import { ref, computed, watch, onMounted, nextTick } from 'vue'
import { systemData } from '../../store.js'
import { createCard, updateCard, loadCardDetail } from '../../composables/useSystem.js'

const props = defineProps({
  /** 'create' | 'edit' */
  mode: { type: String, default: 'create' },
  /** 编辑时的卡片 key；新建时为空 */
  cardKey: { type: String, default: '' },
  /** 新建时预填的分类（仅作提示） */
  presetCategory: { type: String, default: '' },
})
const emit = defineEmits(['close', 'saved'])

const s = systemData.cards

const key = ref('')
const title = ref('')
const content = ref('')
const err = ref('')
const saving = ref(false)
const loading = ref(false)
const bodyEl = ref(null)

const isCreate = computed(() => props.mode === 'create')

/** 编号格式提示：L4 用 PREFIX-NN-NNN（如 CHAR-99-001） */
const keyHint = computed(() => {
  const k = key.value.trim()
  if (!k) return ''
  if (!/^[A-Z]{2,6}-\d{2}-\d{3}$/.test(k)) {
    return '建议格式 PREFIX-NN-NNN（如 CHAR-99-001）；不合法后端也会拒绝'
  }
  return ''
})

/** 字符数（不算空白，与后端统计口径一致） */
const charCount = computed(() => content.value.replace(/\s/g, '').length)

async function loadForEdit() {
  if (isCreate.value || !props.cardKey) return
  loading.value = true
  err.value = ''
  try {
    // 复用 useSystem 的详情缓存；没有就拉
    let d = s.detail[props.cardKey]
    if (!d || !d.content) d = await loadCardDetail(props.cardKey)
    if (d?.err) {
      err.value = d.err
    } else {
      content.value = d?.content || ''
      // 标题从列表项取（详情接口只返回正文）
      const item = s.items.find((x) => x.key === props.cardKey)
      title.value = (item?.title || '').replace(/^[A-Z]+-\d+-\d+｜/, '')
    }
    key.value = props.cardKey
  } catch (e) {
    err.value = String(e.message || e)
  } finally {
    loading.value = false
  }
}

async function save() {
  err.value = ''
  if (isCreate.value) {
    if (!key.value.trim()) {
      err.value = '编号不能为空'
      return
    }
    if (!content.value.trim()) {
      err.value = '正文不能为空'
      return
    }
  } else if (!content.value.trim()) {
    err.value = '正文不能为空'
    return
  }

  saving.value = true
  try {
    const r = isCreate.value
      ? await createCard({
          key: key.value.trim(),
          title: title.value.trim(),
          content: content.value,
        })
      : await updateCard(props.cardKey, {
          title: title.value.trim(),
          content: content.value,
        })
    if (!r.ok) {
      err.value = r.error || '保存失败'
      return
    }
    emit('saved', { mode: props.mode, key: isCreate.value ? key.value.trim() : props.cardKey })
    emit('close')
  } catch (e) {
    err.value = String(e.message || e)
  } finally {
    saving.value = false
  }
}

function close() {
  if (saving.value) return
  emit('close')
}

/** 让 textarea 自适应高度（上限 60vh） */
function autosize() {
  const el = bodyEl.value
  if (!el) return
  el.style.height = 'auto'
  el.style.height = Math.min(el.scrollHeight, window.innerHeight * 0.6) + 'px'
}

watch(content, () => nextTick(autosize))

onMounted(() => {
  if (isCreate.value) {
    key.value = ''
    title.value = ''
    content.value = ''
  } else {
    loadForEdit()
  }
})
</script>

<template>
  <div class="absolute inset-0 z-50 flex flex-col bg-ink-900">
    <!-- ── 头部 ── -->
    <div class="shrink-0 flex items-center gap-2 px-3 py-2.5 border-b border-ink-600/70">
      <button
        class="text-[13px] text-fg-dim px-1 active:scale-95 disabled:opacity-40"
        :disabled="saving"
        @click="close"
      >取消</button>
      <span class="text-[13px] text-fg font-medium">
        {{ isCreate ? '新建卡片' : '编辑卡片' }}
      </span>
      <span class="text-[10px] text-fg-dim font-mono">L4</span>
      <button
        class="ml-auto text-[12px] px-3 py-1.5 rounded-sm
               bg-acc/15 text-acc border border-acc/30
               active:scale-95 disabled:opacity-40"
        :disabled="saving || loading"
        @click="save"
      >{{ saving ? '保存中…' : '保存' }}</button>
    </div>

    <!-- ── 表单 ── -->
    <div class="flex-1 min-h-0 overflow-y-auto px-3 py-3 space-y-3">
      <div v-if="loading" class="text-[12px] text-fg-dim py-6 text-center">
        读取原文中…
      </div>

      <template v-else>
        <!-- 编号 -->
        <label class="block space-y-1">
          <span class="text-[10px] text-fg-dim">
            编号
            <span v-if="!isCreate" class="opacity-60">（已存在的卡不能改编号）</span>
          </span>
          <input
            v-model="key"
            :disabled="!isCreate"
            class="w-full bg-ink-700 rounded-sm px-2 py-1.5 text-[12px] font-mono
                   text-fg outline-none border border-ink-600 focus:border-acc/60
                   disabled:opacity-50"
            placeholder="CHAR-99-001"
            spellcheck="false"
          />
          <span v-if="keyHint" class="text-[10px] text-warn leading-relaxed">
            {{ keyHint }}
          </span>
        </label>

        <!-- 标题 -->
        <label class="block space-y-1">
          <span class="text-[10px] text-fg-dim">标题（不含编号前缀）</span>
          <input
            v-model="title"
            class="w-full bg-ink-700 rounded-sm px-2 py-1.5 text-[12px]
                   text-fg outline-none border border-ink-600 focus:border-acc/60"
            placeholder="白鸦（柒玖）"
            spellcheck="false"
          />
        </label>

        <!-- 分类提示（后端不落库，仅说明） -->
        <div
          v-if="isCreate && presetCategory"
          class="text-[10px] text-fg-dim/70 leading-relaxed"
        >
          当前筛选的分类是「{{ presetCategory }}」。新建后分类由后端按编号前缀推断，
          这里不强行指定。
        </div>

        <!-- 正文 -->
        <label class="block space-y-1">
          <span class="flex items-center gap-2 text-[10px] text-fg-dim">
            <span>正文（Markdown）</span>
            <span class="ml-auto font-mono opacity-60">{{ charCount }} 字</span>
          </span>
          <textarea
            ref="bodyEl"
            v-model="content"
            class="w-full bg-ink-700 rounded-sm px-2 py-2 text-[11.5px] font-mono
                   text-fg outline-none border border-ink-600 focus:border-acc/60
                   leading-relaxed resize-none"
            rows="14"
            placeholder="# 编号｜标题&#10;&#10;> 胶囊：一句话概括&#10;&#10;## 详细信息&#10;…"
            spellcheck="false"
          />
        </label>

        <div
          v-if="err"
          class="rounded-md px-3 py-2 text-[11px] bg-bad/10 text-bad break-all"
        >{{ err }}</div>

        <div class="text-[10px] text-fg-dim/50 leading-relaxed">
          保存时后端会写前快照（可回滚），并把条目登记进 library_index。
          新建的文件路径固定为
          <span class="font-mono">02_资料库/_webui/L4/{编号}.md</span>。
        </div>
      </template>
    </div>
  </div>
</template>