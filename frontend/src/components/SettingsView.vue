<script setup>
// SettingsView.vue —— 设置中心：**真可编辑**的模型配置
//
// 设计约束（来自真实踩坑）：
//   · api_key 输入框的 placeholder 显示掩码（sk-***9999），但**不把它当 value**。
//     用户不动它 → 提交时传空串 → 后端保留原值（这条保护真 key 不被覆盖）。
//     用户改了 → 提交新明文 → 后端写盘。
//   · baseUrl / model 必须能改，改完立刻生效（后端无缓存）。
//   · 保存后必须显示「三处写入结果」，让用户看到 OMP/data 各自成败，
//     而不是只弹一个模糊的"保存成功"。
//   · 角色绑定（modelRoles.default）必须显式展示，并标出两份是否一致。
import { ref, computed, onMounted } from 'vue'
import { ModelAPI } from '../api.js'
import SkinPicker from './base/SkinPicker.vue'

const loading = ref(false)
const saving = ref(false)
const err = ref('')
const okMsg = ref('')

const cfg = ref({ providers: [], model_role: {}, path: '' })
// 每个 provider 的编辑态：key 用「空 = 不改」语义
const forms = ref([])

function pickModel(list) {
  return (list && list[0]) || ''
}

function loadForms(c) {
  forms.value = (c.providers || []).map((p) => ({
    name: p.name,
    base_url: p.base_url || '',
    model: pickModel(p.models),
    // 只用于展示，不作为提交值
    key_mask: p.api_key_mask || '',
    key_set: !!p.api_key_set,
    // 用户新输入的 key（空 = 不改）
    new_key: '',
  }))
}

async function reload() {
  loading.value = true
  err.value = ''
  try {
    const r = await ModelAPI.read()
    if (r.ok === false) {
      err.value = r.error || '读取失败'
    } else {
      cfg.value = r
      loadForms(r)
    }
  } catch (e) {
    err.value = String(e.message || e)
  } finally {
    loading.value = false
  }
}

async function save(f) {
  saving.value = true
  err.value = ''
  okMsg.value = ''
  try {
    const payload = {
      provider: f.name,
      base_url: f.base_url,
      model: f.model,
    }
    // 只有用户真输了新 key 才带上，否则空串 = 后端保留原值
    if (f.new_key.trim()) payload.api_key = f.new_key.trim()

    const r = await ModelAPI.write(payload)
    if (r.ok === false) {
      err.value = r.error || '保存失败'
      return
    }
    // 把三处写入结果拼出来给用户看
    const lines = (r.targets || []).map(
      (t) => `${t.ok ? '✅' : '❌'} ${t.label}：${t.detail}`
    )
    const role = r.model_role?.result || ''
    okMsg.value = [lines.join('\n'), role ? `角色绑定：${role}` : '']
      .filter(Boolean)
      .join('\n')
    f.new_key = ''
    if (r.config) {
      cfg.value = r.config
      loadForms(r.config)
    } else {
      await reload()
    }
  } catch (e) {
    err.value = String(e.message || e)
  } finally {
    saving.value = false
  }
}

const role = computed(() => cfg.value.model_role || {})
const roleOk = computed(() => role.value.consistent === true)

onMounted(reload)
</script>

<template>
  <div class="h-full overflow-y-auto px-3 py-3 space-y-3">
    <!-- ══════ 主题皮肤（放在最前：改一下立刻能看到整站换装）══════ -->
    <div class="glass rounded-md px-3 py-2.5">
      <div class="flex items-center gap-2 mb-2">
        <span class="text-[13px] font-semibold text-fg">主题皮肤</span>
        <span class="text-[11px] text-fg-dim">点色点即刻换装</span>
      </div>
      <SkinPicker />
    </div>

    <!-- 标题 + 刷新 -->
    <div class="flex items-center gap-2">
      <div class="text-[13px] font-semibold text-fg">模型配置</div>
      <div class="text-[11px] text-fg-dim">改完立刻生效，无需重启</div>
      <button
        class="ml-auto glass px-2 py-1 rounded-sm text-[11px] text-fg-dim active:scale-95 ripple-host"
        :disabled="loading"
        @click="reload"
      >
        {{ loading ? '读取中…' : '重新读取' }}
      </button>
    </div>

    <!-- 角色绑定 -->
    <div class="glass rounded-md px-3 py-2 space-y-1">
      <div class="flex items-center gap-1.5">
        <span class="text-[11px] text-fg-dim">当前生效角色</span>
        <span
          class="text-[10px] px-1.5 py-0.5 rounded-sm"
          :class="
            roleOk
              ? 'bg-ok/15 text-ok'
              : 'bg-bad/15 text-bad'
          "
        >
          {{ roleOk ? (role.benign_diff ? '各自有效' : '一致') : '有故障' }}
        </span>
      </div>
      <div class="font-mono text-[12px] text-acc break-all">
        {{ role.effective || '（未设置）' }}
      </div>

      <!-- 正常差异：两池 provider 集合不同，各自指向自己的成员，不是故障 -->
      <div
        v-if="roleOk && role.benign_diff"
        class="text-[10px] text-fg-dim/70 leading-relaxed"
      >
        两个池的 provider 集合不同，各自指向自己池里的成员（正常）：
        <div class="font-mono mt-0.5">omp → {{ role.omp || '空' }}</div>
        <div class="font-mono">data → {{ role.data || '空' }}</div>
      </div>

      <!-- 真故障：role 指向了池里不存在的 provider（会回落公网地址） -->
      <div
        v-if="!roleOk && !role.valid_omp && role.omp"
        class="text-[10px] text-bad leading-relaxed"
      >
        🔴 OMP 角色指向了池里不存在的 provider。真生成（l5.py）读不到 baseUrl 会回落到
        公网地址，必须修：随便改一次模型即可自动纠正。
      </div>
      <div
        v-if="!roleOk && !role.valid_data && role.data"
        class="text-[10px] text-bad leading-relaxed"
      >
        ⚠ data 角色指向了池里不存在的 provider。
      </div>
      <div
        v-if="!roleOk && (!role.omp || !role.data)"
        class="text-[10px] text-warn leading-relaxed"
      >
        有一侧角色绑定为空。改一次模型即可写入。
      </div>
    </div>

    <!-- 写盘位置（排查用） -->
    <div class="text-[10px] text-fg-dim font-mono break-all leading-relaxed">
      <div>data：{{ cfg.path || '-' }}</div>
      <div>omp：{{ role.omp_path || '-' }}</div>
    </div>

    <!-- 结果提示 -->
    <div
      v-if="okMsg"
      class="rounded-md px-3 py-2 text-[11px] leading-relaxed whitespace-pre-wrap bg-ok/10 text-ok"
    >{{ okMsg }}</div>
    <div
      v-if="err"
      class="rounded-md px-3 py-2 text-[11px] leading-relaxed whitespace-pre-wrap bg-bad/10 text-bad"
    >{{ err }}</div>

    <!-- 每个 provider 一张卡 -->
    <div
      v-for="f in forms"
      :key="f.name"
      class="glass rounded-md px-3 py-2.5 space-y-2"
    >
      <div class="flex items-center gap-2">
        <span class="font-mono text-[12px] text-fg">{{ f.name }}</span>
        <span
          class="text-[10px] px-1.5 py-0.5 rounded-sm"
          :class="f.key_set ? 'bg-ok/15 text-ok' : 'bg-ink-600 text-fg-dim'"
        >
          {{ f.key_set ? '已设 key' : '免鉴权' }}
        </span>
      </div>

      <label class="block space-y-1">
        <span class="text-[10px] text-fg-dim">baseUrl</span>
        <input
          v-model="f.base_url"
          class="w-full bg-ink-700 rounded-sm px-2 py-1.5 text-[12px] font-mono
                 text-fg outline-none border border-ink-600 focus:border-acc/60"
          placeholder="http://127.0.0.1:8788/v1"
          spellcheck="false"
        />
      </label>

      <label class="block space-y-1">
        <span class="text-[10px] text-fg-dim">模型名</span>
        <input
          v-model="f.model"
          class="w-full bg-ink-700 rounded-sm px-2 py-1.5 text-[12px] font-mono
                 text-fg outline-none border border-ink-600 focus:border-acc/60"
          placeholder="deepseek-v4.1-flash"
          spellcheck="false"
        />
      </label>

      <label class="block space-y-1">
        <span class="text-[10px] text-fg-dim">
          apiKey
          <span class="text-fg-dim/70">
            （留空 = 不改；当前 {{ f.key_mask || '无' }}）
          </span>
        </span>
        <input
          v-model="f.new_key"
          type="password"
          class="w-full bg-ink-700 rounded-sm px-2 py-1.5 text-[12px] font-mono
                 text-fg outline-none border border-ink-600 focus:border-acc/60"
          :placeholder="f.key_set ? '不修改请留空' : '本地网关可留空'"
          spellcheck="false"
        />
      </label>

      <button
        class="w-full rounded-sm py-2 text-[12px] font-medium active:scale-[0.98]
               bg-acc/15 text-acc border border-acc/30 disabled:opacity-50"
        :disabled="saving"
        @click="save(f)"
      >
        {{ saving ? '保存中…' : '保存（三处同步）' }}
      </button>
    </div>

    <div
      v-if="!loading && !forms.length"
      class="glass rounded-md px-3 py-4 text-center text-[11px] text-fg-dim"
    >
      没读到任何 provider。检查 {{ cfg.path || 'models.yml' }} 是否存在。
    </div>

    <div class="h-2" />
  </div>
</template>