// BaseCell.vue —— 设置单元格（规范 .cell）
//
// 用于设置中心 / 任意「一行一个配置项」的场景。
//
// 右侧插槽的三种常见形态（规范都给了）：
//   · 箭头  → 点进去还有下一级
//   · 开关  → 就地开关（配合 BaseSwitch）
//   · 数值  → 只读展示（如「已上锁」）
// 另有 danger 变体：文字变红，用于「清空数据」这类不可逆操作。
<template>
  <!-- 可点 → button；纯展示 → div。用 is 切换保持语义正确 -->
  <component
    :is="clickable ? 'button' : 'div'"
    :type="clickable ? 'button' : undefined"
    class="cell"
    :class="[
      danger ? 'is-danger' : '',
      clickable ? '' : 'no-tap',
    ]"
    :aria-disabled="disabled ? 'true' : undefined"
    @click="onClick"
  >
    <!-- 左图标 -->
    <span
      v-if="icon || $slots.icon"
      class="cell-ic"
      :class="iconToneClass"
      aria-hidden="true"
    >
      <slot name="icon">{{ icon }}</slot>
    </span>

    <!-- 主文案 -->
    <span class="cell-main">
      <span class="cell-title block">{{ title }}</span>
      <span v-if="sub" class="cell-sub block">{{ sub }}</span>
    </span>

    <!-- 右侧：默认插槽 > value > 箭头 -->
    <slot>
      <span v-if="value !== '' && value != null" class="cell-val">{{ value }}</span>
      <svg
        v-if="arrow"
        class="cell-arrow"
        viewBox="0 0 24 24"
        aria-hidden="true"
      >
        <path d="m9 6 6 6-6 6" />
      </svg>
    </slot>
  </component>
</template>

<script setup>
import { computed } from 'vue'
import { useRipple } from '../../composables/useRipple.js'

const props = defineProps({
  title: { type: String, default: '' },
  sub: { type: String, default: '' },
  /** 左图标（emoji 或单字符），也可用 icon 插槽 */
  icon: { type: String, default: '' },
  /** primary | success | warning | danger */
  iconTone: { type: String, default: 'primary' },
  /** 右侧数值（只读展示） */
  value: { type: [String, Number], default: '' },
  /** 是否显示右箭头 */
  arrow: { type: Boolean, default: false },
  /** 危险操作：文字变红 */
  danger: { type: Boolean, default: false },
  disabled: { type: Boolean, default: false },
  /** 是否可点（默认有 click 监听就可点） */
  clickable: { type: Boolean, default: true },
})

const emit = defineEmits(['click'])
const { onTap } = useRipple()

const iconToneClass = computed(
  () => `tone-${props.iconTone}`
)

function onClick(e) {
  if (props.disabled || !props.clickable) return
  onTap(e)
  emit('click', e)
}
</script>

<style scoped>
.cell {
  width: 100%;
  text-align: left;
  font-family: var(--font);
  /* 危险态 */
}
.cell.is-danger .cell-title {
  color: var(--danger);
}
.cell.is-danger .cell-ic {
  background: var(--danger-soft);
  color: var(--danger);
}
.cell.no-tap {
  cursor: default;
}
.cell[aria-disabled='true'] {
  opacity: 0.5;
  cursor: not-allowed;
}

/* 图标底色变体 */
.cell-ic.tone-primary {
  background: var(--primary-soft);
  color: var(--primary);
}
.cell-ic.tone-success {
  background: var(--success-soft);
  color: var(--success);
}
.cell-ic.tone-warning {
  background: var(--warning-soft);
  color: var(--warning);
}
.cell-ic.tone-danger {
  background: var(--danger-soft);
  color: var(--danger);
}

.cell-ic {
  font-size: 16px;
  line-height: 1;
}

.cell-arrow {
  width: 18px;
  height: 18px;
  stroke: var(--text-disabled);
  fill: none;
  stroke-width: 2;
  stroke-linecap: round;
  stroke-linejoin: round;
  flex-shrink: 0;
}
</style>