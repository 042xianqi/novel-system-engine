// main.js —— 应用入口
//
// 只做四件事：注入皮肤 Token、引入样式、创建应用、挂载。
// 所有业务逻辑都在组件与 composables 里，这里不写任何逻辑。
//
// 顺序很重要：
//   ① skin.css（第 0 层变量）必须先于 style.css 引入，
//      因为 style.css 的 @layer base 一上来就用 var(--bg)，
//      变量没定义的话首帧会闪一下无样式。
//   ② 皮肤值必须早于首帧写进 <html data-skin>，
//      否则会看到「默认青蓝 → 用户皮肤」的硬切。
import { createApp } from 'vue'
import App from './App.vue'
import './skin.css'
import './style.css'
import { initSkin } from './composables/useSkin.js'

// 首帧前就套上皮肤（读 localStorage，无则用默认）
initSkin()

createApp(App).mount('#app')