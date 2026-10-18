import { defineConfig } from 'vite'
import vue from '@vitejs/plugin-vue'

// Vite 配置 —— 为 ne-agent 的零依赖静态托管服务优化
//
// 关键决策：
// 1. base: './'  → 产出相对路径，dist 可以被任意挂载点托管（不写死 /assets/）
// 2. build.target: 'es2020' → 不用太新语法，Android WebView 兼容
// 3. 关闭 sourcemap（生产体积小）；dev 时 Vite 自带
// 4. proxy: 让 `npm run dev` 时直接代理到 8270 后端，前端可热更新
//
// 双模式打包（2026-10-17 新增）：
//   · 默认（分块）  → 供 8270 静态托管；vue 单独一块利于缓存
//   · SINGLE=1      → 单文件模式，全部内联进一个 JS。
//     用于打 ToolPkg 的 resources/web_panel.html（包内资源不走 HTTP，
//     拆分的 ./vue-xxxx.js 相对路径必然 404 → 白屏）。
//     跑法：SINGLE=1 npx vite build --outDir dist-single
const SINGLE = process.env.SINGLE === '1'
export default defineConfig({
  plugins: [vue()],
  base: './',
  build: {
    outDir: SINGLE ? 'dist-single' : 'dist',
    emptyOutDir: true,
    target: 'es2020',
    sourcemap: false,
    assetsDir: 'assets',
    // 单文件超 500KB 才警告（我们核心目标是体积小、加载快）
    chunkSizeWarningLimit: 600,
    rollupOptions: {
      output: SINGLE
        ? {
            // 单文件模式：禁止拆分，动态 import 全部内联
            inlineDynamicImports: true,
            manualChunks: undefined,
          }
        : {
            // 托管模式：手动分包，vue 单独一块（利缓存）
            manualChunks: {
              vue: ['vue'],
            },
          },
    },
  },
  server: {
    port: 5173,
    host: '127.0.0.1',
    proxy: {
      // dev 模式下把所有 /api 请求代理到 Rust 后端
      '/api': {
        target: 'http://127.0.0.1:8270',
        changeOrigin: true
      }
    }
  }
})