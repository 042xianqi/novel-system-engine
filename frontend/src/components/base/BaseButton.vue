// BaseButton.vue —— 基础按钮（规范第 2 层按钮全变体）
//
// 支持：
//   变体 primary / soft / outline / outline-gray / ghost / danger /
//        danger-soft / danger-text
//   尺寸 sm / md / lg / block / icon
//   附加 loading（内嵌转圈 + 禁用） / glow（呼吸光晕）
//
// 与老 .btn 的关系：
//   老 .btn 是本文件的基础 class，样式已在 style.css 里 Token 化。
//   这里只负责「拼 class + 处理交互 + 涟漪」，不重复定义颜色。
<template>
  <button
    :type="type"
    class="btn"
    :class="classes"
    :disabled="disabled || loading"
    :aria-disabled="disabled || loading ? 'true' : undefined"
    :aria-busy="loading ? 'true' : undefined"
    @pointerdown="onTap"
    @click="onClick"
  >
    <!-- loading 时把图标换成转圈，避免布局跳动 -->
    <span v-if="loading" class="spin" aria-hidden="true" />
    <slot v-else name="icon" />
    <span v-if="!iconOnly" class="truncate"><slot /></span>
  </button>
</template>

<script setup>
import { computed } from 'vue'
import { useRipple } from '../../composables/useRipple.js'

const props = defineProps({
  /** primary | soft | outline | outline-gray | ghost | danger | danger-soft | danger-text */
  variant: { type: String, default: 'primary' },
  /** sm | md | lg */
  size: { type: String, default: 'md' },
  /** 占满整行 */
  block: { type: Boolean, default: false },
  /** 纯图标圆钮（40x40） */
  iconOnly: { type: Boolean, default: false },
  /** 加载中：转圈 + 禁用 + 防止重复提交 */
  loading: { type: Boolean, default: false },
  disabled: { type: Boolean, default: false },
  /** 呼吸光晕（用于页面上唯一的主行动按钮） */
  glow: { type: Boolean, default: false },
  type: { type: String, default: 'button' },
})

const emit = defineEmits(['click'])

const { onTap } = useRipple()

const VARIANT_CLASS = {
  primary: 'btn-primary',
  soft: 'btn-soft',
  outline: 'btn-outline',
  'outline-gray': 'btn-outline-gray',
  ghost: 'btn-ghost',
  danger: 'btn-danger',
  'danger-soft': 'btn-danger-soft',
  'danger-text': 'btn-danger-text',
}
const SIZE_CLASS = {
  sm: 'btn-sm',
  md: '',
  lg: 'btn-lg',
}

const classes = computed(() => [
  VARIANT_CLASS[props.variant] || 'btn-primary',
  SIZE_CLASS[props.size] || '',
  props.block ? 'btn-block' : '',
  props.iconOnly ? 'btn-icon' : '',
  props.loading ? 'btn-loading' : '',
  props.glow ? 'glow-btn' : '',
])

function onClick(e) {
  if (props.disabled || props.loading) {
    e.preventDefault()
    e.stopPropagation()
    return
  }
  emit('click', e)
}
</script>
