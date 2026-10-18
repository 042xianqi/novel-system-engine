// BaseInput.vue —— 基础输入框（规范第 2 层输入控件）
//
// 覆盖规范里输入相关的全部细节：
//   · 聚焦光圈        focus-within 时 border 变 primary + 3px ring
//   · 搜索            type=search + 左侧放大镜 + 有值时右侧清除
//   · 密码            type=password + 右侧眼睛切换明文
//   · 多行            multiline → textarea + 自动高度
//   · 报错抖动        error 由 false 变 true 的瞬间抖一下（不是常驻抖动）
//   · 成功态          success → 绿色边框 + 绿勾
//   · 字数计数        showCount → 右下角 n/max
//
// 为什么外壳是 div、input 在里面：
//   要让「图标 + 输入 + 后缀按钮」共享同一个边框和光圈，
//   必须由一个容器统一绘制 border/box-shadow，input 本身透明。
<template>
  <div class="w-full">
    <!-- 标签 -->
    <label v-if="label" class="flex gap-1 text-[12px] font-semibold
                                text-fg-dim mb-1.5">
      <span>{{ label }}</span>
      <span v-if="required" class="text-bad">*</span>
    </label>

    <!-- 输入外壳 -->
    <div
      class="input-shell"
      :class="[
        disabled ? 'is-disabled' : '',
        error ? 'input-error' : '',
        success ? 'is-success' : '',
        shake ? 'shake' : '',
        multiline ? 'items-start' : 'items-center',
      ]"
    >
      <!-- 左图标：搜索优先，其次自定义 -->
      <svg
        v-if="type === 'search'"
        class="ic"
        viewBox="0 0 24 24" aria-hidden="true"
      >
        <circle cx="11" cy="11" r="7" />
        <path d="m20 20-3.5-3.5" />
      </svg>
      <slot v-else name="prefix" />

      <!-- 单行 -->
      <input
        v-if="!multiline"
        ref="inputEl"
        :type="actualType"
        :value="modelValue"
        :placeholder="placeholder"
        :disabled="disabled"
        :maxlength="maxlength"
        :autocomplete="autocomplete"
        :inputmode="inputmode"
        class="ipt"
        @input="onInput"
        @focus="focused = true"
        @blur="focused = false"
        @keydown.enter="emit('enter', $event)"
      />
      <!-- 多行 -->
      <textarea
        v-else
        ref="inputEl"
        :value="modelValue"
        :placeholder="placeholder"
        :disabled="disabled"
        :maxlength="maxlength"
        :rows="rows"
        class="ipt resize-none"
        @input="onInput"
        @focus="focused = true"
        @blur="focused = false"
      />

      <!-- 右后缀区 -->
      <!-- 密码眼睛 -->
      <button
        v-if="type === 'password'"
        type="button"
        class="suffix-btn"
        :aria-label="showPwd ? '隐藏密码' : '显示密码'"
        @click="showPwd = !showPwd"
      >
        <svg viewBox="0 0 24 24" class="ic-sm">
          <path d="M2 12s3.5-7 10-7 10 7 10 7-3.5 7-10 7S2 12 2 12Z" />
          <circle cx="12" cy="12" r="3" />
          <path v-if="!showPwd" d="m4 4 16 16" />
        </svg>
      </button>

      <!-- 搜索清除 -->
      <button
        v-else-if="type === 'search' && modelValue"
        type="button"
        class="suffix-btn"
        aria-label="清除"
        @click="clear"
      >
        <svg viewBox="0 0 24 24" class="ic-sm">
          <circle cx="12" cy="12" r="9" />
          <path d="m9 9 6 6M15 9l-6 6" />
        </svg>
      </button>

      <!-- 成功勾 -->
      <svg
        v-else-if="success"
        viewBox="0 0 24 24"
        class="ic-sm text-ok"
        aria-hidden="true"
      >
        <path d="m5 13 4 4L19 7" />
      </svg>

      <slot name="suffix" />
    </div>

    <!-- 底线提示：错误优先，其次成功，其次普通 tip -->
    <div v-if="msg || showCount" class="flex items-start gap-2 mt-1.5 px-0.5">
      <div
        v-if="msg"
        class="text-[11px] leading-snug flex-1"
        :class="error ? 'text-bad' : success ? 'text-ok' : 'text-fg-dim'"
      >{{ msg }}</div>
      <div v-else class="flex-1" />
      <div
        v-if="showCount"
        class="text-[10.5px] font-mono text-fg-dim/70 shrink-0"
      >{{ (modelValue || '').length }}<template v-if="maxlength">/{{ maxlength }}</template></div>
    </div>
  </div>
</template>

<script setup>
import { computed, ref, watch, nextTick } from 'vue'

const props = defineProps({
  modelValue: { type: [String, Number], default: '' },
  /** text | search | password | email | number | tel */
  type: { type: String, default: 'text' },
  label: { type: String, default: '' },
  placeholder: { type: String, default: '' },
  msg: { type: String, default: '' },
  error: { type: Boolean, default: false },
  success: { type: Boolean, default: false },
  disabled: { type: Boolean, default: false },
  required: { type: Boolean, default: false },
  multiline: { type: Boolean, default: false },
  rows: { type: Number, default: 3 },
  maxlength: { type: [String, Number], default: undefined },
  showCount: { type: Boolean, default: false },
  autocomplete: { type: String, default: 'off' },
  inputmode: { type: String, default: undefined },
})

const emit = defineEmits(['update:modelValue', 'enter', 'clear'])

const inputEl = ref(null)
const focused = ref(false)
const showPwd = ref(false)
const shake = ref(false)

const actualType = computed(() => {
  if (props.type === 'password') return showPwd.value ? 'text' : 'password'
  if (props.type === 'search') return 'search'
  return props.type
})

function onInput(e) {
  emit('update:modelValue', e.target.value)
}

function clear() {
  emit('update:modelValue', '')
  emit('clear')
  nextTick(() => inputEl.value?.focus())
}

// 报错抖动：只在 error 由 false → true 的那一次抖，
// 否则页面重渲染时每次都会抖，非常烦人。
watch(
  () => props.error,
  (now, before) => {
    if (now && !before) {
      shake.value = true
      setTimeout(() => (shake.value = false), 500)
    }
  }
)

/** 供外部聚焦 */
defineExpose({
  focus: () => inputEl.value?.focus(),
  blur: () => inputEl.value?.blur(),
})
</script>

<style scoped>
.input-shell {
  display: flex;
  gap: 8px;
  padding: 10px 14px;
  background: var(--card);
  border: 1.5px solid var(--border);
  border-radius: var(--r-sm);
  transition: border-color var(--dur-2) var(--ease-io),
    box-shadow var(--dur-2) var(--ease-io),
    background-color var(--dur-3) var(--ease-io);
  cursor: text;
}
/* 聚焦光圈：规范里的 3px ring */
.input-shell:focus-within {
  border-color: var(--primary);
  box-shadow: 0 0 0 3px var(--ring);
}
.input-shell.input-error {
  border-color: var(--danger);
}
.input-shell.input-error:focus-within {
  border-color: var(--danger);
  box-shadow: 0 0 0 3px var(--danger-soft);
}
.input-shell.is-success {
  border-color: var(--success);
}
.input-shell.is-success:focus-within {
  border-color: var(--success);
  box-shadow: 0 0 0 3px var(--success-soft);
}
.input-shell.is-disabled {
  background: var(--card-2);
  opacity: 0.6;
  cursor: not-allowed;
}

.ipt {
  flex: 1;
  min-width: 0;
  border: none;
  outline: none;
  background: transparent;
  font-size: 14px;
  color: var(--text);
  font-family: var(--font);
  line-height: 1.5;
}
.ipt::placeholder {
  color: var(--text-disabled);
}
.ipt:disabled {
  cursor: not-allowed;
}
/* 干掉 search 类型浏览器自带的 ✕，我们用统一的 */
.ipt::-webkit-search-cancel-button {
  -webkit-appearance: none;
  appearance: none;
}

.ic {
  width: 17px;
  height: 17px;
  flex-shrink: 0;
  stroke: var(--text-disabled);
  fill: none;
  stroke-width: 2;
  stroke-linecap: round;
  stroke-linejoin: round;
  margin-top: 1px;
}
.ic-sm {
  width: 16px;
  height: 16px;
  stroke: currentColor;
  fill: none;
  stroke-width: 2;
  stroke-linecap: round;
  stroke-linejoin: round;
}
.suffix-btn {
  flex-shrink: 0;
  display: flex;
  align-items: center;
  justify-content: center;
  color: var(--text-disabled);
  background: none;
  border: none;
  padding: 2px;
  cursor: pointer;
  transition: color var(--dur-1) var(--ease-io);
  margin-top: 1px;
}
.suffix-btn:hover {
  color: var(--primary);
}
</style>