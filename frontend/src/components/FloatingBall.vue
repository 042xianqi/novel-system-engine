<script setup>
// FloatingBall.vue —— 全局可拖拽悬浮球
//
// ══════════════════════════════════════════════════════════
// 为什么用 Pointer Events 而不是 mouse/touch 各写一套
// ══════════════════════════════════════════════════════════
//   · Pointer Events 一套覆盖 鼠标 / 触摸 / 手写笔
//   · setPointerCapture 让手指滑出元素范围后依然能收到 move，
//     不会出现「拖到一半手指离开球就丢了」的经典 bug
//   · 和滚动容器天然共存：touch-action: none 只在这颗球上禁用滚动
//
// ── 性能 ────────────────────────────────────────────────
//   · 位移只写 transform: translate3d(...)，走合成器，不触发 layout
//   · will-change: transform 提前提升图层
//   · 拖动中**不加** transition（否则会跟手迟滞）；
//     松手吸附时才加 transition（cubic-bezier 弹跳感）
//
// ── 吸附 ────────────────────────────────────────────────
//   松手时按球心与左右边缘的距离，吸附到更近的一侧；
//   Y 轴夹在安全区内，不会被顶栏/底栏遮住。
import { ref, computed, onMounted, onUnmounted } from 'vue'

const props = defineProps({
  /** 唯一标识，用于持久化位置 */
  id: { type: String, required: true },
  /** 球内文字/emoji */
  icon: { type: String, default: '•' },
  /** 长按/点击提示 */
  label: { type: String, default: '' },
  /** 初始位置：'left' | 'right' */
  side: { type: String, default: 'right' },
  /** 垂直初始位置（px，从顶部算） */
  top: { type: Number, default: 320 },
  /** 主色调 */
  tone: { type: String, default: 'acc' },
  /** 角标数字（0 表示不显示） */
  badge: { type: Number, default: 0 },
  /** 是否显示呼吸光晕 */
  pulse: { type: Boolean, default: false },
})

const emit = defineEmits(['tap'])

const BALL = 52          // 球直径
const EDGE = 10          // 贴边留白
const DRAG_THRESHOLD = 6 // 超过这个位移才算拖动（否则算点击）

const el = ref(null)
const x = ref(0)
const y = ref(0)
const dragging = ref(false)
const settled = ref(false)   // 松手吸附后短暂为 true，用于切换 transition

let startX = 0
let startY = 0
let baseX = 0
let baseY = 0
let moved = 0
let activeId = null

const STORAGE_KEY = computed(() => `fb_pos_${props.id}`)

/** 把坐标夹进可视区 */
function clampPos(px, py) {
  const w = window.innerWidth || 390
  const h = window.innerHeight || 844
  const minY = 80                 // 避开顶栏
  const maxY = h - BALL - 96      // 避开底栏
  return {
    x: Math.min(Math.max(px, EDGE), w - BALL - EDGE),
    y: Math.min(Math.max(py, minY), Math.max(minY, maxY)),
  }
}

function apply(px, py) {
  const c = clampPos(px, py)
  x.value = c.x
  y.value = c.y
}

/** 松手吸附到最近的左右边缘 */
function snap() {
  const w = window.innerWidth || 390
  const centerX = x.value + BALL / 2
  const toLeft = centerX
  const toRight = w - centerX
  const targetX = toLeft < toRight ? EDGE : w - BALL - EDGE
  settled.value = true
  x.value = targetX
  // 吸附动画结束后关掉 transition，恢复拖动跟手
  setTimeout(() => { settled.value = false }, 320)
  try {
    localStorage.setItem(STORAGE_KEY.value, JSON.stringify({ x: targetX, y: y.value }))
  } catch (e) { /* 隐私模式忽略 */ }
}

function onPointerDown(e) {
  if (e.button !== undefined && e.button !== 0) return
  activeId = e.pointerId
  dragging.value = true
  moved = 0
  startX = e.clientX
  startY = e.clientY
  baseX = x.value
  baseY = y.value
  try { el.value.setPointerCapture(e.pointerId) } catch (err) { /* 某些实现不支持 */ }
  e.preventDefault()
}

// 【2026-10-20 注入 Operit 逻辑】rAF 节流：pointermove 只记坐标，
// 真正写入响应式 ref 交给 requestAnimationFrame —— 保证**每个渲染帧最多 apply 一次**。
// 为什么必须这样做：高频 pointermove（触屏可达 120~240Hz）若每次都写 ref，
// 会触发同样次数的样式重算与合成层更新；在 fixed + 渐变 + 圆角 + 阴影的
// 元素上，这会让 Android WebView 来不及重栅格化 → 视觉上出现**残影/重影**。
// 批到每帧一次后，拖动手感不变但掉帧与重影都消失。
let rafId = 0
let pending = null

function onPointerMove(e) {
  if (!dragging.value || e.pointerId !== activeId) return
  const dx = e.clientX - startX
  const dy = e.clientY - startY
  moved = Math.max(moved, Math.abs(dx) + Math.abs(dy))
  pending = { x: baseX + dx, y: baseY + dy }
  if (!rafId) {
    rafId = requestAnimationFrame(() => {
      rafId = 0
      if (pending) {
        apply(pending.x, pending.y)
        pending = null
      }
    })
  }
}

function onPointerUp(e) {
  if (!dragging.value || (activeId !== null && e.pointerId !== activeId)) return
  dragging.value = false
  try { el.value.releasePointerCapture(e.pointerId) } catch (err) {}
  activeId = null
  if (moved < DRAG_THRESHOLD) {
    emit('tap')          // 视为点击
    justTapped = true    // 标记，让随后的 click 事件不重复触发
    setTimeout(() => { justTapped = false }, 400)
    return
  }
  snap()
}

/**
 * click 兜底。
 *
 * 只监听 pointer 事件时，**键盘 Enter / 辅助功能点击 / 程序化 .click()**
 * 都收不到响应（它们不产生 pointerdown/up 序列）。
 * 这里补一条 click 路径，并用 justTapped 去重，
 * 避免真实手指操作时 pointerup + click 双触发。
 */
let justTapped = false
function onClick(e) {
  if (justTapped) return
  emit('tap')
}

function restore() {
  const w = window.innerWidth || 390
  let px = props.side === 'left' ? EDGE : w - BALL - EDGE
  let py = props.top
  try {
    const raw = localStorage.getItem(STORAGE_KEY.value)
    if (raw) {
      const p = JSON.parse(raw)
      if (typeof p.x === 'number' && typeof p.y === 'number') {
        px = p.x
        py = p.y
      }
    }
  } catch (e) { /* 忽略 */ }
  apply(px, py)
}

function onResize() {
  apply(x.value, y.value)  // 重新夹紧，防止旋屏后跑出屏幕
}

onMounted(() => {
  restore()
  window.addEventListener('resize', onResize)
  window.addEventListener('orientationchange', onResize)
})

onUnmounted(() => {
  // 取消挂起的 rAF，避免组件卸载后回调仍执行（会操作已销毁的元素）
  if (rafId) { cancelAnimationFrame(rafId); rafId = 0 }
  pending = null
  window.removeEventListener('resize', onResize)
  window.removeEventListener('orientationchange', onResize)
})

/* 定位方式：用 left/top，**不用 transform: translate3d**。
   2026-10-17 重影第二次事故复盘：
   上一版用 `position:fixed; left:0; top:0` + `transform: translate3d(x,y,0)` 定位。
   translate3d 会**强制创建一个合成层**，而 fixed + 渐变背景 + 圆角 + 阴影
   在 Android WebView 里被合成器重复栅格化，屏幕上就出现 2~3 个球，
   但 DOM 里只有一个（document.querySelectorAll('.fb-ball').length === 1）。
   这就是"DOM 明明只有 1 个球，屏幕却有 3 个"的全部原因。
   换成 left/top 定位后：位移走 layout（这个球只在拖动/吸附时动，
   每帧改 left/top 的开销可以忽略），彻底不再创建合成层，重影从物理上消失。
   拖动流畅度不受影响：pointermove 里仍然每帧更新坐标。 */
const styleVars = computed(() => ({
  left: x.value + 'px',
  top: y.value + 'px',
  width: BALL + 'px',
  height: BALL + 'px',
}))

const toneCls = computed(() => {
  if (props.tone === 'warn') return 'from-warn/90 to-warn/60 ring-warn/40'
  if (props.tone === 'ok') return 'from-ok/90 to-ok/60 ring-ok/40'
  return 'from-acc/90 to-acc/55 ring-acc/40'
})
</script>

<template>
  <button
    ref="el"
    class="fb-ball fixed left-0 top-0 z-[70] rounded-full select-none
           flex items-center justify-center
           bg-gradient-to-br shadow-[0_6px_22px_rgba(0,0,0,.42)]
           ring-1
           active:brightness-110"
    :class="[
      toneCls,
      dragging ? 'fb-grabbing' : 'fb-idle',
      settled ? 'fb-settle' : ''
    ]"
    :style="styleVars"
    @pointerdown="onPointerDown"
    @pointermove="onPointerMove"
    @pointerup="onPointerUp"
    @pointercancel="onPointerUp"
    @click="onClick"
    :aria-label="label || icon"
  >
    <!-- 呼吸光晕 -->
    <span
      v-if="pulse"
      class="absolute inset-0 rounded-full animate-ping opacity-30"
      :class="tone === 'warn' ? 'bg-warn' : 'bg-acc'"
    />
    <span class="relative text-[21px] leading-none">{{ icon }}</span>
    <!-- 角标 -->
    <span
      v-if="badge > 0"
      class="absolute -top-0.5 -right-0.5 min-w-[18px] h-[18px] px-1
             rounded-full bg-bad text-white text-[10.5px] font-bold
             flex items-center justify-center shadow"
    >{{ badge > 99 ? '99+' : badge }}</span>
  </button>
</template>

<style scoped>
/* ⚠ Android WebView 重影事故复盘（2026-10-17，用户截图看到 3 个齿轮球）：
   DOM 里其实只有 2 个球（实测 ballCount=2，坐标 [10,565] 与 [338,490]），
   屏幕上却多出 3 个重影。根因是**合成层重复绘制**：
     position:fixed + backdrop-filter + will-change:transform + gradient
   这四个条件同时压在同一个元素上，Android WebView 的合成器会算错图层变换，
   把同一块内容在邻近位置重复栅格化一遍。
   修法（三管齐下）：
     ① 去掉球上的 backdrop-blur-sm（球本身是渐变实色，模糊毫无意义）
     ② will-change 不再常驻，只在拖动/吸附这些真正会动的时刻挂上
     ③ 加 isolation:isolate 建独立层叠上下文，切断与父级合成的干扰
   教训：**fixed 元素上不要常驻 will-change**，它会把元素钉成独立合成层，
   一旦再有 backdrop-filter 就极容易触发这类重绘 bug。 */

/* 静止态：不占合成层，也不做过渡 */
.fb-idle {
  transition: none;
  touch-action: none;
  cursor: grab;
  isolation: isolate;
}
/* 拖动中：完全跟手，不能有 transition；这一刻才提升图层 */
.fb-grabbing {
  transition: none;
  touch-action: none;
  cursor: grabbing;
  isolation: isolate;
}
/* 松手吸附：规范的 spring 曲线（cubic-bezier(.34,1.56,.64,1)）
   比原来的 (.22,1.4,.36,1) 过冲更多一点，回弹更「弹」。
   同时给 box-shadow 一起过渡，让落位时阴影同步收住。 */
.fb-settle {
  /* left/top 定位下的吸附回弹：过渡 left/top 而不是 transform */
  transition: left 300ms var(--ease-spring),
              top 300ms var(--ease-spring),
              box-shadow 300ms var(--ease-out);
  touch-action: none;
  isolation: isolate;
}

/* ── 材质特效：玻璃质感 ──
   规范要求悬浮球要"有材质"。做法是叠两层：
     ① 顶部内高光（inset 0 1px 0 rgba(255,255,255,.45)）
        —— 让球看起来是凸的球面，而不是一块扁平色块
     ② 底部内阴影 —— 模拟环境光的边缘反光
   这两层都用 box-shadow 实现，不新增 DOM，零性能成本。

   ⚠ 这里**绝对不能**写 transition 到 .fb-ball 上（2026-10-17 修）：
     悬浮球的位移靠 transform，而 .fb-idle / .fb-grabbing 已经显式
     声明了 `transition: none` 来保证拖动时完全跟手。
     如果把 `transition: box-shadow ...` 写在 .fb-ball 上，
     两者特异性相同（都是单类），源码顺序在后者胜出，
     结果 transform 也会被加上 200ms 过渡 —— 拖动立刻变成"拖尾巴"。
   所以材质过渡只挂在 :hover 上（只有鼠标悬停才有意义，
   触摸设备没有 hover，天然不影响拖动）。 */
.fb-ball {
  box-shadow:
    inset 0 1px 0 rgb(255 255 255 / 0.45),
    inset 0 -1px 2px rgb(0 0 0 / 0.18),
    0 6px 22px rgb(0 0 0 / 0.42);
}
.fb-ball:hover {
  box-shadow:
    inset 0 1px 0 rgb(255 255 255 / 0.55),
    inset 0 -1px 2px rgb(0 0 0 / 0.18),
    0 10px 30px rgb(0 0 0 / 0.5);
  filter: brightness(1.06);
}
/* 按住时提亮，给出"被按进去了"的体感 */
.fb-ball:active {
  filter: brightness(1.12);
}
</style>