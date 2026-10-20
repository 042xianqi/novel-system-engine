<script setup>
// SettingsPane.vue —— 设置中心 · 大模型 API 管理（2026-10-20 重构）
//
// ══════════════════════════════════════════════════════════
// 本次重构干了什么
// ══════════════════════════════════════════════════════════
// 1. 删掉旧的内部代号 / 「多目标同步」概念 UI。
//    那是**实现细节泄漏**：用户不需要知道 models.yml 被写到几个目录、
//    也不该在界面上看到后端子系统代号。多目标同步由后端自己负责，
//    前端只当它是「一套可增改的模型端点配置」。
//
// 2. 统一成一张 **【大模型 API 列表】**：每张卡展示
//       名称 name / BaseURL base_url / 模型名 models
//    key **只回显后端打码后的尾 4 位**（形如 sk-***abcd），
//    全串永不进 DOM、永不 console、永不进日志。
//
// 3. 右上角【+ 添加新 API】→ 弹出表单（名称 / BaseURL / API Key / 模型名），
//    带**前端校验**（BaseURL 必须 http(s):// 开头；名称、模型名非空），
//    保存成功后刷新列表。
//
// ══════════════════════════════════════════════════════════
// 后端端点现状（已核对 crates/ne-agent/src/governance/model_config.rs）
// ══════════════════════════════════════════════════════════
//   GET  /api/config/model   → 读 models.yml，返回 { providers:[{name,
//                               base_url, models[], api_key_set, api_key_mask}],
//                               model_role{}, path }        （key 永不回显）
//   POST /api/config/model   → 单 provider 形态 {provider, base_url, model, api_key}
//                               provider 名不存在 = **新建**；存在 = 更新。
//                               api_key 传空串/省略 = 保留原值（保护真 key）。
//   PUT  /api/config/model   → 同 POST。
//
//   ⚠ 缺：**没有删除端点**（DELETE 不存在）。
//     列表卡片里的「删除」会明确提示「后端未支持」，不假装成功。
//     需要后端新增：DELETE /api/config/model?provider=<name>
//     （或 POST /api/config/model/delete {provider}），并同步删掉
//     modelRoles.default 里指向它的绑定。
//
// api_key 是本地网关（127.0.0.1:8788）时可留空；留空不会覆盖已有 key。
import { ref, computed, onMounted, nextTick } from 'vue'
import { ModelAPI } from '../../api.js'
import { toast } from '../../store.js'
import BaseButton from '../base/BaseButton.vue'
import BaseInput from '../base/BaseInput.vue'
import SkinPicker from '../base/SkinPicker.vue'

// ── 状态 ──
const loading = ref(false)
const saving = ref(false)
const err = ref('')

const cfg = ref({ providers: [], path: '' })
const list = ref([]) // [{ name, base_url, models, api_key_set, api_key_mask }]

// 表单形态：null = 关闭；{ mode:'add'|'edit', name, base_url, api_key, model }
const form = ref(null)
const formErr = ref({})

// ══════════════════════════════════════════ 工具
/** 从后端返回的 provider 里取一行安全数据（不含任何明文 key） */
function toRow(p) {
  const models = Array.isArray(p.models) ? p.models.filter(Boolean) : []
  return {
    name: p.name || '(未命名)',
    base_url: p.base_url || '',
    models,
    model: models[0] || '',
    api_key_set: !!p.api_key_set,
    // 后端已打码（sk-***abcd）；这里只做兜底，绝不接触明文
    api_key_mask: p.api_key_mask || '',
  }
}

/** 展示用掩码：只显示尾 4 位。p.mask 已是后端打码结果，原样展示。 */
function maskOf(row) {
  if (row.api_key_mask) return row.api_key_mask
  return row.api_key_set ? '****' : '未设置'
}

function isHttpUrl(s) {
  return /^https?:\/\/.+/i.test(String(s || '').trim())
}

// ══════════════════════════════════════════ 读
async function reload() {
  loading.value = true
  err.value = ''
  try {
    const r = await ModelAPI.read()
    if (r && r.ok === false) {
      err.value = r.error || '读取失败'
    } else {
      cfg.value = r || {}
      list.value = (r.providers || []).map(toRow)
    }
  } catch (e) {
    err.value = String(e.message || e)
  } finally {
    loading.value = false
  }
}

// ══════════════════════════════════════════ 表单
async function openAdd() {
  formErr.value = {}
  form.value = { mode: 'add', name: '', base_url: '', api_key: '', model: '' }
  await nextTick()
}

function openEdit(row) {
  formErr.value = {}
  form.value = {
    mode: 'edit',
    name: row.name,
    base_url: row.base_url,
    api_key: '', // 空 = 不改（保护磁盘上的真 key）
    model: row.model,
  }
}

function closeForm() {
  form.value = null
  formErr.value = {}
}

function validate() {
  const e = {}
  const f = form.value
  if (!String(f.name || '').trim()) e.name = '名称不能为空'
  if (!String(f.base_url || '').trim()) e.base_url = 'BaseURL 不能为空'
  else if (!isHttpUrl(f.base_url)) e.base_url = 'BaseURL 必须以 http:// 或 https:// 开头'
  if (!String(f.model || '').trim()) e.model = '模型名不能为空'
  // api_key 可留空（本地网关 / 不修改）
  formErr.value = e
  return Object.keys(e).length === 0
}

async function submit() {
  if (!validate()) return
  saving.value = true
  err.value = ''
  try {
    const f = form.value
    // ⚠ 绝不打日志、绝不回显。api_key 只有用户真输了才带上；空串=后端保留原值。
    const payload = {
      provider: String(f.name).trim(),
      base_url: String(f.base_url).trim(),
      model: String(f.model).trim(),
    }
    const key = String(f.api_key || '').trim()
    if (key) payload.api_key = key

    const r = await ModelAPI.write(payload)
    if (r && r.ok === false) {
      formErr.value = { _: r.error || '保存失败' }
      return
    }
    // 后端写成功后会回带打码后的新状态，直接用它刷新，省一次 GET
    if (r && r.config) {
      cfg.value = r.config
      list.value = (r.config.providers || []).map(toRow)
    } else {
      await reload()
    }
    toast(f.mode === 'add' ? '已添加新 API' : '已保存修改')
    closeForm()
  } catch (e) {
    formErr.value = { _: String(e.message || e) }
  } finally {
    saving.value = false
  }
}

// ── 删除：后端暂无端点，明确提示，不假装成功 ──
function tryDelete(row) {
  toast(`${row.name}：后端暂无删除端点，需新增 DELETE /api/config/model`, true)
}

const count = computed(() => list.value.length)

onMounted(reload)
</script>

<template>
  <div class="h-full overflow-y-auto px-3 py-3 space-y-3">
    <!-- ══════ 标题 + 右上角【+ 添加新 API】══════ -->
    <div class="flex items-center gap-3">
      <div class="min-w-0">
        <h1 class="text-[15px] font-semibold text-content">设置</h1>
        <p class="text-[11px] text-content-sec mt-0.5 truncate">
          大模型 API 管理 · 共 {{ count }} 套配置
        </p>
      </div>
      <div class="flex-1" />
      <BaseButton
        variant="primary"
        size="md"
        glow
        :disabled="saving"
        @click="openAdd"
      >
        <template #icon><span class="mr-1 text-[15px] leading-none">＋</span></template>
        添加新 API
      </BaseButton>
    </div>

    <!-- 读取错误 -->
    <div
      v-if="err"
      class="rounded-md border border-danger/35 bg-danger-soft px-3 py-2 text-[11.5px] text-danger"
    >
      ⚠ {{ err }}
    </div>

    <!-- ══════ 大模型 API 列表 ══════ -->
    <section class="space-y-2">
      <div class="flex items-center gap-2 px-0.5">
        <span class="text-[12px] font-medium text-content">大模型 API 列表</span>
        <span class="text-[10.5px] text-content-sec">名称 / BaseURL / 模型名</span>
      </div>

      <!-- 骨架 -->
      <div v-if="loading" class="glass rounded-md px-3 py-4 text-center text-[11px] text-content-sec">
        读取中…
      </div>

      <!-- 空态 -->
      <div
        v-else-if="!list.length"
        class="glass rounded-md px-4 py-6 text-center space-y-2"
      >
        <div class="text-[12px] text-content">还没有配置任何模型 API</div>
        <div class="text-[11px] text-content-sec">
          点右上角【＋ 添加新 API】填一个 BaseURL 就能开始用。
        </div>
        <div v-if="cfg.path" class="text-[10.5px] font-mono text-content-sec/70 truncate">
          {{ cfg.path }}
        </div>
      </div>

      <!-- 卡片列表 -->
      <div
        v-for="row in list"
        :key="row.name"
        class="glass rounded-md px-3 py-2.5 space-y-2"
      >
        <!-- 第一行：名称 + key 掩码 + 操作 -->
        <div class="flex items-center gap-2">
          <span class="font-mono text-[12.5px] text-content truncate">{{ row.name }}</span>
          <span
            class="text-[10px] px-1.5 py-0.5 rounded-sm shrink-0"
            :class="row.api_key_set
              ? 'bg-success/15 text-success'
              : 'bg-surface-card-2 text-content-sec'"
          >
            {{ row.api_key_set ? maskOf(row) : '免鉴权' }}
          </span>
          <div class="flex-1" />
          <BaseButton variant="ghost" size="sm" @click="openEdit(row)">编辑</BaseButton>
          <BaseButton
            variant="danger-text"
            size="sm"
            title="后端暂无删除端点"
            @click="tryDelete(row)"
          >删除</BaseButton>
        </div>

        <!-- BaseURL -->
        <div class="flex items-baseline gap-2">
          <span class="text-[10px] text-content-sec shrink-0 w-14">BaseURL</span>
          <span class="font-mono text-[11.5px] text-content break-all">{{ row.base_url || '—' }}</span>
        </div>

        <!-- 模型名 -->
        <div class="flex items-baseline gap-2">
          <span class="text-[10px] text-content-sec shrink-0 w-14">模型名</span>
          <span class="font-mono text-[11.5px] text-content break-all">
            <template v-if="row.models.length">{{ row.models.join(' , ') }}</template>
            <template v-else>—</template>
          </span>
        </div>
      </div>
    </section>

    <!-- ══════ 主题皮肤（保留原有能力）══════ -->
    <div class="glass rounded-md px-3 py-2.5">
      <div class="text-[12px] font-medium text-content mb-2">主题皮肤</div>
      <SkinPicker />
    </div>

    <div class="h-2" />

    <!-- ══════ 添加 / 编辑 表单（弹层）══════ -->
    <Teleport to="body">
      <Transition
        enter-active-class="transition duration-200 ease-out"
        enter-from-class="opacity-0"
        leave-active-class="transition duration-150 ease-in"
        leave-to-class="opacity-0"
      >
        <div
          v-if="form"
          class="fixed inset-0 z-[90] flex items-end sm:items-center justify-center
                 bg-black/45 backdrop-blur-[2px] p-0 sm:p-4"
          @click.self="closeForm"
          @keydown.esc="closeForm"
        >
          <Transition
            appear
            enter-active-class="transition duration-250 ease-out-back"
            enter-from-class="translate-y-6 sm:translate-y-0 sm:scale-95 opacity-0"
          >
            <div
              v-if="form"
              class="glass-strong w-full sm:max-w-md rounded-t-2xl sm:rounded-2xl
                     px-4 py-4 space-y-3 max-h-[90vh] overflow-y-auto"
              role="dialog"
              aria-modal="true"
              aria-label="大模型 API 配置表单"
            >
              <div class="flex items-center gap-2">
                <h2 class="text-[14px] font-semibold text-content">
                  {{ form.mode === 'add' ? '添加新 API' : '编辑 API' }}
                </h2>
                <div class="flex-1" />
                <BaseButton variant="ghost" size="sm" @click="closeForm">关闭</BaseButton>
              </div>

              <div
                v-if="formErr._"
                class="rounded-md border border-danger/35 bg-danger-soft px-3 py-2 text-[11.5px] text-danger"
              >
                ⚠ {{ formErr._ }}
              </div>

              <!-- 名称 -->
              <BaseInput
                v-model="form.name"
                label="名称"
                placeholder="例：本地网关 / deepseek 官方"
                required
                :aria-describedby="form.mode === 'edit' ? 'name-rename-hint' : undefined"
                :error="!!formErr.name"
                :msg="formErr.name || ''"
              />

              <!-- BaseURL -->
              <BaseInput
                v-model="form.base_url"
                label="BaseURL"
                placeholder="http://127.0.0.1:8788/v1"
                required
                :error="!!formErr.base_url"
                :msg="formErr.base_url || '必须以 http:// 或 https:// 开头'"
              />

              <!-- API Key -->
              <BaseInput
                v-model="form.api_key"
                type="password"
                label="API Key"
                :placeholder="form.mode === 'edit' ? '留空 = 不修改' : '本地网关可留空'"
                :msg="form.mode === 'edit'
                  ? '留空则保留原有 key（不会用空值覆盖）'
                  : '本地网关留空即可；仅存后端，不回显明文'"
              />

              <!-- 模型名 -->
              <BaseInput
                v-model="form.model"
                label="模型名"
                placeholder="deepseek-v4.1-flash"
                required
                :error="!!formErr.model"
                :msg="formErr.model || '后端实际调用的 model id'"
              />

              <div class="flex gap-2 pt-1">
                <BaseButton variant="outline-gray" block @click="closeForm">取消</BaseButton>
                <BaseButton variant="primary" block :loading="saving" @click="submit">
                  {{ form.mode === 'add' ? '添加' : '保存' }}
                </BaseButton>
              </div>
            </div>
          </Transition>
        </div>
      </Transition>
    </Teleport>
  </div>
</template>
