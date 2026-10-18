<script setup>
// SkinPicker.vue —— 主题皮肤切换器（6 个色点）
//
// 规范里的 .skin-pill / .skin-dot 是「一个药丸 + 一个纯色圆点」。
// 这里做成两段式：
//   上：6 个色点，横排，当前项外圈加一圈 ring，点谁切谁
//   下：当前皮肤的「点名 + 描述」，让用户知道自己选了什么
//
// 色点是唯一允许写死颜色的地方（规范原文明确说明：
// "色点固定为该皮肤取样色，属于皮肤样本而非 UI"）。
import { SKINS, skinState, setSkin } from '../../composables/useSkin.js'
import { computed } from 'vue'

const current = computed(() => SKINS.find((s) => s.id === skinState.current))
</script>

<template>
  <div class="skin-picker">
    <!-- 色点行 -->
    <div class="flex items-center gap-3">
      <button
        v-for="s in SKINS"
        :key="s.id"
        type="button"
        class="skin-dot-btn"
        :class="skinState.current === s.id ? 'active' : ''"
        :aria-label="`切换到${s.name}皮肤`"
        :aria-pressed="skinState.current === s.id ? 'true' : 'false'"
        :title="`${s.name} · ${s.desc}`"
        @click="setSkin(s.id)"
      >
        <span class="dot" :style="{ background: s.dot }" />
      </button>
    </div>

    <!-- 当前皮肤说明 -->
    <div class="mt-2.5 flex items-baseline gap-2">
      <span class="text-[12.5px] font-semibold text-fg">
        {{ current?.name || '青蓝' }}
      </span>
      <span class="text-[11px] text-fg-dim">{{ current?.desc }}</span>
      <span v-if="current?.dark" class="tag tag-primary !py-0 !px-1.5 !text-[10px]">
        深色
      </span>
    </div>
  </div>
</template>

<script>
export default { name: 'SkinPicker' }
</script>

<style scoped>
/* 色点按钮：本体透明，靠外圈 ring 表达选中态，
   这样色点本身永远是"纯皮肤取样色"，不会被 UI 效果污染 */
.skin-dot-btn {
  width: 34px;
  height: 34px;
  border-radius: 50%;
  display: flex;
  align-items: center;
  justify-content: center;
  background: transparent;
  border: none;
  cursor: pointer;
  padding: 0;
  transition: transform var(--dur-2) var(--ease-spring),
    box-shadow var(--dur-2) var(--ease-out);
  position: relative;
}
.skin-dot-btn:hover {
  transform: scale(1.08);
}
.skin-dot-btn:active {
  transform: scale(0.94);
}
.skin-dot-btn .dot {
  width: 24px;
  height: 24px;
  border-radius: 50%;
  box-shadow: inset 0 0 0 1px rgb(255 255 255 / 0.25);
  transition: width var(--dur-2) var(--ease-spring),
    height var(--dur-2) var(--ease-spring);
}
/* 选中：外圈 2px 主题色环 + 白/黑描边分隔，避免环与色点糊在一起 */
.skin-dot-btn.active {
  box-shadow: 0 0 0 2px var(--card), 0 0 0 4px var(--primary);
}
.skin-dot-btn.active .dot {
  width: 27px;
  height: 27px;
}
</style>