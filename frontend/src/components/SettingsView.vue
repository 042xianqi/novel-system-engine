<script setup>
// SettingsView.vue —— 设置页入口（2026-10-20 重构后仅作壳）
//
// 真正的实现搬到了 components/panes/SettingsPane.vue（与其它 pane 同构）。
// 保留原文件名与默认导出形态，App.vue 的 import 不用改，页面即刻生效。
import SettingsPane from './panes/SettingsPane.vue'
</script>

<template>
  <SettingsPane />
</template>
