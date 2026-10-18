// BaseSwitch.vue —— 开关（规范 .toggle，含拉伸阻尼）
//
// 规范的巧思：按下时滑块**横向拉长**（width 21px → 26px），
// 松开后按新位置归位。这一点点变形让开关有了「橡皮筋」的物理感，
// 是整份规范里最便宜、性价比最高的动效之一。
//
// 实现落在 style.css 的 .toggle 上（用 :active 伪类），
// 本组件只负责状态和可访问性。
<template>
  <button
    type="button"
    role="switch"
    :aria-checked="modelValue ? 'true' : 'false'"
    :aria-label="label || undefined"
    :disabled="disabled"
    class="toggle"
    :class="[modelValue ? 'on' : '', sm ? 'sm' : '', disabled ? 'opacity-40' : '']"
    @click="toggle"
  />
</template>

<script setup>
const props = defineProps({
  modelValue: { type: Boolean, default: false },
  /** 小号（38x22） */
  sm: { type: Boolean, default: false },
  disabled: { type: Boolean, default: false },
  label: { type: String, default: '' },
})

const emit = defineEmits(['update:modelValue', 'change'])

function toggle() {
  if (props.disabled) return
  const next = !props.modelValue
  emit('update:modelValue', next)
  emit('change', next)
}
</script>