// BaseSkeleton.vue —— 骨架屏（规范 .skl / shimmer）
//
// 为什么不用转圈：
//   转圈只告诉用户「在等」，骨架屏还告诉他「马上会出现什么结构」，
//   页面切换时视觉连续性更好，感知等待时间更短。
//
// 用法：
//   基础条：  <BaseSkeleton width="60%" />
//   多行文字：<BaseSkeleton :lines="3" />
//   卡片块：  <BaseSkeleton height="96px" rounded="var(--r-md)" />
<template>
  <!-- 多行模式：最后一行短一些，看起来像真段落 -->
  <div v-if="lines > 1">
    <div
      v-for="i in lines"
      :key="i"
      class="skl skl-line"
      :style="{
        width: i === lines ? shortWidth : '100%',
        height: height,
      }"
    />
  </div>
  <!-- 单块模式 -->
  <div
    v-else
    class="skl"
    :style="{ width, height, borderRadius: rounded }"
  />
</template>

<script setup>
defineProps({
  /** 多行：行数 */
  lines: { type: Number, default: 1 },
  /** 单块宽度（多行模式下无效） */
  width: { type: String, default: '100%' },
  /** 单块高度；多行模式下作为每行高度 */
  height: { type: String, default: '12px' },
  /** 圆角 */
  rounded: { type: String, default: 'var(--r-sm)' },
  /** 多行模式最后一行的宽度（制造自然段落感） */
  shortWidth: { type: String, default: '62%' },
})
</script>